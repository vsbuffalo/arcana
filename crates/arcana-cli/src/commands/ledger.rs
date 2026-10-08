//! `arcana ledger` — set up and inspect a vault that records authorship.

use std::path::Path;

use anyhow::{bail, Context, Result};
use arcana_core::attr::ImportAs;
use arcana_core::ArcanaConfig;
use clap::{Args, Subcommand};

#[derive(Args)]
pub struct LedgerArgs {
    #[command(subcommand)]
    pub command: LedgerCommand,
}

#[derive(Subcommand)]
pub enum LedgerCommand {
    /// Create a new vault (or convert an empty one) that records who wrote every word
    Init,
    /// Bring existing notes under the ledger. Their words are credited to the
    /// named agent, or marked unattributed; never to you.
    Import {
        /// Vault-relative paths of notes already in this vault (without --plan)
        paths: Vec<String>,
        /// Who the words are credited to: agent:<name>, declared (yours, as
        /// you claim at import; reported apart from words arcana saw you
        /// write), or unattributed
        #[arg(long = "as", default_value = "unattributed")]
        as_: String,
        /// Follow a plan file: tab-separated `from  to  as` per line, `#`
        /// comments, `-` in `to` to skip a note, `delete` (in place) to remove
        /// it. The plan is kept in .arcana/imports/.
        #[arg(long, requires = "from")]
        plan: Option<std::path::PathBuf>,
        /// Directory the plan's `from` paths are relative to: another vault
        /// (notes are copied in) or this vault itself (labelled and moved in place)
        #[arg(long)]
        from: Option<std::path::PathBuf>,
        /// Show what would happen without writing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Attribute and commit every outstanding change now (the server also
    /// does this every few minutes)
    Sync,
    /// Pending reviews, unreviewed agent text and write health
    Status {
        /// One short line for a tmux status bar
        #[arg(long)]
        short: bool,
    },
}

const CONFIG: &str = r#"# Arcana ledger vault: every word has a recorded author.
[ledger]
enabled = true
# Edits made outside arcana (Obsidian, Neovim) are yours. Keep this true only
# while agents cannot write this directory directly (Claude Code sandbox).
outside_edits_are_human = true
default_kind = "chapter"

# Kind of a note without a `type:`: a path prefix or a glob; the longest
# matching rule wins.
[ledger.kinds]
"writing/" = "writing"
"blog-posts/" = "writing"
"personal/" = "writing"
"logs/" = "log"
"projects/*/lab/**" = "log"
"#;

const GITIGNORE: &str = ".arcana/index.db
.arcana/index.db-wal
.arcana/index.db-shm
.arcana/cache/
.arcana/pending/
.arcana/ledger.lock
.arcana/chat_history
.claude/
.obsidian/workspace*.json
";

const TYPE_CHAPTER: &str = r#"description = "An explanation of one concept, written for the vault's reader and refined in place"
kind = "chapter"
path = "notes/{subject}/{slug}.md"
tags = ["notes"]
style = "explanation"
template = """
## Idea

## Derivation

## Worked example

## Where this shows up
"""
"#;

const TYPE_LAB_NOTE: &str = r#"description = "A dated bench or lab session: setup, measurements, observations"
kind = "log"
path = "projects/{project}/lab/{date}-{slug}.md"
tags = ["lab"]
style = "lab-note"
template = """
## Context / question

## Setup

## Measurements

## Observations

## Interpretation

## Next
"""
"#;

const TYPE_POST: &str = r#"description = "A blog post or essay; the human's own writing (agents suggest only)"
kind = "writing"
path = "writing/{slug}.md"
tags = ["writing"]
"#;

const SKILL_EXPLANATION: &str = r#"---
title: Explanation style
description: How to write and refine explanations for the vault's reader
---

Write for a sharp scientist reading cold: precise, warm, teacherly, never
breathless.

- Open on the general object, then specialize by stated assumption. Name each
  simplifying assumption and what it costs.
- Define every term, symbol and acronym on first use. Prefer the concrete noun
  to shorthand.
- Let structure emerge from the object; never announce it ("it pays to…",
  "there are really only…" — delete and state the thing).
- Derive, then give a worked example with real numbers, ideally from the
  reader's own lab logs.
- Cite primary sources (author, year, DOI or URL); only citations that carry
  weight in the argument.
- Refine in place. When the reader asks a question about a section, answer it
  in that section rather than appending a new note.
"#;

const SKILL_LAB_NOTE: &str = r#"---
title: Lab note style
description: Dated, factual records of bench work
---

Record what was done and measured, plainly, before any interpretation.

- Context / question: what we were trying to learn or decide.
- Setup: instruments, parts, conditions; enough to reproduce.
- Measurements: numbers with units, tables, and the conditions each was taken
  under. Never round away information.
- Observations: what the data shows, before interpretation.
- Interpretation: what we think it means, with uncertainty flagged.
- Next: what this unblocks or what to measure next.

Entries are append-only: correct a past entry by adding a dated correction,
not by editing it.
"#;

pub fn run_ledger(args: LedgerArgs, config: ArcanaConfig, json: bool) -> Result<()> {
    match args.command {
        LedgerCommand::Init => init(&config),
        LedgerCommand::Import {
            paths,
            as_,
            plan,
            from,
            dry_run,
        } => match (plan, from) {
            (Some(plan), Some(from)) => import_plan(config, &plan, &from, dry_run),
            _ => import(config, &paths, &as_.parse()?),
        },
        LedgerCommand::Status { short } => status(config, short, json),
        LedgerCommand::Sync => {
            let vault = arcana_core::Vault::open(config)?;
            let ledger = vault.ledger().context("not a ledger vault")?;
            let git = vault.git().context("git is not enabled for this vault")?;
            let (paths, err) = ledger.sync(git)?;
            if let Some(e) = err {
                bail!("{e}");
            }
            eprintln!("committed {} outstanding paths", paths.len());
            Ok(())
        }
    }
}

fn write_new(path: &Path, content: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))
}

/// Append any of `wanted`'s lines missing from the file (creating it).
fn ensure_lines(path: &Path, wanted: &str) -> Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let have: std::collections::HashSet<&str> = existing.lines().map(str::trim).collect();
    let missing: Vec<&str> = wanted
        .lines()
        .filter(|l| !have.contains(l.trim()))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut out = existing.clone();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for l in missing {
        out.push_str(l);
        out.push('\n');
    }
    std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))
}

pub fn init(config: &ArcanaConfig) -> Result<()> {
    let root = config.vault.path.clone();
    std::fs::create_dir_all(&root)?;
    let a = root.join(".arcana");
    write_new(&a.join("config.toml"), CONFIG)?;
    ensure_lines(&root.join(".gitignore"), GITIGNORE)?;
    write_new(&a.join("types/chapter.toml"), TYPE_CHAPTER)?;
    write_new(&a.join("types/lab-note.toml"), TYPE_LAB_NOTE)?;
    write_new(&a.join("types/post.toml"), TYPE_POST)?;
    write_new(&a.join("skills/explanation.md"), SKILL_EXPLANATION)?;
    write_new(&a.join("skills/lab-note.md"), SKILL_LAB_NOTE)?;
    for d in ["notes", "writing", "projects", "logs"] {
        std::fs::create_dir_all(root.join(d))?;
    }

    // Re-load so the new vault-local config (ledger enabled) applies, then let
    // Vault::open initialize git.
    let merged = arcana_core::load_merged(
        arcana_core::global_config_path().as_deref(),
        Some(&a.join("config.toml")),
    )
    .map_err(|e| anyhow::anyhow!("config: {e}"))?
    .with_vault_path(root.clone());
    let vault = arcana_core::Vault::open(merged)?;
    if vault.ledger().is_none() {
        bail!(
            "ledger did not enable; check {}",
            a.join("config.toml").display()
        );
    }
    if let Some(git) = vault.git() {
        let paths = [
            ".gitignore",
            ".arcana/config.toml",
            ".arcana/types/chapter.toml",
            ".arcana/types/lab-note.toml",
            ".arcana/types/post.toml",
            ".arcana/skills/explanation.md",
            ".arcana/skills/lab-note.md",
        ];
        let refs: Vec<&Path> = paths.iter().map(Path::new).collect();
        git.commit_paths(&refs, "arcana: initialize ledger vault", true)?;
    }
    eprintln!("ledger vault ready at {}", root.display());
    eprintln!("  note types: chapter, lab-note, post   (edit .arcana/types/*.toml)");
    eprintln!("  styles:     .arcana/skills/explanation.md, lab-note.md");
    eprintln!("  review:     arcana --vault {} review", root.display());
    Ok(())
}

fn import(config: ArcanaConfig, paths: &[String], as_: &ImportAs) -> Result<()> {
    let vault = arcana_core::Vault::open(config)?;
    let ledger = vault
        .ledger()
        .context("not a ledger vault (run `arcana ledger init`)")?;
    for p in paths {
        if let Some(e) = ledger.import(p, as_, vault.git())? {
            eprintln!("{p}: {e}");
        }
        eprintln!("imported {p} as {as_}");
    }
    vault.index()?;
    Ok(())
}

enum Target {
    /// Leave the note out (or, in place, untouched).
    Skip,
    /// In place only: delete the note (git keeps its history).
    Delete,
    Path(String),
}

struct PlanRow {
    line: usize,
    from: String,
    to: Target,
    as_: ImportAs,
}

fn parse_plan(text: &str) -> Result<Vec<PlanRow>> {
    let mut rows = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim_end();
        if t.trim().is_empty() || t.trim_start().starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = t.split('\t').map(str::trim).collect();
        let [from, to, as_, ..] = cols.as_slice() else {
            bail!("plan line {}: expected `from<TAB>to<TAB>as`", i + 1);
        };
        let to = match *to {
            "" | "-" => Target::Skip,
            "delete" => Target::Delete,
            p => Target::Path(p.to_string()),
        };
        rows.push(PlanRow {
            line: i + 1,
            from: from.to_string(),
            to,
            as_: as_
                .parse()
                .with_context(|| format!("plan line {}", i + 1))?,
        });
    }
    Ok(rows)
}

/// Bring notes under the ledger per a plan file. With `--from` another
/// directory, notes are copied in. With `--from` this vault itself, the plan
/// works in place: a note whose `to` equals `from` is labelled where it is, a
/// different `to` moves it, and `delete` removes it (git keeps the history).
/// Every row is checked before anything is written; the plan is kept under
/// `.arcana/imports/` as the record.
fn import_plan(config: ArcanaConfig, plan: &Path, from: &Path, dry_run: bool) -> Result<()> {
    let text =
        std::fs::read_to_string(plan).with_context(|| format!("reading {}", plan.display()))?;
    let rows = parse_plan(&text)?;
    let vault = arcana_core::Vault::open(config)?;
    let ledger = vault
        .ledger()
        .context("not a ledger vault (run `arcana ledger init`)")?;
    let root = vault.root().to_path_buf();
    let in_place = from.canonicalize().ok().as_deref() == Some(root.as_path());
    let tracked = |rel: &str| {
        root.join(arcana_core::attr::Ledger::sidecar_rel(rel))
            .exists()
    };

    let mut problems = Vec::new();
    let mut targets = std::collections::BTreeSet::new();
    for r in &rows {
        let at = |m: String| format!("line {}: {m}", r.line);
        if !from.join(&r.from).is_file() {
            problems.push(at(format!("{} not found under {}", r.from, from.display())));
        }
        if in_place && tracked(&r.from) {
            problems.push(at(format!("{} is already under the ledger", r.from)));
        }
        match &r.to {
            Target::Skip => {}
            Target::Delete if !in_place => {
                problems.push(at("`delete` only applies when importing in place".into()))
            }
            Target::Delete => {}
            Target::Path(to) => {
                arcana_core::vault_path::validate_rel(to)
                    .map_err(|e| anyhow::anyhow!(at(e.to_string())))?;
                let same = in_place && *to == r.from;
                if !same && root.join(to).exists() {
                    problems.push(at(format!("{to} already exists in this vault")));
                }
                if !targets.insert(to.clone()) {
                    problems.push(at(format!("{to} is the target of two lines")));
                }
            }
        }
    }
    if !problems.is_empty() {
        bail!(
            "plan has problems; nothing imported:\n  {}",
            problems.join("\n  ")
        );
    }

    let git = vault.git();
    let (mut imported, mut deleted, mut skipped) = (0, 0, 0);
    for r in &rows {
        match &r.to {
            Target::Skip => skipped += 1,
            Target::Delete => {
                if dry_run {
                    eprintln!("would delete {}", r.from);
                } else {
                    std::fs::remove_file(root.join(&r.from))?;
                    if let Some(git) = git {
                        git.commit_paths(
                            &[Path::new(&r.from)],
                            &format!("arcana: delete {} (import plan)", r.from),
                            true,
                        )?;
                    }
                }
                deleted += 1;
            }
            Target::Path(to) => {
                if dry_run {
                    let verb = if in_place && *to == r.from {
                        "label"
                    } else {
                        "import"
                    };
                    eprintln!("would {verb} {} → {to} as {}", r.from, r.as_);
                    imported += 1;
                    continue;
                }
                let dest = root.join(to);
                if let Some(dir) = dest.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let moved = in_place && *to != r.from;
                if moved {
                    std::fs::rename(root.join(&r.from), &dest)?;
                } else if !in_place {
                    std::fs::copy(from.join(&r.from), &dest)?;
                }
                if let Some(e) = ledger.import(to, &r.as_, git)? {
                    eprintln!("{to}: {e}");
                }
                if moved {
                    if let Some(git) = git {
                        git.commit_paths(
                            &[Path::new(&r.from)],
                            &format!("arcana: move {} → {to}", r.from),
                            true,
                        )?;
                    }
                }
                imported += 1;
            }
        }
    }
    if dry_run {
        eprintln!("{imported} to import, {deleted} to delete, {skipped} untouched (dry run; nothing written)");
        return Ok(());
    }

    let name = plan
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("import.tsv");
    let record = format!(".arcana/imports/{name}");
    let header = format!("# imported from {} into this vault\n", from.display());
    write_new(&root.join(&record), &format!("{header}{text}"))?;
    if let Some(git) = git {
        git.commit_paths(
            &[Path::new(&record)],
            &format!("arcana: record import plan {name}"),
            true,
        )?;
    }
    vault.index()?;
    eprintln!("{imported} imported, {deleted} deleted, {skipped} untouched; plan kept at {record}");
    Ok(())
}

fn status(config: ArcanaConfig, short: bool, json: bool) -> Result<()> {
    let vault = arcana_core::Vault::open(config)?;
    let ledger = vault.ledger().context("not a ledger vault")?;
    let pending = ledger.pending()?.len();
    let unreviewed = ledger.unreviewed()?.len();
    let health = ledger.health();
    if json {
        println!(
            "{}",
            serde_json::json!({"pending": pending, "unreviewed": unreviewed, "health": health})
        );
    } else if short {
        let mut parts = Vec::new();
        // Only decisions that need the human; unreviewed agent text is
        // browsable but never nags.
        if pending > 0 {
            parts.push(format!("✎{pending}"));
        }
        if health.is_some() {
            parts.push("⚠git".into());
        }
        println!("{}", parts.join(" "));
    } else {
        println!("waiting for you:     {pending}   (arcana review)");
        println!("unreviewed passages: {unreviewed}   (arcana review --unreviewed)");
        println!("write health:        {}", health.as_deref().unwrap_or("ok"));
    }
    Ok(())
}
