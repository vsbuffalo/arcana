pub mod oauth;

use std::sync::Arc;

use arcana_core::{Frontmatter, SearchFilters, SearchQuery, SessionMeta, Vault, VaultStats};
pub use oauth::OAuthConfig;
use rmcp::{
    handler::server::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo, ToolsCapability,
    },
    schemars, tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, tower::StreamableHttpServerConfig,
        tower::StreamableHttpService,
    },
    ServerHandler,
};
use serde::{Deserialize, Serialize};
use tracing::info;

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
         vault_list to browse, and vault_stats for overview.",
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
                tags: input.tags,
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

            // Write full note content directly
            let full_path = vault.root().join(&input.path);
            std::fs::write(&full_path, note.to_string()).map_err(|e| {
                rmcp::ErrorData::internal_error(format!("failed to write note: {e}"), None)
            })?;
            vault.reindex_paths(&[full_path]).map_err(vault_err)?;
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
            tags: input.tags,
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

#[tool_handler]
impl ServerHandler for ArcanaServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: Default::default(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability { list_changed: None }),
                ..Default::default()
            },
            server_info: Implementation {
                name: "arcana".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                ..Default::default()
            },
            instructions: Some(self.instructions.clone()),
        }
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
    port: u16,
    bearer_token: Option<String>,
    oauth_config: Option<OAuthConfig>,
) -> anyhow::Result<()> {
    let tree = vault.vault_tree().unwrap_or_default();
    let instructions = build_mcp_instructions(vault.profile(), &tree);
    let vault = Arc::new(tokio::sync::Mutex::new(vault));
    let config = StreamableHttpServerConfig::default();
    let ct = config.cancellation_token.clone();

    let service = StreamableHttpService::new(
        move || {
            let vault = vault.clone();
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

    let mcp_route = axum::Router::new().route(
        "/mcp",
        axum::routing::any(move |req: axum::extract::Request| {
            let svc = service.clone();
            async move { svc.handle(req).await }
        }),
    );

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
            .with_state(oauth_state);

        mcp_route.merge(oauth_routes)
    } else if let Some(token) = bearer_token {
        info!("static bearer token auth enabled for SSE transport");
        let oauth_state =
            oauth::OAuthState::new(
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

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    info!("arcana MCP server listening on http://0.0.0.0:{port}/mcp");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { ct.cancelled().await })
        .await?;
    Ok(())
}
