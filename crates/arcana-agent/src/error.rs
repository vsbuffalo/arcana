use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("llm error: {0}")]
    Llm(String),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("vault error: {0}")]
    Vault(#[from] arcana_core::ArcanaError),

    #[error("config error: {0}")]
    Config(String),

    #[error("missing api key: env var '{0}' is not set")]
    MissingApiKey(String),
}

pub type Result<T> = std::result::Result<T, AgentError>;
