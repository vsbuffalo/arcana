use anyhow::Result;
use arcana_core::ArcanaConfig;
use clap::{Args, ValueEnum};

#[derive(Clone, ValueEnum)]
pub enum Transport {
    Stdio,
    Sse,
}

#[derive(Args)]
pub struct ServeArgs {
    /// Transport to use for the MCP server
    #[arg(short, long, default_value = "stdio")]
    pub transport: Transport,

    /// Port for the SSE transport (ignored for stdio)
    #[arg(short, long, default_value = "8080")]
    pub port: u16,
}

pub fn run_serve(args: ServeArgs, config: ArcanaConfig) -> Result<()> {
    let vault = arcana_core::Vault::open(config)?;

    // Index on startup so the vault is always fresh
    vault.index()?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        match args.transport {
            Transport::Stdio => arcana_server::serve_stdio(vault).await,
            Transport::Sse => arcana_server::serve_sse(vault, args.port).await,
        }
    })
}
