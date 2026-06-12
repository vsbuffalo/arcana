pub mod anthropic;
pub mod openai;

use async_trait::async_trait;

use crate::error::Result;
use crate::types::{LlmResponse, Message, SystemPrompt, ToolDef};

#[async_trait]
pub trait LlmBackend: Send + Sync {
    async fn chat(
        &self,
        system: &SystemPrompt,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<LlmResponse>;

    fn model_name(&self) -> &str;
    fn provider_name(&self) -> &str;
}

/// Retry a request up to `max_retries` times on transient HTTP errors (429, 5xx).
/// Uses exponential backoff starting at 1s.
pub(crate) async fn retry_request<F, Fut>(max_retries: u32, mut f: F) -> Result<reqwest::Response>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<reqwest::Response>>,
{
    let mut attempt = 0;
    loop {
        let resp = f().await?;
        let status = resp.status();
        if status.is_success() || (!status.is_server_error() && status.as_u16() != 429) {
            return Ok(resp);
        }
        attempt += 1;
        if attempt > max_retries {
            return Ok(resp);
        }
        let delay = std::time::Duration::from_secs(1 << (attempt - 1));
        tracing::warn!(
            "request failed with {status}, retrying in {}s (attempt {attempt}/{max_retries})",
            delay.as_secs()
        );
        tokio::time::sleep(delay).await;
    }
}

// ---------------------------------------------------------------------------
// MockBackend (test only)
// ---------------------------------------------------------------------------

#[cfg(test)]
pub mod mock {
    use super::*;
    use crate::types::{ContentBlock, LlmResponse, StopReason, Usage};
    use std::sync::Mutex;

    pub struct MockBackend {
        responses: Mutex<Vec<LlmResponse>>,
        pub model: String,
        pub provider: String,
    }

    impl MockBackend {
        pub fn new(responses: Vec<LlmResponse>) -> Self {
            Self {
                responses: Mutex::new(responses),
                model: "mock-model".into(),
                provider: "mock".into(),
            }
        }

        /// Convenience: single end-turn text response.
        pub fn single_text(text: &str) -> Self {
            Self::new(vec![LlmResponse {
                content: vec![ContentBlock::Text {
                    text: text.to_string(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Default::default()
                },
            }])
        }
    }

    #[async_trait]
    impl LlmBackend for MockBackend {
        async fn chat(
            &self,
            _system: &SystemPrompt,
            _messages: &[Message],
            _tools: &[ToolDef],
        ) -> Result<LlmResponse> {
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                Ok(LlmResponse {
                    content: vec![ContentBlock::Text {
                        text: "no more mock responses".into(),
                    }],
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                })
            } else {
                Ok(responses.remove(0))
            }
        }

        fn model_name(&self) -> &str {
            &self.model
        }

        fn provider_name(&self) -> &str {
            &self.provider
        }
    }
}
