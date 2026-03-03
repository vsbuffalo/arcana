use anyhow::Result;
use arcana_core::{ArcanaConfig, Frontmatter, Vault};
use clap::Args;

use super::Run;

#[derive(Args)]
pub struct CreateArgs {
    /// Path for the new note (relative to vault root)
    path: String,

    /// Note title
    #[arg(short, long)]
    title: Option<String>,

    /// Tags (comma-separated)
    #[arg(long)]
    tags: Option<String>,

    /// Note body content
    #[arg(short, long, default_value = "")]
    body: String,
}

impl Run for CreateArgs {
    fn run(self, config: ArcanaConfig, json: bool) -> Result<()> {
        let vault = Vault::open(config)?;

        let mut fm = Frontmatter::default();
        if let Some(title) = self.title {
            fm.title = Some(title);
        }
        if let Some(tags) = self.tags {
            fm.tags = tags.split(',').map(|t| t.trim().to_string()).collect();
        }

        vault.create_note(&self.path, &self.body, Some(fm))?;

        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "created": self.path,
                }))?
            );
        } else {
            println!("Created: {}", self.path);
        }

        Ok(())
    }
}
