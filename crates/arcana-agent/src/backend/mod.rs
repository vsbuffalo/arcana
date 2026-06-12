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

/// Per-request timeout for the HTTP clients. Long enough for a slow non-streaming
/// generation, short enough to surface a genuinely hung connection (which the
/// retry loop then re-attempts).
pub(crate) const REQUEST_TIMEOUT_SECS: u64 = 300;

/// Largest Retry-After we'll honor — guards against a pathological or hostile
/// value pinning us for a long time.
const MAX_RETRY_AFTER_SECS: u64 = 60;

fn backoff(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(1u64 << (attempt - 1))
}

/// Parse a `Retry-After` header (delta-seconds form, which the LLM APIs use)
/// into a capped Duration.
fn retry_after_duration(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    let secs: u64 = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(std::time::Duration::from_secs(secs.min(MAX_RETRY_AFTER_SECS)))
}

/// Retry a request up to `max_retries` times on transient failures: HTTP 429 /
/// 5xx, and transport/timeout errors (the more common transient case). Honors a
/// `Retry-After` header when the server sends one, otherwise exponential backoff
/// from 1s.
pub(crate) async fn retry_request<F, Fut>(max_retries: u32, mut f: F) -> Result<reqwest::Response>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<reqwest::Response>>,
{
    let mut attempt = 0;
    loop {
        match f().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() || (!status.is_server_error() && status.as_u16() != 429) {
                    return Ok(resp);
                }
                attempt += 1;
                if attempt > max_retries {
                    return Ok(resp);
                }
                let delay =
                    retry_after_duration(resp.headers()).unwrap_or_else(|| backoff(attempt));
                tracing::warn!(
                    "request failed with {status}, retrying in {}s (attempt {attempt}/{max_retries})",
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
            }
            Err(err) => {
                // Transport / timeout errors — usually transient. Retry with backoff.
                attempt += 1;
                if attempt > max_retries {
                    return Err(err);
                }
                let delay = backoff(attempt);
                tracing::warn!(
                    "request errored ({err}), retrying in {}s (attempt {attempt}/{max_retries})",
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn retry_after_parsing() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

        // Absent header → no delay (caller falls back to backoff).
        assert_eq!(retry_after_duration(&HeaderMap::new()), None);

        // Delta-seconds form is honored.
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("5"));
        assert_eq!(
            retry_after_duration(&h),
            Some(std::time::Duration::from_secs(5))
        );

        // A hostile/huge value is capped.
        let mut h2 = HeaderMap::new();
        h2.insert(RETRY_AFTER, HeaderValue::from_static("99999"));
        assert_eq!(
            retry_after_duration(&h2),
            Some(std::time::Duration::from_secs(MAX_RETRY_AFTER_SECS))
        );

        // HTTP-date form isn't parsed → None (backoff applies instead).
        let mut h3 = HeaderMap::new();
        h3.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT"),
        );
        assert_eq!(retry_after_duration(&h3), None);
    }

    #[tokio::test(start_paused = true)]
    async fn transport_errors_are_retried_then_surfaced() {
        // A simulated timeout (transport error) must be retried, not propagated
        // on the first failure. start_paused auto-advances the backoff sleeps.
        let calls = AtomicU32::new(0);
        let result = retry_request(2, || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err::<reqwest::Response, _>(crate::error::AgentError::Llm("timeout".into())) }
        })
        .await;
        assert!(result.is_err());
        // Initial attempt + 2 retries = 3 calls.
        assert_eq!(calls.load(Ordering::SeqCst), 3);
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
