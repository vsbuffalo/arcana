mod commands;
mod output;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use commands::Commands;

const WORKFLOWS_HELP: &str = "\
\x1b[1mTwo kinds of vault:\x1b[0m

  A \x1b[1mledger vault\x1b[0m ([ledger] enabled = true) records who wrote every
  word. Agents write only through planned edits over MCP; your words
  can only receive suggestions; you review agent changes in a terminal
  UI. A \x1b[1mlegacy vault\x1b[0m keeps the older model: agents write notes
  directly, AI pipelines produce drafts, git commit authors stand in
  for authorship.

\x1b[1mLedger vaults:\x1b[0m

  \x1b[4mSet up\x1b[0m

    arcana --vault ~/vault/notes ledger init
    claude mcp add --scope user notes -- arcana --vault ~/vault/notes serve

    init writes note types (.arcana/types/*.toml: chapter, lab-note,
    post) and writing styles (.arcana/skills/*.md). Each type sets what
    agents may do (chapter, writing, log, pointer), a path pattern,
    tags, a template and a style.

  \x1b[4mReview\x1b[0m

    arcana review                     pending changes and unreviewed agent text
    arcana ledger status --short      count for a tmux status bar

    Keys: j/k item · a accept · r reject · c reject with a reason ·
    e edit yourself in $EDITOR · space/b, ctrl-d/ctrl-u, g/G, mouse
    wheel scroll · q quit. A light edit to your words (typo,
    punctuation, citation, at most 3 words) keeps them yours when
    accepted, recorded under the policy light-edit@1.

  \x1b[4mAuthorship\x1b[0m

    arcana blame <note>               who wrote each word
    arcana blame --stats --all        word counts by author, every note
    arcana restore <note> <commit>    restores text with its original authors
    arcana ledger import <notes>      bring notes in (never credited to you)

\x1b[1mLegacy vaults:\x1b[0m

  ingest and chat run plan-first AI pipelines that write drafts to
  .arcana/drafts/<session>/, approved with `arcana review`. blame shows
  line-level attribution from git commit authors. These commands refuse
  ledger vaults, where only planned edits may write.

\x1b[1mBoth:\x1b[0m

    arcana search \"rust async\"     full-text search
    arcana read <note>             print a note
    arcana context \"topic\"         LLM context block from search
    arcana log <note>              git history
    arcana diff <note>             uncommitted changes";

#[derive(Parser)]
#[command(
    name = "arcana",
    about = "Markdown notes with recorded authorship: search, MCP server for agents, review of agent changes",
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

    // Setup creates the vault and config, so it runs before either must exist.
    if let commands::Commands::Setup(args) = cli.command {
        return commands::setup::run_setup(args, cli.vault);
    }

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
