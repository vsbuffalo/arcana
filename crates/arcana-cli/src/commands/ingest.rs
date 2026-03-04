use std::sync::Arc;

use anyhow::Result;
use arcana_agent::{CostEstimate, IngestConfig, IngestEvent, IngestPlan};
use arcana_core::ArcanaConfig;
use clap::Args;
use colored::Colorize;
use tokio::sync::Mutex;

#[derive(Args)]
pub struct IngestArgs {
    /// Path to the project directory to ingest
    pub project: String,

    /// Only show the plan, don't generate drafts
    #[arg(long)]
    pub plan_only: bool,

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

pub fn run_ingest(args: IngestArgs, config: ArcanaConfig) -> Result<()> {
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

    // Build LLM config with CLI overrides
    let mut llm_config = config.llm.clone();
    if let Some(provider) = &args.provider {
        llm_config.provider = provider.clone();
        match provider.as_str() {
            "anthropic" => llm_config.api_key_env = "ANTHROPIC_API_KEY".into(),
            "openai" => llm_config.api_key_env = "OPENAI_API_KEY".into(),
            _ => {}
        }
    }
    if let Some(model) = &args.model {
        llm_config.model = model.clone();
    }

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
    let domain_skill = skill.as_ref().map(|s| s.body.as_str());

    let profile = vault.profile().clone();

    let project_name = project_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&args.project);

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
            if profile.is_empty() {
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

    let ingest_config = IngestConfig {
        plan_only: args.plan_only,
        max_explore_iterations: config
            .agent
            .ingest
            .max_iterations
            .unwrap_or(config.agent.max_iterations),
        max_tokens: config
            .agent
            .ingest
            .max_tokens
            .unwrap_or(config.agent.max_tokens) as u64,
    };

    let rt = tokio::runtime::Runtime::new()?;

    rt.block_on(async {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

        let event_handle = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    IngestEvent::ExploreStart => {
                        eprint!("  {} exploring project... ", "→".dimmed());
                    }
                    IngestEvent::ExploreIteration { iteration } => {
                        if iteration > 0 && iteration % 5 == 0 {
                            eprint!("{}", format!("[{iteration}] ").dimmed());
                        }
                    }
                    IngestEvent::ExploreToolCall { ref name } => {
                        eprint!("{}", format!("{name} ").dimmed());
                    }
                    IngestEvent::ExploreDone { iterations } => {
                        eprintln!(
                            "{}",
                            format!("done ({iterations} iteration{})", if iterations == 1 { "" } else { "s" }).green()
                        );
                    }
                    IngestEvent::PlanStart => {
                        eprint!("  {} generating plan... ", "→".dimmed());
                    }
                    IngestEvent::PlanReady { ref plan } => {
                        eprintln!("{}", "done".green());
                        eprintln!();
                        print_plan(plan);
                    }
                    IngestEvent::GenerateStart { total } => {
                        eprintln!();
                        eprintln!(
                            "  {} generating {} note{}...",
                            "→".dimmed(),
                            total,
                            if total == 1 { "" } else { "s" }
                        );
                    }
                    IngestEvent::GenerateNote { index, ref path } => {
                        let label = format!("    [{}] {}... ", index + 1, path);
                        eprint!("{}", label.cyan());
                    }
                    IngestEvent::GenerateDone { .. } => {
                        eprintln!("{}", "done".green());
                    }
                    IngestEvent::Done {
                        ref session_id,
                        ref usage,
                    } => {
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
        });

        let vault = Arc::new(Mutex::new(vault));
        let result = arcana_agent::run_ingest(
            backend.as_ref(),
            &project_path,
            vault,
            &profile,
            domain_skill,
            &ingest_config,
            Some(&event_tx),
        )
        .await;

        drop(event_tx);
        event_handle.await.ok();

        match result {
            Ok(result) => {
                if result.plan.notes.is_empty() {
                    eprintln!("  {} nothing to ingest", "→".dimmed());
                } else if args.plan_only {
                    eprintln!();
                    eprintln!("  {} plan-only mode, no drafts created", "→".dimmed());
                    eprintln!(
                        "  {} tokens: {} in / {} out",
                        "✓".green().bold(),
                        result.usage.input_tokens,
                        result.usage.output_tokens
                    );
                    if let Some(ref est) = result.cost_estimate {
                        print_cost_estimate(est);
                    }
                } else if let Some(ref session_id) = result.session_id {
                    let drafts_dir = config
                        .vault
                        .path
                        .join(".arcana")
                        .join("drafts")
                        .join(session_id);
                    eprintln!("  {}:", "drafts".dimmed());
                    for path in &result.drafted_paths {
                        eprintln!("    {}", drafts_dir.join(path).display());
                    }
                }
                Ok(())
            }
            Err(e) => {
                eprintln!("{}: {e}", "error".red().bold());
                std::process::exit(1);
            }
        }
    })
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
