use async_trait::async_trait;
use futures::stream::BoxStream;
use reqwest::Client;
use secrecy::ExposeSecret;
use url::Url;

use crate::ai::auth::AuthStrategy;
use crate::ai::openai_compat::OpenAiCompatChat;
use crate::ai::provider::LlmProvider;
use crate::types::{ChatEvent, ChatRequest, ModelInfo, QuotaSnapshot};

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
        match &self.auth {
            AuthStrategy::ApiKey(k) => Ok(k.expose_secret().to_string()),
            _ => Err(anyhow::anyhow!("OpenAI requires an API key")),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn id(&self) -> &str {
        "openai"
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
            },
            ModelInfo {
                id: "gpt-4o".into(),
                display_name: "GPT-4o".into(),
                context_tokens: 128_000,
            },
        ])
    }

    async fn quota(&self) -> anyhow::Result<QuotaSnapshot> {
        Ok(QuotaSnapshot::PerToken { spent_cents: 0 })
    }

    async fn chat(&self, req: ChatRequest) -> anyhow::Result<BoxStream<'static, ChatEvent>> {
        let key = self.api_key()?;
        let endpoint = self.base_url.join("v1/chat/completions")?;
        OpenAiCompatChat {
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
    use secrecy::SecretString;

    #[test]
    fn test_openai_provider_id() {
        let provider = OpenAiProvider::new(
            Url::parse("https://api.openai.com/").unwrap(),
            AuthStrategy::ApiKey(SecretString::new("test".into())),
        );
        assert_eq!(provider.id(), "openai");
    }
}
