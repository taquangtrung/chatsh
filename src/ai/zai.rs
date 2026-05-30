use async_trait::async_trait;
use futures::stream::BoxStream;
use reqwest::Client;
use reqwest::header::HeaderMap;
use serde::Deserialize;
use url::Url;

use crate::ai::AuthStrategy;
use crate::ai::LlmProvider;
use crate::ai::OpenAiStream;
use crate::ai::{ChatEvent, ChatRequest, ModelInfo, PROVIDER_ZAI};

// Z.ai OpenAI-compatible endpoint (official; see https://docs.z.ai/).
const BASE_URL: &str = "https://api.z.ai/api/paas/v4/";

pub struct ZaiProvider {
    client: Client,
    auth: AuthStrategy,
}

impl ZaiProvider {
    pub fn build(auth: AuthStrategy) -> Self {
        Self {
            client: Client::new(),
            auth,
        }
    }

    fn bearer_token(&self) -> anyhow::Result<String> {
        self.auth.require_api_key("Z.ai")
    }
}

#[async_trait]
impl LlmProvider for ZaiProvider {
    fn id(&self) -> &str {
        PROVIDER_ZAI
    }

    fn display_name(&self) -> &str {
        "Z.ai Coding Plan"
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let token = match self.bearer_token() {
            Ok(t) => t,
            Err(_) => return Ok(default_models()),
        };
        let url = Url::parse(BASE_URL)
            .expect("valid url")
            .join("models")
            .expect("valid url");
        let resp = match self
            .client
            .get(url)
            .header("authorization", format!("Bearer {token}"))
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => return Ok(default_models()),
        };
        if !resp.status().is_success() {
            return Ok(default_models());
        }
        let body: ModelsApiResponse = match resp.json().await {
            Ok(b) => b,
            Err(_) => return Ok(default_models()),
        };
        if body.data.is_empty() {
            return Ok(default_models());
        }
        Ok(body
            .data
            .into_iter()
            .map(|m| ModelInfo {
                display_name: m.id.clone(),
                id: m.id,
                context_tokens: 128_000,
                rate_label: None,
            })
            .collect())
    }

    async fn chat(&self, req: ChatRequest) -> anyhow::Result<BoxStream<'static, ChatEvent>> {
        let token = self.bearer_token()?;
        let endpoint = Url::parse(BASE_URL)
            .expect("valid url")
            .join("chat/completions")
            .expect("valid url");
        OpenAiStream {
            provider_id: PROVIDER_ZAI,
            endpoint,
            bearer_token: token,
            extra_headers: HeaderMap::new(),
            client: self.client.clone(),
        }
        .stream(req)
        .await
    }
}

impl ZaiProvider {
    /// Verify the API key by calling the models endpoint.
    /// Returns `Ok(())` on success, `Err` with a status message on failure.
    pub async fn validate_key(&self) -> anyhow::Result<()> {
        let token = self.bearer_token()?;
        let url = Url::parse(BASE_URL)
            .expect("valid url")
            .join("models")
            .expect("valid url");
        let resp = self
            .client
            .get(url)
            .header("authorization", format!("Bearer {token}"))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("connection failed: {e}"))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(anyhow::anyhow!("{status}: {body}"))
    }
}

fn default_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "glm-5.1".into(),
            display_name: "GLM-5.1".into(),
            context_tokens: 128_000,
            rate_label: None,
        },
        ModelInfo {
            id: "glm-4.6".into(),
            display_name: "GLM-4.6".into(),
            context_tokens: 128_000,
            rate_label: None,
        },
        ModelInfo {
            id: "glm-4.5".into(),
            display_name: "GLM-4.5".into(),
            context_tokens: 128_000,
            rate_label: None,
        },
    ]
}

// ========================================================================
// Data Structures
// ========================================================================

#[derive(Deserialize)]
struct ModelsApiResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    #[test]
    fn test_bearer_token_plain_key() {
        let provider = ZaiProvider::build(AuthStrategy::ApiKey(SecretString::new(
            "plainkey123".into(),
        )));
        let token = provider.bearer_token().unwrap();
        assert_eq!(token, "plainkey123");
    }

    #[test]
    fn test_default_models_has_glm51_first() {
        let models = default_models();
        assert_eq!(models[0].id, "glm-5.1");
    }
}
