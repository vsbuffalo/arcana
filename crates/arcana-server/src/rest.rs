//! REST API for the Arcana iOS app and other HTTP clients.
//!
//! All endpoints sit under `/api/` and use JSON request/response bodies.
//! Auth is shared with the MCP routes (bearer token or OAuth).

use std::sync::Arc;

use arcana_core::{Frontmatter, SearchFilters, SearchQuery, Vault};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::warn;

pub type SharedVault = Arc<tokio::sync::Mutex<Vault>>;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct SearchResultResponse {
    pub path: String,
    pub title: Option<String>,
    pub snippet: String,
    pub score: f64,
}

#[derive(Serialize)]
pub struct NoteResponse {
    pub path: String,
    pub title: String,
    pub tags: Vec<String>,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai: Option<AiMetaResponse>,
}

#[derive(Serialize)]
pub struct AiMetaResponse {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub confidence: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateNoteRequest {
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// "ai" or "user" — determines git commit identity
    #[serde(default = "default_author")]
    pub author: String,
}

#[derive(Deserialize)]
pub struct UpdateNoteRequest {
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub append: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// "ai" or "user" — determines git commit identity
    #[serde(default = "default_author")]
    pub author: String,
}

fn default_author() -> String {
    "user".to_string()
}

#[derive(Serialize)]
pub struct TagResponse {
    pub tag: String,
    pub count: usize,
}

#[derive(Serialize)]
pub struct TreeEntry {
    pub path: String,
    pub note_count: usize,
}

#[derive(Serialize)]
pub struct StatsResponse {
    pub total_notes: usize,
    pub total_tags: usize,
    pub total_links: usize,
}

#[derive(Serialize)]
pub struct ProvenanceResponse {
    pub path: String,
    pub total_lines: usize,
    pub human_lines: usize,
    pub ai_lines: usize,
    pub human_pct: f64,
    pub ai_pct: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<Vec<LineProvenanceResponse>>,
}

#[derive(Serialize)]
pub struct LineProvenanceResponse {
    pub line_no: usize,
    pub author: String,
    pub content: String,
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

struct ApiError {
    status: StatusCode,
    /// Client-facing message — must not contain internal paths or error detail.
    public: String,
    /// Full detail logged server-side; never sent to the client.
    internal: Option<String>,
}

impl ApiError {
    fn new(status: StatusCode, public: impl Into<String>) -> Self {
        Self {
            status,
            public: public.into(),
            internal: None,
        }
    }

    /// A 500 with a generic client message; the real cause (which may embed
    /// filesystem paths from libgit2/IO) is logged server-side, not returned.
    fn internal(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            public: "internal server error".into(),
            internal: Some(detail.into()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        if let Some(detail) = &self.internal {
            tracing::error!(status = %self.status, "rest api error: {detail}");
        }
        let body = serde_json::json!({ "error": self.public });
        (self.status, Json(body)).into_response()
    }
}

impl From<arcana_core::ArcanaError> for ApiError {
    fn from(e: arcana_core::ArcanaError) -> Self {
        match &e {
            // The relative note path is the caller's own input — safe to echo.
            arcana_core::ArcanaError::NoteNotFound(_) => {
                ApiError::new(StatusCode::NOT_FOUND, e.to_string())
            }
            _ => ApiError::internal(e.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

async fn get_note(
    State(vault): State<SharedVault>,
    Path(raw_path): Path<String>,
) -> Result<Json<NoteResponse>, ApiError> {
    let path = raw_path.strip_prefix('/').unwrap_or(&raw_path).to_string();
    let vault = vault.lock().await;
    let note = vault.read_note(&path).map_err(ApiError::from)?;
    let ai = note.frontmatter.ai.as_ref().map(|ai| AiMetaResponse {
        model: Some(ai.model.clone()),
        provider: Some(ai.provider.clone()),
        confidence: Some(format!("{:?}", ai.confidence).to_lowercase()),
    });
    Ok(Json(NoteResponse {
        path,
        title: note.title().to_string(),
        tags: note.frontmatter.tags.clone(),
        body: note.body.clone(),
        ai,
    }))
}

async fn update_note(
    State(vault): State<SharedVault>,
    Path(raw_path): Path<String>,
    Json(req): Json<UpdateNoteRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let path = raw_path.strip_prefix('/').unwrap_or(&raw_path).to_string();
    let vault = vault.lock().await;

    if let Some(ref tags) = req.tags {
        // Full read-modify-write for tag changes
        let mut note = vault.read_note(&path).map_err(ApiError::from)?;
        note.frontmatter.tags = tags.clone();
        if let Some(ref new_body) = req.body {
            note.body = new_body.clone();
        }
        if let Some(ref text) = req.append {
            note.body.push_str(text);
        }
        vault
            .write_note_content(&path, &note.to_string())
            .map_err(ApiError::from)?;
    } else {
        vault
            .update_note(&path, req.body.as_deref(), req.append.as_deref(), None)
            .map_err(ApiError::from)?;
    }

    // Git commit with appropriate author
    commit_with_author(&vault, &path, &req.author, "update");

    Ok(Json(serde_json::json!({ "updated": path })))
}

async fn create_note(
    State(vault): State<SharedVault>,
    Json(req): Json<CreateNoteRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let vault = vault.lock().await;
    let fm = if req.title.is_some() || !req.tags.is_empty() {
        Some(Frontmatter {
            title: req.title,
            tags: req.tags,
            ..Default::default()
        })
    } else {
        None
    };

    // create_note already does an AI commit internally, so for user-authored
    // notes we need to write manually and commit as human
    if req.author == "user" {
        let writer = arcana_core::NoteWriter::with_zones(
            vault.root(),
            vault.profile().zones(),
            vault.profile().projects(),
        );
        writer
            .create(&req.path, &req.body, fm)
            .map_err(ApiError::from)?;
        vault
            .reindex_paths(&[vault.root().join(&req.path)])
            .map_err(ApiError::from)?;
        commit_with_author(&vault, &req.path, "user", "create");
    } else {
        vault
            .create_note(&req.path, &req.body, fm)
            .map_err(ApiError::from)?;
    }

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "created": req.path })),
    ))
}

async fn delete_note_handler(
    State(vault): State<SharedVault>,
    Path(raw_path): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let path = raw_path.strip_prefix('/').unwrap_or(&raw_path).to_string();
    let vault = vault.lock().await;
    vault.delete_note(&path).map_err(ApiError::from)?;

    // Commit the deletion as human
    if let Some(git) = vault.git() {
        if let Err(e) = git.commit_human_change(&[vault.root().join(&path)]) {
            warn!("git commit for delete failed: {e}");
        }
    }

    Ok(Json(serde_json::json!({ "deleted": path })))
}

async fn search_notes(
    State(vault): State<SharedVault>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<Vec<SearchResultResponse>>, ApiError> {
    let vault = vault.lock().await;
    let query = SearchQuery {
        text: req.query,
        limit: req.limit,
        filters: SearchFilters {
            tag: req.tags.into_iter().next(),
            path_prefix: req.path_prefix,
            ..Default::default()
        },
    };
    let results = vault.search(&query).map_err(ApiError::from)?;
    let response: Vec<SearchResultResponse> = results
        .into_iter()
        .map(|r| SearchResultResponse {
            path: r.path,
            title: r.title,
            snippet: r.snippet.replace("<mark>", "").replace("</mark>", ""),
            score: r.score,
        })
        .collect();
    Ok(Json(response))
}

async fn get_tags(State(vault): State<SharedVault>) -> Result<Json<Vec<TagResponse>>, ApiError> {
    let vault = vault.lock().await;
    let tags = vault.tags_with_counts().map_err(ApiError::from)?;
    let response: Vec<TagResponse> = tags
        .into_iter()
        .map(|(tag, count)| TagResponse { tag, count })
        .collect();
    Ok(Json(response))
}

async fn get_provenance(
    State(vault): State<SharedVault>,
    Path(raw_path): Path<String>,
) -> Result<Json<ProvenanceResponse>, ApiError> {
    let path = raw_path.strip_prefix('/').unwrap_or(&raw_path).to_string();
    let vault = vault.lock().await;
    let git = vault
        .git()
        .ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "git not enabled"))?;

    // Verify note exists before calling git blame
    vault.read_note(&path).map_err(ApiError::from)?;

    let prov = git
        .provenance(&path)
        .map_err(|e| ApiError::internal(format!("provenance failed for {path}: {e}")))?;
    let lines = git
        .blame(&path)
        .map_err(|e| ApiError::internal(format!("blame failed for {path}: {e}")))?;
    let line_responses: Vec<LineProvenanceResponse> = lines
        .into_iter()
        .map(|l| LineProvenanceResponse {
            line_no: l.line_no,
            author: format!("{:?}", l.author).to_lowercase(),
            content: l.content,
        })
        .collect();

    Ok(Json(ProvenanceResponse {
        path: prov.path,
        total_lines: prov.total_lines,
        human_lines: prov.human_lines,
        ai_lines: prov.ai_lines,
        human_pct: prov.human_pct,
        ai_pct: prov.ai_pct,
        lines: Some(line_responses),
    }))
}

async fn get_tree(State(vault): State<SharedVault>) -> Result<Json<Vec<TreeEntry>>, ApiError> {
    let vault = vault.lock().await;
    let entries = vault.vault_tree_entries().map_err(ApiError::from)?;
    let response: Vec<TreeEntry> = entries
        .into_iter()
        .map(|(path, note_count)| TreeEntry { path, note_count })
        .collect();
    Ok(Json(response))
}

async fn reindex(State(vault): State<SharedVault>) -> Result<Json<serde_json::Value>, ApiError> {
    let vault = vault.lock().await;
    let stats = vault.index().map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({
        "scanned": stats.notes_scanned,
        "added": stats.notes_added,
        "updated": stats.notes_updated,
        "removed": stats.notes_removed,
    })))
}

async fn get_stats(State(vault): State<SharedVault>) -> Result<Json<StatsResponse>, ApiError> {
    let vault = vault.lock().await;
    let stats = vault.stats().map_err(ApiError::from)?;
    Ok(Json(StatsResponse {
        total_notes: stats.total_notes,
        total_tags: stats.total_tags,
        total_links: stats.total_links,
    }))
}

#[derive(Deserialize)]
pub struct ListParams {
    pub prefix: Option<String>,
    pub tag: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct ListEntryResponse {
    pub path: String,
    pub title: Option<String>,
    pub tags: Vec<String>,
}

async fn list_notes(
    State(vault): State<SharedVault>,
    axum::extract::Query(params): axum::extract::Query<ListParams>,
) -> Result<Json<Vec<ListEntryResponse>>, ApiError> {
    let vault = vault.lock().await;
    let filters = SearchFilters {
        tag: params.tag,
        path_prefix: params.prefix,
        ..Default::default()
    };
    let limit = params.limit.unwrap_or(50);
    let results = vault.list(&filters, limit).map_err(ApiError::from)?;

    // Fetch tags for each note by reading the file (tags are in frontmatter)
    let entries: Vec<ListEntryResponse> = results
        .into_iter()
        .map(|r| {
            let tags = vault
                .read_note(&r.path)
                .map(|n| n.frontmatter.tags.clone())
                .unwrap_or_default();
            ListEntryResponse {
                path: r.path,
                title: r.title,
                tags,
            }
        })
        .collect();

    Ok(Json(entries))
}

// ---------------------------------------------------------------------------
// Git commit helper
// ---------------------------------------------------------------------------

fn commit_with_author(vault: &Vault, path: &str, author: &str, action: &str) {
    if let Some(git) = vault.git() {
        let rel = std::path::Path::new(path);
        match author {
            "ai" => {
                let msg = format!("arcana: {action} {path}");
                if let Err(e) = git.commit_ai_write(&[rel], &msg) {
                    warn!("git commit failed for {action}: {e}");
                }
            }
            _ => {
                if let Err(e) = git.commit_human_change(&[vault.root().join(path)]) {
                    warn!("git commit failed for {action}: {e}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn api_router(vault: SharedVault) -> Router {
    Router::new()
        .route(
            "/api/notes/{*path}",
            get(get_note).put(update_note).delete(delete_note_handler),
        )
        .route("/api/notes", post(create_note))
        .route("/api/search", post(search_notes))
        .route("/api/list", get(list_notes))
        .route("/api/tags", get(get_tags))
        .route("/api/stats", get(get_stats))
        .route("/api/provenance/{*path}", get(get_provenance))
        .route("/api/tree", get(get_tree))
        .route("/api/reindex", post(reindex))
        .with_state(vault)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_error_returns_generic_public_message() {
        // Detail embeds a filesystem path (as libgit2/IO errors do).
        let err = ApiError::internal("blame failed for notes/x.md: /Users/vsb/vault/.git error");
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        // Client message is generic — no path leaks.
        assert_eq!(err.public, "internal server error");
        assert!(!err.public.contains('/'));
        // Full detail is retained for server-side logging only.
        assert!(err.internal.as_deref().unwrap().contains("/Users/"));
    }

    #[test]
    fn new_error_keeps_explicit_public_message() {
        let err = ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "git not enabled");
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.public, "git not enabled");
        assert!(err.internal.is_none());
    }
}
