use std::io::Write;
use std::sync::Arc;

use anyhow::Result;
use arcana_agent::{CostEstimate, Tidy, TidyEvent, TidyPlan};
use arcana_core::{ArcanaConfig, SearchFilters, SearchResult};
use clap::Args;
use colored::Colorize;
use tokio::sync::Mutex;

#[derive(Args)]
pub struct TidyArgs {
    /// Path or glob to tidy (e.g. "inbox/", "inbox/dump.md")
    pub target: Option<String>,

    /// Run all phases without prompting
    #[arg(long)]
    pub auto: bool,

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

pub fn run_tidy(args: TidyArgs, config: ArcanaConfig, profile: Option<String>) -> Result<()> {
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

    // Resolve LLM config: base → op profile → --profile → --provider/--model
    let llm_config = config
        .resolve_llm(
            profile.as_deref(),
            config.agent.tidy.profile.as_deref(),
            args.provider.as_deref(),
            args.model.as_deref(),
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let backend = arcana_agent::create_backend(&llm_config).map_err(|e| {
        anyhow::anyhow!(
            "{e}\n\nhint: set ANTHROPIC_API_KEY, or use --provider ollama --model <name> for local inference"
        )
    })?;

    let brain_profile = vault.profile().clone();
    let model_name = backend.model_name().to_string();

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
            if brain_profile.is_empty() {
                ""
            } else {
                ", brain profile loaded"
            }
        )
        .dimmed()
    );
    eprintln!();

    let _tidy_config = arcana_agent::TidyConfig {
        max_tokens: config
            .agent
            .tidy
            .max_tokens
            .unwrap_or(config.agent.max_tokens) as u64,
    };

    let rt = tokio::runtime::Runtime::new()?;

    rt.block_on(async {
        let vault_arc = Arc::new(Mutex::new(vault));

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

        let event_handle = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                handle_event(&event);
            }
        });

        // --- Phase 1+2: Survey + Plan ---
        let tidy = Tidy::new(
            backend,
            vault_arc,
            brain_profile,
            None,
            Some(event_tx),
        );

        let tidy = match tidy.survey(&target_paths).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{}: {e}", "error".red().bold());
                std::process::exit(1);
            }
        };

        let mut tidy = match tidy.plan().await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{}: {e}", "error".red().bold());
                std::process::exit(1);
            }
        };

        // Print plan completion synchronously (don't rely on async event handler)
        eprintln!("{}", "done".green());

        if tidy.plan().actions.is_empty() {
            eprintln!("  {} nothing to tidy", "→".dimmed());
            return Ok(());
        }

        // Show plan and cost
        print_plan(tidy.plan());

        let cost_est = CostEstimate::new(
            &model_name,
            tidy.usage().clone(),
            tidy.estimated_gen().clone(),
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
                        match edit_plan(tidy.plan()) {
                            Ok(edited) => {
                                tidy = tidy.edit_plan(edited);
                                if tidy.plan().actions.is_empty() {
                                    eprintln!(
                                        "  {} plan is empty, nothing to generate",
                                        "→".dimmed()
                                    );
                                    return Ok(());
                                }
                                eprintln!();
                                print_plan(tidy.plan());
                                let cost_est = CostEstimate::new(
                                    &model_name,
                                    tidy.usage().clone(),
                                    tidy.estimated_gen().clone(),
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
        let result = tidy.generate().await;

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

const UP: &str = "\x1b[A";
const CLEAR: &str = "\x1b[2K";

fn handle_event(event: &TidyEvent) {
    match event {
        // -- Survey + Plan: single-line spinners --
        TidyEvent::SurveyStart { count } => {
            eprint!(
                "  {} surveying {} note{}... ",
                "→".dimmed(),
                count,
                if *count == 1 { "" } else { "s" }
            );
            std::io::stderr().flush().ok();
        }
        TidyEvent::SurveyNote { .. } => {}
        TidyEvent::PlanStart => {
            eprintln!("{}", "done".green());
            eprint!("  {} planning... ", "→".dimmed());
            std::io::stderr().flush().ok();
        }
        TidyEvent::PlanReady { .. } => {
            // "done" printed synchronously in main flow to avoid race
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

        // -- Generate: two-line progress block --
        TidyEvent::GenerateStart { .. } => {
            eprintln!();
            eprintln!();
            eprintln!();
        }
        TidyEvent::GenerateNote {
            index,
            total,
            ref path,
            tokens_used,
        } => {
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
        TidyEvent::GenerateDone { .. } => {}
        TidyEvent::Done {
            ref session_id,
            ref usage,
        } => {
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
        TidyEvent::Error { ref message } => {
            eprintln!("  {}: {message}", "warning".yellow().bold());
        }
    }
}

fn edit_plan(
    plan: &TidyPlan,
) -> Result<TidyPlan> {
    let toml_str = toml::to_string_pretty(plan)?;

    let header = "# Edit the tidy plan below.\n\
                  # Remove actions you don't want, adjust paths/titles/summaries.\n\
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

        let edited: TidyPlan = match toml::from_str(&toml_content) {
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
