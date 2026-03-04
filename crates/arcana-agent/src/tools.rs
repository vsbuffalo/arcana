use std::sync::Arc;

use crate::permissions::{ApprovalResult, ToolPermission};
use crate::types::ToolDef;
use arcana_core::{
    AiMeta, Confidence, Frontmatter, SearchFilters, SearchQuery, SearchResult, SessionMeta, Vault,
};
use chrono::Utc;
use serde::Deserialize;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Session context for AiMeta injection
// ---------------------------------------------------------------------------

pub struct SessionContext {
    pub session_id: String,
    pub task: String,
    pub model: String,
    pub provider: String,
}

// ---------------------------------------------------------------------------
// VaultToolExecutor
// ---------------------------------------------------------------------------

/// Function type for permission checking.
pub type PermissionsFn = dyn Fn(&str) -> ToolPermission + Send + Sync;

/// Function type for approval callbacks.
pub type ApprovalFn = dyn Fn(&str, &str, &serde_json::Value) -> ApprovalResult + Send + Sync;

pub struct VaultToolExecutor {
    vault: Arc<Mutex<Vault>>,
    session: SessionContext,
    permissions_fn: Option<Box<PermissionsFn>>,
    approval_fn: Option<Box<ApprovalFn>>,
}

impl VaultToolExecutor {
    pub fn new(vault: Arc<Mutex<Vault>>, session: SessionContext) -> Self {
        Self {
            vault,
            session,
            permissions_fn: None,
            approval_fn: None,
        }
    }

    pub fn with_permissions(mut self, f: Box<PermissionsFn>) -> Self {
        self.permissions_fn = Some(f);
        self
    }

    pub fn with_approval(mut self, f: Box<ApprovalFn>) -> Self {
        self.approval_fn = Some(f);
        self
    }

    pub fn tool_defs() -> Vec<ToolDef> {
        Self::base_tool_defs()
    }

    /// Read-only tool defs for ingest exploration (no create/update/draft).
    pub fn read_only_tool_defs() -> Vec<ToolDef> {
        vec![Self::search_def(), Self::read_def(), Self::list_def()]
    }

    /// Tool defs including draft tools (for chat mode).
    pub fn chat_tool_defs() -> Vec<ToolDef> {
        let defs = vec![
            // Read-only tools
            Self::search_def(),
            Self::read_def(),
            Self::list_def(),
            // Draft tools instead of direct write
            ToolDef {
                name: "vault_draft".into(),
                description: "Create a draft note for review. The note will NOT be written to the vault until the user approves it.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative path for the new note"
                        },
                        "title": {
                            "type": "string",
                            "description": "Note title (stored in frontmatter)"
                        },
                        "body": {
                            "type": "string",
                            "description": "Markdown body content"
                        },
                        "tags": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Tags for the note"
                        }
                    },
                    "required": ["path", "body"]
                }),
            },
            ToolDef {
                name: "vault_suggest_edit".into(),
                description: "Suggest an edit to an existing note. The user will review the proposed changes before they are applied.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the existing note to edit"
                        },
                        "body": {
                            "type": "string",
                            "description": "Proposed new body content"
                        },
                        "reason": {
                            "type": "string",
                            "description": "Explanation of why this edit is suggested"
                        }
                    },
                    "required": ["path", "body", "reason"]
                }),
            },
        ];
        defs
    }

    fn base_tool_defs() -> Vec<ToolDef> {
        vec![
            Self::search_def(),
            Self::read_def(),
            ToolDef {
                name: "vault_create".into(),
                description: "Create a new note with optional title, tags, and markdown body."
                    .into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative path for the new note"
                        },
                        "title": {
                            "type": "string",
                            "description": "Note title (stored in frontmatter)"
                        },
                        "body": {
                            "type": "string",
                            "description": "Markdown body content"
                        },
                        "tags": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Tags for the note"
                        }
                    },
                    "required": ["path", "body"]
                }),
            },
            ToolDef {
                name: "vault_update".into(),
                description:
                    "Update an existing note. Can replace body, append text, or modify tags.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative path to the existing note"
                        },
                        "body": {
                            "type": "string",
                            "description": "New body content (replaces existing)"
                        },
                        "append": {
                            "type": "string",
                            "description": "Text to append to the note"
                        },
                        "add_tags": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Tags to add"
                        },
                        "remove_tags": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Tags to remove"
                        }
                    },
                    "required": ["path"]
                }),
            },
            Self::list_def(),
        ]
    }

    fn search_def() -> ToolDef {
        ToolDef {
            name: "vault_search".into(),
            description: "Full-text search across all notes in the vault. Returns ranked results with snippets.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Full-text search query"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum results to return (default: 20)"
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Filter to notes with ALL of these tags"
                    },
                    "path_prefix": {
                        "type": "string",
                        "description": "Filter to notes under this path prefix"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn read_def() -> ToolDef {
        ToolDef {
            name: "vault_read".into(),
            description: "Read the full content of a note. Returns title, tags, and markdown body."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the note (e.g. 'ideas/cool-idea.md')"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn list_def() -> ToolDef {
        ToolDef {
            name: "vault_list".into(),
            description: "List notes in the vault, optionally filtered by path prefix and/or tags."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path_prefix": {
                        "type": "string",
                        "description": "Filter to notes under this path prefix"
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Filter to notes with ALL of these tags"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum results (default: 50)"
                    }
                }
            }),
        }
    }

    pub async fn execute(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> std::result::Result<String, String> {
        // Check permissions if a permissions function is set
        if let Some(ref perm_fn) = self.permissions_fn {
            match perm_fn(tool_name) {
                ToolPermission::Free => {}
                ToolPermission::RequiresApproval => {
                    if let Some(ref approval_fn) = self.approval_fn {
                        let desc = format_tool_description(tool_name, input);
                        match approval_fn(tool_name, &desc, input) {
                            ApprovalResult::Approve => {}
                            ApprovalResult::Reject(reason) => {
                                return Err(format!("tool {tool_name} rejected: {reason}"));
                            }
                            ApprovalResult::Edit(new_input) => {
                                return self.dispatch(tool_name, &new_input).await;
                            }
                        }
                    }
                }
                ToolPermission::Blocked => {
                    return Err(format!(
                        "Tool '{tool_name}' is not available in this mode. Use vault_draft instead."
                    ));
                }
            }
        }

        self.dispatch(tool_name, input).await
    }

    async fn dispatch(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> std::result::Result<String, String> {
        match tool_name {
            "vault_search" => self.exec_search(input).await,
            "vault_read" => self.exec_read(input).await,
            "vault_create" => self.exec_create(input).await,
            "vault_update" => self.exec_update(input).await,
            "vault_list" => self.exec_list(input).await,
            "vault_draft" => self.exec_draft(input).await,
            "vault_suggest_edit" => self.exec_suggest_edit(input).await,
            _ => Err(format!("unknown tool: {tool_name}")),
        }
    }

    async fn exec_search(&self, input: &serde_json::Value) -> std::result::Result<String, String> {
        #[derive(Deserialize)]
        struct Input {
            query: String,
            #[serde(default)]
            limit: Option<usize>,
            #[serde(default)]
            tags: Vec<String>,
            #[serde(default)]
            path_prefix: Option<String>,
        }
        let input: Input = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;
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
        let results: Vec<SearchResult> = vault.search(&query).map_err(|e| e.to_string())?;
        let out: Vec<serde_json::Value> = results
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "path": r.path,
                    "title": r.title,
                    "snippet": r.snippet.replace("<mark>", "").replace("</mark>", ""),
                    "score": r.score,
                })
            })
            .collect();
        serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
    }

    async fn exec_read(&self, input: &serde_json::Value) -> std::result::Result<String, String> {
        #[derive(Deserialize)]
        struct Input {
            path: String,
        }
        let input: Input = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;
        let vault = self.vault.lock().await;
        let note = vault.read_note(&input.path).map_err(|e| e.to_string())?;
        let out = serde_json::json!({
            "path": input.path,
            "title": note.title(),
            "tags": note.frontmatter.tags,
            "body": note.body,
        });
        serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
    }

    async fn exec_create(&self, input: &serde_json::Value) -> std::result::Result<String, String> {
        #[derive(Deserialize)]
        struct Input {
            path: String,
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            body: String,
            #[serde(default)]
            tags: Vec<String>,
        }
        let input: Input = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;

        let ai_meta = self.build_ai_meta();
        let fm = Frontmatter {
            title: input.title,
            tags: input.tags,
            ai: Some(ai_meta),
            ..Default::default()
        };

        let vault = self.vault.lock().await;
        vault
            .create_note(&input.path, &input.body, Some(fm))
            .map_err(|e| e.to_string())?;

        serde_json::to_string_pretty(&serde_json::json!({"created": input.path}))
            .map_err(|e| e.to_string())
    }

    async fn exec_update(&self, input: &serde_json::Value) -> std::result::Result<String, String> {
        #[derive(Deserialize)]
        struct Input {
            path: String,
            #[serde(default)]
            body: Option<String>,
            #[serde(default)]
            append: Option<String>,
            #[serde(default)]
            add_tags: Vec<String>,
            #[serde(default)]
            remove_tags: Vec<String>,
        }
        let input: Input = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;
        let vault = self.vault.lock().await;

        let has_tag_changes = !input.add_tags.is_empty() || !input.remove_tags.is_empty();
        if has_tag_changes {
            let mut note = vault.read_note(&input.path).map_err(|e| e.to_string())?;
            for tag in &input.add_tags {
                if !note.frontmatter.tags.contains(tag) {
                    note.frontmatter.tags.push(tag.clone());
                }
            }
            note.frontmatter
                .tags
                .retain(|t| !input.remove_tags.contains(t));

            if let Some(new_body) = &input.body {
                note.body = new_body.clone();
            }
            if let Some(text) = &input.append {
                note.body.push_str(text);
            }

            if note.frontmatter.ai.is_none() {
                note.frontmatter.ai = Some(self.build_ai_meta());
            }

            let full_path = vault.root().join(&input.path);
            std::fs::write(&full_path, note.to_string()).map_err(|e| e.to_string())?;
            vault
                .reindex_paths(&[full_path])
                .map_err(|e| e.to_string())?;

            if let Some(git) = vault.git() {
                let rel = std::path::Path::new(&input.path);
                git.commit_ai_write(&[rel], &format!("arcana: update {}", input.path))
                    .map_err(|e| e.to_string())?;
            }
        } else {
            let fm_patch = Frontmatter {
                ai: Some(self.build_ai_meta()),
                ..Default::default()
            };
            vault
                .update_note(
                    &input.path,
                    input.body.as_deref(),
                    input.append.as_deref(),
                    Some(fm_patch),
                )
                .map_err(|e| e.to_string())?;
        }

        serde_json::to_string_pretty(&serde_json::json!({"updated": input.path}))
            .map_err(|e| e.to_string())
    }

    async fn exec_list(&self, input: &serde_json::Value) -> std::result::Result<String, String> {
        #[derive(Deserialize)]
        struct Input {
            #[serde(default)]
            path_prefix: Option<String>,
            #[serde(default)]
            tags: Vec<String>,
            #[serde(default)]
            limit: Option<usize>,
        }
        let input: Input = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;
        let vault = self.vault.lock().await;
        let filters = SearchFilters {
            tags: input.tags,
            path_prefix: input.path_prefix,
            ..Default::default()
        };
        let limit = input.limit.unwrap_or(50);
        let results: Vec<SearchResult> = vault.list(&filters, limit).map_err(|e| e.to_string())?;
        let out: Vec<serde_json::Value> = results
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "path": r.path,
                    "title": r.title,
                })
            })
            .collect();
        serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
    }

    async fn exec_draft(&self, input: &serde_json::Value) -> std::result::Result<String, String> {
        #[derive(Deserialize)]
        struct Input {
            path: String,
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            body: String,
            #[serde(default)]
            tags: Vec<String>,
        }
        let input: Input = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;

        // Build the full note content with frontmatter
        let ai_meta = self.build_ai_meta();
        let fm = Frontmatter {
            title: input.title,
            tags: input.tags,
            ai: Some(ai_meta),
            ..Default::default()
        };

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
        let content = note.to_string();

        let vault = self.vault.lock().await;
        let drafts = vault.drafts();

        // Ensure session exists
        let session_id = self.ensure_draft_session(drafts)?;
        drafts
            .create_draft(&session_id, &input.path, &content)
            .map_err(|e| e.to_string())?;

        serde_json::to_string_pretty(&serde_json::json!({
            "drafted": input.path,
            "session": session_id,
            "status": "pending review"
        }))
        .map_err(|e| e.to_string())
    }

    async fn exec_suggest_edit(
        &self,
        input: &serde_json::Value,
    ) -> std::result::Result<String, String> {
        #[derive(Deserialize)]
        struct Input {
            path: String,
            body: String,
            reason: String,
        }
        let input: Input = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;

        let vault = self.vault.lock().await;
        let drafts = vault.drafts();

        let session_id = self.ensure_draft_session(drafts)?;
        drafts
            .suggest_edit(&session_id, &input.path, &input.body, &input.reason)
            .map_err(|e| e.to_string())?;

        serde_json::to_string_pretty(&serde_json::json!({
            "suggested_edit": input.path,
            "session": session_id,
            "reason": input.reason,
            "status": "pending review"
        }))
        .map_err(|e| e.to_string())
    }

    fn ensure_draft_session(
        &self,
        drafts: &arcana_core::DraftManager,
    ) -> std::result::Result<String, String> {
        // Check if there's already a session with our session_id
        let sessions = drafts.list_sessions().map_err(|e| e.to_string())?;
        for s in &sessions {
            if self.session.session_id.starts_with(&s.id) {
                return Ok(s.id.clone());
            }
        }

        // Create a new one
        drafts
            .create_session(SessionMeta {
                source: "chat".to_string(),
                provider: self.session.provider.clone(),
                model: self.session.model.clone(),
                task: self.session.task.clone(),
                input_hash: None,
            })
            .map_err(|e| e.to_string())
    }

    fn build_ai_meta(&self) -> AiMeta {
        AiMeta {
            model: self.session.model.clone(),
            provider: self.session.provider.clone(),
            agent_session: self.session.session_id.clone(),
            task: self.session.task.clone(),
            prompt: String::new(),
            sources: Vec::new(),
            confidence: Confidence::Medium,
            reviewed: false,
            generated_at: Utc::now(),
        }
    }
}

fn format_tool_description(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "vault_draft" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            format!("Create draft note: {path}")
        }
        "vault_suggest_edit" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let reason = input.get("reason").and_then(|v| v.as_str()).unwrap_or("?");
            format!("Suggest edit to {path}: {reason}")
        }
        _ => tool_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_defs_are_valid_json() {
        let defs = VaultToolExecutor::tool_defs();
        assert_eq!(defs.len(), 5);
        for def in &defs {
            assert!(!def.name.is_empty());
            assert!(!def.description.is_empty());
            assert_eq!(def.input_schema["type"], "object");
        }
    }

    #[test]
    fn chat_tool_defs_include_draft_tools() {
        let defs = VaultToolExecutor::chat_tool_defs();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"vault_draft"));
        assert!(names.contains(&"vault_suggest_edit"));
        assert!(names.contains(&"vault_search"));
        assert!(names.contains(&"vault_read"));
        // Should NOT include direct write tools
        assert!(!names.contains(&"vault_create"));
        assert!(!names.contains(&"vault_update"));
    }

    #[tokio::test]
    async fn execute_unknown_tool() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.md"), "test").unwrap();
        let config = arcana_core::ArcanaConfig::default().with_vault_path(dir.path().to_path_buf());
        let vault = Vault::open(config).unwrap();
        let executor = VaultToolExecutor::new(
            Arc::new(Mutex::new(vault)),
            SessionContext {
                session_id: "test".into(),
                task: "test".into(),
                model: "test".into(),
                provider: "test".into(),
            },
        );
        let result = executor
            .execute("nonexistent", &serde_json::json!({}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown tool"));
    }

    #[tokio::test]
    async fn blocked_tool_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.md"), "test").unwrap();
        let config = arcana_core::ArcanaConfig::default().with_vault_path(dir.path().to_path_buf());
        let vault = Vault::open(config).unwrap();
        let executor = VaultToolExecutor::new(
            Arc::new(Mutex::new(vault)),
            SessionContext {
                session_id: "test".into(),
                task: "test".into(),
                model: "test".into(),
                provider: "test".into(),
            },
        )
        .with_permissions(Box::new(crate::permissions::chat_permissions));

        let result = executor
            .execute(
                "vault_create",
                &serde_json::json!({"path": "x.md", "body": "x"}),
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not available"));
    }

    #[tokio::test]
    async fn search_create_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("seed.md"), "seed note").unwrap();
        let config = arcana_core::ArcanaConfig::default().with_vault_path(dir.path().to_path_buf());
        let vault = Vault::open(config).unwrap();
        vault.index().unwrap();

        let executor = VaultToolExecutor::new(
            Arc::new(Mutex::new(vault)),
            SessionContext {
                session_id: "sess-1".into(),
                task: "test research".into(),
                model: "mock-model".into(),
                provider: "mock".into(),
            },
        );

        // Create a note
        let result = executor
            .execute(
                "vault_create",
                &serde_json::json!({
                    "path": "test-note.md",
                    "title": "Test Note",
                    "body": "This is about Rust programming.\n",
                    "tags": ["rust", "test"]
                }),
            )
            .await;
        assert!(result.is_ok(), "create failed: {:?}", result);

        // Read it back
        let result = executor
            .execute("vault_read", &serde_json::json!({"path": "test-note.md"}))
            .await;
        assert!(result.is_ok());
        let parsed: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(parsed["title"], "Test Note");

        // Search for it
        let result = executor
            .execute("vault_search", &serde_json::json!({"query": "Rust"}))
            .await;
        assert!(result.is_ok());
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&result.unwrap()).unwrap();
        assert!(!parsed.is_empty());

        // List
        let result = executor.execute("vault_list", &serde_json::json!({})).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn draft_tool_creates_draft() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("seed.md"), "seed note").unwrap();
        let config = arcana_core::ArcanaConfig::default().with_vault_path(dir.path().to_path_buf());
        let vault = Vault::open(config).unwrap();
        vault.index().unwrap();

        let executor = VaultToolExecutor::new(
            Arc::new(Mutex::new(vault)),
            SessionContext {
                session_id: "sess-draft".into(),
                task: "test drafting".into(),
                model: "mock-model".into(),
                provider: "mock".into(),
            },
        );

        let result = executor
            .execute(
                "vault_draft",
                &serde_json::json!({
                    "path": "research/new-note.md",
                    "title": "New Research",
                    "body": "Draft content here.\n",
                    "tags": ["draft"]
                }),
            )
            .await;
        assert!(result.is_ok(), "draft failed: {:?}", result);
        let parsed: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(parsed["drafted"], "research/new-note.md");
        assert_eq!(parsed["status"], "pending review");

        // The note should NOT exist in the vault
        assert!(!dir.path().join("research/new-note.md").exists());
    }
}
