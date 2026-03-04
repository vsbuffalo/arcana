use anyhow::Result;
use arcana_core::ArcanaConfig;
use clap::Args;
use colored::Colorize;

use crate::output::is_stderr_tty;

#[derive(Args)]
pub struct DiffArgs {
    /// Path to the note (relative to vault root). Omit for all changes.
    pub path: Option<String>,
}

pub fn run_diff(args: DiffArgs, config: ArcanaConfig, json: bool) -> Result<()> {
    let vault = arcana_core::Vault::open(config)?;
    if !json {
        crate::output::print_git_init_info(&vault);
    }

    let git = vault
        .git()
        .ok_or_else(|| anyhow::anyhow!("git is not enabled for this vault"))?;

    let diff = git.diff(args.path.as_deref())?;

    if diff.is_empty() {
        if json {
            println!("{}", serde_json::json!({"diff": ""}));
        } else {
            eprintln!("{}", "no uncommitted changes".dimmed());
        }
        return Ok(());
    }

    if json {
        println!("{}", serde_json::json!({"diff": diff}));
        return Ok(());
    }

    // Color the diff output
    for line in diff.lines() {
        if is_stderr_tty() {
            if line.starts_with('+') {
                println!("{}", line.green());
            } else if line.starts_with('-') {
                println!("{}", line.red());
            } else {
                println!("{}", line);
            }
        } else {
            println!("{}", line);
        }
    }

    Ok(())
}
