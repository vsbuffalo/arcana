//! MCP tools for a ledger vault: agents write only through planned edits.
//!
//! Six tools: `vault_search`, `vault_read`, `vault_edit`, `vault_create`,
//! `vault_log`, `vault_decisions`. There is no whole-note overwrite.

use std::collections::BTreeMap;

use arcana_core::attr::{
    blocks::blocks, token::tokenize, AgentIdentity, EditRequest, Grant, RawEdit,
};
use arcana_core::{SearchFilters, SearchQuery};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content},
    schemars,
    service::RequestContext,
    tool, tool_router, RoleServer,
};
use serde::{Deserialize, Serialize};

use crate::{strip_mark_tags, to_json_text, vault_err, ArcanaServer};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchInput {
    /// Full-text query
    pub query: String,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Only notes under this path prefix
    #[serde(default)]
    pub path_prefix: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadInput {
    /// Vault-relative path, e.g. "notes/electronics/q-factor.md"
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EditInput {
    /// Note to edit
    pub path: String,
    /// The `base` value from vault_read. If the note changed since, nothing is written.
    #[serde(default)]
    pub base: Option<String>,
    /// What the user asked for, briefly, in their words
    pub request: String,
    /// One line on why this change; shown to the user in review
    pub rationale: String,
    /// Edits, applied in order
    pub edits: Vec<EditOp>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EditOp {
    /// "replace" (find → with), "insert_after" (after find, insert text), or
    /// "append" (text as new paragraphs at the end of the section under
    /// `heading`, or of the note)
    pub op: String,
    /// Exact text that occurs exactly once in the note (replace, insert_after)
    #[serde(default)]
    pub find: Option<String>,
    /// Replacement text (replace)
    #[serde(default)]
    pub with: Option<String>,
    /// Text to insert (insert_after, append). For insert_after, start with a
    /// blank line ("\n\n") to add a new paragraph rather than extend one.
    #[serde(default)]
    pub text: Option<String>,
    /// Section heading text without #s (append)
    #[serde(default)]
    pub heading: Option<String>,
}

impl EditOp {
    fn into_raw(self) -> Result<RawEdit, String> {
        let need = |v: Option<String>, f: &str| v.ok_or(format!("`{}` needs `{f}`", self.op));
        match self.op.as_str() {
            "replace" => Ok(RawEdit::Replace {
                find: need(self.find.clone(), "find")?,
                with: need(self.with.clone(), "with")?,
            }),
            "insert_after" => Ok(RawEdit::InsertAfter {
                find: need(self.find.clone(), "find")?,
                text: need(self.text.clone(), "text")?,
            }),
            "append" => Ok(RawEdit::Append {
                heading: self.heading.clone(),
                text: need(self.text.clone(), "text")?,
            }),
            other => Err(format!(
                "unknown op {other:?}; use replace, insert_after or append"
            )),
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateInput {
    /// Note type (see vault_read's `types` or the server instructions). Gives
    /// the path, kind, tags and template.
    #[serde(default)]
    pub note_type: Option<String>,
    /// Explicit path, if no note type is given
    #[serde(default)]
    pub path: Option<String>,
    pub title: String,
    /// Values for the note type's path template, e.g. {"project": "bench-psu"}
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    /// Body; defaults to the note type's template
    #[serde(default)]
    pub body: Option<String>,
    /// What the user asked for
    pub request: String,
    /// Why no existing note fits; name the candidates you considered
    pub why_new: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LogInput {
    /// Log note to append to
    pub path: String,
    /// The entry; short and factual (at most ~300 words)
    pub entry: String,
    /// What the user asked for
    #[serde(default)]
    pub request: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DecisionsInput {
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Serialize)]
struct BlockOwner {
    starts: String,
    owner: &'static str,
    unreviewed: bool,
}

const MAX_LOG_WORDS: usize = 300;

impl ArcanaServer {
    fn grant(&self, ctx: &RequestContext<RoleServer>) -> Grant {
        let agent = ctx
            .peer
            .peer_info()
            .map(|i| i.client_info.name.clone())
            .unwrap_or_else(|| "mcp-client".into());
        Grant::agent(AgentIdentity {
            agent,
            session: self.session.clone(),
        })
    }
}

fn err(msg: impl Into<String>) -> rmcp::ErrorData {
    rmcp::ErrorData::invalid_params(msg.into(), None)
}

#[tool_router(router = ledger_tool_router, vis = "pub")]
impl ArcanaServer {
    #[tool(
        name = "vault_search",
        annotations(read_only_hint = true, open_world_hint = false),
        description = "Full-text search over the vault. Search before creating anything: refine an existing note rather than adding a new one."
    )]
    async fn ledger_search(
        &self,
        Parameters(input): Parameters<SearchInput>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let vault = self.vault.lock().await;
        let results = vault
            .search(&SearchQuery {
                text: input.query,
                limit: input.limit,
                filters: SearchFilters {
                    path_prefix: input.path_prefix,
                    ..Default::default()
                },
            })
            .map_err(vault_err)?;
        let out: Vec<_> = results
            .into_iter()
            .map(|r| serde_json::json!({"path": r.path, "title": r.title, "snippet": strip_mark_tags(&r.snippet)}))
            .collect();
        Ok(CallToolResult::success(vec![Content::text(to_json_text(
            &out,
        )?)]))
    }

    #[tool(
        name = "vault_read",
        annotations(read_only_hint = true, open_world_hint = false),
        description = "Read a note: its content, kind (chapter, writing, log, pointer), who owns each block (human, agent, mixed), pending changes, its type's style guide, and `base` to pass to vault_edit."
    )]
    async fn ledger_read(
        &self,
        Parameters(input): Parameters<ReadInput>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let vault = self.vault.lock().await;
        let ledger = vault.ledger().ok_or_else(|| err("not a ledger vault"))?;
        let st = ledger.state(&input.path).map_err(vault_err)?;
        let toks = tokenize(&st.content);
        let owners: Vec<BlockOwner> = blocks(&st.content)
            .iter()
            .map(|b| {
                let attrs: Vec<_> = toks
                    .iter()
                    .zip(&st.attribution.tokens)
                    .filter(|(t, _)| t.start >= b.start && t.end <= b.end)
                    .map(|(_, a)| a)
                    .collect();
                let human = attrs.iter().any(|a| st.attribution.author_of(a).is_human());
                let agent = attrs.iter().any(|a| st.attribution.author_of(a).is_agent());
                let owner = match (human, agent) {
                    (true, true) => "mixed",
                    (true, false) => "human",
                    (false, true) => "agent",
                    (false, false) => "unattributed",
                };
                let text = &st.content[b.start..b.end];
                BlockOwner {
                    starts: text.chars().take(60).collect(),
                    owner,
                    unreviewed: attrs.iter().any(|a| a.unreviewed),
                }
            })
            .collect();
        let pending: Vec<_> = ledger
            .pending()
            .map_err(vault_err)?
            .into_iter()
            .filter(|p| p.note == input.path)
            .map(|p| serde_json::json!({"id": p.id, "kind": p.disposition, "before": p.before, "after": p.after}))
            .collect();
        let style = st
            .note_type
            .as_deref()
            .and_then(|t| ledger.types().get(t))
            .and_then(|t| t.style.clone())
            .and_then(|s| arcana_core::resolve_skill(&s, vault.root()).ok())
            .map(|s| s.body);
        let out = serde_json::json!({
            "path": st.rel,
            "kind": st.kind.as_str(),
            "type": st.note_type,
            "base": st.base(),
            "content": st.content,
            "blocks": owners,
            "pending": pending,
            "style": style,
        });
        Ok(CallToolResult::success(vec![Content::text(to_json_text(
            &out,
        )?)]))
    }

    #[tool(
        name = "vault_edit",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        ),
        description = "Edit a note with text-anchored operations. You never overwrite a note. In chapters, new text and changes to agent-written text are written immediately (marked unreviewed). Anything touching the user's own words becomes a suggestion they accept or reject; keep those small and specific. Returns what happened to each edit and a word diff of what was written."
    )]
    async fn ledger_edit(
        &self,
        Parameters(input): Parameters<EditInput>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let edits = input
            .edits
            .into_iter()
            .map(EditOp::into_raw)
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        let grant = self.grant(&ctx);
        let vault = self.vault.lock().await;
        let ledger = vault.ledger().ok_or_else(|| err("not a ledger vault"))?;
        let outcome = ledger
            .agent_edit(
                EditRequest {
                    note: input.path.clone(),
                    base: input.base,
                    request: Some(input.request),
                    rationale: Some(input.rationale),
                    edits,
                },
                grant,
                vault.git(),
            )
            .map_err(vault_err)?;
        let _ = vault.reindex_paths(&[vault.root().join(&input.path)]);
        Ok(CallToolResult::success(vec![Content::text(to_json_text(
            &outcome,
        )?)]))
    }

    #[tool(
        name = "vault_create",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        ),
        description = "Create a new note, usually from a note type. Only after vault_search shows no existing note where this belongs; say why in `why_new`. The text lands marked unreviewed. You cannot create the user's own writing."
    )]
    async fn ledger_create(
        &self,
        Parameters(input): Parameters<CreateInput>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let grant = self.grant(&ctx);
        let vault = self.vault.lock().await;
        let ledger = vault.ledger().ok_or_else(|| err("not a ledger vault"))?;
        let request = format!("{} (new note: {})", input.request, input.why_new);
        let (path, commit_error) = ledger
            .agent_create(
                input.note_type.as_deref(),
                input.path.as_deref(),
                &input.title,
                &input.fields,
                input.body.as_deref(),
                Some(&request),
                grant,
                vault.git(),
            )
            .map_err(vault_err)?;
        let _ = vault.reindex_paths(&[vault.root().join(&path)]);
        let out = serde_json::json!({"created": path, "commit_error": commit_error});
        Ok(CallToolResult::success(vec![Content::text(to_json_text(
            &out,
        )?)]))
    }

    #[tool(
        name = "vault_log",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        ),
        description = "Append a short dated entry to a log note (lab notebook, inventory changes, decisions). Logs are append-only. Do not write session summaries anywhere: put lasting knowledge into the chapter it belongs to with vault_edit."
    )]
    async fn ledger_log(
        &self,
        Parameters(input): Parameters<LogInput>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let words = input.entry.split_whitespace().count();
        if words > MAX_LOG_WORDS {
            return Err(err(format!(
                "log entries are at most {MAX_LOG_WORDS} words ({words} given); refine a chapter instead"
            )));
        }
        let grant = self.grant(&ctx);
        let vault = self.vault.lock().await;
        let ledger = vault.ledger().ok_or_else(|| err("not a ledger vault"))?;
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let has_heading = ledger
            .state(&input.path)
            .map(|s| {
                s.content
                    .lines()
                    .any(|l| l.trim_start_matches('#').trim() == today && l.starts_with('#'))
            })
            .map_err(vault_err)?;
        let edit = if has_heading {
            RawEdit::Append {
                heading: Some(today),
                text: input.entry,
            }
        } else {
            RawEdit::Append {
                heading: None,
                text: format!("## {today}\n\n{}", input.entry.trim()),
            }
        };
        let outcome = ledger
            .agent_edit(
                EditRequest {
                    note: input.path.clone(),
                    base: None,
                    request: input.request,
                    rationale: None,
                    edits: vec![edit],
                },
                grant,
                vault.git(),
            )
            .map_err(vault_err)?;
        let _ = vault.reindex_paths(&[vault.root().join(&input.path)]);
        Ok(CallToolResult::success(vec![Content::text(to_json_text(
            &outcome,
        )?)]))
    }

    #[tool(
        name = "vault_decisions",
        annotations(read_only_hint = true, open_world_hint = false),
        description = "Recent review decisions by the user on agent changes, with reasons for rejections. Read this before proposing similar changes again."
    )]
    async fn ledger_decisions(
        &self,
        Parameters(input): Parameters<DecisionsInput>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let vault = self.vault.lock().await;
        let ledger = vault.ledger().ok_or_else(|| err("not a ledger vault"))?;
        let out: Vec<_> = ledger
            .decided(input.limit.unwrap_or(20))
            .map_err(vault_err)?
            .into_iter()
            .map(|d| {
                serde_json::json!({
                    "note": d.pending.note,
                    "accepted": d.accepted,
                    "reason": d.reason,
                    "before": d.pending.before,
                    "after": d.pending.after,
                    "request": d.pending.request,
                })
            })
            .collect();
        Ok(CallToolResult::success(vec![Content::text(to_json_text(
            &out,
        )?)]))
    }
}

/// Server instructions for a ledger vault.
pub fn ledger_instructions(vault: &arcana_core::Vault) -> String {
    let mut s = String::from(
        "Arcana keeps the user's notes: a personal textbook that agents write and refine \
         for them, plus their own writing and logs. Every word has a recorded author.\n\n\
         Rules:\n\
         - Refine, don't accrete. vault_search first; improve the chapter where a topic \
           belongs with vault_edit. Create a note only when nothing fits, and say why.\n\
         - Never write session summaries, status reports or handoffs into the vault. \
           Lasting knowledge goes into the chapter it belongs to; dated facts \
           (measurements, decisions) go into a log with vault_log.\n\
         - vault_read before vault_edit, and pass its `base`. Anchor edits on exact text.\n\
         - The user's own words are theirs. Edits touching them become suggestions the \
           user reviews; keep such suggestions small and specific (typo, citation, \
           wording), never rewrites.\n\
         - New paragraphs you add to chapters are visible immediately, marked unreviewed.\n\
         - Write for this reader: clear, precise, textbook-like; define terms and \
           notation on first use. Follow a note type's style guide when vault_read \
           returns one.\n\
         - Check vault_decisions for the user's past rejections before proposing similar \
           changes.\n",
    );
    if let Some(ledger) = vault.ledger() {
        if !ledger.types().is_empty() {
            s.push_str("\nNote types (vault_create note_type):\n");
            for t in ledger.types().values() {
                s.push_str(&format!(
                    "- {} ({}): {} — path {}\n",
                    t.name,
                    t.kind.as_str(),
                    t.description,
                    t.path
                ));
            }
        }
    }
    let tree = vault.vault_tree().unwrap_or_default();
    if !tree.is_empty() {
        s.push_str("\n<vault_structure>\n");
        s.push_str(&tree);
        s.push_str("</vault_structure>");
    }
    s
}
