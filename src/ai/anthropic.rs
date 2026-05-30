use async_trait::async_trait;
use futures::stream::BoxStream;
use reqwest::Client;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::ai::AuthStrategy;
use crate::ai::LlmProvider;
use crate::ai::stream::sse_chat_stream;
use crate::ai::{ChatEvent, ChatMessage, ChatRequest, ChatRole, ModelInfo};

pub struct AnthropicProvider {
    id: String,
    display_name: String,
    base_url: Url,
    client: Client,
    auth: AuthStrategy,
    default_models: Vec<ModelInfo>,
}

impl AnthropicProvider {
    pub fn new(id: impl Into<String>, base_url: Url, auth: AuthStrategy) -> Self {
        Self::with_options(id, "Anthropic", base_url, auth, default_models())
    }

    pub fn with_options(
        id: impl Into<String>,
        display_name: impl Into<String>,
        base_url: Url,
        auth: AuthStrategy,
        models: Vec<ModelInfo>,
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            base_url,
            client: Client::new(),
            auth,
            default_models: models,
        }
    }

    fn api_key(&self) -> anyhow::Result<String> {
        self.auth.require_api_key(&self.display_name)
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let key = match &self.auth {
            AuthStrategy::ApiKey(k) => k.expose_secret().to_string(),
            _ => return Ok(self.default_models.clone()),
        };
        let url = match self.base_url.join("v1/models") {
            Ok(u) => u,
            Err(_) => return Ok(self.default_models.clone()),
        };
        let resp = match self
            .client
            .get(url)
            .header("x-api-key", &key)
            .header("authorization", format!("Bearer {key}"))
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => return Ok(self.default_models.clone()),
        };
        if !resp.status().is_success() {
            return Ok(self.default_models.clone());
        }
        let body: ListModelsResponse = match resp.json().await {
            Ok(b) => b,
            Err(_) => return Ok(self.default_models.clone()),
        };
        if body.data.is_empty() {
            return Ok(self.default_models.clone());
        }
        let by_id: std::collections::HashMap<&str, &ModelInfo> = self
            .default_models
            .iter()
            .map(|m| (m.id.as_str(), m))
            .collect();
        Ok(body
            .data
            .into_iter()
            .map(|m| {
                let known = by_id.get(m.id.as_str());
                ModelInfo {
                    display_name: m
                        .display_name
                        .filter(|s| !s.is_empty())
                        .or_else(|| known.map(|k| k.display_name.clone()))
                        .unwrap_or_else(|| m.id.clone()),
                    context_tokens: known.map(|k| k.context_tokens).unwrap_or(128_000),
                    rate_label: known.and_then(|k| k.rate_label.clone()),
                    id: m.id,
                }
            })
            .collect())
    }

    async fn chat(&self, req: ChatRequest) -> anyhow::Result<BoxStream<'static, ChatEvent>> {
        let key = self.api_key()?;
        let url = self.base_url.join("v1/messages")?;
        let body = AnthropicRequest::from(&req);
        let request = self
            .client
            .post(url)
            .header("x-api-key", &key)
            .header("authorization", format!("Bearer {key}"))
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body);
        Ok(sse_chat_stream(self.id.clone(), request, parse_sse_event))
    }
}

fn parse_sse_event(raw: &[u8]) -> Option<ChatEvent> {
    let text = std::str::from_utf8(raw).ok()?;
    let mut data = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data.push_str(rest.trim_start());
        }
    }
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    let parsed: AnthropicStreamEvent = serde_json::from_str(&data).ok()?;
    match parsed {
        AnthropicStreamEvent::ContentBlockDelta { delta } => match delta {
            AnthropicDelta::TextDelta { text } => Some(ChatEvent::Token { text }),
            AnthropicDelta::ThinkingDelta { thinking } => {
                Some(ChatEvent::Thinking { text: thinking })
            }
            AnthropicDelta::Other => None,
        },
        AnthropicStreamEvent::MessageStop => Some(ChatEvent::Done),
        AnthropicStreamEvent::Other => None,
    }
}

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    system: String,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<AnthropicThinking>,
}

const THINKING_BUDGET_TOKENS: u32 = 4096;
const MAX_TOKENS_WITH_THINKING: u32 = 8192;
const MAX_TOKENS_DEFAULT: u32 = 4096;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicThinking {
    Enabled { budget_tokens: u32 },
}

fn supports_thinking(model: &str) -> bool {
    let m = model.to_lowercase();
    m.starts_with("claude-opus-4")
        || m.starts_with("claude-sonnet-4")
        || m.starts_with("claude-haiku-4")
}

impl From<&ChatRequest> for AnthropicRequest {
    fn from(req: &ChatRequest) -> Self {
        let system = req
            .messages
            .iter()
            .filter(|m| m.role == ChatRole::System)
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        let thinking = if supports_thinking(&req.model) {
            Some(AnthropicThinking::Enabled {
                budget_tokens: THINKING_BUDGET_TOKENS,
            })
        } else {
            None
        };
        Self {
            model: req.model.clone(),
            max_tokens: if thinking.is_some() {
                MAX_TOKENS_WITH_THINKING
            } else {
                MAX_TOKENS_DEFAULT
            },
            stream: true,
            system,
            messages: req
                .messages
                .iter()
                .filter(|m| m.role != ChatRole::System)
                .map(AnthropicMessage::from)
                .collect(),
            thinking,
        }
    }
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

impl From<&ChatMessage> for AnthropicMessage {
    fn from(m: &ChatMessage) -> Self {
        let role = match m.role {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::System => "user",
        };
        Self {
            role: role.to_string(),
            content: m.content.clone(),
        }
    }
}

#[derive(Deserialize)]
struct ListModelsResponse {
    #[serde(default)]
    data: Vec<ListModelsItem>,
}

#[derive(Deserialize)]
struct ListModelsItem {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum AnthropicStreamEvent {
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { delta: AnthropicDelta },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum AnthropicDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(other)]
    Other,
}

fn default_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "claude-sonnet-4-20250514".into(),
            display_name: "Claude Sonnet 4".into(),
            context_tokens: 200_000,
            rate_label: None,
        },
        ModelInfo {
            id: "claude-haiku-4-20250414".into(),
            display_name: "Claude Haiku 4".into(),
            context_tokens: 200_000,
            rate_label: None,
        },
    ]
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supports_thinking() {
        assert!(supports_thinking("claude-sonnet-4-20250514"));
        assert!(supports_thinking("claude-opus-4-7"));
        assert!(!supports_thinking("glm-4.6"));
        assert!(!supports_thinking("gpt-4o"));
    }

    #[test]
    fn test_anthropic_request_from_chat_request() {
        let req = ChatRequest {
            provider_id: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            messages: vec![
                ChatMessage::system("be helpful"),
                ChatMessage::user("hello"),
            ],
        };
        let body = AnthropicRequest::from(&req);
        assert_eq!(body.model, "claude-sonnet-4-20250514");
        assert_eq!(body.system, "be helpful");
        assert_eq!(body.messages.len(), 1);
        assert!(body.thinking.is_some());
        assert_eq!(body.max_tokens, MAX_TOKENS_WITH_THINKING);
    }

    #[test]
    fn test_parse_sse_event_text_delta() {
        let raw = b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n";
        let event = parse_sse_event(raw).unwrap();
        assert_eq!(event, ChatEvent::Token { text: "hi".into() });
    }

    #[test]
    fn test_parse_sse_event_done() {
        let raw = b"data: {\"type\":\"message_stop\"}\n\n";
        let event = parse_sse_event(raw).unwrap();
        assert_eq!(event, ChatEvent::Done);
    }
}
