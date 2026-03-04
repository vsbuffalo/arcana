use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use arcana_core::{BrainProfile, SessionMeta, Vault};

use crate::backend::LlmBackend;
use crate::context::generate_context;
use crate::error::{AgentError, Result};
use crate::project_tools::ProjectToolExecutor;
use crate::prompt::build_system_prompt;
use crate::tidy::extract_json;
use crate::tools::VaultToolExecutor;
use crate::types::{ContentBlock, Message, StopReason, ToolDef, Usage};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

pub struct IngestConfig {
    /// Maximum exploration iterations (tool-use rounds).
    pub max_explore_iterations: usize,
    /// Maximum tokens across the entire pipeline.
    pub max_tokens: u64,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            max_explore_iterations: 20,
            max_tokens: 200_000,
        }
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum IngestEvent {
    ExploreStart,
    ExploreIteration { iteration: usize },
    ExploreToolCall { name: String },
    ExploreDone { iterations: usize },
    PlanStart,
    PlanReady { plan: IngestPlan },
    GenerateStart { total: usize },
    GenerateNote { index: usize, path: String },
    GenerateDone { index: usize, path: String },
    Done { session_id: String, usage: Usage },
    Error { message: String },
}

// ---------------------------------------------------------------------------
// Plan types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestPlan {
    pub notes: Vec<PlannedNote>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedNote {
    pub path: String,
    pub title: String,
    pub summary: String,
    /// Project-relative paths used as source material.
    pub source_files: Vec<String>,
}

// ---------------------------------------------------------------------------
// Phase result types
// ---------------------------------------------------------------------------

pub struct ExploreResult {
    pub summary: String,
    pub usage: Usage,
}

pub struct PlanResult {
    pub plan: IngestPlan,
    pub usage: Usage,
    pub estimated_gen: Usage,
}

pub struct GenerateResult {
    pub session_id: String,
    pub drafted_paths: Vec<String>,
    pub usage: Usage,
}

// ---------------------------------------------------------------------------
// Result (for convenience wrapper)
// ---------------------------------------------------------------------------

pub struct IngestResult {
    pub plan: IngestPlan,
    pub session_id: Option<String>,
    pub drafted_paths: Vec<String>,
    pub usage: Usage,
}

// ---------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------

const EXPLORE_TASK: &str = r#"You are exploring an external codebase to understand its structure, purpose, and key concepts.

## Your job

Use the available tools to explore the project. You have:
- `project_tree` — show directory structure
- `project_list_files` — list files, optionally filtered by glob
- `project_read_file` — read a file's contents
- `project_search` — search file contents with regex
- `vault_search` — search existing vault notes (to avoid duplicates)
- `vault_read` — read an existing vault note
- `vault_list` — list existing vault notes

## Strategy

1. Start with `project_tree` to understand the layout
2. Read key files (README, main entry points, config files)
3. Identify the most important concepts, patterns, and components
4. Check the vault for existing notes on these topics
5. Focus on what would be most valuable as vault notes

## When done

When you have gathered enough information, output your findings inside <summary> tags:

<summary>
Describe:
- Project purpose and architecture
- Key concepts worth documenting
- Important files and their roles
- What already exists in the vault (to avoid duplication)
- Suggested note topics with source files
</summary>"#;

const PLAN_TASK: &str = r#"You are planning which vault notes to create from an exploration of an external codebase.

## Your job

Based on the exploration summary below, decide which notes to create. Each note should capture a distinct concept, pattern, or component worth remembering.

## Rules

1. One idea per note. Keep notes atomic and self-contained.
2. Don't duplicate content that already exists in the vault.
3. Choose descriptive paths following the vault's taxonomy.
4. Include source_files listing which project files informed each note.
5. Keep summaries concise but specific enough to guide generation.

## Output format

Respond with ONLY a JSON object (no markdown fences, no explanation):

{
  "notes": [
    {
      "path": "concepts/topic-name.md",
      "title": "Topic Name",
      "summary": "One-line description of what this note should contain",
      "source_files": ["src/main.rs", "src/lib.rs"]
    }
  ]
}"#;

const GENERATE_TASK: &str = r#"You are writing a single note for a knowledge vault, based on source material from an external codebase.

## Your job

Write the note described below, following the style guide exactly. The note should be:
- Self-contained and readable without the source code
- Cross-linked to relevant existing notes using [[wikilinks]]
- Tagged appropriately in the frontmatter
- Focused on concepts and understanding, not just code listings

## Output format

Respond with ONLY the note content in markdown. Start with YAML frontmatter (---), then the body.
Do not wrap in code fences. Do not add explanations before or after.

Example structure:
---
title: Note Title
tags: [tag1, tag2]
---

**One-line bold summary.**

Body content here with [[wikilinks]] to related notes."#;

// ---------------------------------------------------------------------------
// IngestEngine
// ---------------------------------------------------------------------------

pub struct IngestEngine<'a> {
    llm: &'a dyn LlmBackend,
    project_executor: ProjectToolExecutor,
    project_name: String,
    vault: Arc<Mutex<Vault>>,
    vault_executor: VaultToolExecutor,
    profile: &'a BrainProfile,
    domain_skill: Option<&'a str>,
    event_tx: Option<&'a mpsc::UnboundedSender<IngestEvent>>,
}

impl<'a> IngestEngine<'a> {
    pub fn new(
        llm: &'a dyn LlmBackend,
        project_root: &Path,
        vault: Arc<Mutex<Vault>>,
        profile: &'a BrainProfile,
        domain_skill: Option<&'a str>,
        event_tx: Option<&'a mpsc::UnboundedSender<IngestEvent>>,
    ) -> Result<Self> {
        let project_executor = ProjectToolExecutor::new(project_root)
            .map_err(|e| AgentError::Config(e.to_string()))?;

        let project_name = project_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project")
            .to_string();

        let vault_executor = VaultToolExecutor::new(
            vault.clone(),
            crate::tools::SessionContext {
                session_id: String::new(),
                task: "ingest-explore".into(),
                model: llm.model_name().into(),
                provider: llm.provider_name().into(),
            },
        );

        Ok(Self {
            llm,
            project_executor,
            project_name,
            vault,
            vault_executor,
            profile,
            domain_skill,
            event_tx,
        })
    }

    pub fn model_name(&self) -> &str {
        self.llm.model_name()
    }

    pub async fn explore(&self, config: &IngestConfig) -> Result<ExploreResult> {
        let (summary, usage) = run_explore(
            self.llm,
            &self.project_executor,
            &self.vault_executor,
            self.profile,
            self.domain_skill,
            config,
            self.event_tx,
        )
        .await?;
        Ok(ExploreResult { summary, usage })
    }

    pub async fn plan(
        &self,
        summary: &str,
        explore_usage: Usage,
    ) -> Result<PlanResult> {
        // Generate vault context for plan phase
        let vault_context = {
            let v = self.vault.lock().await;
            if summary.is_empty() {
                String::new()
            } else {
                let query: String = summary.chars().take(500).collect();
                generate_context(&v, &query, 20)
            }
        };

        let (plan, plan_usage) = run_plan(
            self.llm,
            summary,
            &vault_context,
            self.profile,
            self.domain_skill,
            self.event_tx,
        )
        .await?;

        let mut spent = explore_usage;
        spent.accumulate(&plan_usage);

        let estimated_gen = estimate_generation_usage(&plan);

        Ok(PlanResult {
            plan,
            usage: spent,
            estimated_gen,
        })
    }

    pub async fn generate(
        &self,
        plan: &IngestPlan,
        plan_usage: Usage,
    ) -> Result<GenerateResult> {
        // Re-generate vault context for generate phase
        let vault_context = {
            let v = self.vault.lock().await;
            // Use first note title as rough search query
            let query = plan
                .notes
                .first()
                .map(|n| n.title.clone())
                .unwrap_or_default();
            if query.is_empty() {
                String::new()
            } else {
                generate_context(&v, &query, 20)
            }
        };

        // Compute input hash for dedup
        let input_hash = hash_project(&self.project_executor);

        // Conflict detection
        {
            let v = self.vault.lock().await;
            let drafts = v.drafts();
            let output_paths: Vec<&str> =
                plan.notes.iter().map(|n| n.path.as_str()).collect();

            let sessions = drafts.list_sessions().map_err(AgentError::Vault)?;
            for session in &sessions {
                if session.pending_drafts > 0 {
                    if let Some(ref h) = session.input_hash {
                        if *h == input_hash {
                            send_event(
                                self.event_tx,
                                IngestEvent::Error {
                                    message: format!(
                                        "same project already has pending drafts in session {} ({}/{})",
                                        session.id, session.provider, session.model
                                    ),
                                },
                            );
                        }
                    }
                }
            }

            if let Ok(conflicts) = drafts.find_conflicts(&output_paths) {
                for (sid, info, paths) in conflicts {
                    let model = format!("{}/{}", info.provider, info.model);
                    for path in paths {
                        send_event(
                            self.event_tx,
                            IngestEvent::Error {
                                message: format!(
                                    "{path} already has a pending draft from session {sid} ({model})"
                                ),
                            },
                        );
                    }
                }
            }
        }

        // Create session and generate

        let (session_id, drafted_paths, gen_usage) = {
            let v = self.vault.lock().await;
            let drafts = v.drafts();

            let session_id = drafts
                .create_session(SessionMeta {
                    source: "ingest".to_string(),
                    provider: self.llm.provider_name().to_string(),
                    model: self.llm.model_name().to_string(),
                    task: format!("ingest {}", self.project_name),
                    input_hash: Some(input_hash),
                })
                .map_err(AgentError::Vault)?;

            let (drafted_paths, gen_usage) = run_generate(
                self.llm,
                plan,
                &self.project_executor,
                &vault_context,
                self.profile,
                self.domain_skill,
                drafts,
                &session_id,
                self.event_tx,
            )
            .await?;

            (session_id, drafted_paths, gen_usage)
        };

        let mut total_usage = plan_usage;
        total_usage.accumulate(&gen_usage);

        send_event(
            self.event_tx,
            IngestEvent::Done {
                session_id: session_id.clone(),
                usage: total_usage.clone(),
            },
        );

        Ok(GenerateResult {
            session_id,
            drafted_paths,
            usage: total_usage,
        })
    }

    pub fn estimate_generation(&self, plan: &IngestPlan) -> Usage {
        estimate_generation_usage(plan)
    }
}

// ---------------------------------------------------------------------------
// Cost estimation helper
// ---------------------------------------------------------------------------

pub fn estimate_generation_usage(plan: &IngestPlan) -> Usage {
    let avg_source_tokens: u64 = 4000;
    let est_input: u64 = plan
        .notes
        .iter()
        .map(|n| n.source_files.len() as u64 * avg_source_tokens * 2)
        .sum();
    let est_output: u64 = plan.notes.len() as u64 * 2000;
    Usage {
        input_tokens: est_input,
        output_tokens: est_output,
    }
}

// ---------------------------------------------------------------------------
// Phase 1: Explore (private)
// ---------------------------------------------------------------------------

async fn run_explore(
    llm: &dyn LlmBackend,
    project_executor: &ProjectToolExecutor,
    vault_executor: &VaultToolExecutor,
    profile: &BrainProfile,
    domain_skill: Option<&str>,
    config: &IngestConfig,
    event_tx: Option<&mpsc::UnboundedSender<IngestEvent>>,
) -> Result<(String, Usage)> {
    send_event(event_tx, IngestEvent::ExploreStart);

    // Combined tool defs
    let mut tools: Vec<ToolDef> = ProjectToolExecutor::tool_defs();
    tools.extend(VaultToolExecutor::read_only_tool_defs());

    let system = build_system_prompt(
        profile.taxonomy(),
        profile.style(),
        domain_skill,
        EXPLORE_TASK,
        None,
    );

    let mut messages = vec![Message::user(
        "Explore this project and summarize your findings.",
    )];
    let mut total_usage = Usage::default();

    for iteration in 0..config.max_explore_iterations {
        send_event(
            event_tx,
            IngestEvent::ExploreIteration { iteration },
        );

        let response = llm.chat(&system, &messages, &tools).await?;
        total_usage.accumulate(&response.usage);

        let text = response.text();

        // Check for <summary> in text output
        if let Some(summary) = extract_summary(&text) {
            send_event(
                event_tx,
                IngestEvent::ExploreDone {
                    iterations: iteration + 1,
                },
            );
            info!(
                "exploration done after {} iterations ({} tokens)",
                iteration + 1,
                total_usage.total()
            );
            return Ok((summary.to_string(), total_usage));
        }

        // Token budget check
        if total_usage.total() >= config.max_tokens / 2 {
            warn!(
                "exploration using 50%+ of token budget ({}/{}), stopping",
                total_usage.total(),
                config.max_tokens
            );
            // Force a summary by returning what we have
            if !text.is_empty() {
                send_event(
                    event_tx,
                    IngestEvent::ExploreDone {
                        iterations: iteration + 1,
                    },
                );
                return Ok((text, total_usage));
            }
        }

        // Check stop reason — if no tool calls, we're done
        if response.stop_reason == StopReason::EndTurn
            || response.stop_reason == StopReason::MaxTokens
        {
            send_event(
                event_tx,
                IngestEvent::ExploreDone {
                    iterations: iteration + 1,
                },
            );
            return Ok((text, total_usage));
        }

        // Handle tool calls
        let tool_calls: Vec<_> = response
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
            .cloned()
            .collect();

        if tool_calls.is_empty() {
            send_event(
                event_tx,
                IngestEvent::ExploreDone {
                    iterations: iteration + 1,
                },
            );
            return Ok((text, total_usage));
        }

        messages.push(Message::assistant(response.content));

        let mut results = Vec::new();
        for call in &tool_calls {
            if let ContentBlock::ToolUse { id, name, input } = call {
                send_event(
                    event_tx,
                    IngestEvent::ExploreToolCall { name: name.clone() },
                );

                let result = dispatch_tool(name, input, project_executor, vault_executor).await;

                match result {
                    Ok(output) => {
                        debug!("tool {name} ok: {}...", &output[..output.len().min(100)]);
                        results.push(ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: output,
                            is_error: false,
                        });
                    }
                    Err(err) => {
                        debug!("tool {name} error: {err}");
                        results.push(ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: err,
                            is_error: true,
                        });
                    }
                }
            }
        }

        messages.push(Message::tool_results(results));
    }

    // Exhausted iterations
    warn!(
        "exploration hit max iterations ({})",
        config.max_explore_iterations
    );
    Err(AgentError::Llm(format!(
        "exploration exceeded max iterations ({})",
        config.max_explore_iterations
    )))
}

/// Dispatch a tool call to the appropriate executor by name prefix.
async fn dispatch_tool(
    name: &str,
    input: &serde_json::Value,
    project_executor: &ProjectToolExecutor,
    vault_executor: &VaultToolExecutor,
) -> std::result::Result<String, String> {
    if name.starts_with("project_") {
        project_executor.execute(name, input)
    } else if name.starts_with("vault_") {
        vault_executor.execute(name, input).await
    } else {
        Err(format!("unknown tool: {name}"))
    }
}

/// Extract content from <summary>...</summary> tags.
fn extract_summary(text: &str) -> Option<&str> {
    let start_tag = "<summary>";
    let end_tag = "</summary>";
    let start = text.find(start_tag)?;
    let end = text.find(end_tag)?;
    if end <= start {
        return None;
    }
    Some(text[start + start_tag.len()..end].trim())
}

// ---------------------------------------------------------------------------
// Phase 2: Plan (private)
// ---------------------------------------------------------------------------

async fn run_plan(
    llm: &dyn LlmBackend,
    exploration_summary: &str,
    vault_context: &str,
    profile: &BrainProfile,
    domain_skill: Option<&str>,
    event_tx: Option<&mpsc::UnboundedSender<IngestEvent>>,
) -> Result<(IngestPlan, Usage)> {
    send_event(event_tx, IngestEvent::PlanStart);

    let user_msg = format!(
        "<exploration_summary>\n{exploration_summary}\n</exploration_summary>"
    );

    let system = build_system_prompt(
        profile.taxonomy(),
        profile.style(),
        domain_skill,
        PLAN_TASK,
        if vault_context.is_empty() {
            None
        } else {
            Some(vault_context)
        },
    );

    let response = llm.chat(&system, &[Message::user(user_msg)], &[]).await?;

    let text = response.text();
    debug!("plan response: {}", &text[..text.len().min(500)]);

    let json_str = extract_json(&text);
    let plan: IngestPlan = serde_json::from_str(json_str).map_err(|e| {
        AgentError::Llm(format!(
            "failed to parse ingest plan JSON: {e}\n\nraw response:\n{text}"
        ))
    })?;

    info!("ingest plan: {} notes", plan.notes.len());

    send_event(event_tx, IngestEvent::PlanReady { plan: plan.clone() });

    Ok((plan, response.usage))
}

// ---------------------------------------------------------------------------
// Phase 3: Generate (private)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_generate(
    llm: &dyn LlmBackend,
    plan: &IngestPlan,
    project_executor: &ProjectToolExecutor,
    vault_context: &str,
    profile: &BrainProfile,
    domain_skill: Option<&str>,
    drafts: &arcana_core::DraftManager,
    session_id: &str,
    event_tx: Option<&mpsc::UnboundedSender<IngestEvent>>,
) -> Result<(Vec<String>, Usage)> {
    send_event(
        event_tx,
        IngestEvent::GenerateStart {
            total: plan.notes.len(),
        },
    );

    let mut total_usage = Usage::default();
    let mut drafted_paths = Vec::new();

    for (i, note) in plan.notes.iter().enumerate() {
        send_event(
            event_tx,
            IngestEvent::GenerateNote {
                index: i,
                path: note.path.clone(),
            },
        );

        // Re-read source files for this note
        let mut source_material = String::new();
        for src_path in &note.source_files {
            let content = project_executor.execute(
                "project_read_file",
                &serde_json::json!({"path": src_path}),
            );
            match content {
                Ok(text) => {
                    source_material.push_str(&format!(
                        "<source_file path=\"{src_path}\">\n{text}\n</source_file>\n\n"
                    ));
                }
                Err(e) => {
                    debug!("could not re-read source {src_path}: {e}");
                }
            }
        }

        let user_msg = format!(
            "<assignment>\n\
             Write a note at: {path}\n\
             Title: {title}\n\
             Description: {summary}\n\
             </assignment>\n\n\
             {source_material}",
            path = note.path,
            title = note.title,
            summary = note.summary,
        );

        let system = build_system_prompt(
            profile.taxonomy(),
            profile.style(),
            domain_skill,
            GENERATE_TASK,
            if vault_context.is_empty() {
                None
            } else {
                Some(vault_context)
            },
        );

        let response = llm.chat(&system, &[Message::user(user_msg)], &[]).await?;
        total_usage.accumulate(&response.usage);

        let raw = response.text();

        // Parse LLM output and inject AI provenance
        let content = match arcana_core::Note::parse(
            std::path::PathBuf::from(&note.path),
            &raw,
            arcana_core::FileMeta {
                size_bytes: 0,
                modified_on_disk: std::time::SystemTime::now(),
                content_hash: 0,
            },
        ) {
            Ok(mut parsed) => {
                parsed.frontmatter.ai = Some(arcana_core::AiMeta {
                    model: llm.model_name().to_string(),
                    provider: llm.provider_name().to_string(),
                    agent_session: session_id.to_string(),
                    task: format!("ingest: {}", note.summary),
                    prompt: String::new(),
                    sources: note.source_files.clone(),
                    confidence: arcana_core::Confidence::Medium,
                    reviewed: false,
                    generated_at: chrono::Utc::now(),
                });
                parsed.to_string()
            }
            Err(_) => raw,
        };

        drafts
            .create_draft(session_id, &note.path, &content)
            .map_err(AgentError::Vault)?;

        drafted_paths.push(note.path.clone());

        send_event(
            event_tx,
            IngestEvent::GenerateDone {
                index: i,
                path: note.path.clone(),
            },
        );

        debug!(
            "generated draft {}/{}: {}",
            i + 1,
            plan.notes.len(),
            note.path
        );
    }

    Ok((drafted_paths, total_usage))
}

// ---------------------------------------------------------------------------
// Input hashing
// ---------------------------------------------------------------------------

/// Compute a stable hash from the project tree output for dedup across runs.
fn hash_project(project_executor: &ProjectToolExecutor) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let tree = project_executor
        .execute("project_tree", &serde_json::json!({"depth": 4}))
        .unwrap_or_default();

    let mut hasher = DefaultHasher::new();
    tree.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

// ---------------------------------------------------------------------------
// Convenience wrapper
// ---------------------------------------------------------------------------

/// Run the full ingest pipeline: explore → plan → generate → drafts.
///
/// Thin wrapper over `IngestEngine` — calls all three phases and returns
/// the combined result. Used by tests and `--auto` mode.
pub async fn run_ingest(
    llm: &dyn LlmBackend,
    project_root: &Path,
    vault: Arc<Mutex<Vault>>,
    profile: &BrainProfile,
    domain_skill: Option<&str>,
    config: &IngestConfig,
    event_tx: Option<&mpsc::UnboundedSender<IngestEvent>>,
) -> Result<IngestResult> {
    let engine = IngestEngine::new(
        llm,
        project_root,
        vault,
        profile,
        domain_skill,
        event_tx,
    )?;

    let explored = engine.explore(config).await?;

    let planned = engine
        .plan(&explored.summary, explored.usage)
        .await?;

    if planned.plan.notes.is_empty() {
        return Ok(IngestResult {
            plan: planned.plan,
            session_id: None,
            drafted_paths: Vec::new(),
            usage: planned.usage,
        });
    }

    let generated = engine
        .generate(&planned.plan, planned.usage)
        .await?;

    Ok(IngestResult {
        plan: planned.plan,
        session_id: Some(generated.session_id),
        drafted_paths: generated.drafted_paths,
        usage: generated.usage,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn send_event(tx: Option<&mpsc::UnboundedSender<IngestEvent>>, event: IngestEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::MockBackend;
    use crate::types::{LlmResponse, StopReason, Usage};

    #[test]
    fn extract_summary_basic() {
        let text = "I explored the project.\n<summary>\nThis is a Rust CLI tool.\n</summary>\n";
        assert_eq!(
            extract_summary(text),
            Some("This is a Rust CLI tool.")
        );
    }

    #[test]
    fn extract_summary_none() {
        assert_eq!(extract_summary("no summary here"), None);
    }

    #[test]
    fn extract_summary_empty() {
        assert_eq!(extract_summary("<summary>\n</summary>"), Some(""));
    }

    #[test]
    fn parse_ingest_plan() {
        let json = r#"{
            "notes": [
                {
                    "path": "concepts/error-handling.md",
                    "title": "Error Handling",
                    "summary": "Patterns for error handling in Rust",
                    "source_files": ["src/error.rs", "src/main.rs"]
                },
                {
                    "path": "projects/my-app/architecture.md",
                    "title": "Architecture",
                    "summary": "High-level architecture of the app",
                    "source_files": ["src/lib.rs"]
                }
            ]
        }"#;
        let plan: IngestPlan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.notes.len(), 2);
        assert_eq!(plan.notes[0].path, "concepts/error-handling.md");
        assert_eq!(plan.notes[0].source_files.len(), 2);
    }

    #[tokio::test]
    async fn ingest_explore_and_plan() {
        let project_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project_dir.path().join("src")).unwrap();
        std::fs::write(
            project_dir.path().join("src/main.rs"),
            "fn main() { println!(\"hello\"); }\n",
        )
        .unwrap();
        std::fs::write(project_dir.path().join("README.md"), "# Test\n").unwrap();

        let vault_dir = tempfile::tempdir().unwrap();
        std::fs::write(vault_dir.path().join("seed.md"), "seed note\n").unwrap();
        let vault_config =
            arcana_core::ArcanaConfig::default().with_vault_path(vault_dir.path().to_path_buf());
        let vault = arcana_core::Vault::open(vault_config).unwrap();
        vault.index().unwrap();

        let plan_json = r#"{"notes":[{"path":"concepts/hello-world.md","title":"Hello World","summary":"A simple hello world program","source_files":["src/main.rs"]}]}"#;

        // Explore response: end turn with summary (no tool use)
        let explore_response = LlmResponse {
            content: vec![ContentBlock::Text {
                text: "<summary>\nThis is a simple hello world project.\n</summary>".into(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 500,
                output_tokens: 100,
            },
        };

        // Plan response
        let plan_response = LlmResponse {
            content: vec![ContentBlock::Text {
                text: plan_json.into(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 300,
                output_tokens: 80,
            },
        };

        let mock = MockBackend::new(vec![explore_response, plan_response]);
        let profile = BrainProfile::default();
        let vault = Arc::new(Mutex::new(vault));

        let engine = IngestEngine::new(
            &mock,
            project_dir.path(),
            vault,
            &profile,
            None,
            None,
        )
        .unwrap();

        let explored = engine.explore(&IngestConfig::default()).await.unwrap();
        assert!(explored.summary.contains("hello world"));

        let planned = engine.plan(&explored.summary, explored.usage).await.unwrap();
        assert_eq!(planned.plan.notes.len(), 1);
        assert_eq!(planned.plan.notes[0].path, "concepts/hello-world.md");
        assert!(planned.estimated_gen.total() > 0);
    }

    #[tokio::test]
    async fn ingest_full_pipeline() {
        let project_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project_dir.path().join("src")).unwrap();
        std::fs::write(
            project_dir.path().join("src/main.rs"),
            "fn main() { println!(\"hello\"); }\n",
        )
        .unwrap();

        let vault_dir = tempfile::tempdir().unwrap();
        std::fs::write(vault_dir.path().join("seed.md"), "seed note\n").unwrap();
        let vault_config =
            arcana_core::ArcanaConfig::default().with_vault_path(vault_dir.path().to_path_buf());
        let vault = arcana_core::Vault::open(vault_config).unwrap();
        vault.index().unwrap();

        let plan_json = r#"{"notes":[{"path":"concepts/hello.md","title":"Hello","summary":"Hello world pattern","source_files":["src/main.rs"]}]}"#;
        let generated_note = "---\ntitle: Hello\ntags: [concept, rust]\n---\n\n**Hello world pattern.**\n\nA simple hello world.\n";

        let mock = MockBackend::new(vec![
            // Explore
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "<summary>\nSimple project.\n</summary>".into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 500,
                    output_tokens: 100,
                },
            },
            // Plan
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: plan_json.into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 300,
                    output_tokens: 80,
                },
            },
            // Generate
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: generated_note.into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 400,
                    output_tokens: 200,
                },
            },
        ]);

        let profile = BrainProfile::default();

        let result = run_ingest(
            &mock,
            project_dir.path(),
            Arc::new(Mutex::new(vault)),
            &profile,
            None,
            &IngestConfig::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.plan.notes.len(), 1);
        assert!(result.session_id.is_some());
        assert_eq!(result.drafted_paths, vec!["concepts/hello.md"]);
        assert_eq!(result.usage.input_tokens, 1200);
        assert_eq!(result.usage.output_tokens, 380);
    }

    #[tokio::test]
    async fn ingest_with_tool_use() {
        let project_dir = tempfile::tempdir().unwrap();
        std::fs::write(project_dir.path().join("README.md"), "# My Project\n").unwrap();

        let vault_dir = tempfile::tempdir().unwrap();
        std::fs::write(vault_dir.path().join("seed.md"), "seed\n").unwrap();
        let vault_config =
            arcana_core::ArcanaConfig::default().with_vault_path(vault_dir.path().to_path_buf());
        let vault = arcana_core::Vault::open(vault_config).unwrap();
        vault.index().unwrap();

        let mock = MockBackend::new(vec![
            // Explore iteration 1: tool call
            LlmResponse {
                content: vec![
                    ContentBlock::Text {
                        text: "Let me explore...".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "project_tree".into(),
                        input: serde_json::json!({}),
                    },
                ],
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                },
            },
            // Explore iteration 2: done with summary
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "<summary>\nA project with a README.\n</summary>".into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 200,
                    output_tokens: 60,
                },
            },
            // Plan
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: r#"{"notes":[]}"#.into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 20,
                },
            },
        ]);

        let result = run_ingest(
            &mock,
            project_dir.path(),
            Arc::new(Mutex::new(vault)),
            &BrainProfile::default(),
            None,
            &IngestConfig::default(),
            None,
        )
        .await
        .unwrap();

        assert!(result.plan.notes.is_empty());
    }

    #[tokio::test]
    async fn ingest_events_sent() {
        let project_dir = tempfile::tempdir().unwrap();
        std::fs::write(project_dir.path().join("file.rs"), "fn main() {}\n").unwrap();

        let vault_dir = tempfile::tempdir().unwrap();
        std::fs::write(vault_dir.path().join("seed.md"), "seed\n").unwrap();
        let vault_config =
            arcana_core::ArcanaConfig::default().with_vault_path(vault_dir.path().to_path_buf());
        let vault = arcana_core::Vault::open(vault_config).unwrap();
        vault.index().unwrap();

        let mock = MockBackend::new(vec![
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "<summary>\nTest project.\n</summary>".into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: r#"{"notes":[{"path":"test.md","title":"Test","summary":"Test note","source_files":["file.rs"]}]}"#.into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "---\ntitle: Test\ntags: [test]\n---\n\nTest.\n".into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
        ]);

        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let result = run_ingest(
            &mock,
            project_dir.path(),
            Arc::new(Mutex::new(vault)),
            &BrainProfile::default(),
            None,
            &IngestConfig::default(),
            Some(&event_tx),
        )
        .await
        .unwrap();

        drop(event_tx);

        let mut events = Vec::new();
        while let Some(e) = event_rx.recv().await {
            events.push(e);
        }

        assert!(result.session_id.is_some());
        assert!(events
            .iter()
            .any(|e| matches!(e, IngestEvent::ExploreStart)));
        assert!(events
            .iter()
            .any(|e| matches!(e, IngestEvent::ExploreDone { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, IngestEvent::PlanReady { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, IngestEvent::GenerateStart { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, IngestEvent::Done { .. })));
    }
}
