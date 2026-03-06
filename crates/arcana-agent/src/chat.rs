use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::debug;

use crate::agent::{AgentConfig, AgentEvent};
use crate::backend::LlmBackend;
use crate::error::Result;
use crate::executor::{PermissionedExecutor, ToolExecutor};
use crate::permissions::{chat_permissions, ApprovalResult};
use crate::tools::{SessionContext, VaultToolExecutor};
use crate::types::{ContentBlock, Message, StopReason, ToolDef, Usage};
use arcana_core::{BrainProfile, Vault};

/// Callback type for approval requests.
pub type ApprovalFn<'a> =
    &'a (dyn Fn(&str, &str, &serde_json::Value) -> ApprovalResult + Send + Sync);

/// Response from a chat turn.
pub struct ChatResponse {
    pub text: String,
    pub tools_used: Vec<String>,
    pub drafts_created: Vec<String>,
    pub usage: Usage,
}

/// A multi-turn chat session with tool use.
pub struct ChatSession {
    llm: Box<dyn LlmBackend>,
    vault: Arc<Mutex<Vault>>,
    messages: Vec<Message>,
    session_id: String,
    system_prompt: String,
    tools: Vec<ToolDef>,
    config: AgentConfig,
}

impl ChatSession {
    pub fn new(
        llm: Box<dyn LlmBackend>,
        vault: Arc<Mutex<Vault>>,
        session_id: String,
        config: AgentConfig,
        profile: &BrainProfile,
        user_prompts: &crate::prompts::UserPrompts,
    ) -> Self {
        let system_prompt =
            build_librarian_prompt(profile.taxonomy(), profile.style(), user_prompts);
        let tools = VaultToolExecutor::chat_tool_defs();

        Self {
            llm,
            vault,
            messages: Vec::new(),
            session_id,
            system_prompt,
            tools,
            config,
        }
    }

    /// Send a user message and get a response. Handles multi-turn tool use internally.
    pub async fn send(
        &mut self,
        user_message: &str,
        approval_fn: Option<ApprovalFn<'_>>,
        event_tx: Option<&tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    ) -> Result<ChatResponse> {
        self.messages.push(Message::user(user_message));

        let vault_executor = VaultToolExecutor::new(
            self.vault.clone(),
            SessionContext {
                session_id: self.session_id.clone(),
                task: user_message.chars().take(100).collect(),
                model: self.llm.model_name().to_string(),
                provider: self.llm.provider_name().to_string(),
            },
        );

        let mut permissioned =
            PermissionedExecutor::new(vault_executor, Box::new(chat_permissions));
        if let Some(af) = approval_fn {
            permissioned = permissioned
                .with_approval(Box::new(move |name, desc, input| af(name, desc, input)));
        }
        let executor: &dyn ToolExecutor = &permissioned;

        let mut total_usage = Usage::default();
        let mut tools_used = Vec::new();
        let mut drafts_created = Vec::new();
        let mut final_text = String::new();

        for iteration in 0..self.config.max_iterations {
            if let Some(tx) = event_tx {
                let _ = tx.send(AgentEvent::IterationStart { iteration });
            }

            let response = self
                .llm
                .chat(&self.system_prompt, &self.messages, &self.tools)
                .await?;
            total_usage.accumulate(&response.usage);

            let text = response.text();
            if !text.is_empty() {
                final_text = text.clone();
                if let Some(tx) = event_tx {
                    let _ = tx.send(AgentEvent::Text { text });
                }
            }

            // Check for end of turn
            if response.stop_reason == StopReason::EndTurn
                || response.stop_reason == StopReason::MaxTokens
            {
                self.messages.push(Message::assistant(response.content));
                break;
            }

            // Handle tool calls
            let tool_calls: Vec<_> = response
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                .cloned()
                .collect();

            if tool_calls.is_empty() {
                self.messages.push(Message::assistant(response.content));
                break;
            }

            self.messages
                .push(Message::assistant(response.content.clone()));

            // Execute tools via PermissionedExecutor
            let mut results = Vec::new();
            for call in &tool_calls {
                if let ContentBlock::ToolUse { id, name, input } = call {
                    if let Some(tx) = event_tx {
                        let _ = tx.send(AgentEvent::ToolStart {
                            name: name.clone(),
                            input: input.clone(),
                        });
                    }

                    let result = executor.execute(name, input).await;

                    match &result {
                        Ok(output) => {
                            debug!("tool {name} ok: {}...", &output[..output.len().min(100)]);

                            // Track drafts
                            if name == "vault_draft" || name == "vault_suggest_edit" {
                                if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                                    drafts_created.push(path.to_string());
                                }
                            }

                            results.push(ContentBlock::ToolResult {
                                tool_use_id: id.clone(),
                                content: output.clone(),
                                is_error: false,
                            });
                        }
                        Err(err) => {
                            results.push(ContentBlock::ToolResult {
                                tool_use_id: id.clone(),
                                content: err.clone(),
                                is_error: true,
                            });
                        }
                    }

                    tools_used.push(name.clone());

                    if let Some(tx) = event_tx {
                        let _ = tx.send(AgentEvent::ToolFinish { name: name.clone() });
                    }
                }
            }

            self.messages.push(Message::tool_results(results));

            // Token budget check
            let total_tokens = total_usage.total();
            if total_tokens >= self.config.max_tokens {
                if let Some(tx) = event_tx {
                    let _ = tx.send(AgentEvent::TokenBudgetExhausted {
                        used: total_tokens,
                        budget: self.config.max_tokens,
                    });
                }
                break;
            }
        }

        Ok(ChatResponse {
            text: final_text,
            tools_used,
            drafts_created,
            usage: total_usage,
        })
    }

    pub fn model_name(&self) -> &str {
        self.llm.model_name()
    }

    pub fn provider_name(&self) -> &str {
        self.llm.provider_name()
    }

    pub fn vault_ref(&self) -> Arc<Mutex<Vault>> {
        self.vault.clone()
    }
}

/// Default task prompt for chat, for use with `--show-prompt`.
pub fn default_task_prompt() -> (&'static str, &'static str) {
    ("chat.md", LIBRARIAN_TASK)
}

const LIBRARIAN_TASK: &str = r#"You are a librarian for this Obsidian knowledge vault.

## Conversational style
- Concise and direct. No filler, no preamble.
- No emojis. Use unicode symbols (→, —, ·) sparingly if needed.
- Respond like a knowledgeable colleague, not a chatbot.

## Capabilities
- Search, read, and list notes freely
- Draft new notes for user review (vault_draft)
- Suggest edits to existing notes (vault_suggest_edit)

## Rules
- Never write directly to the vault. Always use drafts.
- Follow the taxonomy for note placement and the style guide for formatting.
- Cross-link to existing notes with [[wikilinks]] when relevant.
- When the user dumps raw thoughts, apply the taxonomy routing rules.
- One idea per note. Split if needed.
- When the user asks about their vault's contents, search first before answering.

## Tips for the user
- Suggest `arcana context "<topic>"` when the user wants to export vault context for use in other tools or conversations"#;

fn build_librarian_prompt(
    taxonomy: Option<&str>,
    style: Option<&str>,
    user_prompts: &crate::prompts::UserPrompts,
) -> String {
    let task = user_prompts.chat.as_deref().unwrap_or(LIBRARIAN_TASK);
    crate::prompt::build_system_prompt(taxonomy, style, None, task, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::MockBackend;
    use crate::types::LlmResponse;

    #[tokio::test]
    async fn basic_chat_response() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("note.md"), "hello world").unwrap();
        let config = arcana_core::ArcanaConfig::default().with_vault_path(dir.path().to_path_buf());
        let vault = arcana_core::Vault::open(config).unwrap();
        vault.index().unwrap();

        let mock = MockBackend::single_text("I can help you with that!");

        let mut session = ChatSession::new(
            Box::new(mock),
            Arc::new(Mutex::new(vault)),
            "test-session".into(),
            AgentConfig::default(),
            &BrainProfile::default(),
            &crate::prompts::UserPrompts::default(),
        );

        let response = session.send("hello", None, None).await.unwrap();
        assert_eq!(response.text, "I can help you with that!");
        assert!(response.tools_used.is_empty());
    }

    #[tokio::test]
    async fn chat_with_tool_use() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rust.md"), "# Rust\nA systems language\n").unwrap();
        let config = arcana_core::ArcanaConfig::default().with_vault_path(dir.path().to_path_buf());
        let vault = arcana_core::Vault::open(config).unwrap();
        vault.index().unwrap();

        let mock = MockBackend::new(vec![
            LlmResponse {
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "vault_search".into(),
                    input: serde_json::json!({"query": "rust"}),
                }],
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                },
            },
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "I found a note about Rust!".into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 200,
                    output_tokens: 30,
                },
            },
        ]);

        let mut session = ChatSession::new(
            Box::new(mock),
            Arc::new(Mutex::new(vault)),
            "test-session".into(),
            AgentConfig::default(),
            &BrainProfile::default(),
            &crate::prompts::UserPrompts::default(),
        );

        let response = session
            .send("what notes do I have about rust?", None, None)
            .await
            .unwrap();
        assert_eq!(response.text, "I found a note about Rust!");
        assert!(response.tools_used.contains(&"vault_search".to_string()));
    }

    #[tokio::test]
    async fn multi_turn_conversation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("note.md"), "test").unwrap();
        let config = arcana_core::ArcanaConfig::default().with_vault_path(dir.path().to_path_buf());
        let vault = arcana_core::Vault::open(config).unwrap();
        vault.index().unwrap();

        let mock = MockBackend::new(vec![
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "First response.".into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                },
            },
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "Second response.".into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                },
            },
        ]);

        let mut session = ChatSession::new(
            Box::new(mock),
            Arc::new(Mutex::new(vault)),
            "test-session".into(),
            AgentConfig::default(),
            &BrainProfile::default(),
            &crate::prompts::UserPrompts::default(),
        );

        let r1 = session.send("hello", None, None).await.unwrap();
        assert_eq!(r1.text, "First response.");

        let r2 = session.send("follow up", None, None).await.unwrap();
        assert_eq!(r2.text, "Second response.");
    }
}
