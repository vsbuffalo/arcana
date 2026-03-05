use std::io::Write;
use std::sync::Arc;

use anyhow::Result;
use arcana_agent::{CostEstimate, Ingest, IngestEvent, IngestPlan};
use arcana_core::ArcanaConfig;
use clap::Args;
use colored::Colorize;
use tokio::sync::Mutex;

#[derive(Args)]
pub struct IngestArgs {
    /// Path to the project directory to ingest
    pub project: String,

    /// Run all phases without prompting
    #[arg(long)]
    pub auto: bool,

    /// Skill to use for exploration (e.g. "model-extract")
    #[arg(long)]
    pub skill: Option<String>,

    /// LLM provider override (anthropic, openai, ollama)
    #[arg(long)]
    pub provider: Option<String>,

    /// Model override
    #[arg(long)]
    pub model: Option<String>,
}

pub fn run_ingest(args: IngestArgs, config: ArcanaConfig, profile: Option<String>) -> Result<()> {
    let vault = arcana_core::Vault::open(config.clone())?;
    crate::output::print_git_init_info(&vault);
    vault.index()?;

    // Resolve project path
    let project_path = std::path::PathBuf::from(&args.project);
    if !project_path.is_dir() {
        eprintln!(
            "{}: not a directory: {}",
            "error".red().bold(),
            args.project
        );
        std::process::exit(1);
    }

    // Resolve LLM config: base → op profile → --profile → --provider/--model
    let llm_config = config
        .resolve_llm(
            profile.as_deref(),
            config.agent.ingest.profile.as_deref(),
            args.provider.as_deref(),
            args.model.as_deref(),
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let backend = arcana_agent::create_backend(&llm_config).map_err(|e| {
        anyhow::anyhow!(
            "{e}\n\nhint: set ANTHROPIC_API_KEY, or use --provider ollama --model <name> for local inference"
        )
    })?;

    // Resolve skill if specified
    let skill = if let Some(ref skill_name) = args.skill {
        Some(arcana_core::resolve_skill(skill_name, &config.vault.path)?)
    } else {
        None
    };
    let domain_skill = skill.as_ref().map(|s| s.body.clone());

    let brain_profile = vault.profile().clone();

    let project_name = project_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&args.project);

    let model_name = backend.model_name().to_string();

    eprintln!(
        "{} {} ({})",
        "arcana ingest".bold(),
        format!("v{}", env!("CARGO_PKG_VERSION")).dimmed(),
        format!("{}/{}", backend.provider_name(), backend.model_name()).cyan()
    );
    eprintln!(
        "{}",
        format!(
            "project: {project_name}{}{}",
            if brain_profile.is_empty() {
                ""
            } else {
                ", brain profile loaded"
            },
            if skill.is_some() {
                format!(", skill: {}", args.skill.as_deref().unwrap_or(""))
            } else {
                String::new()
            }
        )
        .dimmed()
    );
    eprintln!();

    let ingest_config = arcana_agent::IngestConfig {
        max_explore_iterations: config
            .agent
            .ingest
            .max_explore_iterations
            .or(config.agent.ingest.max_iterations)
            .unwrap_or(config.agent.max_iterations),
        max_tokens: config
            .agent
            .ingest
            .max_tokens
            .unwrap_or(config.agent.max_tokens) as u64,
    };

    let rt = tokio::runtime::Runtime::new()?;

    rt.block_on(async {
        let vault_arc = Arc::new(Mutex::new(vault));

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let budget_exhausted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let budget_exhausted_flag = budget_exhausted.clone();

        let event_handle = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                if matches!(event, IngestEvent::ExploreBudgetExhausted { .. }) {
                    budget_exhausted_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                handle_event(&event);
            }
        });

        // --- Phase 1+2: Explore + Plan ---
        let ingest = Ingest::new(
            backend,
            &project_path,
            vault_arc,
            brain_profile,
            domain_skill,
            Some(event_tx),
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;

        let ingest = match ingest.explore(&ingest_config).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{}: {e}", "error".red().bold());
                std::process::exit(1);
            }
        };

        let mut ingest = match ingest.plan().await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{}: {e}", "error".red().bold());
                std::process::exit(1);
            }
        };

        // Print plan completion synchronously (don't rely on async event handler)
        eprintln!("{}", "done".green());

        if ingest.plan().notes.is_empty() {
            if budget_exhausted.load(std::sync::atomic::Ordering::Relaxed) {
                eprintln!(
                    "  {} nothing to ingest (explore hit token budget — try increasing agent.max_tokens or agent.ingest.max_tokens)",
                    "→".yellow()
                );
            } else {
                eprintln!("  {} nothing to ingest", "→".dimmed());
            }
            return Ok(());
        }

        // Show plan and cost
        print_plan(ingest.plan());

        let cost_est = CostEstimate::new(
            &model_name,
            ingest.usage().clone(),
            ingest.estimated_gen().clone(),
        );
        print_cost_estimate(&cost_est);
        eprintln!();

        // --- Interactive prompt ---
        if !args.auto {
            loop {
                eprint!(
                    "  {} ",
                    "[g]enerate / [e]dit plan / [q]uit:".bold()
                );
                std::io::stderr().flush().ok();

                let mut input = String::new();
                if std::io::stdin().read_line(&mut input).is_err() {
                    break;
                }
                let choice = input.trim().to_lowercase();

                match choice.as_str() {
                    "g" | "generate" => break,
                    "q" | "quit" => {
                        eprintln!("  {} aborted", "→".dimmed());
                        return Ok(());
                    }
                    "e" | "edit" => {
                        match edit_plan(ingest.plan()) {
                            Ok(edited) => {
                                ingest = ingest.edit_plan(edited);
                                if ingest.plan().notes.is_empty() {
                                    eprintln!(
                                        "  {} plan is empty, nothing to generate",
                                        "→".dimmed()
                                    );
                                    return Ok(());
                                }
                                eprintln!();
                                print_plan(ingest.plan());
                                let cost_est = CostEstimate::new(
                                    &model_name,
                                    ingest.usage().clone(),
                                    ingest.estimated_gen().clone(),
                                );
                                print_cost_estimate(&cost_est);
                                eprintln!();
                            }
                            Err(e) => {
                                eprintln!("  {}: {e}", "error".red().bold());
                            }
                        }
                    }
                    _ => {
                        eprintln!("  {} unrecognized choice", "→".dimmed());
                    }
                }
            }
        }

        // --- Phase 3: Generate ---
        let result = ingest.generate().await;

        match result {
            Ok(done) => {
                let session_id = done.session_id().unwrap_or("unknown");
                let drafts_dir = config
                    .vault
                    .path
                    .join(".arcana")
                    .join("drafts")
                    .join(session_id);
                eprintln!("  {}:", "drafts".dimmed());
                for path in done.drafted() {
                    eprintln!("    {}", drafts_dir.join(path).display());
                }

                // Drop done to close the event channel
                drop(done);
                event_handle.await.ok();

                Ok(())
            }
            Err(e) => {
                eprintln!("{}: {e}", "error".red().bold());
                std::process::exit(1);
            }
        }
    })
}

const CLEAR: &str = "\x1b[2K";
const UP: &str = "\x1b[A";

fn handle_event(event: &IngestEvent) {
    match event {
        // -- Explore: single-line progress, rewrites in place --
        IngestEvent::ExploreStart { .. } => {
            eprint!("  {} exploring...", "→".dimmed());
            std::io::stderr().flush().ok();
        }
        IngestEvent::ExploreIteration {
            iteration,
            max_iterations,
            tool_count,
            tokens_used,
        } => {
            eprint!(
                "{CLEAR}\r  {} exploring [{}/{}]  {} tools, {}K tokens",
                "→".dimmed(),
                iteration + 1,
                max_iterations,
                tool_count,
                tokens_used / 1000,
            );
            std::io::stderr().flush().ok();
        }
        IngestEvent::ExploreToolCall {
            ref description, ..
        } => {
            // Append tool description after the status on same line
            eprint!("  {}", description.dimmed());
            std::io::stderr().flush().ok();
        }
        IngestEvent::ExploreBudgetExhausted { used, budget } => {
            eprintln!(
                "{CLEAR}\r  {} explore token budget exhausted ({}K/{}K)",
                "!".yellow().bold(),
                used / 1000,
                budget / 1000,
            );
        }
        IngestEvent::ExploreDone {
            iterations,
            tokens_used,
            ..
        } => {
            eprintln!(
                "{CLEAR}\r  {} explored {iterations} iterations, {}K tokens  {}",
                "→".dimmed(),
                tokens_used / 1000,
                "done".green()
            );
        }

        // -- Plan: single-line spinner --
        IngestEvent::PlanStart => {
            eprint!("  {} planning... ", "→".dimmed());
            std::io::stderr().flush().ok();
        }
        IngestEvent::PlanReady { .. } => {
            // "done" printed synchronously in main flow to avoid race
        }

        // -- Generate: two-line progress block --
        IngestEvent::GenerateStart { .. } => {
            // Set up two-line block (blank status + blank detail)
            // Extra blank line for spacing after plan output
            eprintln!();
            eprintln!();
            eprintln!();
        }
        IngestEvent::GenerateNote {
            index,
            total,
            ref path,
            tokens_used,
        } => {
            // Rewrite both lines
            let status = format!(
                "  {} generating [{}/{}]  {}K tokens",
                "→".dimmed(),
                index + 1,
                total,
                tokens_used / 1000,
            );
            eprint!("{UP}{UP}{CLEAR}\r{status}\n{CLEAR}\r    writing {}\n", path.cyan());
            std::io::stderr().flush().ok();
        }
        IngestEvent::GenerateDone { .. } => {}
        IngestEvent::Done {
            ref session_id,
            ref usage,
        } => {
            // Collapse generate block into one final line
            let total_tokens = usage.input_tokens + usage.output_tokens;
            eprint!(
                "{UP}{UP}{CLEAR}\r  {} generated, {}K tokens  {}\n{CLEAR}\r",
                "→".dimmed(),
                total_tokens / 1000,
                "done".green()
            );
            std::io::stderr().flush().ok();
            eprintln!();
            eprintln!("  {} session: {}", "✓".green().bold(), session_id.cyan());
            eprintln!(
                "  {} tokens: {} in / {} out",
                "✓".green().bold(),
                usage.input_tokens,
                usage.output_tokens
            );
            eprintln!();
            eprintln!(
                "  {}",
                "run 'arcana review' to approve or reject drafts".dimmed()
            );
        }
        IngestEvent::Error { ref message } => {
            eprintln!("  {}: {message}", "warning".yellow().bold());
        }
    }
}

fn edit_plan(plan: &IngestPlan) -> Result<IngestPlan> {
    let toml_str = toml::to_string_pretty(plan)?;

    let header = "# Edit the ingest plan below.\n\
                  # Remove notes you don't want, adjust paths/titles/summaries.\n\
                  # Save and close the editor to continue.\n\n";

    let mut tmp = tempfile::Builder::new()
        .suffix(".toml")
        .tempfile()?;
    tmp.write_all(header.as_bytes())?;
    tmp.write_all(toml_str.as_bytes())?;
    tmp.flush()?;

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".into());
    let tmp_path = tmp.path().to_path_buf();

    loop {
        let status = std::process::Command::new(&editor)
            .arg(&tmp_path)
            .status()?;

        if !status.success() {
            anyhow::bail!("editor exited with non-zero status");
        }

        let content = std::fs::read_to_string(&tmp_path)?;

        // Strip comment header lines
        let toml_content: String = content
            .lines()
            .filter(|line| !line.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");

        let edited: IngestPlan = match toml::from_str(&toml_content) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  {}: {e}", "parse error".red().bold());
                eprintln!("  {} reopening editor...", "→".dimmed());
                continue;
            }
        };

        return Ok(edited);
    }
}

fn print_cost_estimate(est: &CostEstimate) {
    eprintln!();
    eprintln!(
        "  {} ({}, pricing as of {}):",
        "cost".bold(),
        est.model.cyan(),
        arcana_agent::pricing::LAST_UPDATED.dimmed()
    );
    eprintln!(
        "    spent so far:        {}K in / {}K out    {}",
        est.spent.input_tokens / 1000,
        est.spent.output_tokens / 1000,
        est.spent_cost
            .map(|c| format!("→  ${:.2}", c))
            .unwrap_or_default()
            .dimmed()
    );
    if est.estimated_remaining.total() > 0 {
        eprintln!(
            "    estimated remaining: {}K in / {}K out    {}",
            est.estimated_remaining.input_tokens / 1000,
            est.estimated_remaining.output_tokens / 1000,
            est.remaining_cost
                .map(|c| format!("→  ${:.2}", c))
                .unwrap_or_default()
                .dimmed()
        );
        if let Some(total) = est.total_cost {
            eprintln!(
                "    {}                         {}",
                "estimated total:".bold(),
                format!("→  ~${:.2}", total).dimmed()
            );
        }
    }
}

fn print_plan(plan: &IngestPlan) {
    eprintln!(
        "  {} {} note{}:",
        "plan:".bold(),
        plan.notes.len(),
        if plan.notes.len() == 1 { "" } else { "s" },
    );
    eprintln!();

    for note in &plan.notes {
        eprintln!("  {} {}", "→".dimmed(), note.path.cyan());
        eprintln!(
            "    {} \"{}\"",
            note.title.bold(),
            note.summary.dimmed()
        );
        if !note.source_files.is_empty() {
            eprintln!(
                "    {} {}",
                "sources:".dimmed(),
                note.source_files.join(", ").dimmed()
            );
        }
    }
}
