use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::stream::BoxStream;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use url::Url;

use crate::ai::LlmProvider;
use crate::ai::OpenAiStream;
use crate::ai::{AuthStrategy, CachedToken, OAuthState};
use crate::ai::{ChatEvent, ChatRequest, ModelInfo, PROVIDER_COPILOT};

const COPILOT_CLIENT_ID_DEFAULT: &str = "Iv1.b507a08c87ecfe98";

/// Returns the GitHub OAuth client ID to use for the Copilot device flow.
/// Override with `CHATSH_COPILOT_CLIENT_ID` if you have registered your own
/// GitHub OAuth application.
fn copilot_client_id() -> String {
    std::env::var("CHATSH_COPILOT_CLIENT_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| COPILOT_CLIENT_ID_DEFAULT.to_string())
}
const COPILOT_INTEGRATION_ID: &str = "vscode-chat";
const EDITOR_VERSION: &str = "vscode/1.95.0";
const EDITOR_PLUGIN_VERSION: &str = "copilot-chat/0.22.0";
const USER_AGENT: &str = "GitHubCopilotChat/0.22.0";
const REFRESH_MARGIN_SECONDS: i64 = 60;

#[derive(Debug, Clone)]
pub struct DeviceFlowChallenge {
    pub user_code: String,
    pub verification_uri: String,
    pub device_code: String,
    pub interval_seconds: u32,
    pub expires_in_seconds: u32,
}

#[derive(Debug, Clone)]
pub enum DeviceFlowPoll {
    Authorized(String),
    Pending,
    SlowDown,
    Expired,
    Denied,
    Other(String),
}

pub struct CopilotProvider {
    client: Client,
    state: std::sync::Arc<OAuthState>,
}

impl CopilotProvider {
    pub fn new(auth: AuthStrategy) -> anyhow::Result<Self> {
        let state = match auth {
            AuthStrategy::OAuthDevice(s) => s,
            _ => {
                return Err(anyhow::anyhow!(
                    "Copilot requires AuthStrategy::OAuthDevice"
                ));
            }
        };
        Ok(Self {
            client: Client::new(),
            state,
        })
    }

    pub fn set_github_token(&self, token: String) {
        *self.state.github_token.write() = Some(SecretString::new(token));
    }

    pub async fn device_flow_start(&self) -> anyhow::Result<DeviceFlowChallenge> {
        let resp = self
            .client
            .post("https://github.com/login/device/code")
            .header("accept", "application/json")
            .form(&[
                ("client_id", copilot_client_id().as_str()),
                ("scope", "read:user"),
            ])
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!(
                "device flow start failed: {}",
                resp.status()
            ));
        }
        let parsed: DeviceFlowResponse = resp.json().await?;
        Ok(DeviceFlowChallenge {
            user_code: parsed.user_code,
            verification_uri: parsed.verification_uri,
            device_code: parsed.device_code,
            interval_seconds: parsed.interval,
            expires_in_seconds: parsed.expires_in,
        })
    }

    pub async fn device_flow_poll(&self, device_code: &str) -> anyhow::Result<DeviceFlowPoll> {
        let client_id = copilot_client_id();
        let resp = self
            .client
            .post("https://github.com/login/oauth/access_token")
            .header("accept", "application/json")
            .form(&[
                ("client_id", client_id.as_str()),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await?;
        let body: serde_json::Value = resp.json().await?;
        if let Some(token) = body.get("access_token").and_then(|v| v.as_str()) {
            self.set_github_token(token.to_string());
            return Ok(DeviceFlowPoll::Authorized(token.to_string()));
        }
        if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
            return Ok(match err {
                "authorization_pending" => DeviceFlowPoll::Pending,
                "slow_down" => DeviceFlowPoll::SlowDown,
                "expired_token" => DeviceFlowPoll::Expired,
                "access_denied" => DeviceFlowPoll::Denied,
                other => DeviceFlowPoll::Other(other.to_string()),
            });
        }
        Ok(DeviceFlowPoll::Other("unexpected response".into()))
    }

    async fn ensure_session_token(&self) -> anyhow::Result<String> {
        if let Some(cached) = self.state.session_token.read().clone() {
            if cached.expires_at_unix - REFRESH_MARGIN_SECONDS > now_unix() {
                return Ok(cached.value.expose_secret().to_string());
            }
        }

        let gh_token = self
            .state
            .github_token
            .read()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Copilot: no GitHub token; run device flow"))?;

        let resp = self
            .client
            .get("https://api.github.com/copilot_internal/v2/token")
            .header(
                "authorization",
                format!("token {}", gh_token.expose_secret()),
            )
            .header("accept", "application/json")
            .header("editor-version", EDITOR_VERSION)
            .header("editor-plugin-version", EDITOR_PLUGIN_VERSION)
            .header("user-agent", USER_AGENT)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow::anyhow!(
                "session token exchange failed: {}",
                resp.status()
            ));
        }
        let parsed: SessionTokenResponse = resp.json().await?;

        let cached = CachedToken {
            value: SecretString::new(parsed.token.clone()),
            expires_at_unix: parsed.expires_at,
        };
        *self.state.session_token.write() = Some(cached);
        Ok(parsed.token)
    }
}

#[async_trait]
impl LlmProvider for CopilotProvider {
    fn id(&self) -> &str {
        PROVIDER_COPILOT
    }

    fn display_name(&self) -> &str {
        "GitHub Copilot"
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let token = match self.ensure_session_token().await {
            Ok(t) => t,
            Err(_) => return Ok(Vec::new()),
        };
        let resp = self
            .client
            .get("https://api.githubcopilot.com/models")
            .header("authorization", format!("Bearer {token}"))
            .header("accept", "application/json")
            .header("copilot-integration-id", COPILOT_INTEGRATION_ID)
            .header("editor-version", EDITOR_VERSION)
            .header("editor-plugin-version", EDITOR_PLUGIN_VERSION)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(Vec::new());
        }
        let body: ModelsResponse = resp.json().await?;
        let mut out: Vec<ModelInfo> = body
            .data
            .into_iter()
            .filter(|m| m.model_picker_enabled.unwrap_or(true))
            .filter(|m| {
                m.policy
                    .as_ref()
                    .and_then(|p| p.state.as_deref())
                    .map(|s| s != "disabled")
                    .unwrap_or(true)
            })
            .filter(|m| {
                // Only keep models that support /chat/completions.
                // If the field is absent (legacy models), assume /chat/completions.
                m.supported_endpoints.is_empty()
                    || m.supported_endpoints
                        .iter()
                        .any(|e| e == "/chat/completions")
            })
            .map(|m| {
                let context_tokens = m
                    .capabilities
                    .as_ref()
                    .and_then(|c| c.limits.as_ref())
                    .and_then(|l| l.max_context_window_tokens)
                    .unwrap_or(0);
                let rate_label = m.premium_requests_multiplier.map(|v| {
                    if v == v.floor() && v >= 0.0 {
                        format!("{}x", v as u64)
                    } else {
                        format!("{v}x")
                    }
                });
                ModelInfo {
                    id: m.id,
                    display_name: m.name.unwrap_or_default(),
                    context_tokens,
                    rate_label,
                }
            })
            .collect();
        out.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        Ok(out)
    }

    async fn chat(&self, req: ChatRequest) -> anyhow::Result<BoxStream<'static, ChatEvent>> {
        let token = self.ensure_session_token().await?;
        let endpoint = Url::parse("https://api.githubcopilot.com/chat/completions")?;

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("copilot-integration-id"),
            HeaderValue::from_static(COPILOT_INTEGRATION_ID),
        );
        headers.insert(
            HeaderName::from_static("editor-version"),
            HeaderValue::from_static(EDITOR_VERSION),
        );
        headers.insert(
            HeaderName::from_static("editor-plugin-version"),
            HeaderValue::from_static(EDITOR_PLUGIN_VERSION),
        );
        headers.insert(
            HeaderName::from_static("user-agent"),
            HeaderValue::from_static(USER_AGENT),
        );
        headers.insert(
            HeaderName::from_static("openai-intent"),
            HeaderValue::from_static("conversation-panel"),
        );

        OpenAiStream {
            provider_id: PROVIDER_COPILOT,
            endpoint,
            bearer_token: token,
            extra_headers: headers,
            client: self.client.clone(),
        }
        .stream(req)
        .await
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Deserialize)]
struct DeviceFlowResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u32,
    interval: u32,
}

#[derive(Deserialize)]
struct SessionTokenResponse {
    token: String,
    expires_at: i64,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<CopilotModel>,
}

#[derive(Deserialize)]
struct CopilotModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    model_picker_enabled: Option<bool>,
    #[serde(default)]
    policy: Option<ModelPolicy>,
    #[serde(default)]
    capabilities: Option<ModelCapabilities>,
    /// Endpoints this model supports (e.g. ["/chat/completions"], ["/responses"]).
    /// Absent on legacy Azure-backed models, which always support /chat/completions.
    #[serde(default)]
    supported_endpoints: Vec<String>,
    /// Billing multiplier relative to the base request quota (1x = standard).
    #[serde(default)]
    premium_requests_multiplier: Option<f64>,
}

#[derive(Deserialize)]
struct ModelPolicy {
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize)]
struct ModelCapabilities {
    #[serde(default)]
    limits: Option<ModelLimits>,
}

#[derive(Deserialize)]
struct ModelLimits {
    #[serde(default)]
    max_context_window_tokens: Option<u32>,
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
}
