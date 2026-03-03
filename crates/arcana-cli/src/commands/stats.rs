use anyhow::Result;
use arcana_core::{ArcanaConfig, Vault};

use crate::output;

pub fn run_stats(config: ArcanaConfig, json: bool) -> Result<()> {
    let vault = Vault::open(config)?;
    let stats = vault.stats()?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "notes": stats.total_notes,
                "tags": stats.total_tags,
                "links": stats.total_links,
            }))?
        );
    } else {
        output::print_header("Vault Statistics");
        output::print_kv("Notes", &stats.total_notes.to_string());
        output::print_kv("Unique tags", &stats.total_tags.to_string());
        output::print_kv("Wikilinks", &stats.total_links.to_string());
    }

    Ok(())
}
