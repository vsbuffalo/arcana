use anyhow::Result;
use arcana_core::{ArcanaConfig, ProvenanceAuthor};
use clap::Args;
use colored::Colorize;

use crate::output::is_stderr_tty;

#[derive(Args)]
pub struct LogArgs {
    /// Path to the note (relative to vault root)
    pub path: Option<String>,

    /// Maximum number of entries to show
    #[arg(long, default_value = "20")]
    pub limit: usize,
}

pub fn run_log(args: LogArgs, config: ArcanaConfig, json: bool) -> Result<()> {
    let vault = arcana_core::Vault::open(config)?;
    if !json {
        crate::output::print_git_init_info(&vault);
    }

    let git = vault
        .git()
        .ok_or_else(|| anyhow::anyhow!("git is not enabled for this vault"))?;

    let commits = git.log(args.path.as_deref(), args.limit)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&commits)?);
        return Ok(());
    }

    if commits.is_empty() {
        eprintln!("{}", "no commits found".dimmed());
        return Ok(());
    }

    for commit in &commits {
        let commit_short = &commit.id[..7.min(commit.id.len())];
        let ts = chrono::DateTime::from_timestamp(commit.timestamp, 0)
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();

        let author_label = match commit.author {
            ProvenanceAuthor::Ai => {
                if is_stderr_tty() {
                    commit.author_name.blue().bold().to_string()
                } else {
                    commit.author_name.clone()
                }
            }
            ProvenanceAuthor::Human => {
                if is_stderr_tty() {
                    commit.author_name.green().bold().to_string()
                } else {
                    commit.author_name.clone()
                }
            }
            ProvenanceAuthor::Unknown => commit.author_name.clone(),
        };

        let message = commit.message.lines().next().unwrap_or("");

        if is_stderr_tty() {
            eprintln!(
                "{} {} {} {}",
                commit_short.yellow().bold(),
                ts.dimmed(),
                author_label,
                message,
            );
        } else {
            println!(
                "{}\t{}\t{}\t{}",
                commit_short, ts, commit.author_name, message
            );
        }
    }

    Ok(())
}
