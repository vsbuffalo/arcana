use anyhow::Result;
use arcana_core::{ArcanaConfig, ProvenanceAuthor};
use clap::Args;
use colored::Colorize;

use crate::output::is_stderr_tty;

#[derive(Args)]
pub struct BlameArgs {
    /// Path to the note (relative to vault root)
    pub path: Option<String>,

    /// Show aggregate stats instead of line-by-line blame
    #[arg(long)]
    pub stats: bool,

    /// When used with --stats, show stats for all notes
    #[arg(long)]
    pub all: bool,
}

pub fn run_blame(args: BlameArgs, config: ArcanaConfig, json: bool) -> Result<()> {
    let vault = arcana_core::Vault::open(config)?;
    if !json {
        crate::output::print_git_init_info(&vault);
    }

    let git = vault
        .git()
        .ok_or_else(|| anyhow::anyhow!("git is not enabled for this vault"))?;

    if args.stats && args.all {
        // Vault-wide provenance report
        vault.index()?;
        let results = vault.list(&arcana_core::SearchFilters::default(), 10000)?;

        let mut stats: Vec<serde_json::Value> = Vec::new();
        for result in &results {
            match git.provenance(&result.path) {
                Ok(prov) => {
                    if json {
                        stats.push(serde_json::json!({
                            "path": prov.path,
                            "total_lines": prov.total_lines,
                            "human_lines": prov.human_lines,
                            "ai_lines": prov.ai_lines,
                            "human_pct": prov.human_pct,
                            "ai_pct": prov.ai_pct,
                        }));
                    } else if is_stderr_tty() {
                        let ai_label = if prov.ai_pct > 0.0 {
                            format!("{:.0}% AI", prov.ai_pct).blue().to_string()
                        } else {
                            "human".green().to_string()
                        };
                        eprintln!("  {} {}", ai_label, result.path.dimmed());
                    } else {
                        println!(
                            "{}\t{:.0}%\t{:.0}%",
                            result.path, prov.human_pct, prov.ai_pct
                        );
                    }
                }
                Err(_) => continue,
            }
        }

        if json {
            println!("{}", serde_json::to_string_pretty(&stats)?);
        }
        return Ok(());
    }

    let path = args
        .path
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("path is required (use --all for vault-wide stats)"))?;

    if args.stats {
        let prov = git.provenance(path)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&prov)?);
        } else {
            eprintln!("{} {}", "provenance:".bold(), path.cyan());
            eprintln!(
                "  total: {} lines, {} human ({:.0}%), {} AI ({:.0}%)",
                prov.total_lines, prov.human_lines, prov.human_pct, prov.ai_lines, prov.ai_pct,
            );
        }
        return Ok(());
    }

    // Line-by-line blame
    let lines = git.blame(path)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&lines)?);
        return Ok(());
    }

    for line in &lines {
        let commit_short = &line.commit_id[..7.min(line.commit_id.len())];
        let author_display = match line.author {
            ProvenanceAuthor::Ai => {
                if is_stderr_tty() {
                    format!("{} {}", commit_short.blue(), line.author_name.blue())
                } else {
                    format!("{} {}", commit_short, line.author_name)
                }
            }
            ProvenanceAuthor::Human => {
                if is_stderr_tty() {
                    format!("{} {}", commit_short.green(), line.author_name.green())
                } else {
                    format!("{} {}", commit_short, line.author_name)
                }
            }
            ProvenanceAuthor::Unknown => {
                format!("{} {}", commit_short, line.author_name)
            }
        };

        println!("{:>4} {:30} {}", line.line_no, author_display, line.content);
    }

    Ok(())
}
