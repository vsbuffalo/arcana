use std::io::Write;
use std::sync::Arc;

use anyhow::Result;
use arcana_agent::{CostEstimate, TidyEngine, TidyEvent, TidyPlan};
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

    let tidy_config = arcana_agent::TidyConfig {
        max_tokens: config
            .agent
            .tidy
            .max_tokens
            .unwrap_or(config.agent.max_tokens) as u64,
    };

    let rt = tokio::runtime::Runtime::new()?;

    rt.block_on(async {
        let vault_arc = Arc::new(Mutex::new(vault));
        let model_name = backend.model_name().to_string();

        // --- Phase 1+2: Survey + Plan ---
        let (plan_result, plan_usage, estimated_gen, sources, vault_context) = {
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

            let event_handle = tokio::spawn(async move {
                while let Some(event) = event_rx.recv().await {
                    handle_plan_event(&event);
                }
            });

            let engine = TidyEngine::new(
                backend.as_ref(),
                vault_arc.clone(),
                &profile,
                None,
                Some(&event_tx),
            );

            let surveyed = match engine.survey(&target_paths).await {
                Ok(r) => r,
                Err(e) => {
                    drop(event_tx);
                    event_handle.await.ok();
                    eprintln!("{}: {e}", "error".red().bold());
                    std::process::exit(1);
                }
            };

            let planned = match engine.plan(&surveyed.sources, &surveyed.vault_context).await {
                Ok(r) => r,
                Err(e) => {
                    drop(event_tx);
                    event_handle.await.ok();
                    eprintln!("{}: {e}", "error".red().bold());
                    std::process::exit(1);
                }
            };

            drop(event_tx);
            event_handle.await.ok();

            (
                planned.plan,
                planned.usage,
                planned.estimated_gen,
                surveyed.sources,
                surveyed.vault_context,
            )
        };

        // Suppress unused warning
        let _ = &tidy_config;

        if plan_result.actions.is_empty() {
            eprintln!("  {} nothing to tidy", "→".dimmed());
            return Ok(());
        }

        // Show plan and cost
        print_plan(&plan_result);

        let cost_est = CostEstimate::new(&model_name, plan_usage.clone(), estimated_gen.clone());
        print_cost_estimate(&cost_est);
        eprintln!();

        // --- Interactive prompt ---
        let mut plan = plan_result;

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
                        match edit_plan(&plan, &profile) {
                            Ok(edited) => {
                                plan = edited;
                                if plan.actions.is_empty() {
                                    eprintln!(
                                        "  {} plan is empty, nothing to generate",
                                        "→".dimmed()
                                    );
                                    return Ok(());
                                }
                                eprintln!();
                                print_plan(&plan);
                                let est = arcana_agent::tidy::estimate_generation_usage(
                                    &plan, &sources,
                                );
                                let cost_est =
                                    CostEstimate::new(&model_name, plan_usage.clone(), est);
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
        let (gen_event_tx, mut gen_event_rx) = tokio::sync::mpsc::unbounded_channel();

        let gen_handle = tokio::spawn(async move {
            while let Some(event) = gen_event_rx.recv().await {
                handle_generate_event(&event);
            }
        });

        let engine = TidyEngine::new(
            backend.as_ref(),
            vault_arc,
            &profile,
            None,
            Some(&gen_event_tx),
        );

        let result = engine
            .generate(&plan, &sources, &vault_context, plan_usage)
            .await;

        drop(gen_event_tx);
        gen_handle.await.ok();

        match result {
            Ok(generated) => {
                let drafts_dir = config
                    .vault
                    .path
                    .join(".arcana")
                    .join("drafts")
                    .join(&generated.session_id);
                eprintln!("  {}:", "drafts".dimmed());
                for path in &generated.drafted_paths {
                    eprintln!("    {}", drafts_dir.join(path).display());
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

fn handle_plan_event(event: &TidyEvent) {
    match event {
        TidyEvent::SurveyStart { count } => {
            eprint!(
                "  {} surveying {} note{}... ",
                "→".dimmed(),
                count,
                if *count == 1 { "" } else { "s" }
            );
        }
        TidyEvent::SurveyNote { .. } => {}
        TidyEvent::PlanStart => {
            eprintln!("{}", "done".green());
            eprint!("  {} generating plan... ", "→".dimmed());
        }
        TidyEvent::PlanReady { .. } => {
            eprintln!("{}", "done".green());
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
        TidyEvent::Error { ref message } => {
            eprintln!("{}: {message}", "error".red().bold());
        }
        _ => {}
    }
}

fn handle_generate_event(event: &TidyEvent) {
    match event {
        TidyEvent::GenerateStart { total } => {
            eprintln!();
            eprintln!(
                "  {} generating {} note{}...",
                "→".dimmed(),
                total,
                if *total == 1 { "" } else { "s" }
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
        _ => {}
    }
}

fn edit_plan(
    plan: &TidyPlan,
    profile: &arcana_core::BrainProfile,
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

        // Validate zones
        let zones = profile.zones();
        let projects = profile.projects();
        if !zones.is_empty() {
            let output_paths = edited.output_paths();
            let mut zone_errors = Vec::new();
            for path in &output_paths {
                if let Err(e) =
                    arcana_core::writer::validate_zone(path, &zones, &projects)
                {
                    zone_errors.push(format!("  {path}: {e}"));
                }
            }
            if !zone_errors.is_empty() {
                eprintln!("  {}: invalid zones", "error".red().bold());
                for err in &zone_errors {
                    eprintln!("    {err}");
                }
                eprintln!(
                    "  {} allowed zones: {}",
                    "hint:".dimmed(),
                    zones.join(", ").dimmed()
                );
                eprintln!("  {} reopening editor...", "→".dimmed());
                continue;
            }
        }

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
