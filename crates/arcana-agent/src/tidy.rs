use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, info};

use arcana_core::{BrainProfile, DraftManager, SessionMeta, Vault};

use crate::backend::LlmBackend;
use crate::context::generate_context;
use crate::error::{AgentError, Result};
use crate::prompt::build_system_prompt;
use crate::types::{Message, Usage};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for a tidy run.
pub struct TidyConfig {
    /// Maximum tokens per LLM call.
    pub max_tokens: u64,
}

impl Default for TidyConfig {
    fn default() -> Self {
        Self {
            max_tokens: 100_000,
        }
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum TidyEvent {
    SurveyStart {
        count: usize,
    },
    SurveyNote {
        path: String,
    },
    PlanStart,
    PlanReady {
        plan: TidyPlan,
    },
    ConflictWarning {
        path: String,
        existing_session: String,
        existing_model: String,
    },
    SameInputWarning {
        existing_session: String,
        existing_model: String,
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
// Plan types (deserialized from LLM JSON output)
// ---------------------------------------------------------------------------

/// The full tidy plan: what to do with each source note.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TidyPlan {
    pub actions: Vec<TidyAction>,
}

/// A single action in the tidy plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TidyAction {
    /// Move a note to a new location with optional rewrite.
    Move {
        from: String,
        to: String,
        title: String,
        summary: String,
        rewrite: bool,
    },
    /// Merge multiple source notes into one destination (needs LLM).
    /// Created by post-processing when a Move's destination already has content.
    Merge {
        sources: Vec<String>,
        to: String,
        title: String,
        summary: String,
    },
    /// Split a note into multiple new notes.
    Split {
        from: String,
        notes: Vec<PlannedNote>,
    },
    /// Extract a concept from a note (original stays, concept is new).
    ExtractConcept { from: String, concept: PlannedNote },
}

/// A note that will be generated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedNote {
    pub path: String,
    pub title: String,
    pub summary: String,
}

impl TidyPlan {
    /// All output paths that will be generated.
    pub fn output_paths(&self) -> Vec<&str> {
        let mut paths = Vec::new();
        for action in &self.actions {
            match action {
                TidyAction::Move { to, .. } | TidyAction::Merge { to, .. } => {
                    paths.push(to.as_str())
                }
                TidyAction::Split { notes, .. } => {
                    for n in notes {
                        paths.push(n.path.as_str());
                    }
                }
                TidyAction::ExtractConcept { concept, .. } => {
                    paths.push(concept.path.as_str());
                }
            }
        }
        paths
    }

    /// Total number of notes that will be generated.
    pub fn output_count(&self) -> usize {
        self.output_paths().len()
    }

    /// Source paths involved in the plan.
    pub fn source_paths(&self) -> Vec<&str> {
        let mut paths = Vec::new();
        for action in &self.actions {
            match action {
                TidyAction::Move { from, .. } => paths.push(from.as_str()),
                TidyAction::Merge { sources, .. } => {
                    for s in sources {
                        paths.push(s.as_str());
                    }
                }
                TidyAction::Split { from, .. } => paths.push(from.as_str()),
                TidyAction::ExtractConcept { from, .. } => paths.push(from.as_str()),
            }
        }
        paths
    }
}

// ---------------------------------------------------------------------------
// Source note
// ---------------------------------------------------------------------------

pub struct SourceNote {
    pub path: String,
    pub content: String,
}

/// Lightweight note summary for vault-wide structural audit (no body content).
pub struct VaultNoteSummary {
    pub path: String,
    pub title: String,
}

// ---------------------------------------------------------------------------
// Phase marker types
// ---------------------------------------------------------------------------

pub struct Unsurveyed;

pub struct Surveyed {
    pub sources: Vec<SourceNote>,
    pub vault_context: String,
}

pub struct VaultSurveyed {
    pub summaries: Vec<VaultNoteSummary>,
    pub vault_tree: String,
}

pub struct TidyPlanned {
    pub plan: TidyPlan,
    pub sources: Vec<SourceNote>,
    pub vault_context: String,
    pub usage: Usage,
    pub estimated_gen: Usage,
}

pub struct TidyDone {
    pub plan: TidyPlan,
    pub session_id: Option<String>,
    pub drafted: Vec<String>,
    pub usage: Usage,
}

// ---------------------------------------------------------------------------
// Shared context (owned, no lifetimes)
// ---------------------------------------------------------------------------

struct TidyInner {
    llm: Box<dyn LlmBackend>,
    vault: Arc<Mutex<Vault>>,
    profile: BrainProfile,
    domain_skill: Option<String>,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<TidyEvent>>,
}

// ---------------------------------------------------------------------------
// Tidy pipeline (typestate)
// ---------------------------------------------------------------------------

pub struct Tidy<Phase> {
    pub phase: Phase,
    inner: TidyInner,
}

impl<P> Tidy<P> {
    pub fn model_name(&self) -> &str {
        self.inner.llm.model_name()
    }

    pub fn provider_name(&self) -> &str {
        self.inner.llm.provider_name()
    }
}

impl Tidy<Unsurveyed> {
    pub fn new(
        llm: Box<dyn LlmBackend>,
        vault: Arc<Mutex<Vault>>,
        profile: BrainProfile,
        domain_skill: Option<String>,
        event_tx: Option<tokio::sync::mpsc::UnboundedSender<TidyEvent>>,
    ) -> Self {
        Self {
            phase: Unsurveyed,
            inner: TidyInner {
                llm,
                vault,
                profile,
                domain_skill,
                event_tx,
            },
        }
    }

    pub async fn survey(self, target_paths: &[String]) -> Result<Tidy<Surveyed>> {
        let (sources, vault_context) = {
            let v = self.inner.vault.lock().await;
            run_survey(&v, target_paths, self.inner.event_tx.as_ref())?
        };

        if sources.is_empty() {
            return Err(AgentError::Config("no notes found to tidy".into()));
        }

        Ok(Tidy {
            phase: Surveyed {
                sources,
                vault_context,
            },
            inner: self.inner,
        })
    }

    /// Lightweight vault-wide survey: collect paths and titles only (no bodies).
    pub async fn survey_vault(self) -> Result<Tidy<VaultSurveyed>> {
        let (summaries, vault_tree) = {
            let v = self.inner.vault.lock().await;
            run_vault_survey(&v, self.inner.event_tx.as_ref())?
        };

        if summaries.is_empty() {
            return Err(AgentError::Config("vault has no notes to audit".into()));
        }

        Ok(Tidy {
            phase: VaultSurveyed {
                summaries,
                vault_tree,
            },
            inner: self.inner,
        })
    }
}

impl Tidy<Surveyed> {
    pub fn sources(&self) -> &[SourceNote] {
        &self.phase.sources
    }

    pub fn vault_context(&self) -> &str {
        &self.phase.vault_context
    }

    pub async fn plan(self) -> Result<Tidy<TidyPlanned>> {
        let (plan, plan_usage) = run_plan_tidy(
            self.inner.llm.as_ref(),
            &self.phase.sources,
            &self.phase.vault_context,
            &self.inner.profile,
            self.inner.domain_skill.as_deref(),
            self.inner.event_tx.as_ref(),
        )
        .await?;

        let estimated_gen = estimate_generation_usage(&plan, &self.phase.sources);

        Ok(Tidy {
            phase: TidyPlanned {
                plan,
                sources: self.phase.sources,
                vault_context: self.phase.vault_context,
                usage: plan_usage,
                estimated_gen,
            },
            inner: self.inner,
        })
    }
}

impl Tidy<VaultSurveyed> {
    pub fn summaries(&self) -> &[VaultNoteSummary] {
        &self.phase.summaries
    }

    pub async fn plan(self) -> Result<Tidy<TidyPlanned>> {
        let (mut plan, plan_usage) = run_plan_vault(
            self.inner.llm.as_ref(),
            &self.phase.summaries,
            &self.phase.vault_tree,
            &self.inner.profile,
            self.inner.event_tx.as_ref(),
        )
        .await?;

        // Post-process: detect moves whose destination already exists in the
        // vault and upgrade them to merges.
        let existing: std::collections::HashSet<&str> = self
            .phase
            .summaries
            .iter()
            .map(|s| s.path.as_str())
            .collect();
        plan = upgrade_moves_to_merges(plan, &existing);

        let estimated_gen = estimate_vault_generation(&plan, &self.phase.summaries);

        // Sources are loaded lazily in the generate phase.
        let sources = Vec::new();

        Ok(Tidy {
            phase: TidyPlanned {
                plan,
                sources,
                vault_context: self.phase.vault_tree,
                usage: plan_usage,
                estimated_gen,
            },
            inner: self.inner,
        })
    }
}

impl Tidy<TidyPlanned> {
    pub fn plan(&self) -> &TidyPlan {
        &self.phase.plan
    }

    pub fn sources(&self) -> &[SourceNote] {
        &self.phase.sources
    }

    pub fn usage(&self) -> &Usage {
        &self.phase.usage
    }

    pub fn estimated_gen(&self) -> &Usage {
        &self.phase.estimated_gen
    }

    /// Replace the plan (Planned → Planned self-transition). Recomputes estimated cost.
    pub fn edit_plan(mut self, new_plan: TidyPlan) -> Self {
        self.phase.estimated_gen = estimate_generation_usage(&new_plan, &self.phase.sources);
        self.phase.plan = new_plan;
        self
    }

    pub async fn generate(mut self) -> Result<Tidy<TidyDone>> {
        // In vault mode, sources are empty — load them lazily from the plan.
        if self.phase.sources.is_empty() {
            let v = self.inner.vault.lock().await;
            for path in self.phase.plan.source_paths() {
                match v.read_note(path) {
                    Ok(note) => self.phase.sources.push(SourceNote {
                        path: path.to_string(),
                        content: note.body.clone(),
                    }),
                    Err(e) => {
                        send_event(
                            self.inner.event_tx.as_ref(),
                            TidyEvent::Error {
                                message: format!("could not read {path}: {e}"),
                            },
                        );
                    }
                }
            }
        }

        let input_hash = hash_sources(&self.phase.sources);
        let gen_tasks = generation_tasks(&self.phase.plan, &self.phase.sources);
        let target_desc: Vec<&str> = self.phase.sources.iter().map(|s| s.path.as_str()).collect();

        // Conflict detection
        {
            let v = self.inner.vault.lock().await;
            let drafts = v.drafts();
            let output_paths = self.phase.plan.output_paths();

            let sessions = drafts.list_sessions().map_err(AgentError::Vault)?;
            for session in &sessions {
                if session.pending_drafts > 0 {
                    if let Some(ref h) = session.input_hash {
                        if *h == input_hash {
                            send_event(
                                self.inner.event_tx.as_ref(),
                                TidyEvent::SameInputWarning {
                                    existing_session: session.id.clone(),
                                    existing_model: format!(
                                        "{}/{}",
                                        session.provider, session.model
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
                            TidyEvent::ConflictWarning {
                                path,
                                existing_session: sid.clone(),
                                existing_model: model.clone(),
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
                    source: "tidy".to_string(),
                    provider: self.inner.llm.provider_name().to_string(),
                    model: self.inner.llm.model_name().to_string(),
                    task: format!("tidy {}", target_desc.join(", ")),
                    input_hash: Some(input_hash),
                })
                .map_err(AgentError::Vault)?;

            let (drafted_paths, gen_usage) = run_generate(
                self.inner.llm.as_ref(),
                &gen_tasks,
                &self.phase.vault_context,
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
            TidyEvent::Done {
                session_id: session_id.clone(),
                usage: total_usage.clone(),
            },
        );

        Ok(Tidy {
            phase: TidyDone {
                plan: self.phase.plan,
                session_id: Some(session_id),
                drafted: drafted_paths,
                usage: total_usage,
            },
            inner: self.inner,
        })
    }

    /// Skip generate phase when the plan is empty.
    fn skip_generate(self) -> Tidy<TidyDone> {
        Tidy {
            phase: TidyDone {
                plan: self.phase.plan,
                session_id: None,
                drafted: Vec::new(),
                usage: self.phase.usage,
            },
            inner: self.inner,
        }
    }
}

impl Tidy<TidyDone> {
    pub fn plan(&self) -> &TidyPlan {
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

/// Run the full tidy pipeline: survey → plan → generate → drafts.
pub async fn run_tidy_auto(
    llm: Box<dyn LlmBackend>,
    vault: Arc<Mutex<Vault>>,
    target_paths: Vec<String>,
    profile: BrainProfile,
    domain_skill: Option<String>,
    config: &TidyConfig,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<TidyEvent>>,
) -> Result<Tidy<TidyDone>> {
    // config reserved for future use (e.g. max_tokens guard)
    let _ = config;

    let tidy = Tidy::new(llm, vault, profile, domain_skill, event_tx)
        .survey(&target_paths)
        .await?
        .plan()
        .await?;

    if tidy.plan().actions.is_empty() {
        return Ok(tidy.skip_generate());
    }

    tidy.generate().await
}

// ---------------------------------------------------------------------------
// Cost estimation helper
// ---------------------------------------------------------------------------

pub fn estimate_generation_usage(plan: &TidyPlan, sources: &[SourceNote]) -> Usage {
    let source_tokens: u64 = sources
        .iter()
        .map(|s| s.content.len() as u64 / 4) // ~4 chars per token
        .sum();
    let est_input = source_tokens * 2 * plan.output_count() as u64;
    let est_output = plan.output_count() as u64 * 2000;
    Usage {
        input_tokens: est_input,
        output_tokens: est_output,
    }
}

// ---------------------------------------------------------------------------
// Phase 1: Survey (private)
// ---------------------------------------------------------------------------

/// Read target notes and gather vault context for cross-linking.
fn run_survey(
    vault: &Vault,
    target_paths: &[String],
    event_tx: Option<&tokio::sync::mpsc::UnboundedSender<TidyEvent>>,
) -> Result<(Vec<SourceNote>, String)> {
    send_event(
        event_tx,
        TidyEvent::SurveyStart {
            count: target_paths.len(),
        },
    );

    let mut sources = Vec::new();
    let mut search_terms = Vec::new();

    for path in target_paths {
        send_event(event_tx, TidyEvent::SurveyNote { path: path.clone() });

        let note = vault.read_note(path).map_err(AgentError::Vault)?;

        // Extract keywords for vault context search
        let title = note.title().to_string();
        if !title.is_empty() && title != "Untitled" {
            search_terms.push(title);
        }
        // Use first 200 chars of body as search hint
        let hint: String = note.body.chars().take(200).collect();
        if !hint.is_empty() {
            search_terms.push(hint);
        }

        sources.push(SourceNote {
            path: path.clone(),
            content: note.body.clone(),
        });
    }

    // Generate vault context from combined search terms
    let combined_query = search_terms.join(" ");
    let vault_context = if combined_query.is_empty() {
        String::new()
    } else {
        generate_context(vault, &combined_query, 20)
    };

    Ok((sources, vault_context))
}

// ---------------------------------------------------------------------------
// Vault survey (private) — lightweight, paths + titles only
// ---------------------------------------------------------------------------

fn run_vault_survey(
    vault: &Vault,
    event_tx: Option<&tokio::sync::mpsc::UnboundedSender<TidyEvent>>,
) -> Result<(Vec<VaultNoteSummary>, String)> {
    use arcana_core::SearchFilters;

    let all = vault
        .list(&SearchFilters::default(), 5000)
        .map_err(AgentError::Vault)?;

    send_event(event_tx, TidyEvent::SurveyStart { count: all.len() });

    let summaries: Vec<VaultNoteSummary> = all
        .into_iter()
        .map(|r| VaultNoteSummary {
            title: r.title.unwrap_or_default(),
            path: r.path,
        })
        .collect();

    let vault_tree = vault.vault_tree().map_err(AgentError::Vault)?;

    Ok((summaries, vault_tree))
}

// ---------------------------------------------------------------------------
// Vault plan (private) — structural audit, moves only
// ---------------------------------------------------------------------------

const VAULT_PLAN_TASK: &str = r#"You are auditing the structure of a knowledge vault against its taxonomy.

## Your job

Look at every note path and title below. Find notes that are misplaced — in the
wrong zone, at the root level instead of a zone, duplicated across locations, or
in a zone that doesn't exist in the taxonomy.

## Rules

1. Only propose **moves** (no splits, no concept extractions, no rewrites).
2. Set `"rewrite": false` on every action — the content stays unchanged, just the path changes.
3. Route notes to the correct zone per the taxonomy. Root-level notes/dirs that
   clearly belong in an existing zone should be moved there.
4. If a note is already in the right place, do NOT include it.
5. When two zones overlap (e.g. a project note that's also a general concept),
   prefer the more specific location (project > notes).
6. Preserve subfolder structure where it makes sense (e.g. `electronics/basics/x.md`
   → `notes/electronics/basics/x.md`).
7. Keep filenames the same unless they conflict.

## Output format

Respond with ONLY a JSON object (no markdown fences, no explanation):

{
  "actions": [
    {
      "type": "move",
      "from": "misplaced/note.md",
      "to": "notes/correct-zone/note.md",
      "title": "Note Title",
      "summary": "Why this move is correct",
      "rewrite": false
    }
  ]
}

If everything is already in the right place, return: {"actions": []}"#;

/// Post-process a plan: if multiple moves target the same destination, or a
/// move targets a path that already exists in the vault, upgrade to a Merge.
fn upgrade_moves_to_merges(
    plan: TidyPlan,
    existing_paths: &std::collections::HashSet<&str>,
) -> TidyPlan {
    use std::collections::HashMap;

    // Group moves by destination path.
    let mut by_dest: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
    let mut other_actions: Vec<TidyAction> = Vec::new();

    for action in plan.actions {
        match action {
            TidyAction::Move {
                from,
                to,
                title,
                summary,
                ..
            } => {
                by_dest
                    .entry(to.clone())
                    .or_default()
                    .push((from, title, summary));
            }
            other => other_actions.push(other),
        }
    }

    let mut actions = other_actions;

    for (to, moves) in by_dest {
        // Destination already has content in vault — include it as a source.
        let dest_exists = existing_paths.contains(to.as_str());

        if moves.len() == 1 && !dest_exists {
            // Simple move, no collision.
            let (from, title, summary) = moves.into_iter().next().unwrap();
            actions.push(TidyAction::Move {
                from,
                to,
                title,
                summary,
                rewrite: false,
            });
        } else {
            // Multiple sources targeting the same dest, or dest already exists.
            let mut sources: Vec<String> = moves.iter().map(|(f, _, _)| f.clone()).collect();
            if dest_exists {
                sources.push(to.clone());
            }
            let title = moves[0].1.clone();
            let summary = if moves.len() > 1 || dest_exists {
                format!("Merge {} sources into {}", sources.len(), to)
            } else {
                moves[0].2.clone()
            };
            actions.push(TidyAction::Merge {
                sources,
                to,
                title,
                summary,
            });
        }
    }

    TidyPlan { actions }
}

async fn run_plan_vault(
    llm: &dyn LlmBackend,
    summaries: &[VaultNoteSummary],
    vault_tree: &str,
    profile: &BrainProfile,
    event_tx: Option<&tokio::sync::mpsc::UnboundedSender<TidyEvent>>,
) -> Result<(TidyPlan, Usage)> {
    send_event(event_tx, TidyEvent::PlanStart);

    let mut user_msg = String::from("<vault_notes>\n");
    for s in summaries {
        user_msg.push_str(&format!("- {} — {}\n", s.path, s.title));
    }
    user_msg.push_str("</vault_notes>\n\n");
    user_msg.push_str("<vault_tree>\n");
    user_msg.push_str(vault_tree);
    user_msg.push_str("</vault_tree>");

    let system = build_system_prompt(
        profile.taxonomy(),
        profile.style(),
        None,
        VAULT_PLAN_TASK,
        None,
    );

    let response = llm.chat(&system, &[Message::user(user_msg)], &[]).await?;

    let text = response.text();
    debug!("vault plan response: {}", &text[..text.len().min(500)]);

    let json_str = extract_json(&text);
    let plan: TidyPlan = serde_json::from_str(json_str).map_err(|e| {
        AgentError::Llm(format!(
            "failed to parse vault plan JSON: {e}\n\nraw response:\n{text}"
        ))
    })?;

    info!(
        "vault plan: {} actions, {} moves",
        plan.actions.len(),
        plan.output_count()
    );

    send_event(event_tx, TidyEvent::PlanReady { plan: plan.clone() });

    Ok((plan, response.usage))
}

fn estimate_vault_generation(plan: &TidyPlan, _summaries: &[VaultNoteSummary]) -> Usage {
    // Only count actions that actually need LLM generation.
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    for action in &plan.actions {
        match action {
            TidyAction::Move { rewrite: false, .. } => {} // free copy
            TidyAction::Move { rewrite: true, .. } => {
                input_tokens += 1000;
                output_tokens += 500;
            }
            TidyAction::Merge { sources, .. } => {
                // Merges send all source bodies to LLM — estimate per source.
                let n = sources.len() as u64;
                input_tokens += n * 1500; // system + all source bodies
                output_tokens += 1500; // merged output
            }
            TidyAction::Split { .. } | TidyAction::ExtractConcept { .. } => {
                input_tokens += 1000;
                output_tokens += 500;
            }
        }
    }
    Usage {
        input_tokens,
        output_tokens,
    }
}

// ---------------------------------------------------------------------------
// Phase 2: Plan (private)
// ---------------------------------------------------------------------------

const PLAN_TASK: &str = r#"You are organizing messy inbox notes into a structured knowledge vault.

## Your job

Read the source notes below. For each one, decide how to route it according to the taxonomy.

## Rules

1. One idea per output note. If a source note has multiple distinct ideas, split it.
2. Extract general concepts into concepts/ (or equivalent zone). Keep them atomic and self-contained.
3. Route project-specific content to the appropriate project subfolder.
4. Preserve the original meaning. Don't add information that isn't in the source.
5. Choose descriptive filenames (kebab-case, no dates unless relevant).
6. Propose cross-links to existing vault notes where relevant.

## Output format

Respond with ONLY a JSON object (no markdown fences, no explanation):

{
  "actions": [
    {
      "type": "move",
      "from": "inbox/source-note.md",
      "to": "concepts/target-note.md",
      "title": "Note Title",
      "summary": "One-line description of what this note will contain",
      "rewrite": true
    },
    {
      "type": "split",
      "from": "inbox/big-dump.md",
      "notes": [
        {"path": "concepts/thing-a.md", "title": "Thing A", "summary": "..."},
        {"path": "projects/proj/thing-b.md", "title": "Thing B", "summary": "..."}
      ]
    },
    {
      "type": "extract_concept",
      "from": "inbox/project-work.md",
      "concept": {"path": "concepts/general-idea.md", "title": "General Idea", "summary": "..."}
    }
  ]
}"#;

async fn run_plan_tidy(
    llm: &dyn LlmBackend,
    sources: &[SourceNote],
    vault_context: &str,
    profile: &BrainProfile,
    domain_skill: Option<&str>,
    event_tx: Option<&tokio::sync::mpsc::UnboundedSender<TidyEvent>>,
) -> Result<(TidyPlan, Usage)> {
    send_event(event_tx, TidyEvent::PlanStart);

    // Build user message with source notes
    let mut user_msg = String::from("<source_notes>\n");
    for src in sources {
        user_msg.push_str(&format!("<note path=\"{}\">\n", src.path));
        user_msg.push_str(&src.content);
        user_msg.push_str("\n</note>\n\n");
    }
    user_msg.push_str("</source_notes>");

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

    // Parse JSON from response — strip markdown fences if present
    let json_str = extract_json(&text);
    let plan: TidyPlan = serde_json::from_str(json_str).map_err(|e| {
        AgentError::Llm(format!(
            "failed to parse tidy plan JSON: {e}\n\nraw response:\n{text}"
        ))
    })?;

    info!(
        "tidy plan: {} actions, {} output notes",
        plan.actions.len(),
        plan.output_count()
    );

    send_event(event_tx, TidyEvent::PlanReady { plan: plan.clone() });

    Ok((plan, response.usage))
}

/// Extract JSON from LLM response, stripping markdown code fences if present.
pub(crate) fn extract_json(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return &trimmed[start..=end];
        }
    }
    trimmed
}

// ---------------------------------------------------------------------------
// Phase 3: Generate (private)
// ---------------------------------------------------------------------------

const GENERATE_TASK: &str = r#"You are writing a single note for a knowledge vault.

## Your job

Write the note described below, following the style guide exactly. The note should be:
- Self-contained and readable with no context
- Cross-linked to relevant existing notes using [[wikilinks]]
- Tagged appropriately in the frontmatter

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

/// A single generation task derived from the plan.
struct GenerationTask {
    /// Path where this note will be drafted.
    output_path: String,
    /// Title for the note.
    title: String,
    /// Summary / description of what to write.
    summary: String,
    /// Source material (from the source note).
    source_content: String,
    /// Source path (for provenance).
    source_path: String,
    /// Whether to rewrite content via LLM. When false, source content is copied as-is.
    rewrite: bool,
    /// For moves: source path to delete on approve. None for splits/extracts.
    move_source: Option<String>,
}

fn generation_tasks(plan: &TidyPlan, sources: &[SourceNote]) -> Vec<GenerationTask> {
    let find_source = |path: &str| -> String {
        sources
            .iter()
            .find(|s| s.path == path)
            .map(|s| s.content.clone())
            .unwrap_or_default()
    };

    let mut tasks = Vec::new();

    for action in &plan.actions {
        match action {
            TidyAction::Move {
                from,
                to,
                title,
                summary,
                rewrite,
            } => {
                tasks.push(GenerationTask {
                    output_path: to.clone(),
                    title: title.clone(),
                    summary: summary.clone(),
                    source_content: find_source(from),
                    source_path: from.clone(),
                    rewrite: *rewrite,
                    move_source: Some(from.clone()),
                });
            }
            TidyAction::Split { from, notes } => {
                let source = find_source(from);
                for note in notes {
                    tasks.push(GenerationTask {
                        output_path: note.path.clone(),
                        title: note.title.clone(),
                        summary: note.summary.clone(),
                        source_content: source.clone(),
                        source_path: from.clone(),
                        rewrite: true,
                        move_source: None, // splits don't delete source (multiple outputs)
                    });
                }
            }
            TidyAction::Merge {
                sources,
                to,
                title,
                summary,
            } => {
                let combined: String = sources
                    .iter()
                    .map(|s| format!("<source path=\"{s}\">\n{}\n</source>\n", find_source(s)))
                    .collect();
                let source_path = sources.first().cloned().unwrap_or_default();
                // For merges, delete sources that differ from the destination.
                // Sources matching `to` are the existing dest note — don't delete that.
                let merge_sources: Vec<String> = sources
                    .iter()
                    .filter(|s| s.as_str() != to.as_str())
                    .cloned()
                    .collect();
                tasks.push(GenerationTask {
                    output_path: to.clone(),
                    title: title.clone(),
                    summary: summary.clone(),
                    source_content: combined,
                    source_path,
                    rewrite: true,
                    // Store first non-dest source; approve handles one source per draft.
                    // For multiple sources, we'd need multiple original_paths — for now
                    // the most common case is one misplaced note merging into an existing one.
                    move_source: merge_sources.into_iter().next(),
                });
            }
            TidyAction::ExtractConcept { from, concept } => {
                tasks.push(GenerationTask {
                    output_path: concept.path.clone(),
                    title: concept.title.clone(),
                    summary: concept.summary.clone(),
                    source_content: find_source(from),
                    source_path: from.clone(),
                    rewrite: true,
                    move_source: None, // extracts don't delete source
                });
            }
        }
    }

    tasks
}

/// Compute a stable hash of source notes for deduplication across runs.
fn hash_sources(sources: &[SourceNote]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    for s in sources {
        s.path.hash(&mut hasher);
        s.content.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

#[allow(clippy::too_many_arguments)]
async fn run_generate(
    llm: &dyn LlmBackend,
    tasks: &[GenerationTask],
    vault_context: &str,
    profile: &BrainProfile,
    domain_skill: Option<&str>,
    drafts: &DraftManager,
    session_id: &str,
    event_tx: Option<&tokio::sync::mpsc::UnboundedSender<TidyEvent>>,
) -> Result<(Vec<String>, Usage)> {
    send_event(event_tx, TidyEvent::GenerateStart { total: tasks.len() });

    let mut total_usage = Usage::default();
    let mut drafted_paths = Vec::new();

    for (i, task) in tasks.iter().enumerate() {
        send_event(
            event_tx,
            TidyEvent::GenerateNote {
                index: i,
                total: tasks.len(),
                path: task.output_path.clone(),
                tokens_used: total_usage.total(),
            },
        );

        // Fast path: rewrite=false means copy source content as-is (no LLM call).
        let content = if !task.rewrite {
            task.source_content.clone()
        } else {
            let user_msg = format!(
                "<assignment>\n\
                 Write a note at: {path}\n\
                 Title: {title}\n\
                 Description: {summary}\n\
                 </assignment>\n\n\
                 <source_material path=\"{source}\">\n\
                 {content}\n\
                 </source_material>",
                path = task.output_path,
                title = task.title,
                summary = task.summary,
                source = task.source_path,
                content = task.source_content,
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
            match arcana_core::Note::parse(
                std::path::PathBuf::from(&task.output_path),
                &raw,
                arcana_core::FileMeta {
                    size_bytes: 0,
                    modified_on_disk: std::time::SystemTime::now(),
                    content_hash: 0,
                },
            ) {
                Ok(mut note) => {
                    note.frontmatter.ai = Some(arcana_core::AiMeta {
                        model: llm.model_name().to_string(),
                        provider: llm.provider_name().to_string(),
                        agent_session: session_id.to_string(),
                        task: format!("tidy: {}", task.summary),
                        sources: vec![task.source_path.clone()],
                        confidence: arcana_core::Confidence::Medium,
                        reviewed: false,
                        generated_at: chrono::Utc::now(),
                    });
                    note.to_string()
                }
                Err(_) => raw, // fallback to raw if parsing fails
            }
        };

        // Create draft — use move draft when there's a source to delete on approve
        if let Some(ref source) = task.move_source {
            drafts
                .create_move_draft(session_id, &task.output_path, &content, source)
                .map_err(AgentError::Vault)?;
        } else {
            drafts
                .create_draft(session_id, &task.output_path, &content)
                .map_err(AgentError::Vault)?;
        }

        drafted_paths.push(task.output_path.clone());

        send_event(
            event_tx,
            TidyEvent::GenerateDone {
                index: i,
                path: task.output_path.clone(),
            },
        );

        debug!(
            "generated draft {}/{}: {}",
            i + 1,
            tasks.len(),
            task.output_path
        );
    }

    Ok((drafted_paths, total_usage))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn send_event(tx: Option<&tokio::sync::mpsc::UnboundedSender<TidyEvent>>, event: TidyEvent) {
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
    use crate::types::{ContentBlock, LlmResponse, StopReason, Usage};

    #[test]
    fn parse_tidy_plan_move() {
        let json = r#"{
            "actions": [{
                "type": "move",
                "from": "inbox/test.md",
                "to": "concepts/test.md",
                "title": "Test Concept",
                "summary": "A test concept note",
                "rewrite": true
            }]
        }"#;
        let plan: TidyPlan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.output_count(), 1);
        assert_eq!(plan.output_paths(), vec!["concepts/test.md"]);
    }

    #[test]
    fn parse_tidy_plan_split() {
        let json = r#"{
            "actions": [{
                "type": "split",
                "from": "inbox/dump.md",
                "notes": [
                    {"path": "concepts/a.md", "title": "A", "summary": "First thing"},
                    {"path": "projects/b.md", "title": "B", "summary": "Second thing"}
                ]
            }]
        }"#;
        let plan: TidyPlan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.output_count(), 2);
    }

    #[test]
    fn parse_tidy_plan_extract() {
        let json = r#"{
            "actions": [{
                "type": "extract_concept",
                "from": "inbox/project-work.md",
                "concept": {
                    "path": "concepts/general.md",
                    "title": "General Idea",
                    "summary": "A general concept"
                }
            }]
        }"#;
        let plan: TidyPlan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.output_count(), 1);
        assert_eq!(plan.source_paths(), vec!["inbox/project-work.md"]);
    }

    #[test]
    fn extract_json_plain() {
        assert_eq!(extract_json(r#"{"a": 1}"#), r#"{"a": 1}"#);
    }

    #[test]
    fn extract_json_with_fences() {
        let input = "```json\n{\"a\": 1}\n```";
        assert_eq!(extract_json(input), r#"{"a": 1}"#);
    }

    #[test]
    fn extract_json_with_preamble() {
        let input = "Here is the plan:\n{\"actions\": []}";
        assert_eq!(extract_json(input), r#"{"actions": []}"#);
    }

    #[tokio::test]
    async fn tidy_survey_and_plan() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(
            inbox.join("dump.md"),
            "---\ntitle: Brain Dump\ntags: [unsorted]\n---\n\nI learned about MCMC today. Also starsim needs calibration.\n",
        )
        .unwrap();

        let config = arcana_core::ArcanaConfig::default().with_vault_path(dir.path().to_path_buf());
        let vault = arcana_core::Vault::open(config).unwrap();
        vault.index().unwrap();

        let plan_json = r#"{"actions":[{"type":"split","from":"inbox/dump.md","notes":[{"path":"concepts/mcmc.md","title":"MCMC","summary":"Markov chain Monte Carlo"},{"path":"projects/starsim/calibration.md","title":"Starsim Calibration","summary":"Calibration needs"}]}]}"#;

        let mock = MockBackend::new(vec![LlmResponse {
            content: vec![ContentBlock::Text {
                text: plan_json.into(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 500,
                output_tokens: 100,
            },
        }]);

        let profile = BrainProfile::default();

        let tidy = Tidy::new(
            Box::new(mock),
            Arc::new(Mutex::new(vault)),
            profile,
            None,
            None,
        )
        .survey(&["inbox/dump.md".into()])
        .await
        .unwrap();

        assert_eq!(tidy.sources().len(), 1);

        let tidy = tidy.plan().await.unwrap();
        assert_eq!(tidy.plan().actions.len(), 1);
        assert_eq!(tidy.plan().output_count(), 2);
        assert!(tidy.estimated_gen().total() > 0);
    }

    #[tokio::test]
    async fn tidy_full_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(
            inbox.join("note.md"),
            "---\ntitle: Quick Note\ntags: [unsorted]\n---\n\nInterior mutability in Rust.\n",
        )
        .unwrap();

        let config = arcana_core::ArcanaConfig::default().with_vault_path(dir.path().to_path_buf());
        let vault = arcana_core::Vault::open(config).unwrap();
        vault.index().unwrap();

        let plan_json = r#"{"actions":[{"type":"move","from":"inbox/note.md","to":"concepts/interior-mutability.md","title":"Interior Mutability","summary":"Rust's escape hatch from borrow rules","rewrite":true}]}"#;

        let generated_note = "---\ntitle: Interior Mutability\ntags: [concept, rust]\n---\n\n**Rust's escape hatch from borrow rules — runtime checking instead of compile-time.**\n\nInterior mutability lets you mutate data through a shared reference.\n";

        let mock = MockBackend::new(vec![
            // Plan response
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: plan_json.into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 500,
                    output_tokens: 100,
                },
            },
            // Generate response
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
        let tidy_config = TidyConfig::default();

        let result = run_tidy_auto(
            Box::new(mock),
            Arc::new(Mutex::new(vault)),
            vec!["inbox/note.md".into()],
            profile,
            None,
            &tidy_config,
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.plan().output_count(), 1);
        assert!(result.session_id().is_some());
        assert_eq!(result.drafted(), &["concepts/interior-mutability.md"]);
        assert_eq!(result.usage().input_tokens, 900);
        assert_eq!(result.usage().output_tokens, 300);
    }

    #[tokio::test]
    async fn tidy_events_sent() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(inbox.join("test.md"), "test content\n").unwrap();

        let config = arcana_core::ArcanaConfig::default().with_vault_path(dir.path().to_path_buf());
        let vault = arcana_core::Vault::open(config).unwrap();
        vault.index().unwrap();

        let plan_json = r#"{"actions":[{"type":"move","from":"inbox/test.md","to":"notes/test.md","title":"Test","summary":"A test","rewrite":false}]}"#;

        let mock = MockBackend::new(vec![
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: plan_json.into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "---\ntitle: Test\ntags: [note]\n---\n\nTest.\n".into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
        ]);

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

        let result = run_tidy_auto(
            Box::new(mock),
            Arc::new(Mutex::new(vault)),
            vec!["inbox/test.md".into()],
            BrainProfile::default(),
            None,
            &TidyConfig::default(),
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

        // Should have: SurveyStart, SurveyNote, PlanStart, PlanReady,
        //              GenerateStart, GenerateNote, GenerateDone, Done
        assert!(events
            .iter()
            .any(|e| matches!(e, TidyEvent::SurveyStart { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, TidyEvent::PlanReady { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, TidyEvent::GenerateStart { .. })));
        assert!(events.iter().any(|e| matches!(e, TidyEvent::Done { .. })));
    }

    #[test]
    fn upgrade_no_collision_stays_move() {
        let plan = TidyPlan {
            actions: vec![TidyAction::Move {
                from: "clasp/note.md".into(),
                to: "projects/clasp/note.md".into(),
                title: "Note".into(),
                summary: "move it".into(),
                rewrite: false,
            }],
        };
        let existing: std::collections::HashSet<&str> =
            ["projects/clasp/other.md"].into_iter().collect();
        let result = upgrade_moves_to_merges(plan, &existing);
        assert_eq!(result.actions.len(), 1);
        assert!(matches!(result.actions[0], TidyAction::Move { .. }));
    }

    #[test]
    fn upgrade_dest_exists_becomes_merge() {
        let plan = TidyPlan {
            actions: vec![TidyAction::Move {
                from: "clasp/note.md".into(),
                to: "projects/clasp/note.md".into(),
                title: "Note".into(),
                summary: "move it".into(),
                rewrite: false,
            }],
        };
        let existing: std::collections::HashSet<&str> =
            ["projects/clasp/note.md"].into_iter().collect();
        let result = upgrade_moves_to_merges(plan, &existing);
        assert_eq!(result.actions.len(), 1);
        match &result.actions[0] {
            TidyAction::Merge { sources, to, .. } => {
                assert_eq!(sources, &["clasp/note.md", "projects/clasp/note.md"]);
                assert_eq!(to, "projects/clasp/note.md");
            }
            other => panic!("expected Merge, got {other:?}"),
        }
    }

    #[test]
    fn upgrade_multiple_moves_same_dest_becomes_merge() {
        let plan = TidyPlan {
            actions: vec![
                TidyAction::Move {
                    from: "a/note.md".into(),
                    to: "notes/note.md".into(),
                    title: "Note".into(),
                    summary: "a".into(),
                    rewrite: false,
                },
                TidyAction::Move {
                    from: "b/note.md".into(),
                    to: "notes/note.md".into(),
                    title: "Note".into(),
                    summary: "b".into(),
                    rewrite: false,
                },
            ],
        };
        let existing: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let result = upgrade_moves_to_merges(plan, &existing);
        assert_eq!(result.actions.len(), 1);
        match &result.actions[0] {
            TidyAction::Merge { sources, .. } => {
                assert_eq!(sources.len(), 2);
            }
            other => panic!("expected Merge, got {other:?}"),
        }
    }

    #[test]
    fn parse_tidy_plan_merge() {
        let json = r#"{
            "actions": [{
                "type": "merge",
                "sources": ["a/note.md", "b/note.md"],
                "to": "notes/note.md",
                "title": "Note",
                "summary": "Merged"
            }]
        }"#;
        let plan: TidyPlan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.output_count(), 1);
        assert_eq!(plan.source_paths(), vec!["a/note.md", "b/note.md"]);
    }
}
