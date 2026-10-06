pub mod blame;
pub mod chat;
pub mod context;
pub mod create;
pub mod diff;
pub mod index;
pub mod ingest;
pub mod ledger;
pub mod log;
pub mod read;
pub mod restore;
pub mod review;
pub mod review_tui;
pub mod search;
pub mod serve;
pub mod skills;
pub mod stats;
pub mod tidy;

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
    /// Start the MCP server (for Claude Code / Claude Web)
    Serve(serve::ServeArgs),
    /// Interactive chat with your vault using an LLM
    Chat(chat::ChatArgs),
    /// Review and approve/reject AI-generated drafts
    Review(review::ReviewArgs),
    /// Show line-level provenance (human vs AI) for a note
    Blame(blame::BlameArgs),
    /// Generate a context block from vault search for use with LLMs
    Context(context::ContextArgs),
    /// Show git history for a note or the vault
    Log(log::LogArgs),
    /// Show uncommitted changes for a note
    Diff(diff::DiffArgs),
    /// Restore a note to a previous version
    Restore(restore::RestoreArgs),
    /// Reorganize existing vault notes — move, split, and extract concepts
    Tidy(tidy::TidyArgs),
    /// Ingest an external project into vault notes using an LLM
    Ingest(ingest::IngestArgs),
    /// List available skills from .arcana/skills/
    Skills(skills::SkillsArgs),
    /// Set up or inspect a vault that records who wrote every word
    Ledger(ledger::LedgerArgs),
}

pub trait Run {
    fn run(self, config: ArcanaConfig, json: bool) -> Result<()>;
}

impl Commands {
    pub fn run(self, config: ArcanaConfig, json: bool, profile: Option<String>) -> Result<()> {
        match self {
            Commands::Index(args) => args.run(config, json),
            Commands::Search(args) => args.run(config, json),
            Commands::Read(args) => args.run(config, json),
            Commands::Create(args) => args.run(config, json),
            Commands::Stats => stats::run_stats(config, json),
            Commands::Serve(args) => serve::run_serve(args, config),
            Commands::Chat(args) => chat::run_chat(args, config, profile),
            Commands::Review(args) => review::run_review(args, config, json, profile),
            Commands::Blame(args) => blame::run_blame(args, config, json),
            Commands::Context(args) => context::run_context(args, config),
            Commands::Log(args) => log::run_log(args, config, json),
            Commands::Diff(args) => diff::run_diff(args, config, json),
            Commands::Restore(args) => restore::run_restore(args, config, json),
            Commands::Tidy(args) => tidy::run_tidy(args, config, profile),
            Commands::Ingest(args) => ingest::run_ingest(args, config, profile),
            Commands::Skills(args) => skills::run_skills(args, config, json),
            Commands::Ledger(args) => ledger::run_ledger(args, config, json),
        }
    }
}
