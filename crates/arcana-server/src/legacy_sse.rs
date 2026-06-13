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
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;

use crate::ArcanaServer;

// ---------------------------------------------------------------------------
// DoS bounds
// ---------------------------------------------------------------------------

/// Max concurrent legacy-SSE sessions. New connections beyond this are rejected
/// with 503 (after reaping stale ones), so abandoned streams can't accumulate.
const MAX_SESSIONS: usize = 64;
/// Sessions older than this are reaped on the next connection — a coarse idle
/// timeout for streams whose client vanished without a clean close.
const SESSION_MAX_AGE: Duration = Duration::from_secs(3600);
/// Bound on the per-session request channel; a client that floods POST /message
/// faster than the session drains gets 503, not unbounded memory growth.
const CHANNEL_BOUND: usize = 64;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

type SessionMap = Arc<RwLock<HashMap<String, SessionHandle>>>;

struct SessionHandle {
    /// Send JSON-RPC request lines here (from POST /message). Bounded.
    tx: tokio::sync::mpsc::Sender<String>,
    /// When the session opened — used to reap stale sessions.
    created_at: Instant,
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

pub async fn sse_handler(State(state): State<LegacySseState>) -> Response {
    // Reap stale/closed sessions, then enforce the concurrency cap. Dropping a
    // SessionHandle closes its channel, which cascades to shut down the session's
    // spawned tasks (forwarder → rmcp server → SSE stream all see EOF).
    {
        let mut sessions = state.sessions.write().await;
        sessions.retain(|_, s| s.created_at.elapsed() < SESSION_MAX_AGE && !s.tx.is_closed());
        if sessions.len() >= MAX_SESSIONS {
            tracing::warn!("legacy SSE session cap reached ({MAX_SESSIONS}); rejecting connection");
            return (StatusCode::SERVICE_UNAVAILABLE, "too many active sessions").into_response();
        }
    }

    let session_id = uuid::Uuid::new_v4().to_string();

    // Duplex pairs: one for rmcp reads (client→server), one for writes (server→client).
    let (client_read, server_write) = tokio::io::duplex(16384);
    let (server_read, client_write) = tokio::io::duplex(16384);

    // Bounded channel for POST /message → duplex writer.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(CHANNEL_BOUND);

    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(
            session_id.clone(),
            SessionHandle {
                tx,
                created_at: Instant::now(),
            },
        );
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
        yield Ok::<_, Infallible>(Event::default().event("endpoint").data(endpoint));

        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            yield Ok(Event::default().event("message").data(trimmed));
        }

        // Clean up when the stream closes.
        let mut sessions = sessions.write().await;
        sessions.remove(&session_id_for_stream);
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
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
    // Clone the sender and release the read lock before sending, so a slow send
    // never holds the session map locked.
    let tx = {
        let sessions = state.sessions.read().await;
        sessions.get(&params.session_id).map(|s| s.tx.clone())
    };

    use tokio::sync::mpsc::error::TrySendError;
    match tx {
        // try_send (not await) sheds load instead of tying up the handler when a
        // client floods faster than its session drains.
        Some(tx) => match tx.try_send(body) {
            Ok(()) => StatusCode::ACCEPTED,
            Err(TrySendError::Full(_)) => StatusCode::SERVICE_UNAVAILABLE,
            Err(TrySendError::Closed(_)) => StatusCode::NOT_FOUND,
        },
        None => StatusCode::NOT_FOUND,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn message_to_unknown_session_is_not_found() {
        let state = LegacySseState::new(|| unreachable!("factory not used in this test"));
        let resp = message_handler(
            State(state),
            Query(MessageParams {
                session_id: "nope".into(),
            }),
            "{}".into(),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn message_to_closed_session_is_not_found() {
        let state = LegacySseState::new(|| unreachable!());
        // A session whose receiver was dropped (forwarder ended) → channel closed.
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(CHANNEL_BOUND);
        drop(rx);
        state.sessions.write().await.insert(
            "s1".into(),
            SessionHandle {
                tx,
                created_at: Instant::now(),
            },
        );
        let resp = message_handler(
            State(state),
            Query(MessageParams {
                session_id: "s1".into(),
            }),
            "{}".into(),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
