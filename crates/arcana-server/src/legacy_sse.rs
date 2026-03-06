//! Legacy SSE transport bridge (GET /sse + POST /message).
//!
//! Claude Code (as of v2.1.70) speaks the deprecated MCP SSE transport:
//!   - GET /sse → opens an SSE stream, receives an `endpoint` event
//!   - POST /message?sessionId=… → sends JSON-RPC requests, gets 202 back
//!   - Responses arrive as `message` events on the SSE stream
//!
//! This module bridges that protocol to rmcp's async-rw transport so the
//! same ArcanaServer handles both legacy SSE and streamable HTTP clients.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;

use crate::ArcanaServer;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

type SessionMap = Arc<RwLock<HashMap<String, SessionHandle>>>;

struct SessionHandle {
    /// Send JSON-RPC request lines here (from POST /message).
    tx: tokio::sync::mpsc::UnboundedSender<String>,
}

#[derive(Clone)]
pub struct LegacySseState {
    sessions: SessionMap,
    server_factory: Arc<dyn Fn() -> ArcanaServer + Send + Sync>,
}

impl LegacySseState {
    pub fn new(server_factory: impl Fn() -> ArcanaServer + Send + Sync + 'static) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            server_factory: Arc::new(server_factory),
        }
    }
}

// ---------------------------------------------------------------------------
// GET /sse — open SSE stream
// ---------------------------------------------------------------------------

pub async fn sse_handler(
    State(state): State<LegacySseState>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let session_id = uuid::Uuid::new_v4().to_string();

    // Duplex pairs: one for rmcp reads (client→server), one for writes (server→client).
    let (client_read, server_write) = tokio::io::duplex(16384);
    let (server_read, client_write) = tokio::io::duplex(16384);

    // Channel for POST /message → duplex writer.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id.clone(), SessionHandle { tx });
    }

    // Spawn rmcp server on the duplex transport.
    let server = (state.server_factory)();
    tokio::spawn(async move {
        use rmcp::ServiceExt;
        let transport = (server_read, server_write);
        match server.serve(transport).await {
            Ok(svc) => {
                let _ = svc.waiting().await;
            }
            Err(e) => tracing::warn!("legacy SSE session error: {e}"),
        }
    });

    // Forward POST bodies into the rmcp read-half.
    let mut writer = client_write;
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if writer.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
            if writer.write_all(b"\n").await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
    });

    // SSE stream: first event is `endpoint`, then relay JSON-RPC responses.
    let reader = BufReader::new(client_read);
    let session_id_for_stream = session_id.clone();
    let sessions = state.sessions.clone();

    let stream = async_stream::stream! {
        // Legacy SSE spec: first event tells the client where to POST.
        let endpoint = format!("/message?sessionId={session_id_for_stream}");
        yield Ok(Event::default().event("endpoint").data(endpoint));

        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            yield Ok(Event::default().event("message").data(trimmed.to_string()));
        }

        // Clean up when the stream closes.
        let mut sessions = sessions.write().await;
        sessions.remove(&session_id_for_stream);
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------------------------------------------------------------------------
// POST /message?sessionId=… — send JSON-RPC request
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct MessageParams {
    #[serde(rename = "sessionId")]
    session_id: String,
}

pub async fn message_handler(
    State(state): State<LegacySseState>,
    Query(params): Query<MessageParams>,
    body: String,
) -> impl IntoResponse {
    let sessions = state.sessions.read().await;
    if let Some(session) = sessions.get(&params.session_id) {
        if session.tx.send(body).is_ok() {
            return StatusCode::ACCEPTED;
        }
    }
    StatusCode::NOT_FOUND
}
