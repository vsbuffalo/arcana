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

    /// Host/address to bind the SSE server to (also reads ARCANA_HOST).
    /// Defaults to loopback; binding a non-loopback address (e.g. 0.0.0.0)
    /// requires authentication.
    #[arg(long, env = "ARCANA_HOST", default_value = "127.0.0.1")]
    pub host: String,

    /// Port for the SSE transport (ignored for stdio)
    #[arg(short, long, default_value = "8080")]
    pub port: u16,

    /// Static bearer token for SSE auth (also reads ARCANA_BEARER_TOKEN).
    /// Accepted alongside OAuth tokens when OAuth is configured.
    #[arg(long, env = "ARCANA_BEARER_TOKEN")]
    pub bearer_token: Option<String>,

    /// OAuth client ID (also reads ARCANA_OAUTH_CLIENT_ID).
    /// Setting this enables OAuth 2.1 auth for the SSE transport.
    #[arg(long, env = "ARCANA_OAUTH_CLIENT_ID")]
    pub oauth_client_id: Option<String>,

    /// OAuth client secret (also reads ARCANA_OAUTH_CLIENT_SECRET).
    #[arg(long, env = "ARCANA_OAUTH_CLIENT_SECRET")]
    pub oauth_client_secret: Option<String>,

    /// Password for the OAuth authorization page (also reads ARCANA_OAUTH_PASSWORD).
    #[arg(long, env = "ARCANA_OAUTH_PASSWORD")]
    pub oauth_password: Option<String>,

    /// Public hostname the server is reached through, e.g. a tunnel such as
    /// arcana-mcp.example.com (also reads ARCANA_PUBLIC_HOSTS, comma-separated).
    /// Requests whose Host header is not loopback or one of these are rejected
    /// as possible DNS rebinding.
    #[arg(
        long = "public-host",
        env = "ARCANA_PUBLIC_HOSTS",
        value_delimiter = ','
    )]
    pub public_hosts: Vec<String>,
}

pub fn run_serve(args: ServeArgs, config: ArcanaConfig) -> Result<()> {
    let vault = arcana_core::Vault::open(config)?;

    // Index on startup so the vault is always fresh
    vault.index()?;

    let oauth_config = match (
        &args.oauth_client_id,
        &args.oauth_client_secret,
        &args.oauth_password,
    ) {
        (Some(id), Some(secret), Some(password)) => Some(arcana_server::OAuthConfig {
            client_id: id.clone(),
            client_secret: secret.clone(),
            password: password.clone(),
        }),
        (None, None, None) => None,
        _ => {
            anyhow::bail!(
                "OAuth requires all three: --oauth-client-id, --oauth-client-secret, and --oauth-password"
            );
        }
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        match args.transport {
            Transport::Stdio => arcana_server::serve_stdio(vault).await,
            Transport::Sse => {
                arcana_server::serve_sse(
                    vault,
                    args.host,
                    args.port,
                    args.bearer_token,
                    oauth_config,
                    args.public_hosts,
                )
                .await
            }
        }
    })
}
