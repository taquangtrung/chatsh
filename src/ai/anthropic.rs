use anyhow::anyhow;
use async_stream::try_stream;
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::ai::auth::AuthStrategy;
use crate::ai::provider::LlmProvider;
use crate::types::{ChatEvent, ChatMessage, ChatRequest, ChatRole, ModelInfo, QuotaSnapshot};

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
        match &self.auth {
            AuthStrategy::ApiKey(k) => Ok(k.expose_secret().to_string()),
            _ => Err(anyhow!("{} requires an API key", self.display_name)),
        }
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
        Ok(body
            .data
            .into_iter()
            .map(|m| {
                let display_name = m
                    .display_name
                    .clone()
                    .unwrap_or_else(|| m.id.clone());
                ModelInfo {
                    id: m.id,
                    display_name,
                    context_tokens: 128_000,
                }
            })
            .collect())
    }

    async fn quota(&self) -> anyhow::Result<QuotaSnapshot> {
        Ok(QuotaSnapshot::PerToken { spent_cents: 0 })
    }

    async fn chat(&self, req: ChatRequest) -> anyhow::Result<BoxStream<'static, ChatEvent>> {
        let key = self.api_key()?;
        let url = self.base_url.join("v1/messages")?;
        let body = AnthropicRequest::from(&req);
        let client = self.client.clone();
        let provider_id = self.id.clone();

        let stream = try_stream! {
            let resp = client
                .post(url)
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| anyhow!("{provider_id}: {e}"))?;

            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                Err(anyhow!("{provider_id}: {status}: {text}"))?;
            } else {
                let mut bytes = resp.bytes_stream();
                let mut buf = Vec::new();
                while let Some(chunk) = bytes.next().await {
                    let chunk = chunk.map_err(|e| anyhow!("{provider_id}: {e}"))?;
                    buf.extend_from_slice(&chunk);
                    while let Some(pos) = find_event_end(&buf) {
                        let event_bytes = buf.drain(..pos).collect::<Vec<u8>>();
                        if let Some(evt) = parse_sse_event(&event_bytes) {
                            yield evt;
                        }
                    }
                }
                yield ChatEvent::Done;
            }
        };

        Ok(Box::pin(stream.map(|res: anyhow::Result<ChatEvent>| match res {
            Ok(ev) => ev,
            Err(e) => ChatEvent::Error {
                message: e.to_string(),
            },
        })))
    }
}

fn find_event_end(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2)
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
            AnthropicDelta::ThinkingDelta { thinking } => Some(ChatEvent::Thinking {
                text: thinking,
            }),
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
        || m.starts_with("glm-4.5")
        || m.starts_with("glm-4.6")
        || m.starts_with("glm-5")
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
        },
        ModelInfo {
            id: "claude-haiku-4-20250414".into(),
            display_name: "Claude Haiku 4".into(),
            context_tokens: 200_000,
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
        assert!(supports_thinking("glm-4.6"));
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
    fn test_find_event_end() {
        assert_eq!(find_event_end(b"data:{}\n\n"), Some(9));
        assert_eq!(find_event_end(b"hello"), None);
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
