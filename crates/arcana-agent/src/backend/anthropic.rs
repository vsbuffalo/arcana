use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{retry_request, LlmBackend};
use crate::error::{AgentError, Result};
use crate::types::{ContentBlock, LlmResponse, Message, Role, StopReason, ToolDef, Usage};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const MAX_RETRIES: u32 = 3;

pub struct AnthropicBackend {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl AnthropicBackend {
    pub fn new(api_key: String, model: Option<String>) -> Self {
        Self {
            api_key,
            model: model.unwrap_or_else(|| "claude-sonnet-4-5-20250929".into()),
            client: reqwest::Client::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire types — closely mirror Anthropic's Messages API
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ApiRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ApiTool>,
}

#[derive(Serialize)]
struct ApiMessage {
    role: &'static str,
    content: Vec<ApiContent>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ApiContent {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

#[derive(Serialize)]
struct ApiTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ApiResponseContent>,
    stop_reason: String,
    usage: ApiUsage,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ApiResponseContent {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Deserialize)]
struct ApiUsage {
    input_tokens: u64,
    output_tokens: u64,
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

fn to_api_message(msg: &Message) -> ApiMessage {
    let role = match msg.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let content = msg
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => ApiContent::Text { text: text.clone() },
            ContentBlock::ToolUse { id, name, input } => ApiContent::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => ApiContent::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content.clone(),
                is_error: *is_error,
            },
        })
        .collect();
    ApiMessage { role, content }
}

fn from_api_response(resp: ApiResponse) -> LlmResponse {
    let content = resp
        .content
        .into_iter()
        .map(|c| match c {
            ApiResponseContent::Text { text } => ContentBlock::Text { text },
            ApiResponseContent::ToolUse { id, name, input } => {
                ContentBlock::ToolUse { id, name, input }
            }
        })
        .collect();

    let stop_reason = match resp.stop_reason.as_str() {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    };

    LlmResponse {
        content,
        stop_reason,
        usage: Usage {
            input_tokens: resp.usage.input_tokens,
            output_tokens: resp.usage.output_tokens,
        },
    }
}

// ---------------------------------------------------------------------------
// LlmBackend impl
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmBackend for AnthropicBackend {
    async fn chat(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<LlmResponse> {
        let api_messages: Vec<ApiMessage> = messages.iter().map(to_api_message).collect();
        let api_tools: Vec<ApiTool> = tools
            .iter()
            .map(|t| ApiTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            })
            .collect();

        let body = ApiRequest {
            model: &self.model,
            max_tokens: 8192,
            system,
            messages: api_messages,
            tools: api_tools,
        };

        let body_bytes = serde_json::to_vec(&body).map_err(|e| AgentError::Llm(e.to_string()))?;

        let resp = retry_request(MAX_RETRIES, || {
            let req = self
                .client
                .post(API_URL)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .header("content-type", "application/json")
                .body(body_bytes.clone());
            async { Ok(req.send().await?) }
        })
        .await?;

        let status = resp.status();
        let resp_text = resp.text().await?;

        if !status.is_success() {
            let msg = serde_json::from_str::<ApiError>(&resp_text)
                .map(|e| e.error.message)
                .unwrap_or(resp_text);
            return Err(AgentError::Llm(format!("anthropic api {status}: {msg}")));
        }

        let api_resp: ApiResponse = serde_json::from_str(&resp_text)?;
        Ok(from_api_response(api_resp))
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn provider_name(&self) -> &str {
        "anthropic"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_conversion_roundtrip() {
        let msg = Message::user("hello world");
        let api_msg = to_api_message(&msg);
        assert_eq!(api_msg.role, "user");
        assert_eq!(api_msg.content.len(), 1);
    }

    #[test]
    fn tool_use_message_conversion() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "t1".into(),
                name: "vault_search".into(),
                input: serde_json::json!({"query": "rust"}),
            }],
        };
        let api_msg = to_api_message(&msg);
        assert_eq!(api_msg.role, "assistant");
        let json = serde_json::to_value(&api_msg.content[0]).unwrap();
        assert_eq!(json["type"], "tool_use");
        assert_eq!(json["name"], "vault_search");
    }

    #[test]
    fn response_parsing() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "Here are the results"},
                {"type": "tool_use", "id": "t1", "name": "search", "input": {"q": "test"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 100, "output_tokens": 50}
        }"#;
        let api_resp: ApiResponse = serde_json::from_str(json).unwrap();
        let resp = from_api_response(api_resp);
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
        assert_eq!(resp.content.len(), 2);
        assert_eq!(resp.usage.input_tokens, 100);
    }
}
