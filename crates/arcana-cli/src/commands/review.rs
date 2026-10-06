use std::io::Write;

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

    /// Run LLM style verification on each draft before review
    #[arg(long)]
    pub verify_style: bool,

    /// Prune old resolved sessions
    #[arg(long)]
    pub prune: bool,
}

pub fn run_review(
    args: ReviewArgs,
    config: ArcanaConfig,
    json: bool,
    profile: Option<String>,
) -> Result<()> {
    let vault = arcana_core::Vault::open(config.clone())?;
    if vault.ledger().is_some() {
        return crate::commands::review_tui::run(vault);
    }
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
            // Filter by --session if provided
            if let Some(ref filter_id) = args.session {
                if !session.id.starts_with(filter_id.as_str()) {
                    continue;
                }
            }

            let status = if session.total_drafts == 0 {
                "empty".dimmed().to_string()
            } else if session.pending_drafts > 0 {
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
                chrono::DateTime::<chrono::Local>::from(session.created_at)
                    .format("%Y-%m-%d %H:%M"),
                status,
                session.provider,
                session.model,
            );

            // Show draft paths under each session
            let draft_list = drafts.list_drafts(&session.id)?;
            for draft in &draft_list {
                let status_icon = match draft.status {
                    arcana_core::DraftStatus::Pending => "○".yellow().to_string(),
                    arcana_core::DraftStatus::Approved => "✓".green().to_string(),
                    arcana_core::DraftStatus::Rejected => "✗".red().to_string(),
                    arcana_core::DraftStatus::Edited => "~".cyan().to_string(),
                };
                eprintln!("  {} {}", status_icon, draft.path.dimmed());
            }
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
        eprintln!(
            "{} in session {}",
            "no pending drafts".dimmed(),
            session_id.cyan()
        );
        if !draft_list.is_empty() {
            for draft in &draft_list {
                let (icon, label) = match draft.status {
                    arcana_core::DraftStatus::Approved => ("✓".green().to_string(), "approved"),
                    arcana_core::DraftStatus::Rejected => ("✗".red().to_string(), "rejected"),
                    arcana_core::DraftStatus::Edited => ("~".cyan().to_string(), "edited"),
                    arcana_core::DraftStatus::Pending => ("○".yellow().to_string(), "pending"),
                };
                eprintln!("  {} {} {}", icon, draft.path, label.dimmed());
            }
        }
        return Ok(());
    }

    // Set up LLM for --verify-style
    let style_verifier = if args.verify_style {
        let brain_profile = vault.profile().clone();
        let style_guide = brain_profile.style().unwrap_or("").to_string();
        if style_guide.is_empty() {
            eprintln!(
                "{}: --verify-style requires a style guide in .arcana/style.md",
                "warning".yellow().bold()
            );
            None
        } else {
            let llm_config = config
                .resolve_llm(profile.as_deref(), None, None, None)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            match arcana_agent::create_backend(&llm_config) {
                Ok(backend) => {
                    eprintln!(
                        "{} ({}/{})",
                        "style verification enabled".dimmed(),
                        backend.provider_name(),
                        backend.model_name()
                    );
                    eprintln!();
                    Some((backend, style_guide))
                }
                Err(e) => {
                    eprintln!(
                        "{}: could not create LLM backend for style verification: {e}",
                        "warning".yellow().bold()
                    );
                    None
                }
            }
        }
    } else {
        None
    };

    eprintln!(
        "{} pending drafts in session {}",
        pending.len().to_string().bold(),
        session_id.cyan()
    );
    eprintln!();

    let rt = tokio::runtime::Runtime::new()?;

    for (i, draft) in pending.iter().enumerate() {
        let content = drafts.read_draft(session_id, &draft.path)?;
        let line_count = content.lines().count();

        eprintln!(
            "  {} {}  {}",
            format!("[{}/{}]", i + 1, pending.len()).dimmed(),
            draft.path.cyan(),
            format!("{line_count} lines").dimmed(),
        );
        if let Some(ref reason) = draft.reason {
            eprintln!("  {} {}", "reason:".dimmed(), reason);
        }

        // Show first few lines as a teaser
        let teaser: String = content
            .lines()
            .take(5)
            .map(|l| format!("  {}", l.dimmed()))
            .collect::<Vec<_>>()
            .join("\n");
        eprintln!("{teaser}");
        if line_count > 5 {
            eprintln!("  {}", "...".dimmed());
        }

        // Heuristic checks
        let warnings = heuristic_checks(&draft.path, &content);
        if !warnings.is_empty() {
            eprintln!();
            for w in &warnings {
                eprintln!("  {} {}", "⚠".yellow(), w.yellow());
            }
        }

        // LLM style verification
        if let Some((ref backend, ref style_guide)) = style_verifier {
            eprint!("  {} ", "verifying style...".dimmed());
            std::io::stderr().flush().ok();
            match rt.block_on(verify_style(backend.as_ref(), style_guide, &content)) {
                Ok(feedback) if feedback.is_empty() => {
                    eprintln!("{}", "✓ passes style guide".green());
                }
                Ok(feedback) => {
                    eprintln!();
                    for line in feedback.lines() {
                        if !line.trim().is_empty() {
                            eprintln!("  {} {}", "⚠".yellow(), line.yellow());
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{}: {e}", "error".red());
                }
            }
        }

        let mut done = false;
        loop {
            eprintln!();
            eprint!(
                "  {} ",
                "[a]pprove / [v]iew / [e]dit / [r]evise / [d]elete / [s]kip / [A]ll:".bold()
            );
            std::io::stderr().flush().ok();

            let choice = read_choice()?;

            match choice.as_str() {
                "v" | "V" | "view" => {
                    view_in_pager(&content)?;
                    eprintln!();
                    eprintln!("  {}", draft.path.cyan());
                    continue;
                }
                "e" | "E" | "edit" => {
                    let draft_path = config
                        .vault
                        .path
                        .join(".arcana")
                        .join("drafts")
                        .join(session_id)
                        .join(&draft.path);
                    let edited = edit_in_editor(&draft_path)?;
                    if edited {
                        let target = drafts.approve(session_id, &draft.path)?;
                        vault.reindex_paths(std::slice::from_ref(&target))?;

                        if let Some(git) = vault.git() {
                            let msg = format!("arcana: approve draft (edited) {}", draft.path);
                            git.commit_ai_write(&[std::path::Path::new(&draft.path)], &msg)
                                .ok();
                        }
                        eprintln!("  {}", "approved (edited)".green());
                    } else {
                        eprintln!("  {}", "no changes, skipped".dimmed());
                    }
                    break;
                }
                "a" | "approve" | "d" | "D" | "delete" | "r" | "R" | "revise" | "s" | "S"
                | "skip" | "A" | "all" | "" => {
                    done = handle_draft_action(
                        &choice, draft, &pending, i, session_id, drafts, &vault,
                    )?;
                    break;
                }
                other => {
                    eprintln!("  {}", format!("unknown: \"{other}\"").dimmed());
                    continue;
                }
            }
        }
        eprintln!();
        if done {
            break;
        }
    }

    Ok(())
}

fn read_choice() -> Result<String> {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

/// Returns true if the caller should stop iterating (e.g. "approve all").
fn handle_draft_action(
    choice: &str,
    draft: &arcana_core::DraftInfo,
    pending: &[&arcana_core::DraftInfo],
    current_idx: usize,
    session_id: &str,
    drafts: &arcana_core::DraftManager,
    vault: &arcana_core::Vault,
) -> Result<bool> {
    match choice {
        "a" | "approve" => {
            approve_draft(session_id, &draft.path, drafts, vault)?;
            eprintln!("  {}", "approved".green());
            Ok(false)
        }
        "d" | "D" | "delete" => {
            drafts.reject(session_id, &draft.path)?;
            eprintln!("  {}", "deleted".red());
            Ok(false)
        }
        "A" | "all" => {
            // Approve this one and all remaining
            approve_draft(session_id, &draft.path, drafts, vault)?;
            eprintln!("  {}", "approved".green());

            for remaining in pending.iter().skip(current_idx + 1) {
                approve_draft(session_id, &remaining.path, drafts, vault)?;
                eprintln!("  {} {}", "approved:".green(), remaining.path);
            }
            Ok(true)
        }
        "r" | "R" | "revise" => {
            eprintln!(
                "  {}",
                "revise is not yet implemented — coming soon".yellow()
            );
            Ok(false)
        }
        "s" | "S" | "skip" | "" => {
            eprintln!("  {}", "skipped".dimmed());
            Ok(false)
        }
        other => {
            eprintln!("  {}", format!("unknown: \"{other}\" — skipped").dimmed());
            Ok(false)
        }
    }
}

fn approve_draft(
    session_id: &str,
    path: &str,
    drafts: &arcana_core::DraftManager,
    vault: &arcana_core::Vault,
) -> Result<()> {
    let target = drafts.approve(session_id, path)?;
    vault.reindex_paths(std::slice::from_ref(&target))?;

    if let Some(git) = vault.git() {
        let msg = format!("arcana: approve draft {path}");
        git.commit_ai_write(&[std::path::Path::new(path)], &msg)
            .ok();
    }
    Ok(())
}

fn view_in_pager(content: &str) -> Result<()> {
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".into());

    let mut child = std::process::Command::new(&pager)
        .arg("-R") // interpret ANSI color escapes
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|_| {
            eprint!("{content}");
            std::io::Error::new(std::io::ErrorKind::NotFound, "no pager")
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(content.as_bytes()).ok();
    }
    child.wait()?;
    Ok(())
}

async fn verify_style(
    llm: &dyn arcana_agent::LlmBackend,
    style_guide: &str,
    content: &str,
) -> Result<String> {
    let system = format!(
        "You are a style guide reviewer. Check the note below against the style guide.\n\n\
         If the note conforms, respond with exactly: PASS\n\n\
         If there are issues, list them as brief bullet points (max 5). \
         Focus only on style guide violations, not content quality.\n\n\
         <style_guide>\n{style_guide}\n</style_guide>"
    );

    // Truncate on a char boundary — note content routinely contains multi-byte
    // glyphs, and byte-slicing would panic when 8000 splits a codepoint.
    let note: String = content.chars().take(8000).collect();
    let msg = arcana_agent::Message::user(format!("<note>\n{note}\n</note>"));

    let response = llm
        .chat(&arcana_agent::SystemPrompt::cached(system), &[msg], &[])
        .await
        .map_err(|e| anyhow::anyhow!("style verification failed: {e}"))?;

    let text = response.text();
    if text.trim() == "PASS" || text.trim().to_uppercase() == "PASS" {
        Ok(String::new())
    } else {
        Ok(text)
    }
}

/// Quick heuristic checks on draft content. Returns a list of warnings.
fn heuristic_checks(path: &str, content: &str) -> Vec<String> {
    let mut warnings = Vec::new();

    // Check for frontmatter
    if !content.starts_with("---\n") {
        warnings.push("missing YAML frontmatter".into());
    } else if let Some(end) = content[4..].find("\n---\n") {
        let fm = &content[4..4 + end];
        if !fm.contains("title:") {
            warnings.push("frontmatter missing 'title' field".into());
        }
        if !fm.contains("tags:") {
            warnings.push("frontmatter missing 'tags' field".into());
        }
    }

    // Check filename convention: should be lowercase with hyphens
    let filename = path.rsplit('/').next().unwrap_or(path);
    if let Some(stem) = filename.strip_suffix(".md") {
        if stem != stem.to_lowercase() {
            warnings.push("filename contains uppercase characters".into());
        }
        if stem.contains('_') {
            warnings.push("filename uses underscores (prefer hyphens)".into());
        }
    }

    // Very short notes may be stubs
    let body_lines = content.lines().count();
    if body_lines < 5 {
        warnings.push(format!("very short ({body_lines} lines)"));
    }

    warnings
}

fn edit_in_editor(path: &std::path::Path) -> Result<bool> {
    let before = std::fs::read_to_string(path)?;

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".into());
    let status = std::process::Command::new(&editor).arg(path).status()?;

    if !status.success() {
        anyhow::bail!("editor exited with non-zero status");
    }

    let after = std::fs::read_to_string(path)?;
    Ok(before != after)
}
