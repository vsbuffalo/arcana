use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info};

use arcana_core::{BrainProfile, SessionMeta, Vault};

use crate::agent::{agent_loop, AgentConfig, AgentEvent};
use crate::backend::LlmBackend;
use crate::context::generate_context;
use crate::error::{AgentError, Result};
use crate::project_tools::ProjectToolExecutor;
use crate::prompt::build_system_prompt;
use crate::tools::VaultToolExecutor;
use crate::types::{Message, Usage};
use crate::util::extract_json;

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
    ExploreStart {
        max_iterations: usize,
    },
    ExploreIteration {
        iteration: usize,
        max_iterations: usize,
        tool_count: usize,
        tokens_used: u64,
    },
    ExploreToolCall {
        name: String,
        description: String,
    },
    ExploreBudgetWarning {
        used: u64,
        budget: u64,
    },
    ExploreBudgetExhausted {
        used: u64,
        budget: u64,
    },
    ExploreDone {
        iterations: usize,
        tool_count: usize,
        tokens_used: u64,
    },
    PlanStart,
    PlanReady {
        plan: IngestPlan,
    },
    GenerateStart {
        total: usize,
    },
    GenerateNote {
        index: usize,
        total: usize,
        path: String,
        tokens_used: u64,
    },
    GenerateDone {
        index: usize,
        path: String,
    },
    Done {
        session_id: String,
        usage: Usage,
    },
    Error {
        message: String,
    },
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
// Phase marker types
// ---------------------------------------------------------------------------

pub struct Fresh;

pub struct Explored {
    pub summary: String,
    pub usage: Usage,
}

pub struct Planned {
    pub plan: IngestPlan,
    pub usage: Usage,
    pub estimated_gen: Usage,
    source_sizes: std::collections::HashMap<String, u64>,
}

pub struct Done {
    pub plan: IngestPlan,
    pub session_id: Option<String>,
    pub drafted: Vec<String>,
    pub usage: Usage,
}

// ---------------------------------------------------------------------------
// Shared context (owned, no lifetimes)
// ---------------------------------------------------------------------------

struct IngestInner {
    llm: Box<dyn LlmBackend>,
    project: ProjectToolExecutor,
    project_name: String,
    vault: Arc<Mutex<Vault>>,
    vault_executor: VaultToolExecutor,
    profile: BrainProfile,
    domain_skill: Option<String>,
    event_tx: Option<mpsc::UnboundedSender<IngestEvent>>,
}

// ---------------------------------------------------------------------------
// Ingest pipeline (typestate)
// ---------------------------------------------------------------------------

pub struct Ingest<Phase> {
    pub phase: Phase,
    inner: IngestInner,
}

impl<P> Ingest<P> {
    pub fn model_name(&self) -> &str {
        self.inner.llm.model_name()
    }

    pub fn provider_name(&self) -> &str {
        self.inner.llm.provider_name()
    }
}

impl Ingest<Fresh> {
    pub fn new(
        llm: Box<dyn LlmBackend>,
        project_root: &Path,
        vault: Arc<Mutex<Vault>>,
        profile: BrainProfile,
        domain_skill: Option<String>,
        event_tx: Option<mpsc::UnboundedSender<IngestEvent>>,
    ) -> Result<Self> {
        let project = ProjectToolExecutor::new(project_root)
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
            phase: Fresh,
            inner: IngestInner {
                llm,
                project,
                project_name,
                vault,
                vault_executor,
                profile,
                domain_skill,
                event_tx,
            },
        })
    }

    pub async fn explore(self, config: &IngestConfig) -> Result<Ingest<Explored>> {
        let (summary, usage) = run_explore(
            self.inner.llm.as_ref(),
            &self.inner.project,
            &self.inner.vault_executor,
            &self.inner.profile,
            self.inner.domain_skill.as_deref(),
            config,
            self.inner.event_tx.as_ref(),
        )
        .await?;

        Ok(Ingest {
            phase: Explored { summary, usage },
            inner: self.inner,
        })
    }
}

impl Ingest<Explored> {
    pub fn summary(&self) -> &str {
        &self.phase.summary
    }

    pub fn usage(&self) -> &Usage {
        &self.phase.usage
    }

    pub async fn plan(self) -> Result<Ingest<Planned>> {
        let vault_context = {
            let v = self.inner.vault.lock().await;
            if self.phase.summary.is_empty() {
                String::new()
            } else {
                let query: String = self.phase.summary.chars().take(500).collect();
                generate_context(&v, &query, 20)
            }
        };

        let (plan, plan_usage) = run_plan(
            self.inner.llm.as_ref(),
            &self.phase.summary,
            &vault_context,
            &self.inner.profile,
            self.inner.domain_skill.as_deref(),
            self.inner.event_tx.as_ref(),
        )
        .await?;

        let mut spent = self.phase.usage;
        spent.accumulate(&plan_usage);

        // Collect actual source file sizes for better cost estimation
        let source_sizes = collect_source_sizes(&plan, &self.inner.project);
        let estimated_gen = estimate_generation_usage(&plan, &source_sizes);

        Ok(Ingest {
            phase: Planned {
                plan,
                usage: spent,
                estimated_gen,
                source_sizes,
            },
            inner: self.inner,
        })
    }
}

impl Ingest<Planned> {
    pub fn plan(&self) -> &IngestPlan {
        &self.phase.plan
    }

    pub fn usage(&self) -> &Usage {
        &self.phase.usage
    }

    pub fn estimated_gen(&self) -> &Usage {
        &self.phase.estimated_gen
    }

    /// Replace the plan (Planned → Planned self-transition). Recomputes estimated cost.
    pub fn edit_plan(mut self, new_plan: IngestPlan) -> Self {
        self.phase.estimated_gen = estimate_generation_usage(&new_plan, &self.phase.source_sizes);
        self.phase.plan = new_plan;
        self
    }

    pub async fn generate(self) -> Result<Ingest<Done>> {
        // Compute input hash for dedup
        let input_hash = hash_project(&self.inner.project);

        // Conflict detection
        {
            let v = self.inner.vault.lock().await;
            let drafts = v.drafts();
            let output_paths: Vec<&str> = self
                .phase
                .plan
                .notes
                .iter()
                .map(|n| n.path.as_str())
                .collect();

            let sessions = drafts.list_sessions().map_err(AgentError::Vault)?;
            for session in &sessions {
                if session.pending_drafts > 0 {
                    if let Some(ref h) = session.input_hash {
                        if *h == input_hash {
                            send_event(
                                self.inner.event_tx.as_ref(),
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
                            self.inner.event_tx.as_ref(),
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
            let v = self.inner.vault.lock().await;
            let drafts = v.drafts();

            let session_id = drafts
                .create_session(SessionMeta {
                    source: "ingest".to_string(),
                    provider: self.inner.llm.provider_name().to_string(),
                    model: self.inner.llm.model_name().to_string(),
                    task: format!("ingest {}", self.inner.project_name),
                    input_hash: Some(input_hash),
                })
                .map_err(AgentError::Vault)?;

            let (drafted_paths, gen_usage) = run_generate(
                self.inner.llm.as_ref(),
                &self.phase.plan,
                &self.inner.project,
                &v,
                &self.inner.profile,
                self.inner.domain_skill.as_deref(),
                drafts,
                &session_id,
                self.inner.event_tx.as_ref(),
            )
            .await?;

            (session_id, drafted_paths, gen_usage)
        };

        let mut total_usage = self.phase.usage;
        total_usage.accumulate(&gen_usage);

        send_event(
            self.inner.event_tx.as_ref(),
            IngestEvent::Done {
                session_id: session_id.clone(),
                usage: total_usage.clone(),
            },
        );

        Ok(Ingest {
            phase: Done {
                plan: self.phase.plan,
                session_id: Some(session_id),
                drafted: drafted_paths,
                usage: total_usage,
            },
            inner: self.inner,
        })
    }

    /// Skip generate phase when the plan is empty.
    fn skip_generate(self) -> Ingest<Done> {
        Ingest {
            phase: Done {
                plan: self.phase.plan,
                session_id: None,
                drafted: Vec::new(),
                usage: self.phase.usage,
            },
            inner: self.inner,
        }
    }
}

impl Ingest<Done> {
    pub fn plan(&self) -> &IngestPlan {
        &self.phase.plan
    }

    pub fn session_id(&self) -> Option<&str> {
        self.phase.session_id.as_deref()
    }

    pub fn drafted(&self) -> &[String] {
        &self.phase.drafted
    }

    pub fn usage(&self) -> &Usage {
        &self.phase.usage
    }
}

// ---------------------------------------------------------------------------
// Convenience wrapper
// ---------------------------------------------------------------------------

/// Run the full ingest pipeline: explore → plan → generate → drafts.
pub async fn run_ingest_auto(
    llm: Box<dyn LlmBackend>,
    project_root: &Path,
    vault: Arc<Mutex<Vault>>,
    profile: BrainProfile,
    domain_skill: Option<String>,
    config: &IngestConfig,
    event_tx: Option<mpsc::UnboundedSender<IngestEvent>>,
) -> Result<Ingest<Done>> {
    let ingest = Ingest::new(llm, project_root, vault, profile, domain_skill, event_tx)?
        .explore(config)
        .await?
        .plan()
        .await?;

    if ingest.plan().notes.is_empty() {
        return Ok(ingest.skip_generate());
    }

    ingest.generate().await
}

// ---------------------------------------------------------------------------
// Cost estimation helper
// ---------------------------------------------------------------------------

/// Estimate generation cost. When `source_sizes` is provided (bytes per file),
/// uses `bytes / 4` for token approximation. Otherwise falls back to a constant.
pub fn estimate_generation_usage(
    plan: &IngestPlan,
    source_sizes: &std::collections::HashMap<String, u64>,
) -> Usage {
    const FALLBACK_TOKENS: u64 = 4000;
    // System prompt + vault context overhead per LLM call
    const OVERHEAD_TOKENS: u64 = 2000;

    let est_input: u64 = plan
        .notes
        .iter()
        .map(|n| {
            let source_tokens: u64 = n
                .source_files
                .iter()
                .map(|f| {
                    source_sizes
                        .get(f)
                        .map(|bytes| bytes / 4)
                        .unwrap_or(FALLBACK_TOKENS)
                })
                .sum();
            source_tokens + OVERHEAD_TOKENS
        })
        .sum();
    let est_output: u64 = plan.notes.len() as u64 * 2000;
    Usage {
        input_tokens: est_input,
        output_tokens: est_output,
    }
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
Do NOT include an `ai:` block in the frontmatter — it will be injected automatically.

Example structure:
---
title: Note Title
tags: [tag1, tag2]
---

**One-line bold summary.**

Body content here with [[wikilinks]] to related notes."#;

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
    // Scale iterations with project size, capped by config
    let file_count = project_executor.file_count();
    let auto_iters = if file_count < 100 {
        10
    } else if file_count < 1000 {
        15
    } else {
        20
    };
    let max_iters = auto_iters.min(config.max_explore_iterations);

    send_event(
        event_tx,
        IngestEvent::ExploreStart {
            max_iterations: max_iters,
        },
    );

    let system = build_system_prompt(
        profile.taxonomy(),
        profile.style(),
        domain_skill,
        EXPLORE_TASK,
        None,
    );

    let agent_config = AgentConfig {
        max_iterations: max_iters,
        max_tokens: config.max_tokens / 2, // explore gets half the budget
        wrap_up_message: Some(
            "You are running low on your token budget. Stop exploring now and provide your \
             exploration summary with what you have gathered so far. Do not make any more \
             tool calls — just write your summary."
                .into(),
        ),
    };

    // Composite executor: project tools + read-only vault tools
    let explore_executor = ExploreExecutor {
        project: project_executor,
        vault: vault_executor,
    };

    // Bridge AgentEvents → IngestEvents with running counters
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let ingest_tx = event_tx.cloned();
    let bridge = tokio::spawn(async move {
        let mut tool_count: usize = 0;
        let mut tokens_used: u64 = 0;

        while let Some(ev) = agent_rx.recv().await {
            if let Some(ref tx) = ingest_tx {
                match ev {
                    AgentEvent::IterationStart { iteration } => {
                        let _ = tx.send(IngestEvent::ExploreIteration {
                            iteration,
                            max_iterations: max_iters,
                            tool_count,
                            tokens_used,
                        });
                    }
                    AgentEvent::ToolStart { name, input } => {
                        tool_count += 1;
                        let description = describe_tool_call(&name, &input);
                        let _ = tx.send(IngestEvent::ExploreToolCall { name, description });
                    }
                    AgentEvent::Done { usage } => {
                        tokens_used = usage.total();
                    }
                    AgentEvent::TokenWarning { used, budget } => {
                        let _ = tx.send(IngestEvent::ExploreBudgetWarning { used, budget });
                    }
                    AgentEvent::TokenBudgetExhausted { used, budget } => {
                        tokens_used = used;
                        let _ = tx.send(IngestEvent::ExploreBudgetExhausted { used, budget });
                    }
                    _ => {}
                }
            }
        }
    });

    // Give the AI context about budget and project size so it can pace itself
    let budget_k = agent_config.max_tokens / 1000;
    let strategy_hint = if file_count < 100 {
        "This is a small project — you can afford to read most files."
    } else if file_count < 1000 {
        "This is a medium project — read key files (READMEs, entry points, config), skim the rest via tree and search."
    } else {
        "This is a large project — be very selective. Use tree and search to navigate, only read the most important files."
    };
    let initial_msg = format!(
        "Explore this project and summarize your findings.\n\n\
         Context: this project has ~{file_count} files. Your token budget for exploration \
         is {budget_k}K tokens. {strategy_hint} \
         You will be warned when your budget is running low."
    );
    let mut messages = vec![Message::user(initial_msg)];

    let result = agent_loop(
        llm,
        &system,
        &mut messages,
        &explore_executor,
        &agent_config,
        Some(&agent_tx),
    )
    .await;

    drop(agent_tx);
    let _ = bridge.await;

    let (text, usage) = result?;

    // Extract <summary> tags if present, otherwise use raw text
    let summary = extract_summary(&text)
        .map(|s| s.to_string())
        .unwrap_or(text);

    let iterations = messages.len() / 2; // approximate
    send_event(
        event_tx,
        IngestEvent::ExploreDone {
            iterations,
            tool_count: 0, // bridge tracked this but we don't have it here; CLI uses Done event
            tokens_used: usage.total(),
        },
    );

    info!("exploration done ({} tokens)", usage.total());

    Ok((summary, usage))
}

// ---------------------------------------------------------------------------
// ExploreExecutor: combines project + read-only vault tools for exploration
// ---------------------------------------------------------------------------

/// Composite executor for the explore phase. Dispatches by tool name prefix.
struct ExploreExecutor<'a> {
    project: &'a ProjectToolExecutor,
    vault: &'a VaultToolExecutor,
}

#[async_trait::async_trait]
impl crate::executor::ToolExecutor for ExploreExecutor<'_> {
    async fn execute(
        &self,
        name: &str,
        input: &serde_json::Value,
    ) -> std::result::Result<String, String> {
        if name.starts_with("project_") {
            self.project.dispatch(name, input)
        } else if name.starts_with("vault_") {
            self.vault.dispatch(name, input).await
        } else {
            Err(format!("unknown tool: {name}"))
        }
    }

    fn tool_defs(&self) -> Vec<crate::types::ToolDef> {
        let mut defs = ProjectToolExecutor::tool_defs();
        defs.extend(VaultToolExecutor::read_only_tool_defs());
        defs
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

    let user_msg = format!("<exploration_summary>\n{exploration_summary}\n</exploration_summary>");

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
    vault: &arcana_core::Vault,
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
                total: plan.notes.len(),
                path: note.path.clone(),
                tokens_used: total_usage.total(),
            },
        );

        // Re-read source files for this note
        let mut source_material = String::new();
        for src_path in &note.source_files {
            let content = project_executor
                .dispatch("project_read_file", &serde_json::json!({"path": src_path}));
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

        // Per-note vault context for better cross-linking accuracy
        let query = format!("{} {}", note.title, note.summary);
        let vault_context = generate_context(vault, &query, 20);

        let system = build_system_prompt(
            profile.taxonomy(),
            profile.style(),
            domain_skill,
            GENERATE_TASK,
            Some(&vault_context),
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

/// Query project source file sizes for cost estimation.
fn collect_source_sizes(
    plan: &IngestPlan,
    project: &ProjectToolExecutor,
) -> std::collections::HashMap<String, u64> {
    let mut sizes = std::collections::HashMap::new();
    for note in &plan.notes {
        for path in &note.source_files {
            if sizes.contains_key(path) {
                continue;
            }
            if let Ok(content) =
                project.dispatch("project_read_file", &serde_json::json!({"path": path}))
            {
                sizes.insert(path.clone(), content.len() as u64);
            }
        }
    }
    sizes
}

/// Compute a stable hash from the project tree output for dedup across runs.
fn hash_project(project_executor: &ProjectToolExecutor) -> String {
    let tree = project_executor
        .dispatch("project_tree", &serde_json::json!({"depth": 4}))
        .unwrap_or_default();

    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(tree.as_bytes()))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn send_event(tx: Option<&mpsc::UnboundedSender<IngestEvent>>, event: IngestEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

/// Convert a raw tool call into a human-readable description.
fn describe_tool_call(name: &str, input: &serde_json::Value) -> String {
    match name {
        "project_tree" => "tree".to_string(),
        "project_list_files" => {
            let glob = input["glob"].as_str().unwrap_or("*");
            format!("listing {glob}")
        }
        "project_read_file" => {
            let path = input["path"].as_str().unwrap_or("?");
            format!("reading {path}")
        }
        "project_search" => {
            let query = input["query"].as_str().unwrap_or("?");
            format!("searching \"{query}\"")
        }
        "vault_search" => {
            let query = input["query"].as_str().unwrap_or("?");
            format!("vault search \"{query}\"")
        }
        "vault_list" => {
            if let Some(prefix) = input["path_prefix"].as_str() {
                format!("vault list {prefix}")
            } else {
                "vault list".to_string()
            }
        }
        "vault_read" => {
            let path = input["path"].as_str().unwrap_or("?");
            format!("vault read {path}")
        }
        _ => name.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::MockBackend;
    use crate::types::{ContentBlock, LlmResponse, StopReason, Usage};

    #[test]
    fn extract_summary_basic() {
        let text = "I explored the project.\n<summary>\nThis is a Rust CLI tool.\n</summary>\n";
        assert_eq!(extract_summary(text), Some("This is a Rust CLI tool."));
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

        let mock = MockBackend::new(vec![
            // Explore response
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "<summary>\nThis is a simple hello world project.\n</summary>".into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 500,
                    output_tokens: 100,
                },
            },
            // Plan response
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
        ]);

        let profile = BrainProfile::default();

        let ingest = Ingest::new(
            Box::new(mock),
            project_dir.path(),
            Arc::new(Mutex::new(vault)),
            profile,
            None,
            None,
        )
        .unwrap();

        let ingest = ingest.explore(&IngestConfig::default()).await.unwrap();
        assert!(ingest.summary().contains("hello world"));

        let ingest = ingest.plan().await.unwrap();
        assert_eq!(ingest.plan().notes.len(), 1);
        assert_eq!(ingest.plan().notes[0].path, "concepts/hello-world.md");
        assert!(ingest.estimated_gen().total() > 0);
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

        let result = run_ingest_auto(
            Box::new(mock),
            project_dir.path(),
            Arc::new(Mutex::new(vault)),
            profile,
            None,
            &IngestConfig::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.plan().notes.len(), 1);
        assert!(result.session_id().is_some());
        assert_eq!(result.drafted(), &["concepts/hello.md"]);
        assert_eq!(result.usage().input_tokens, 1200);
        assert_eq!(result.usage().output_tokens, 380);
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

        let result = run_ingest_auto(
            Box::new(mock),
            project_dir.path(),
            Arc::new(Mutex::new(vault)),
            BrainProfile::default(),
            None,
            &IngestConfig::default(),
            None,
        )
        .await
        .unwrap();

        assert!(result.plan().notes.is_empty());
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

        let result = run_ingest_auto(
            Box::new(mock),
            project_dir.path(),
            Arc::new(Mutex::new(vault)),
            BrainProfile::default(),
            None,
            &IngestConfig::default(),
            Some(event_tx),
        )
        .await
        .unwrap();

        assert!(result.session_id().is_some());

        // Drop the result to close the event channel
        drop(result);

        let mut events = Vec::new();
        while let Some(e) = event_rx.recv().await {
            events.push(e);
        }

        assert!(events
            .iter()
            .any(|e| matches!(e, IngestEvent::ExploreStart { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, IngestEvent::ExploreDone { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, IngestEvent::PlanReady { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, IngestEvent::GenerateStart { .. })));
        assert!(events.iter().any(|e| matches!(e, IngestEvent::Done { .. })));
    }
}
