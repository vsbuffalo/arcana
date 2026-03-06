pub mod agent;
pub mod backend;
pub mod chat;
pub mod context;
pub mod error;
pub mod executor;
pub mod ingest;
pub mod permissions;
pub mod pricing;
pub mod project_tools;
pub mod prompt;
pub mod tidy;
pub mod tools;
pub mod types;

pub use agent::{AgentConfig, AgentEvent};
pub use backend::LlmBackend;
pub use chat::{ChatResponse, ChatSession};
pub use context::generate_context;
pub use error::AgentError;
pub use executor::{CompositeExecutor, PermissionedExecutor, ToolExecutor};
pub use ingest::{
    run_ingest_auto, Done as IngestDone, Explored as IngestExplored, Fresh as IngestFresh, Ingest,
    IngestConfig, IngestEvent, IngestPlan, Planned as IngestPlanned,
};
pub use permissions::{ApprovalResult, ToolPermission};
pub use pricing::CostEstimate;
pub use project_tools::ProjectToolExecutor;
pub use prompt::build_system_prompt;
pub use tidy::{
    run_tidy_auto, SourceNote, Tidy, TidyConfig, TidyDone, TidyEvent, TidyPlan, TidyPlanned,
    Unsurveyed, VaultSurveyed,
};
pub use tools::{SessionContext, VaultToolExecutor};
pub use types::{ContentBlock, LlmResponse, Message, StopReason, ToolDef, Usage};

use arcana_core::LlmConfig;

/// Create an LLM backend from config. Resolves the API key from environment.
///
/// Uses `LlmConfig::effective_api_key_env()` to auto-resolve the env var name
/// from the provider when not explicitly set (e.g. `anthropic` → `ANTHROPIC_API_KEY`).
pub fn create_backend(config: &LlmConfig) -> error::Result<Box<dyn LlmBackend>> {
    let max_output_tokens = config.max_output_tokens.unwrap_or(8192);

    match config.provider.as_str() {
        "anthropic" => {
            let api_key = resolve_api_key(config)?;
            Ok(Box::new(backend::anthropic::AnthropicBackend::new(
                api_key,
                Some(config.model.clone()),
                max_output_tokens,
            )))
        }
        "openai" => {
            let api_key = resolve_api_key(config)?;
            Ok(Box::new(backend::openai::OpenAiBackend::new(
                config
                    .endpoint
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com/v1".into()),
                Some(api_key),
                config.model.clone(),
                "openai".into(),
                max_output_tokens,
            )))
        }
        "ollama" => Ok(Box::new(backend::openai::OpenAiBackend::new_ollama(
            config.endpoint.clone(),
            config.model.clone(),
            max_output_tokens,
        ))),
        other => Err(AgentError::Config(format!(
            "unknown llm provider: '{other}' (expected 'anthropic', 'openai', or 'ollama')"
        ))),
    }
}

fn resolve_api_key(config: &LlmConfig) -> error::Result<String> {
    let env_var = config.effective_api_key_env().ok_or_else(|| {
        AgentError::Config(format!(
            "provider '{}' requires api_key_env to be set (no default for this provider)",
            config.provider
        ))
    })?;
    std::env::var(env_var).map_err(|_| AgentError::MissingApiKey(env_var.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_backend_unknown_provider() {
        let config = LlmConfig {
            provider: "unknown".into(),
            ..Default::default()
        };
        match create_backend(&config) {
            Err(e) => assert!(e.to_string().contains("unknown llm provider")),
            Ok(_) => panic!("expected error for unknown provider"),
        }
    }

    #[test]
    fn create_backend_missing_api_key() {
        std::env::remove_var("TEST_NONEXISTENT_KEY_12345");
        let config = LlmConfig {
            provider: "anthropic".into(),
            api_key_env: Some("TEST_NONEXISTENT_KEY_12345".into()),
            ..Default::default()
        };
        match create_backend(&config) {
            Err(e) => assert!(e.to_string().contains("missing api key")),
            Ok(_) => panic!("expected error for missing api key"),
        }
    }

    #[test]
    fn create_backend_ollama_no_key_needed() {
        let config = LlmConfig {
            provider: "ollama".into(),
            model: "llama3.1".into(),
            ..Default::default()
        };
        let backend = create_backend(&config).unwrap();
        assert_eq!(backend.provider_name(), "ollama");
        assert_eq!(backend.model_name(), "llama3.1");
    }
}
