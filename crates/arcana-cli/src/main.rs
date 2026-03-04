mod commands;
mod output;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use commands::{Commands, Run};

#[derive(Parser)]
#[command(name = "arcana", about = "Fast Obsidian vault indexer and search")]
pub struct Cli {
    /// Path to the vault root directory
    #[arg(long, global = true, env = "ARCANA_VAULT")]
    vault: Option<PathBuf>,

    /// Path to config file
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, global = true, default_value = "warn")]
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

        arcana_core::load_merged(
            global_path.as_deref(),
            Some(&vault_local),
        )
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

    cli.command.run(config, cli.json)
}

fn resolve_vault_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    // 1. Explicit --vault flag
    if let Some(path) = explicit {
        return Ok(path);
    }

    // 2. ARCANA_VAULT env var (handled by clap env)
    // If we get here, neither --vault nor env var was set.

    // 3. Walk up from cwd looking for .obsidian/ directory
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join(".obsidian").is_dir() {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }

    anyhow::bail!(
        "could not find vault. Use --vault, set ARCANA_VAULT, or run from within an Obsidian vault"
    )
}
