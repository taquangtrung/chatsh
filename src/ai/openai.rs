use async_trait::async_trait;
use futures::stream::BoxStream;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::ai::AuthStrategy;
use crate::ai::LlmProvider;
use crate::ai::stream::sse_chat_stream;
use crate::ai::{ChatEvent, ChatMessage, ChatRequest, ChatRole, ModelInfo, PROVIDER_OPENAI};

pub struct OpenAiProvider {
    base_url: Url,
    client: Client,
    auth: AuthStrategy,
}

impl OpenAiProvider {
    pub fn new(base_url: Url, auth: AuthStrategy) -> Self {
        Self {
            base_url,
            client: Client::new(),
            auth,
        }
    }

    fn api_key(&self) -> anyhow::Result<String> {
        self.auth.require_api_key("OpenAI")
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn id(&self) -> &str {
        PROVIDER_OPENAI
    }

    fn display_name(&self) -> &str {
        "OpenAI"
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        Ok(vec![
            ModelInfo {
                id: "gpt-4.1".into(),
                display_name: "GPT-4.1".into(),
                context_tokens: 1_000_000,
                rate_label: None,
            },
            ModelInfo {
                id: "gpt-4o".into(),
                display_name: "GPT-4o".into(),
                context_tokens: 128_000,
                rate_label: None,
            },
        ])
    }

    async fn chat(&self, req: ChatRequest) -> anyhow::Result<BoxStream<'static, ChatEvent>> {
        let key = self.api_key()?;
        let endpoint = self.base_url.join("v1/chat/completions")?;
        OpenAiStream {
            provider_id: "openai",
            endpoint,
            bearer_token: key,
            extra_headers: reqwest::header::HeaderMap::new(),
            client: self.client.clone(),
        }
        .stream(req)
        .await
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sse_event_token() {
        let raw = b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n";
        let parsed = parse_sse_event(raw).unwrap();
        assert_eq!(parsed, ChatEvent::Token { text: "hello".into() });
    }

    #[test]
    fn test_parse_sse_event_done() {
        let raw = b"data: [DONE]\n\n";
        let parsed = parse_sse_event(raw).unwrap();
        assert_eq!(parsed, ChatEvent::Done);
    }

    #[test]
    fn test_openai_request_from_chat_request() {
        let req = ChatRequest {
            provider_id: "openai".into(),
            model: "gpt-4o".into(),
            messages: vec![ChatMessage::user("hi")],
        };
        let body = OpenAiRequest::from(&req);
        assert_eq!(body.model, "gpt-4o");
        assert!(body.stream);
        assert_eq!(body.messages.len(), 1);
    }
}

pub struct OpenAiStream<'a> {
    pub provider_id: &'a str,
    pub endpoint: Url,
    pub bearer_token: String,
    pub extra_headers: HeaderMap,
    pub client: Client,
}

impl OpenAiStream<'_> {
    pub async fn stream(self, req: ChatRequest) -> anyhow::Result<BoxStream<'static, ChatEvent>> {
        let body = OpenAiRequest::from(&req);
        let mut headers = self.extra_headers;
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_str(&format!("Bearer {}", self.bearer_token))?,
        );
        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );

        let request = self.client.post(self.endpoint).headers(headers).json(&body);
        Ok(sse_chat_stream(
            self.provider_id.to_string(),
            request,
            parse_sse_event,
        ))
    }
}

fn parse_sse_event(raw: &[u8]) -> Option<ChatEvent> {
    let text = std::str::from_utf8(raw).ok()?;
    let mut data = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
    }
    if data.is_empty() {
        return None;
    }
    if data == "[DONE]" {
        return Some(ChatEvent::Done);
    }
    let parsed: OpenAiStreamChunk = serde_json::from_str(&data).ok()?;
    let choice = parsed.choices.into_iter().next()?;
    let text = choice.delta.content?;
    Some(ChatEvent::Token { text })
}

#[derive(Serialize)]
struct OpenAiRequest {
    model: String,
    stream: bool,
    messages: Vec<OpenAiMessage>,
}

impl From<&ChatRequest> for OpenAiRequest {
    fn from(req: &ChatRequest) -> Self {
        Self {
            model: req.model.clone(),
            stream: true,
            messages: req.messages.iter().map(OpenAiMessage::from).collect(),
        }
    }
}

#[derive(Serialize)]
struct OpenAiMessage {
    role: String,
    content: String,
}

impl From<&ChatMessage> for OpenAiMessage {
    fn from(m: &ChatMessage) -> Self {
        let role = match m.role {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
        };
        Self {
            role: role.to_string(),
            content: m.content.clone(),
        }
    }
}

#[derive(Deserialize)]
struct OpenAiStreamChunk {
    choices: Vec<OpenAiStreamChoice>,
}

#[derive(Deserialize)]
struct OpenAiStreamChoice {
    delta: OpenAiDelta,
}

#[derive(Deserialize)]
struct OpenAiDelta {
    #[serde(default)]
    content: Option<String>,
}
