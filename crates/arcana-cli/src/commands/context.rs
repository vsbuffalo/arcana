use anyhow::Result;
use arcana_core::ArcanaConfig;
use clap::Args;

#[derive(Args)]
pub struct ContextArgs {
    /// Search query for finding relevant notes
    pub query: String,

    /// Maximum number of notes to include (default: 5)
    #[arg(long, default_value = "5")]
    pub limit: usize,

    /// Output to a file instead of stdout
    #[arg(long)]
    pub output: Option<String>,
}

pub fn run_context(args: ContextArgs, config: ArcanaConfig) -> Result<()> {
    let vault = arcana_core::Vault::open(config)?;
    crate::output::print_git_init_info(&vault);
    vault.index()?;

    let context = arcana_agent::generate_context(&vault, &args.query, args.limit);

    if let Some(ref path) = args.output {
        std::fs::write(path, &context)?;
        eprintln!("context written to {path}");
    } else {
        print!("{context}");
    }

    Ok(())
}
