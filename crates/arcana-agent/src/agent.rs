use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::backend::LlmBackend;
use crate::error::{AgentError, Result};
use crate::executor::ToolExecutor;
use crate::types::{ContentBlock, Message, StopReason, Usage};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

pub struct AgentConfig {
    pub max_iterations: usize,
    pub max_tokens: u64,
    /// Message injected into the conversation when token usage hits 75%.
    /// Tells the AI to wrap up. If None, no injection (just the log warning).
    pub wrap_up_message: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 20,
            max_tokens: 100_000,
            wrap_up_message: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum AgentEvent {
    IterationStart {
        iteration: usize,
    },
    ToolStart {
        name: String,
        input: serde_json::Value,
    },
    ToolFinish {
        name: String,
    },
    Text {
        text: String,
    },
    TokenWarning {
        used: u64,
        budget: u64,
    },
    Done {
        usage: Usage,
    },
    MaxIterationsReached,
    TokenBudgetExhausted {
        used: u64,
        budget: u64,
    },
}

// ---------------------------------------------------------------------------
// Agent loop
// ---------------------------------------------------------------------------

pub async fn agent_loop(
    llm: &dyn LlmBackend,
    system_prompt: &str,
    messages: &mut Vec<Message>,
    executor: &dyn ToolExecutor,
    config: &AgentConfig,
    event_tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
) -> Result<(String, Usage)> {
    let tools = executor.tool_defs();
    let mut total_usage = Usage::default();
    let mut warned_50pct = false;

    for iteration in 0..config.max_iterations {
        send_event(event_tx, AgentEvent::IterationStart { iteration });

        let response = llm.chat(system_prompt, messages, &tools).await?;
        total_usage.accumulate(&response.usage);

        // Log any text output
        let text = response.text();
        if !text.is_empty() {
            debug!("llm: {}", text.chars().take(200).collect::<String>());
            send_event(event_tx, AgentEvent::Text { text: text.clone() });
        }

        // Token budget check
        let total_tokens = total_usage.total();
        if total_tokens >= config.max_tokens {
            send_event(
                event_tx,
                AgentEvent::TokenBudgetExhausted {
                    used: total_tokens,
                    budget: config.max_tokens,
                },
            );
            warn!(
                "token budget exhausted: {total_tokens}/{} tokens",
                config.max_tokens
            );
            return Ok((response.text(), total_usage));
        }
        if !warned_50pct && total_tokens >= config.max_tokens * 3 / 4 {
            warned_50pct = true;
            send_event(
                event_tx,
                AgentEvent::TokenWarning {
                    used: total_tokens,
                    budget: config.max_tokens,
                },
            );
            info!(
                "token usage at 75%: {total_tokens}/{} tokens, injecting wrap-up",
                config.max_tokens
            );
            if let Some(ref msg) = config.wrap_up_message {
                messages.push(Message::user(msg.clone()));
            }
        }

        // Check stop reason
        if response.stop_reason == StopReason::EndTurn
            || response.stop_reason == StopReason::MaxTokens
        {
            info!(
                "agent finished after {} iterations ({} tokens)",
                iteration + 1,
                total_tokens
            );
            send_event(
                event_tx,
                AgentEvent::Done {
                    usage: total_usage.clone(),
                },
            );
            return Ok((response.text(), total_usage));
        }

        // Handle tool calls
        let tool_calls: Vec<_> = response
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
            .cloned()
            .collect();

        if tool_calls.is_empty() {
            // No tool calls and not EndTurn — treat as done
            send_event(
                event_tx,
                AgentEvent::Done {
                    usage: total_usage.clone(),
                },
            );
            return Ok((response.text(), total_usage));
        }

        // Append assistant message with all content
        messages.push(Message::assistant(response.content));

        // Execute each tool and collect results
        let mut results = Vec::new();
        for call in &tool_calls {
            if let ContentBlock::ToolUse { id, name, input } = call {
                send_event(
                    event_tx,
                    AgentEvent::ToolStart {
                        name: name.clone(),
                        input: input.clone(),
                    },
                );

                let result = executor.execute(name, input).await;

                send_event(event_tx, AgentEvent::ToolFinish { name: name.clone() });

                match result {
                    Ok(output) => {
                        debug!("tool {name} ok: {}...", &output[..output.len().min(100)]);
                        results.push(ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: output,
                            is_error: false,
                        });
                    }
                    Err(err) => {
                        warn!("tool {name} error: {err}");
                        results.push(ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: err,
                            is_error: true,
                        });
                    }
                }
            }
        }

        messages.push(Message::tool_results(results));
    }

    // Exhausted max iterations
    send_event(event_tx, AgentEvent::MaxIterationsReached);
    warn!(
        "agent hit max iterations ({}), stopping",
        config.max_iterations
    );
    Err(AgentError::Llm(format!(
        "agent exceeded max iterations ({})",
        config.max_iterations
    )))
}

fn send_event(tx: Option<&mpsc::UnboundedSender<AgentEvent>>, event: AgentEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::MockBackend;
    use crate::tools::{SessionContext, VaultToolExecutor};
    use crate::types::{LlmResponse, Usage};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn test_executor(dir: &std::path::Path) -> VaultToolExecutor {
        let config = arcana_core::ArcanaConfig::default().with_vault_path(dir.to_path_buf());
        let vault = arcana_core::Vault::open(config).unwrap();
        vault.index().unwrap();
        VaultToolExecutor::new(
            Arc::new(Mutex::new(vault)),
            SessionContext {
                session_id: "test".into(),
                task: "test".into(),
                model: "mock".into(),
                provider: "mock".into(),
            },
        )
    }

    #[tokio::test]
    async fn simple_end_turn() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("note.md"), "hello").unwrap();
        let executor = test_executor(dir.path());
        let mock = MockBackend::single_text("I found the answer.");

        let mut messages = vec![Message::user("search for rust")];
        let (text, usage) = agent_loop(
            &mock,
            "you are helpful",
            &mut messages,
            &executor,
            &AgentConfig::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(text, "I found the answer.");
        assert!(usage.total() > 0);
    }

    #[tokio::test]
    async fn tool_use_then_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("note.md"), "hello world").unwrap();
        let executor = test_executor(dir.path());

        let mock = MockBackend::new(vec![
            // First response: call vault_search
            LlmResponse {
                content: vec![
                    ContentBlock::Text {
                        text: "Let me search...".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "vault_search".into(),
                        input: serde_json::json!({"query": "hello"}),
                    },
                ],
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                },
            },
            // Second response: end turn
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "Found it!".into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 200,
                    output_tokens: 30,
                },
            },
        ]);

        let (rx_tx, mut rx) = mpsc::unbounded_channel();
        let mut messages = vec![Message::user("search for hello")];
        let (text, usage) = agent_loop(
            &mock,
            "you are helpful",
            &mut messages,
            &executor,
            &AgentConfig::default(),
            Some(&rx_tx),
        )
        .await
        .unwrap();

        assert_eq!(text, "Found it!");
        assert_eq!(usage.input_tokens, 300);
        assert_eq!(usage.output_tokens, 80);

        // Verify events were sent
        let mut events = Vec::new();
        rx.close();
        while let Some(e) = rx.recv().await {
            events.push(e);
        }
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolStart { name, .. } if name == "vault_search")));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolFinish { name } if name == "vault_search")));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Done { .. })));
    }

    #[tokio::test]
    async fn token_budget_exhaustion() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("note.md"), "hello").unwrap();
        let executor = test_executor(dir.path());

        // Response uses more tokens than budget
        let mock = MockBackend::new(vec![LlmResponse {
            content: vec![
                ContentBlock::Text {
                    text: "searching...".into(),
                },
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "vault_search".into(),
                    input: serde_json::json!({"query": "test"}),
                },
            ],
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input_tokens: 80,
                output_tokens: 30,
            },
        }]);

        let config = AgentConfig {
            max_iterations: 20,
            max_tokens: 100, // Very small budget
            wrap_up_message: None,
        };

        let mut messages = vec![Message::user("search")];
        let (text, usage) = agent_loop(&mock, "", &mut messages, &executor, &config, None)
            .await
            .unwrap();

        // Should have stopped due to budget
        assert!(usage.total() >= 100);
        assert_eq!(text, "searching...");
    }
}
