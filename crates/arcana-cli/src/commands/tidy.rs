use std::sync::Arc;

use anyhow::Result;
use arcana_agent::{TidyConfig, TidyEvent, TidyPlan};
use arcana_core::{ArcanaConfig, SearchFilters, SearchResult};
use clap::Args;
use colored::Colorize;
use tokio::sync::Mutex;

#[derive(Args)]
pub struct TidyArgs {
    /// Path or glob to tidy (e.g. "inbox/", "inbox/dump.md")
    pub target: Option<String>,

    /// Only show the plan, don't generate drafts
    #[arg(long)]
    pub plan_only: bool,

    /// Filter by tag instead of path
    #[arg(long)]
    pub tags: Vec<String>,

    /// LLM provider override (anthropic, openai, ollama)
    #[arg(long)]
    pub provider: Option<String>,

    /// Model override
    #[arg(long)]
    pub model: Option<String>,

}

pub fn run_tidy(args: TidyArgs, config: ArcanaConfig) -> Result<()> {
    let vault = arcana_core::Vault::open(config.clone())?;
    crate::output::print_git_init_info(&vault);
    vault.index()?;

    // Resolve target paths
    let target_paths = resolve_targets(&vault, &args)?;
    if target_paths.is_empty() {
        eprintln!(
            "{}: no notes found matching the target",
            "error".red().bold()
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

    let profile = vault.profile().clone();

    eprintln!(
        "{} {} ({})",
        "arcana tidy".bold(),
        format!("v{}", env!("CARGO_PKG_VERSION")).dimmed(),
        format!("{}/{}", backend.provider_name(), backend.model_name()).cyan()
    );
    eprintln!(
        "{}",
        format!(
            "target: {} note{}{}",
            target_paths.len(),
            if target_paths.len() == 1 { "" } else { "s" },
            if profile.is_empty() {
                ""
            } else {
                ", brain profile loaded"
            }
        )
        .dimmed()
    );
    eprintln!();

    let tidy_config = TidyConfig {
        plan_only: args.plan_only,
        ..Default::default()
    };

    let rt = tokio::runtime::Runtime::new()?;

    rt.block_on(async {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

        // Spawn event handler
        let event_handle = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    TidyEvent::SurveyStart { count } => {
                        eprint!(
                            "  {} surveying {} note{}... ",
                            "→".dimmed(),
                            count,
                            if count == 1 { "" } else { "s" }
                        );
                    }
                    TidyEvent::SurveyNote { .. } => {}
                    TidyEvent::PlanStart => {
                        eprintln!("{}", "done".green());
                        eprint!("  {} generating plan... ", "→".dimmed());
                    }
                    TidyEvent::PlanReady { ref plan } => {
                        eprintln!("{}", "done".green());
                        eprintln!();
                        print_plan(plan);
                    }
                    TidyEvent::ConflictWarning {
                        ref path,
                        ref existing_session,
                        ref existing_model,
                    } => {
                        eprintln!(
                            "  {} {} already has a pending draft from session {} ({})",
                            "warning:".yellow().bold(),
                            path.cyan(),
                            existing_session.cyan(),
                            existing_model.dimmed()
                        );
                    }
                    TidyEvent::SameInputWarning {
                        ref existing_session,
                        ref existing_model,
                    } => {
                        eprintln!(
                            "  {} same input already has pending drafts in session {} ({})",
                            "warning:".yellow().bold(),
                            existing_session.cyan(),
                            existing_model.dimmed()
                        );
                    }
                    TidyEvent::GenerateStart { total } => {
                        eprintln!();
                        eprintln!(
                            "  {} generating {} note{}...",
                            "→".dimmed(),
                            total,
                            if total == 1 { "" } else { "s" }
                        );
                    }
                    TidyEvent::GenerateNote { index, ref path } => {
                        let label = format!("    [{}] {}... ", index + 1, path);
                        eprint!("{}", label.cyan());
                    }
                    TidyEvent::GenerateDone { .. } => {
                        eprintln!("{}", "done".green());
                    }
                    TidyEvent::Done {
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
                    TidyEvent::Error { ref message } => {
                        eprintln!("{}: {message}", "error".red().bold());
                    }
                }
            }
        });

        let vault = Arc::new(Mutex::new(vault));
        let result = arcana_agent::run_tidy(
            backend.as_ref(),
            vault,
            target_paths,
            &profile,
            None,
            &tidy_config,
            Some(&event_tx),
        )
        .await;

        drop(event_tx);
        event_handle.await.ok();

        match result {
            Ok(result) => {
                if result.plan.actions.is_empty() {
                    eprintln!("  {} nothing to tidy", "→".dimmed());
                } else if args.plan_only {
                    eprintln!();
                    eprintln!("  {} plan-only mode, no drafts created", "→".dimmed());
                    eprintln!(
                        "  {} tokens: {} in / {} out",
                        "✓".green().bold(),
                        result.usage.input_tokens,
                        result.usage.output_tokens
                    );
                } else if let Some(ref session_id) = result.session_id {
                    // Print full paths to draft files for easy access
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

fn resolve_targets(vault: &arcana_core::Vault, args: &TidyArgs) -> Result<Vec<String>> {
    // If tags are specified, search by tags
    if !args.tags.is_empty() {
        let filters = SearchFilters {
            tags: args.tags.clone(),
            path_prefix: args.target.clone(),
            ..Default::default()
        };
        let results: Vec<SearchResult> = vault.list(&filters, 200)?;
        return Ok(results.into_iter().map(|r| r.path).collect());
    }

    // Otherwise, resolve by path/name
    let target = args
        .target
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("specify a target path (e.g. 'inbox/') or use --tags"))?;

    // 1. Exact file path
    let full_path = vault.root().join(target);
    if full_path.is_file() {
        return Ok(vec![target.to_string()]);
    }

    // 2. Try with .md extension
    let with_md = format!("{target}.md");
    let full_with_md = vault.root().join(&with_md);
    if full_with_md.is_file() {
        return Ok(vec![with_md]);
    }

    // 3. Directory / path prefix
    if full_path.is_dir() || target.ends_with('/') {
        let prefix = if target.ends_with('/') {
            target.to_string()
        } else {
            format!("{target}/")
        };
        let filters = SearchFilters {
            path_prefix: Some(prefix),
            ..Default::default()
        };
        let results: Vec<SearchResult> = vault.list(&filters, 200)?;
        if !results.is_empty() {
            return Ok(results.into_iter().map(|r| r.path).collect());
        }
    }

    // 4. Search by filename — find notes whose path ends with the target
    let all = vault.list(&SearchFilters::default(), 5000)?;
    let target_lower = target.to_lowercase();
    let target_md = format!("{}.md", target_lower);
    let matches: Vec<String> = all
        .into_iter()
        .filter(|r| {
            let filename = r.path.rsplit('/').next().unwrap_or(&r.path).to_lowercase();
            filename == target_md || filename == target_lower
        })
        .map(|r| r.path)
        .collect();

    if !matches.is_empty() {
        return Ok(matches);
    }

    // 5. Fuzzy: path prefix match (user typed partial path like "inbox" without /)
    let filters = SearchFilters {
        path_prefix: Some(format!("{target}/")),
        ..Default::default()
    };
    let results: Vec<SearchResult> = vault.list(&filters, 200)?;
    Ok(results.into_iter().map(|r| r.path).collect())
}

fn print_plan(plan: &TidyPlan) {
    use arcana_agent::tidy::TidyAction;

    eprintln!(
        "  {} {} action{}, {} output note{}:",
        "plan:".bold(),
        plan.actions.len(),
        if plan.actions.len() == 1 { "" } else { "s" },
        plan.output_count(),
        if plan.output_count() == 1 { "" } else { "s" },
    );
    eprintln!();

    for action in &plan.actions {
        match action {
            TidyAction::Move {
                from,
                to,
                title,
                summary,
                ..
            } => {
                eprintln!("  {} {} {}", from.dimmed(), "→".dimmed(), to.cyan());
                eprintln!("    {} \"{}\"", title.bold(), summary.dimmed());
            }
            TidyAction::Split { from, notes } => {
                eprintln!(
                    "  {} {} {} into:",
                    from.dimmed(),
                    "→".dimmed(),
                    "SPLIT".yellow().bold()
                );
                for note in notes {
                    eprintln!("    {} \"{}\"", note.path.cyan(), note.summary.dimmed());
                }
            }
            TidyAction::ExtractConcept { from, concept } => {
                eprintln!(
                    "  {} {} {} {}",
                    from.dimmed(),
                    "→".dimmed(),
                    "EXTRACT".yellow().bold(),
                    concept.path.cyan()
                );
                eprintln!(
                    "    {} \"{}\"",
                    concept.title.bold(),
                    concept.summary.dimmed()
                );
            }
        }
    }
}
