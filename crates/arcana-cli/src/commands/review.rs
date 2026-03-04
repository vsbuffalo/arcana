use anyhow::Result;
use arcana_core::ArcanaConfig;
use clap::Args;
use colored::Colorize;

use crate::output::is_stderr_tty;

#[derive(Args)]
pub struct ReviewArgs {
    /// List all sessions with pending drafts
    #[arg(long)]
    pub list: bool,

    /// Approve all pending drafts in a session
    #[arg(long)]
    pub approve_all: bool,

    /// Session ID to operate on
    #[arg(long)]
    pub session: Option<String>,

    /// Prune old resolved sessions
    #[arg(long)]
    pub prune: bool,
}

pub fn run_review(args: ReviewArgs, config: ArcanaConfig, json: bool) -> Result<()> {
    let vault = arcana_core::Vault::open(config.clone())?;
    if !json {
        crate::output::print_git_init_info(&vault);
    }
    vault.index()?;

    let drafts = vault.drafts();

    if args.prune {
        let pruned = drafts.prune(config.drafts.retention_days)?;
        if json {
            println!("{}", serde_json::json!({"pruned": pruned}));
        } else {
            eprintln!("pruned {pruned} old sessions");
        }
        return Ok(());
    }

    if args.list || (!args.approve_all && args.session.is_none()) {
        let sessions = drafts.list_sessions()?;

        if json {
            println!("{}", serde_json::to_string_pretty(&sessions)?);
            return Ok(());
        }

        if sessions.is_empty() {
            eprintln!("{}", "no draft sessions found".dimmed());
            return Ok(());
        }

        for session in &sessions {
            let status = if session.pending_drafts > 0 {
                format!("{} pending", session.pending_drafts)
                    .yellow()
                    .to_string()
            } else {
                "all resolved".green().to_string()
            };

            eprintln!(
                "{} {} ({}) - {} ({}/{})",
                session.id.bold(),
                session.task.dimmed(),
                session.created_at.format("%Y-%m-%d %H:%M"),
                status,
                session.provider,
                session.model,
            );
        }
        return Ok(());
    }

    let session_id = args.session.as_ref().ok_or_else(|| {
        anyhow::anyhow!("--session required when approving. Use --list to see sessions.")
    })?;

    if args.approve_all {
        let draft_list = drafts.list_drafts(session_id)?;
        let mut approved = 0;
        for draft in &draft_list {
            if draft.status == arcana_core::DraftStatus::Pending {
                let target = drafts.approve(session_id, &draft.path)?;
                vault.reindex_paths(std::slice::from_ref(&target))?;

                if let Some(git) = vault.git() {
                    let msg = format!("arcana: approve draft {}", draft.path);
                    git.commit_ai_write(&[std::path::Path::new(&draft.path)], &msg)
                        .ok();
                }

                if is_stderr_tty() {
                    eprintln!("{} {}", "approved:".green(), draft.path);
                }
                approved += 1;
            }
        }

        if json {
            println!("{}", serde_json::json!({"approved": approved}));
        } else {
            eprintln!("{}", format!("approved {approved} drafts").green().bold());
        }
        return Ok(());
    }

    // Interactive review
    let draft_list = drafts.list_drafts(session_id)?;
    let pending: Vec<_> = draft_list
        .iter()
        .filter(|d| d.status == arcana_core::DraftStatus::Pending)
        .collect();

    if pending.is_empty() {
        eprintln!("{}", "no pending drafts in this session".dimmed());
        return Ok(());
    }

    eprintln!(
        "{} pending drafts in session {}",
        pending.len().to_string().bold(),
        session_id.cyan()
    );
    eprintln!();

    for draft in &pending {
        let content = drafts.read_draft(session_id, &draft.path)?;

        eprintln!("{}", "─".repeat(60).dimmed());
        eprintln!("{} {}", "draft:".bold(), draft.path.cyan());
        if let Some(ref reason) = draft.reason {
            eprintln!("{} {}", "reason:".bold(), reason);
        }
        eprintln!("{}", "─".repeat(60).dimmed());

        // Show content preview (first 30 lines)
        let preview: String = content.lines().take(30).collect::<Vec<_>>().join("\n");
        eprintln!("{preview}");
        if content.lines().count() > 30 {
            eprintln!("{}", "... (truncated)".dimmed());
        }

        eprintln!();
        eprint!(
            "{}",
            "[a]pprove / [d]elete / [s]kip / [A]pprove all: ".bold()
        );

        let mut response = String::new();
        std::io::stdin().read_line(&mut response)?;
        let response = response.trim();

        match response {
            "a" | "approve" => {
                let target = drafts.approve(session_id, &draft.path)?;
                vault.reindex_paths(std::slice::from_ref(&target))?;

                if let Some(git) = vault.git() {
                    let msg = format!("arcana: approve draft {}", draft.path);
                    git.commit_ai_write(&[std::path::Path::new(&draft.path)], &msg)
                        .ok();
                }

                eprintln!("{}", "approved".green());
            }
            "d" | "delete" => {
                drafts.reject(session_id, &draft.path)?;
                eprintln!("{}", "deleted".red());
            }
            "A" => {
                // Approve this one and all remaining
                let target = drafts.approve(session_id, &draft.path)?;
                vault.reindex_paths(std::slice::from_ref(&target))?;

                if let Some(git) = vault.git() {
                    let msg = format!("arcana: approve draft {}", draft.path);
                    git.commit_ai_write(&[std::path::Path::new(&draft.path)], &msg)
                        .ok();
                }

                eprintln!("{}", "approved".green());

                // Approve remaining
                for remaining in pending.iter().skip_while(|d| d.path != draft.path).skip(1) {
                    let target = drafts.approve(session_id, &remaining.path)?;
                    vault.reindex_paths(&[target])?;

                    if let Some(git) = vault.git() {
                        let msg = format!("arcana: approve draft {}", remaining.path);
                        git.commit_ai_write(&[std::path::Path::new(&remaining.path)], &msg)
                            .ok();
                    }

                    eprintln!("{} {}", "approved:".green(), remaining.path);
                }
                break;
            }
            "s" | "skip" | "" => {
                eprintln!("{}", "skipped".dimmed());
            }
            _ => {
                eprintln!("{}", "skipped (unknown input)".dimmed());
            }
        }
        eprintln!();
    }

    Ok(())
}
