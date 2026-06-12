use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{retry_request, LlmBackend};
use crate::error::{AgentError, Result};
use crate::types::{ContentBlock, LlmResponse, Message, Role, StopReason, ToolDef, Usage};

const MAX_RETRIES: u32 = 3;

pub struct OpenAiBackend {
    base_url: String,
    api_key: Option<String>,
    model: String,
    max_output_tokens: u32,
    provider: String,
    client: reqwest::Client,
}

impl OpenAiBackend {
    pub fn new(
        base_url: String,
        api_key: Option<String>,
        model: String,
        provider: String,
        max_output_tokens: u32,
    ) -> Self {
        Self {
            base_url,
            api_key,
            model,
            max_output_tokens,
            provider,
            client: reqwest::Client::new(),
        }
    }

    pub fn new_ollama(endpoint: Option<String>, model: String, max_output_tokens: u32) -> Self {
        Self::new(
            endpoint.unwrap_or_else(|| "http://localhost:11434/v1".into()),
            None,
            model,
            "ollama".into(),
            max_output_tokens,
        )
    }
}

// ---------------------------------------------------------------------------
// Wire types — OpenAI Chat Completions API
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ApiRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ApiTool>,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ApiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct ApiToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: ApiFunction,
}

#[derive(Serialize, Deserialize, Clone)]
struct ApiFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct ApiTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: ApiToolFunction,
}

#[derive(Serialize)]
struct ApiToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct ApiResponse {
    choices: Vec<ApiChoice>,
    usage: Option<ApiUsage>,
}

#[derive(Deserialize)]
struct ApiChoice {
    message: ApiResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ApiResponseMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ApiToolCall>,
}

#[derive(Deserialize)]
struct ApiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    // Automatic prompt caching reports the cached subset here; `prompt_tokens`
    // *includes* it (unlike Anthropic, where input_tokens excludes the cache).
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
}

#[derive(Deserialize)]
struct ApiError {
    error: ApiErrorDetail,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    message: String,
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

fn to_api_messages(system: &str, messages: &[Message]) -> Vec<ApiMessage> {
    let mut out = Vec::with_capacity(messages.len() + 1);

    // System prompt as first message
    if !system.is_empty() {
        out.push(ApiMessage {
            role: "system".into(),
            content: Some(system.into()),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    for msg in messages {
        match msg.role {
            Role::User => {
                // User messages: collect text and tool results
                let mut text_parts = Vec::new();
                let mut tool_results = Vec::new();

                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => text_parts.push(text.clone()),
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            tool_results.push((tool_use_id.clone(), content.clone()));
                        }
                        _ => {}
                    }
                }

                // Emit tool result messages first
                for (id, content) in tool_results {
                    out.push(ApiMessage {
                        role: "tool".into(),
                        content: Some(content),
                        tool_calls: None,
                        tool_call_id: Some(id),
                    });
                }

                // Emit user text if present
                if !text_parts.is_empty() {
                    out.push(ApiMessage {
                        role: "user".into(),
                        content: Some(text_parts.join("\n")),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
            }
            Role::Assistant => {
                let mut text = String::new();
                let mut tool_calls = Vec::new();

                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text: t } => text.push_str(t),
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(ApiToolCall {
                                id: id.clone(),
                                call_type: "function".into(),
                                function: ApiFunction {
                                    name: name.clone(),
                                    arguments: serde_json::to_string(input).unwrap_or_default(),
                                },
                            });
                        }
                        _ => {}
                    }
                }

                out.push(ApiMessage {
                    role: "assistant".into(),
                    content: if text.is_empty() { None } else { Some(text) },
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                    tool_call_id: None,
                });
            }
        }
    }

    out
}

fn from_api_response(resp: ApiResponse, provider: &str) -> Result<LlmResponse> {
    let choice = resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| AgentError::Llm("empty choices array".into()))?;

    let mut content = Vec::new();

    if let Some(text) = choice.message.content {
        if !text.is_empty() {
            content.push(ContentBlock::Text { text });
        }
    }

    for tc in choice.message.tool_calls {
        let input: serde_json::Value =
            serde_json::from_str(&tc.function.arguments).map_err(|e| {
                if provider == "ollama" {
                    tracing::warn!("model may not support tool use — try llama3.1 or newer: {e}");
                }
                AgentError::Llm(format!("failed to parse tool arguments: {e}"))
            })?;
        content.push(ContentBlock::ToolUse {
            id: tc.id,
            name: tc.function.name,
            input,
        });
    }

    let stop_reason = match choice.finish_reason.as_deref() {
        Some("stop") => StopReason::EndTurn,
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        _ => {
            // Infer from content: if we got tool calls, it's a tool_use stop
            if content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
            {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            }
        }
    };

    let usage = resp.usage.map_or(Usage::default(), |u| {
        // Normalize to the crate-wide invariant: input_tokens is the *uncached*
        // remainder, with the cached portion split out into cache_read_tokens.
        let cached = u
            .prompt_tokens_details
            .map_or(0, |d| d.cached_tokens)
            .min(u.prompt_tokens);
        Usage {
            input_tokens: u.prompt_tokens - cached,
            output_tokens: u.completion_tokens,
            // OpenAI auto-caches; there is no separate cache-write count.
            cache_creation_tokens: 0,
            cache_read_tokens: cached,
        }
    });

    Ok(LlmResponse {
        content,
        stop_reason,
        usage,
    })
}

// ---------------------------------------------------------------------------
// LlmBackend impl
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmBackend for OpenAiBackend {
    async fn chat(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<LlmResponse> {
        let api_messages = to_api_messages(system, messages);
        let api_tools: Vec<ApiTool> = tools
            .iter()
            .map(|t| ApiTool {
                tool_type: "function".into(),
                function: ApiToolFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                },
            })
            .collect();

        let body = ApiRequest {
            model: self.model.clone(),
            max_tokens: self.max_output_tokens,
            messages: api_messages,
            tools: api_tools,
        };

        let body_bytes = serde_json::to_vec(&body).map_err(|e| AgentError::Llm(e.to_string()))?;
        let url = format!("{}/chat/completions", self.base_url);

        let provider = self.provider.clone();
        let resp = retry_request(MAX_RETRIES, || {
            let mut req = self
                .client
                .post(&url)
                .header("content-type", "application/json")
                .body(body_bytes.clone());
            if let Some(key) = &self.api_key {
                req = req.header("authorization", format!("Bearer {key}"));
            }
            async { Ok(req.send().await?) }
        })
        .await?;

        let status = resp.status();
        let resp_text = resp.text().await?;

        if !status.is_success() {
            let msg = serde_json::from_str::<ApiError>(&resp_text)
                .map(|e| e.error.message)
                .unwrap_or(resp_text);
            return Err(AgentError::Llm(format!(
                "{} api {status}: {msg}",
                self.provider
            )));
        }

        let api_resp: ApiResponse = serde_json::from_str(&resp_text).map_err(|e| {
            if provider == "ollama" {
                tracing::warn!(
                    "model {} may not support tool use — try llama3.1 or newer",
                    self.model
                );
            }
            AgentError::Llm(format!("failed to parse response: {e}"))
        })?;

        from_api_response(api_resp, &provider)
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn provider_name(&self) -> &str {
        &self.provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_message_conversion() {
        let messages = vec![Message::user("hi")];
        let api_msgs = to_api_messages("you are helpful", &messages);
        assert_eq!(api_msgs.len(), 2);
        assert_eq!(api_msgs[0].role, "system");
        assert_eq!(api_msgs[0].content.as_deref(), Some("you are helpful"));
        assert_eq!(api_msgs[1].role, "user");
    }

    #[test]
    fn tool_call_roundtrip() {
        let msg = Message::assistant(vec![
            ContentBlock::Text {
                text: "Searching...".into(),
            },
            ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "vault_search".into(),
                input: serde_json::json!({"query": "rust"}),
            },
        ]);
        let result_msg = Message::tool_results(vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".into(),
            content: "found 3 notes".into(),
            is_error: false,
        }]);
        let api_msgs = to_api_messages("", &[msg, result_msg]);
        // No system (empty), assistant with tool_calls, tool result
        assert_eq!(api_msgs.len(), 2);
        assert_eq!(api_msgs[0].role, "assistant");
        assert!(api_msgs[0].tool_calls.is_some());
        assert_eq!(api_msgs[1].role, "tool");
    }

    #[test]
    fn response_parsing_end_turn() {
        let json = r#"{
            "choices": [{
                "message": {"content": "Hello!", "tool_calls": []},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        }"#;
        let api_resp: ApiResponse = serde_json::from_str(json).unwrap();
        let resp = from_api_response(api_resp, "openai").unwrap();
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        assert_eq!(resp.text(), "Hello!");
    }

    #[test]
    fn response_parsing_tool_use() {
        let json = r#"{
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "vault_search", "arguments": "{\"query\":\"test\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 20, "completion_tokens": 10}
        }"#;
        let api_resp: ApiResponse = serde_json::from_str(json).unwrap();
        let resp = from_api_response(api_resp, "openai").unwrap();
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
        assert_eq!(resp.tool_calls().len(), 1);
    }
}
