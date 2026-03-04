use std::io::IsTerminal;

use anyhow::Result;
use arcana_core::{ArcanaConfig, SearchFilters, SearchQuery, Vault};
use clap::Args;
use colored::Colorize;

use super::Run;
use crate::output;

#[derive(Args)]
pub struct SearchArgs {
    /// Search query
    query: String,

    /// Maximum number of results
    #[arg(short, long, default_value = "20")]
    limit: usize,

    /// Filter by tag
    #[arg(short, long)]
    tag: Vec<String>,

    /// Filter by path prefix
    #[arg(short, long)]
    path: Option<String>,

    /// Only show AI-generated notes
    #[arg(long)]
    ai_only: bool,

    /// Output only file paths (one per line, for piping)
    #[arg(long)]
    paths: bool,
}

impl Run for SearchArgs {
    fn run(self, config: ArcanaConfig, json: bool) -> Result<()> {
        let vault = Vault::open(config)?;
        if !json {
            output::print_git_init_info(&vault);
        }

        let query = SearchQuery {
            text: self.query,
            limit: Some(self.limit),
            filters: SearchFilters {
                tags: self.tag,
                path_prefix: self.path,
                ai_only: self.ai_only,
            },
        };

        let results = vault.search(&query)?;

        if json {
            println!("{}", serde_json::to_string_pretty(&results)?);
        } else if self.paths || !std::io::stdout().is_terminal() {
            // Pipe-friendly: one path per line
            for result in &results {
                println!("{}", result.path);
            }
        } else if results.is_empty() {
            println!("No results found.");
        } else {
            println!(
                "{}",
                format!(
                    "{} result{}",
                    results.len(),
                    if results.len() == 1 { "" } else { "s" }
                )
                .dimmed()
            );
            println!();
            for result in &results {
                output::print_search_result(&result.path, &result.snippet);
            }
        }

        Ok(())
    }
}
