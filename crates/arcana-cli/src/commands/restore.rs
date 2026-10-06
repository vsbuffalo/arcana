use anyhow::Result;
use arcana_core::ArcanaConfig;
use clap::Args;
use colored::Colorize;

#[derive(Args)]
pub struct RestoreArgs {
    /// Path to the note (relative to vault root)
    pub path: String,

    /// Commit hash to restore from
    pub commit: String,
}

pub fn run_restore(args: RestoreArgs, config: ArcanaConfig, json: bool) -> Result<()> {
    let vault = arcana_core::Vault::open(config)?;
    if !json {
        crate::output::print_git_init_info(&vault);
    }

    let git = vault
        .git()
        .ok_or_else(|| anyhow::anyhow!("git is not enabled for this vault"))?;

    if let Some(ledger) = vault.ledger() {
        if let Some(e) = ledger.restore(&args.path, &args.commit, git)? {
            eprintln!("warning: {e}");
        }
        vault.reindex_paths(&[vault.root().join(&args.path)])?;
        eprintln!(
            "{} {} to {} with its authorship at that commit",
            "restored".green().bold(),
            args.path,
            args.commit
        );
        return Ok(());
    }

    let oid = git.restore(&args.path, &args.commit)?;
    vault.reindex_paths(&[vault.root().join(&args.path)])?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "restored": args.path,
                "from_commit": args.commit,
                "new_commit": oid.to_string(),
            })
        );
    } else {
        eprintln!(
            "{} {} to commit {}",
            "restored".green().bold(),
            args.path.cyan(),
            &args.commit[..7.min(args.commit.len())],
        );
    }

    Ok(())
}
