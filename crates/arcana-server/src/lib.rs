pub mod legacy_sse;
pub mod oauth;
pub mod rest;

use std::sync::Arc;

use std::path::PathBuf;

use arcana_core::{
    ArcanaConfig, Frontmatter, SearchFilters, SearchQuery, SessionMeta, Vault, VaultStats,
    VaultWatcher, WatchHandle,
};
pub use oauth::OAuthConfig;
use rmcp::{
    handler::server::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, tower::StreamableHttpServerConfig,
        tower::StreamableHttpService,
    },
    ServerHandler,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Tool input structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct VaultSearchInput {
    /// Full-text search query (e.g. "machine learning", "rust async")
    pub query: String,
    /// Maximum number of results to return (default: 20)
    #[serde(default)]
    pub limit: Option<usize>,
    /// Filter results to notes with ALL of these tags
    #[serde(default)]
    pub tags: Vec<String>,
    /// Filter results to notes under this path prefix (e.g. "research/")
    #[serde(default)]
    pub path_prefix: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct VaultReadInput {
    /// Relative path to the note (e.g. "ideas/cool-idea.md")
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct VaultCreateInput {
    /// Relative path for the new note (e.g. "ideas/new-idea.md")
    pub path: String,
    /// Note title (stored in YAML frontmatter)
    #[serde(default)]
    pub title: Option<String>,
    /// Markdown body content
    #[serde(default)]
    pub body: String,
    /// Tags to add to the note's frontmatter
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct VaultUpdateInput {
    /// Relative path to the existing note
    pub path: String,
    /// New body content (replaces existing body). Omit to keep current body.
    #[serde(default)]
    pub body: Option<String>,
    /// Text to append to the end of the note. Omit to not append.
    #[serde(default)]
    pub append: Option<String>,
    /// Tags to add to the note's frontmatter
    #[serde(default)]
    pub add_tags: Vec<String>,
    /// Tags to remove from the note's frontmatter
    #[serde(default)]
    pub remove_tags: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct VaultListInput {
    /// Filter to notes under this path prefix
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Filter to notes with ALL of these tags
    #[serde(default)]
    pub tags: Vec<String>,
    /// Maximum number of results (default: 50)
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct VaultDraftInput {
    /// Relative path for the new note (e.g. "research/quantum.md")
    pub path: String,
    /// Note title (stored in YAML frontmatter)
    #[serde(default)]
    pub title: Option<String>,
    /// Markdown body content
    #[serde(default)]
    pub body: String,
    /// Tags to add to the note's frontmatter
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct VaultSuggestEditInput {
    /// Path to the existing note to suggest changes for
    pub path: String,
    /// Proposed new body content
    pub body: String,
    /// Explanation of why this edit is suggested
    pub reason: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct VaultProvenanceInput {
    /// Relative path to the note
    pub path: String,
}

// ---------------------------------------------------------------------------
// JSON output structs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct SearchResultJson {
    path: String,
    title: Option<String>,
    snippet: String,
    score: f64,
}

#[derive(Serialize)]
struct NoteJson {
    path: String,
    title: String,
    tags: Vec<String>,
    body: String,
}

#[derive(Serialize)]
struct StatsJson {
    total_notes: usize,
    total_tags: usize,
    total_links: usize,
}

#[derive(Serialize)]
struct ListEntryJson {
    path: String,
    title: Option<String>,
}

// ---------------------------------------------------------------------------
// ArcanaServer
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ArcanaServer {
    vault: Arc<tokio::sync::Mutex<Vault>>,
    tool_router: ToolRouter<Self>,
    instructions: String,
}

impl ArcanaServer {
    pub fn new(vault: Vault) -> Self {
        let tool_router = Self::tool_router();
        let tree = vault.vault_tree().unwrap_or_default();
        let instructions = build_mcp_instructions(vault.profile(), &tree);
        Self {
            vault: Arc::new(tokio::sync::Mutex::new(vault)),
            tool_router,
            instructions,
        }
    }
}

fn build_mcp_instructions(profile: &arcana_core::BrainProfile, tree: &str) -> String {
    let mut instructions = String::from(
        "Arcana is an Obsidian vault indexer. Use vault_search to find notes, \
         vault_read to read full content, vault_create/vault_update to write notes, \
         vault_list to browse, and vault_stats for overview.\n\n\
         When the user asks you to create or write notes, use vault_create to write \
         them directly. Only use vault_draft when the user explicitly asks for a draft \
         or says \"draft\" — the conversation itself is the review loop, so drafts \
         add unnecessary friction.",
    );

    if !profile.is_empty() {
        instructions.push_str("\n\n");
        if let Some(tax) = profile.taxonomy() {
            instructions.push_str("<taxonomy>\n");
            instructions.push_str(tax);
            instructions.push_str("\n</taxonomy>\n\n");
        }
        if let Some(sty) = profile.style() {
            instructions.push_str("<style_guide>\n");
            instructions.push_str(sty);
            instructions.push_str("\n</style_guide>\n\n");
        }
    }

    if !tree.is_empty() {
        instructions.push_str("\n<vault_structure>\n");
        instructions.push_str(tree);
        instructions.push_str("</vault_structure>");
    }

    instructions
}

/// Strip `<mark>` tags from search snippets (terminal highlighting, not useful for LLMs).
fn strip_mark_tags(s: &str) -> String {
    s.replace("<mark>", "").replace("</mark>", "")
}

fn to_json_text<T: Serialize>(val: &T) -> Result<String, rmcp::ErrorData> {
    serde_json::to_string_pretty(val).map_err(|e| {
        rmcp::ErrorData::internal_error(format!("json serialization failed: {e}"), None)
    })
}

fn vault_err(e: arcana_core::ArcanaError) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(e.to_string(), None)
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[tool_router(vis = "pub")]
impl ArcanaServer {
    #[tool(
        description = "Full-text search across all notes in the Obsidian vault. Returns ranked results with snippets. Use this to find notes by content, title, or tags."
    )]
    async fn vault_search(
        &self,
        Parameters(input): Parameters<VaultSearchInput>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let vault = self.vault.lock().await;
        let query = SearchQuery {
            text: input.query,
            limit: input.limit,
            filters: SearchFilters {
                tag: input.tags.into_iter().next(),
                path_prefix: input.path_prefix,
                ..Default::default()
            },
        };
        let results = vault.search(&query).map_err(vault_err)?;
        let json_results: Vec<SearchResultJson> = results
            .into_iter()
            .map(|r| SearchResultJson {
                path: r.path,
                title: r.title,
                snippet: strip_mark_tags(&r.snippet),
                score: r.score,
            })
            .collect();
        let text = to_json_text(&json_results)?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Read the full content of a note from the vault. Returns the note's title, tags, and complete markdown body."
    )]
    async fn vault_read(
        &self,
        Parameters(input): Parameters<VaultReadInput>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let vault = self.vault.lock().await;
        let note = vault.read_note(&input.path).map_err(vault_err)?;
        let json = NoteJson {
            path: input.path,
            title: note.title().to_string(),
            tags: note.frontmatter.tags.clone(),
            body: note.body.clone(),
        };
        let text = to_json_text(&json)?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Create a new note in the vault with optional title, tags, and body. The note is immediately indexed for search. Path MUST start with a valid zone prefix (e.g. concepts/, projects/, notes/). Notes targeting a specific project go under projects/<project-name>/. Invalid paths will be rejected with zone suggestions."
    )]
    async fn vault_create(
        &self,
        Parameters(input): Parameters<VaultCreateInput>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let vault = self.vault.lock().await;
        let fm = if input.title.is_some() || !input.tags.is_empty() {
            Some(Frontmatter {
                title: input.title,
                tags: input.tags,
                ..Default::default()
            })
        } else {
            None
        };
        vault
            .create_note(&input.path, &input.body, fm)
            .map_err(vault_err)?;
        let text = to_json_text(&serde_json::json!({ "created": input.path }))?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Update an existing note. You can replace the body, append text, and add/remove tags. The note is re-indexed after update."
    )]
    async fn vault_update(
        &self,
        Parameters(input): Parameters<VaultUpdateInput>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let vault = self.vault.lock().await;
        let has_tag_changes = !input.add_tags.is_empty() || !input.remove_tags.is_empty();

        if has_tag_changes {
            // merge_frontmatter only appends tags, so for removal we do a full
            // read-modify-write: read the note, compute final state, rewrite.
            let mut note = vault.read_note(&input.path).map_err(vault_err)?;

            // Apply tag changes
            for tag in &input.add_tags {
                if !note.frontmatter.tags.contains(tag) {
                    note.frontmatter.tags.push(tag.clone());
                }
            }
            note.frontmatter
                .tags
                .retain(|t| !input.remove_tags.contains(t));

            // Apply body changes
            if let Some(new_body) = &input.body {
                note.body = new_body.clone();
            }
            if let Some(text) = &input.append {
                note.body.push_str(text);
            }

            // Write atomically and reindex
            vault
                .write_note_content(&input.path, &note.to_string())
                .map_err(vault_err)?;
        } else {
            // No tag removal needed — use the standard update path
            vault
                .update_note(
                    &input.path,
                    input.body.as_deref(),
                    input.append.as_deref(),
                    None,
                )
                .map_err(vault_err)?;
        }

        let text = to_json_text(&serde_json::json!({ "updated": input.path }))?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "List notes in the vault, optionally filtered by path prefix and/or tags. Returns paths and titles without full content."
    )]
    async fn vault_list(
        &self,
        Parameters(input): Parameters<VaultListInput>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let vault = self.vault.lock().await;
        let filters = SearchFilters {
            tag: input.tags.into_iter().next(),
            path_prefix: input.path_prefix,
            ..Default::default()
        };
        let limit = input.limit.unwrap_or(50);
        let results = vault.list(&filters, limit).map_err(vault_err)?;
        let entries: Vec<ListEntryJson> = results
            .into_iter()
            .map(|r| ListEntryJson {
                path: r.path,
                title: r.title,
            })
            .collect();
        let text = to_json_text(&entries)?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Get vault statistics: total number of notes, unique tags, and wikilinks."
    )]
    async fn vault_stats(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let vault = self.vault.lock().await;
        let stats: VaultStats = vault.stats().map_err(vault_err)?;
        let json = StatsJson {
            total_notes: stats.total_notes,
            total_tags: stats.total_tags,
            total_links: stats.total_links,
        };
        let text = to_json_text(&json)?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Create a draft note for review. The note goes to a staging area and must be approved before entering the vault. Path MUST start with a valid zone prefix (e.g. concepts/, projects/, notes/). Notes targeting a specific project go under projects/<project-name>/. Invalid paths will be rejected with zone suggestions."
    )]
    async fn vault_draft(
        &self,
        Parameters(input): Parameters<VaultDraftInput>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let vault = self.vault.lock().await;
        let drafts = vault.drafts();

        let session_id = drafts
            .create_session(SessionMeta {
                source: "mcp".to_string(),
                provider: "mcp-client".to_string(),
                model: "unknown".to_string(),
                task: "draft".to_string(),
                input_hash: None,
            })
            .map_err(vault_err)?;

        // Build note content with frontmatter
        let fm = if input.title.is_some() || !input.tags.is_empty() {
            Some(Frontmatter {
                title: input.title,
                tags: input.tags,
                ..Default::default()
            })
        } else {
            None
        };

        let content = if let Some(fm) = fm {
            let note = arcana_core::Note {
                path: std::path::PathBuf::from(&input.path),
                frontmatter: fm,
                body: input.body,
                file_meta: arcana_core::FileMeta {
                    size_bytes: 0,
                    modified_on_disk: std::time::SystemTime::now(),
                    content_hash: 0,
                },
            };
            note.to_string()
        } else {
            input.body
        };

        drafts
            .create_draft(&session_id, &input.path, &content)
            .map_err(vault_err)?;

        let text = to_json_text(&serde_json::json!({
            "drafted": input.path,
            "session": session_id,
            "status": "pending review"
        }))?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Suggest an edit to an existing note. The suggestion is staged for review and must be approved before being applied."
    )]
    async fn vault_suggest_edit(
        &self,
        Parameters(input): Parameters<VaultSuggestEditInput>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let vault = self.vault.lock().await;
        let drafts = vault.drafts();

        let session_id = drafts
            .create_session(SessionMeta {
                source: "mcp".to_string(),
                provider: "mcp-client".to_string(),
                model: "unknown".to_string(),
                task: "suggest-edit".to_string(),
                input_hash: None,
            })
            .map_err(vault_err)?;

        drafts
            .suggest_edit(&session_id, &input.path, &input.body, &input.reason)
            .map_err(vault_err)?;

        let text = to_json_text(&serde_json::json!({
            "suggested_edit": input.path,
            "session": session_id,
            "reason": input.reason,
            "status": "pending review"
        }))?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Get provenance stats for a note: percentage of content authored by humans vs AI, based on git blame."
    )]
    async fn vault_provenance(
        &self,
        Parameters(input): Parameters<VaultProvenanceInput>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let vault = self.vault.lock().await;
        let git = vault.git().ok_or_else(|| {
            rmcp::ErrorData::internal_error("git not enabled for this vault".to_string(), None)
        })?;

        let prov = git.provenance(&input.path).map_err(vault_err)?;
        let text = to_json_text(&prov)?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }
}

// ---------------------------------------------------------------------------
// ServerHandler impl
// ---------------------------------------------------------------------------

// rmcp 1.x's #[tool_handler] defaults its router to `Self::tool_router()` (a
// fresh router built per call); point it at the cached field instead.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for ArcanaServer {
    fn get_info(&self) -> ServerInfo {
        // rmcp 1.x marks these model structs #[non_exhaustive], so they can no
        // longer be built with a struct literal cross-crate — use the provided
        // constructors/builders and mutate the public fields.
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new("arcana", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(self.instructions.clone());
        info
    }
}

// ---------------------------------------------------------------------------
// Transport entry points
// ---------------------------------------------------------------------------

/// Serve the MCP server over stdio (for Claude Code).
pub async fn serve_stdio(vault: Vault) -> anyhow::Result<()> {
    use rmcp::ServiceExt;

    let server = ArcanaServer::new(vault);
    let transport = rmcp::transport::io::stdio();
    let service = server.serve(transport).await?;
    info!("arcana MCP server running on stdio");
    service.waiting().await?;
    Ok(())
}

/// Serve the MCP server over HTTP with streamable SSE (for Claude Web / remote clients).
pub async fn serve_sse(
    vault: Vault,
    host: String,
    port: u16,
    bearer_token: Option<String>,
    oauth_config: Option<OAuthConfig>,
) -> anyhow::Result<()> {
    let tree = vault.vault_tree().unwrap_or_default();
    let instructions = build_mcp_instructions(vault.profile(), &tree);
    let vault_root = vault.root().to_path_buf();
    let vault_config = vault.config().clone();
    let vault = Arc::new(tokio::sync::Mutex::new(vault));

    // Start file watcher for live reindexing
    let watcher_vault = vault.clone();
    start_watcher(vault_root, vault_config, watcher_vault);

    let config = StreamableHttpServerConfig::default();
    let ct = config.cancellation_token.clone();

    let sse_instructions = instructions.clone();

    let service_vault = vault.clone();
    let service = StreamableHttpService::new(
        move || {
            let vault = service_vault.clone();
            let instructions = instructions.clone();
            Ok(ArcanaServer {
                vault,
                tool_router: ArcanaServer::tool_router(),
                instructions,
            })
        },
        Arc::new(LocalSessionManager::default()),
        config,
    );

    // Legacy SSE state (for Claude Code and other legacy SSE clients).
    let sse_vault = vault.clone();
    let legacy_sse_state = legacy_sse::LegacySseState::new(move || {
        let vault = sse_vault.clone();
        let instructions = sse_instructions.clone();
        ArcanaServer {
            vault,
            tool_router: ArcanaServer::tool_router(),
            instructions,
        }
    });

    // REST API routes (for iOS app and other HTTP clients)
    let rest_routes = rest::api_router(vault.clone());

    let mcp_route = axum::Router::new()
        .route(
            "/mcp",
            axum::routing::any(move |req: axum::extract::Request| {
                let svc = service.clone();
                async move { svc.handle(req).await }
            }),
        )
        .route("/sse", axum::routing::get(legacy_sse::sse_handler))
        .route(
            "/message",
            // JSON-RPC requests are small; cap the body well under axum's 2 MB
            // default so a single POST can't buffer an outsized payload.
            axum::routing::post(legacy_sse::message_handler)
                .layer(axum::extract::DefaultBodyLimit::max(256 * 1024)),
        )
        .with_state(legacy_sse_state)
        .merge(rest_routes);

    // Capture auth presence before the options are moved into the app below.
    let oauth_config_present = oauth_config.is_some();
    let bearer_token_present = bearer_token.is_some();

    let app = if let Some(oauth) = oauth_config {
        info!("OAuth 2.1 auth enabled for SSE transport");
        let oauth_state = oauth::OAuthState::new(oauth, bearer_token);

        let mcp_route = mcp_route.layer(axum::middleware::from_fn_with_state(
            oauth_state.clone(),
            oauth::bearer_auth,
        ));

        let oauth_routes = axum::Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(oauth::metadata),
            )
            .route(
                "/.well-known/oauth-protected-resource",
                axum::routing::get(oauth::protected_resource),
            )
            // Claude.ai may append the MCP path to the protected resource URL
            .route(
                "/.well-known/oauth-protected-resource/mcp",
                axum::routing::get(oauth::protected_resource),
            )
            .route(
                "/authorize",
                axum::routing::get(oauth::authorize_form).post(oauth::authorize_submit),
            )
            .route("/token", axum::routing::post(oauth::token))
            .route("/register", axum::routing::post(oauth::register))
            .with_state(oauth_state);

        mcp_route.merge(oauth_routes)
    } else if let Some(token) = bearer_token {
        info!("static bearer token auth enabled for SSE transport");
        let oauth_state = oauth::OAuthState::new(
            OAuthConfig {
                client_id: String::new(),
                client_secret: String::new(),
                password: String::new(),
            },
            Some(token),
        );
        mcp_route.layer(axum::middleware::from_fn_with_state(
            oauth_state,
            oauth::bearer_auth,
        ))
    } else {
        tracing::warn!("no auth configured — SSE transport is unauthenticated");
        mcp_route
    };

    // Fail closed: only bind a non-loopback address when auth is configured, so
    // the default `serve --transport sse` cannot expose the vault (and, through
    // the tools, the host filesystem) to the network unauthenticated.
    let is_loopback = matches!(host.as_str(), "127.0.0.1" | "::1" | "localhost");
    let auth_configured = oauth_config_present || bearer_token_present;
    if !is_loopback && !auth_configured {
        anyhow::bail!(
            "refusing to bind non-loopback address '{host}' without authentication. \
             Configure --bearer-token (ARCANA_BEARER_TOKEN) or OAuth, or bind 127.0.0.1."
        );
    }

    let bind_addr = if host.contains(':') {
        format!("[{host}]:{port}") // bracket IPv6 literals
    } else {
        format!("{host}:{port}")
    };
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("arcana MCP server listening on http://{bind_addr}/mcp");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { ct.cancelled().await })
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// File watcher for live reindexing
// ---------------------------------------------------------------------------

fn start_watcher(
    root: PathBuf,
    config: ArcanaConfig,
    vault: Arc<tokio::sync::Mutex<Vault>>,
) -> Option<WatchHandle> {
    let commit_interval = config.git.commit_interval_secs;
    let watcher = VaultWatcher::new(root, config);
    match watcher.start() {
        Ok(handle) => {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<PathBuf>>();

            // Blocking thread: drain FS events, batch, forward to async channel
            std::thread::spawn({
                move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let paths = handle.drain_paths();
                    if !paths.is_empty() && tx.send(paths).is_err() {
                        break;
                    }
                }
            });

            // Async task: reindex immediately, git-commit periodically
            tokio::spawn(async move {
                let mut dirty_paths: Vec<PathBuf> = Vec::new();
                let commit_duration = std::time::Duration::from_secs(commit_interval);
                let mut commit_timer = tokio::time::interval(commit_duration);
                commit_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                // Skip the first immediate tick
                commit_timer.tick().await;

                loop {
                    tokio::select! {
                        Some(paths) = rx.recv() => {
                            // Reindex immediately — keeps search fresh
                            let vault = vault.lock().await;
                            match vault.reindex_paths(&paths) {
                                Ok(stats) => {
                                    if stats.notes_added > 0
                                        || stats.notes_updated > 0
                                        || stats.notes_removed > 0
                                    {
                                        info!(
                                            "watcher reindex: +{} ~{} -{} notes",
                                            stats.notes_added, stats.notes_updated, stats.notes_removed
                                        );
                                        // Accumulate for periodic commit
                                        for p in &paths {
                                            if !dirty_paths.contains(p) {
                                                dirty_paths.push(p.clone());
                                            }
                                        }
                                    }
                                }
                                Err(e) => warn!("watcher reindex failed: {e}"),
                            }
                        }
                        _ = commit_timer.tick() => {
                            let vault = vault.lock().await;
                            if let Some(git) = vault.git() {
                                // Adopt any untracked .md files (created outside arcana)
                                match git.adopt_untracked() {
                                    Ok(Some(n)) => info!("git: adopted {n} untracked notes"),
                                    Ok(None) => {}
                                    Err(e) => warn!("git adopt failed: {e}"),
                                }

                                // Commit accumulated dirty paths
                                if !dirty_paths.is_empty() {
                                    match git.commit_human_change(&dirty_paths) {
                                        Ok(Some(_)) => {
                                            let msg = describe_changes(&dirty_paths);
                                            info!("git: {msg}");
                                            dirty_paths.clear();
                                        }
                                        Ok(None) => {
                                            // all AI-written, nothing to commit
                                            dirty_paths.clear();
                                        }
                                        Err(e) => {
                                            // Retain dirty_paths for retry on next tick
                                            warn!("git commit failed: {e}");
                                        }
                                    }
                                }
                            }
                        }
                        else => break,
                    }
                }
            });

            // Handle is consumed by the thread, return None since we can't return it
            None
        }
        Err(e) => {
            warn!("failed to start file watcher: {e}");
            None
        }
    }
}

/// Build a human-readable commit description from changed paths.
fn describe_changes(paths: &[PathBuf]) -> String {
    let names: Vec<&str> = paths
        .iter()
        .filter_map(|p| p.to_str())
        .map(|s| {
            // Use just the relative filename or last two path components
            let parts: Vec<&str> = s.rsplitn(3, '/').collect();
            if parts.len() >= 2 {
                // e.g. "microcontrollers/gpio.md"
                let idx = s.len() - parts[0].len() - parts[1].len() - 1;
                &s[idx..]
            } else {
                s
            }
        })
        .collect();

    let count = names.len();
    match count {
        1 => format!("update {}", names[0]),
        2 => format!("update {} and {}", names[0], names[1]),
        _ => format!("update {} (+{} more)", names[..2].join(", "), count - 2),
    }
}
