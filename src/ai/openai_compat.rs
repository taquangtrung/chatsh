use anyhow::anyhow;
use async_stream::try_stream;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::types::{ChatEvent, ChatMessage, ChatRequest, ChatRole};

pub struct OpenAiCompatChat<'a> {
    pub provider_id: &'a str,
    pub endpoint: Url,
    pub bearer_token: String,
    pub extra_headers: HeaderMap,
    pub client: Client,
}

impl OpenAiCompatChat<'_> {
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

        let provider_id = self.provider_id.to_string();
        let endpoint = self.endpoint;
        let client = self.client;

        let stream = try_stream! {
            let resp = client
                .post(endpoint)
                .headers(headers)
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
                            match evt {
                                Parsed::Token(text) => yield ChatEvent::Token { text },
                                Parsed::Done => yield ChatEvent::Done,
                            }
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

enum Parsed {
    Token(String),
    Done,
}

fn find_event_end(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2)
}

fn parse_sse_event(raw: &[u8]) -> Option<Parsed> {
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
        return Some(Parsed::Done);
    }
    let parsed: OpenAiStreamChunk = serde_json::from_str(&data).ok()?;
    let choice = parsed.choices.into_iter().next()?;
    let text = choice.delta.content?;
    Some(Parsed::Token(text))
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

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_event_end() {
        assert_eq!(find_event_end(b"data:{}\n\n"), Some(9));
        assert_eq!(find_event_end(b"nope"), None);
    }

    #[test]
    fn test_parse_sse_event_token() {
        let raw = b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n";
        let parsed = parse_sse_event(raw).unwrap();
        match parsed {
            Parsed::Token(text) => assert_eq!(text, "hello"),
            Parsed::Done => panic!("expected token"),
        }
    }

    #[test]
    fn test_parse_sse_event_done() {
        let raw = b"data: [DONE]\n\n";
        let parsed = parse_sse_event(raw).unwrap();
        assert!(matches!(parsed, Parsed::Done));
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
