pub mod create;
pub mod index;
pub mod read;
pub mod search;
pub mod stats;

use anyhow::Result;
use arcana_core::ArcanaConfig;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum Commands {
    /// Build or rebuild the vault index
    Index(index::IndexArgs),
    /// Search notes by keyword
    Search(search::SearchArgs),
    /// Read a note's content
    Read(read::ReadArgs),
    /// Create a new note
    Create(create::CreateArgs),
    /// Show vault statistics
    Stats,
}

pub trait Run {
    fn run(self, config: ArcanaConfig, json: bool) -> Result<()>;
}

impl Run for Commands {
    fn run(self, config: ArcanaConfig, json: bool) -> Result<()> {
        match self {
            Commands::Index(args) => args.run(config, json),
            Commands::Search(args) => args.run(config, json),
            Commands::Read(args) => args.run(config, json),
            Commands::Create(args) => args.run(config, json),
            Commands::Stats => stats::run_stats(config, json),
        }
    }
}
