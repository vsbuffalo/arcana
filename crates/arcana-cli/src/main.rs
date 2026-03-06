mod commands;
mod output;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use commands::Commands;

const WORKFLOWS_HELP: &str = "\
\x1b[1mWorkflows:\x1b[0m

  All AI pipelines follow the same pattern: plan first, generate second.
  Planning is cheap ($0.30-0.50), generation is expensive ($2-5).
  You always see what you'll get and what it costs before committing.

  \x1b[4mIngest (external project → vault notes)\x1b[0m

    arcana ingest ~/code/my-project
    arcana ingest ~/code/my-project --skill model-extract

    Reads an external codebase and authors new knowledge notes from
    scratch. The AI explores the project (tree, README, source files),
    understands its architecture and key concepts, then writes
    self-contained vault notes. Source material is code; output is
    explanatory notes — not copy-paste, but synthesized understanding.

      1. Explore  AI reads the project using tools (tree, read, search)
      2. Plan     proposes notes to create, with paths and summaries
      3. Cost     shows tokens spent so far + estimated generation cost
      4. Prompt   [g]enerate / [e]dit plan in $EDITOR / [q]uit
      5. Generate writes drafts to .arcana/drafts/<session>/

    Use --skill to load domain-specific extraction instructions (e.g.
    a skill that knows how to find model equations in scientific code).
    Use --auto to skip the interactive prompt (for scripts/CI).

  \x1b[4mTidy (inbox → structured notes)\x1b[0m

    arcana tidy inbox/
    arcana tidy inbox/brain-dump.md
    arcana tidy --tags unsorted

    Takes existing messy vault notes and reorganizes them — moves to
    the right zone, splits multi-topic dumps, extracts reusable
    concepts. The AI reads your notes and rewrites them to fit your
    vault's taxonomy. Source material is vault notes; output is
    restructured vault notes.

      1. Survey   reads target notes, gathers vault context
      2. Plan     proposes moves / splits / concept extractions
      3. Cost     shows tokens spent + estimated generation cost
      4. Prompt   [g]enerate / [e]dit plan in $EDITOR / [q]uit
      5. Generate writes drafts to .arcana/drafts/<session>/

    Use --auto to skip the prompt. Use --tags to filter by tag.

  \x1b[4mReview (approve or reject AI drafts)\x1b[0m

    arcana review

    All AI output lands in drafts — never directly in the vault.
    Review shows each pending draft with a diff against the vault.
    Accept, reject, or edit before committing. Git tracks provenance
    (human vs AI authorship) per line via arcana blame.

  \x1b[4mSkills (domain-specific AI instructions)\x1b[0m

    arcana skills                    list available skills
    arcana ingest . --skill extract  use a skill during ingest

    Skills are markdown files in .arcana/skills/ that teach the AI
    how to extract knowledge for a specific domain. They're injected
    into the system prompt alongside your brain profile (taxonomy +
    style guide). Examples: extracting model equations from scientific
    code, mapping API patterns, documenting infrastructure.

  \x1b[4mSearch & read\x1b[0m

    arcana search \"rust async\"     full-text search across all notes
    arcana read concepts/foo.md    print a note's content
    arcana context \"topic\"         generate LLM context block from vault

  \x1b[4mProvenance\x1b[0m

    arcana blame concepts/foo.md   line-level human vs AI attribution
    arcana log concepts/foo.md     git history for a note
    arcana diff concepts/foo.md    uncommitted changes
    arcana restore <note> <hash>   restore to a previous version";

#[derive(Parser)]
#[command(
    name = "arcana",
    about = "Fast Obsidian vault indexer and search",
    after_long_help = WORKFLOWS_HELP
)]
pub struct Cli {
    /// Path to the vault root directory (overrides config file)
    #[arg(long, global = true)]
    vault: Option<PathBuf>,

    /// Path to config file
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, global = true, default_value = "error")]
    log_level: String,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Git author name
    #[arg(long, global = true, env = "ARCANA_USER_NAME")]
    name: Option<String>,

    /// Git author email
    #[arg(long, global = true, env = "ARCANA_USER_EMAIL")]
    email: Option<String>,

    /// LLM profile to use (defined in [profiles.<name>] config)
    #[arg(short = 'P', long, global = true, env = "ARCANA_PROFILE")]
    profile: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // The `colored` crate checks stdout for TTY, but we write human output to
    // stderr. Force color on when stderr is a terminal.
    if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        colored::control::set_override(true);
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
        )
        .with_target(false)
        .init();

    let vault_path = resolve_vault_path(cli.vault)?;

    let config = if let Some(ref config_path) = cli.config {
        // Explicit --config: use only that file (no merge)
        arcana_core::ArcanaConfig::load(config_path)
            .map_err(|e| anyhow::anyhow!("failed to load config: {}", e))?
            .with_vault_path(vault_path)
    } else {
        // Merge: compiled defaults → global → vault-local
        let global_path = arcana_core::global_config_path();
        let vault_local = vault_path.join(".arcana").join("config.toml");

        if let Some(ref gp) = global_path {
            if gp.is_file() {
                tracing::debug!("loading global config from {}", gp.display());
            }
        }
        if vault_local.is_file() {
            tracing::debug!("loading vault config from {}", vault_local.display());
        }

        arcana_core::load_merged(global_path.as_deref(), Some(&vault_local))
            .map_err(|e| anyhow::anyhow!("failed to load config: {}", e))?
            .with_vault_path(vault_path)
    };

    let mut config = config;
    if let Some(name) = cli.name {
        config.git.user_name = name;
    }
    if let Some(email) = cli.email {
        config.git.user_email = email;
    }

    cli.command.run(config, cli.json, cli.profile)
}

fn resolve_vault_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    // 1. Explicit --vault flag
    if let Some(path) = explicit {
        return Ok(path);
    }

    // 2. vault.path from global config (~/.config/arcana/config.toml)
    if let Some(global_path) = arcana_core::global_config_path() {
        if global_path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&global_path) {
                if let Ok(config) = toml::from_str::<arcana_core::ArcanaConfig>(&content) {
                    if !config.vault.path.as_os_str().is_empty() {
                        let path = config.vault.path;
                        if path.is_dir() {
                            return Ok(path);
                        }
                    }
                }
            }
        }
    }

    let config_path = arcana_core::global_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.config/arcana/config.toml".to_string());

    anyhow::bail!(
        "no vault configured\n\n\
         Set vault.path in your config:\n\n  \
         mkdir -p $(dirname {config_path})\n  \
         echo '[vault]\\npath = \"/path/to/your/vault\"' > {config_path}\n\n\
         Or pass --vault /path/to/vault"
    )
}
