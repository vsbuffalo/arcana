use anyhow::Result;
use arcana_core::{ArcanaConfig, ProvenanceAuthor};
use clap::Args;
use colored::Colorize;

use crate::output::is_stderr_tty;

#[derive(Args)]
pub struct BlameArgs {
    /// Path to the note (relative to vault root)
    pub path: Option<String>,

    /// Show counts by author instead of the annotated note
    #[arg(long)]
    pub stats: bool,

    /// With --stats and a path, still report the whole vault (--stats alone does too)
    #[arg(long)]
    pub all: bool,
}

pub fn run_blame(args: BlameArgs, config: ArcanaConfig, json: bool) -> Result<()> {
    let vault = arcana_core::Vault::open(config)?;
    if let Some(ledger) = vault.ledger() {
        return match (args.stats, args.path.as_deref()) {
            (true, Some(path)) if !args.all => ledger_stats(ledger, &[path.to_string()], json),
            (true, _) => {
                vault.index()?;
                let notes: Vec<String> = vault
                    .list(&arcana_core::SearchFilters::default(), 100_000)?
                    .into_iter()
                    .map(|r| r.path)
                    .collect();
                ledger_stats(ledger, &notes, json)
            }
            (false, Some(path)) => ledger_blame(ledger, path, json),
            (false, None) => anyhow::bail!("give a note path, or --stats --all"),
        };
    }
    if !json {
        crate::output::print_git_init_info(&vault);
    }

    let git = vault
        .git()
        .ok_or_else(|| anyhow::anyhow!("git is not enabled for this vault"))?;

    if args.stats && (args.all || args.path.is_none()) {
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
        .ok_or_else(|| anyhow::anyhow!("give a note path, or --stats for the whole vault"))?;

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

/// Word counts by author class, one line per note.
fn ledger_stats(ledger: &arcana_core::attr::Ledger, notes: &[String], json: bool) -> Result<()> {
    let mut rows = Vec::new();
    for n in notes {
        let st = ledger.state(n)?;
        rows.push((n.clone(), st.kind.as_str(), st.attribution.summary()));
    }
    if json {
        let v: Vec<_> = rows
            .iter()
            .map(|(n, k, s)| serde_json::json!({"path": n, "kind": k, "words": s}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!(
        "{:>7} {:>7} {:>10} {:>12}  note",
        "yours", "agent", "unreviewed", "unattributed"
    );
    for (n, k, s) in rows {
        println!(
            "{:>7} {:>7} {:>10} {:>12}  {n} [{k}]",
            s.human, s.agent, s.unreviewed, s.unattributed
        );
    }
    Ok(())
}

/// Word-level authorship from the ledger: your words plain, agent words cyan
/// (dimmed while unreviewed), unattributed words yellow.
fn ledger_blame(ledger: &arcana_core::attr::Ledger, path: &str, json: bool) -> Result<()> {
    use arcana_core::attr::{attribution::zip, Author};

    let st = ledger.state(path)?;
    let a = &st.attribution;
    let sum = a.summary();
    if json {
        let runs: Vec<_> = a
            .runs()
            .iter()
            .map(|(start, len, t)| {
                serde_json::json!({
                    "start": start, "len": len,
                    "author": a.author_of(t),
                    "origin": t.origin,
                    "unreviewed": t.unreviewed,
                    "policy": t.policy,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "path": path, "kind": st.kind.as_str(), "summary": sum, "runs": runs
            }))?
        );
        return Ok(());
    }
    let mut out = String::new();
    let mut pos = 0;
    for (tok, attr) in zip(&st.content, a) {
        out.push_str(&st.content[pos..tok.start]);
        let text = tok.text(&st.content);
        let shown = match a.author_of(attr) {
            Author::Human { .. } if attr.policy.is_some() => text.underline().to_string(),
            Author::Human { .. } => text.normal().to_string(),
            Author::Agent { .. } if attr.unreviewed => text.cyan().dimmed().to_string(),
            Author::Agent { .. } => text.cyan().to_string(),
            Author::Unattributed => text.yellow().to_string(),
        };
        out.push_str(&shown);
        pos = tok.end;
    }
    out.push_str(&st.content[pos..]);
    print!("{out}");
    eprintln!(
        "\n{}  {} yours · {} agent ({} unreviewed) · {} unattributed  [{}]",
        path.bold(),
        sum.human,
        sum.agent.to_string().cyan(),
        sum.unreviewed,
        sum.unattributed.to_string().yellow(),
        st.kind.as_str()
    );
    eprintln!(
        "{}",
        "words: yours plain · light edits to yours underlined · agent cyan (dim = unreviewed) · unattributed yellow"
            .dimmed()
    );
    Ok(())
}
