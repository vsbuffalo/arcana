use anyhow::Result;
use arcana_core::{ArcanaConfig, Vault};
use clap::Args;

use super::Run;
use crate::output;

#[derive(Args)]
pub struct ReadArgs {
    /// Path to the note (relative to vault root)
    path: String,
}

impl Run for ReadArgs {
    fn run(self, config: ArcanaConfig, json: bool) -> Result<()> {
        let vault = Vault::open(config)?;
        let note = vault.read_note(&self.path)?;

        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "path": note.path,
                    "title": note.title(),
                    "body": note.body,
                    "tags": note.frontmatter.tags,
                }))?
            );
        } else {
            output::print_header(note.title());
            output::print_kv("Path", &note.path.display().to_string());
            if !note.frontmatter.tags.is_empty() {
                output::print_kv("Tags", &note.frontmatter.tags.join(", "));
            }
            output::print_separator();
            println!("{}", note.body);
        }

        Ok(())
    }
}
