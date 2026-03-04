use anyhow::Result;
use arcana_core::{ArcanaConfig, Vault};
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};

use super::Run;
use crate::output;

#[derive(Args)]
pub struct IndexArgs {
    /// Force full reindex (ignore content hashes)
    #[arg(long)]
    force: bool,
}

impl Run for IndexArgs {
    fn run(self, config: ArcanaConfig, json: bool) -> Result<()> {
        let pb = if !json {
            let pb = ProgressBar::new_spinner();
            pb.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.cyan} {msg}")
                    .unwrap(),
            );
            pb.set_message("Indexing vault...");
            pb.enable_steady_tick(std::time::Duration::from_millis(80));
            Some(pb)
        } else {
            None
        };

        let vault = Vault::open(config)?;
        let stats = vault.index()?;

        if let Some(pb) = pb {
            pb.finish_and_clear();
        }

        if !json {
            output::print_git_init_info(&vault);
        }

        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "scanned": stats.notes_scanned,
                    "added": stats.notes_added,
                    "updated": stats.notes_updated,
                    "removed": stats.notes_removed,
                    "unchanged": stats.notes_unchanged,
                }))?
            );
        } else {
            output::print_header("Index complete");
            output::print_kv("Scanned", &stats.notes_scanned.to_string());
            output::print_kv("Added", &stats.notes_added.to_string());
            output::print_kv("Updated", &stats.notes_updated.to_string());
            output::print_kv("Removed", &stats.notes_removed.to_string());
            output::print_kv("Unchanged", &stats.notes_unchanged.to_string());
        }

        Ok(())
    }
}
