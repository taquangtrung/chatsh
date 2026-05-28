use url::Url;

use crate::ai::anthropic::AnthropicProvider;
use crate::ai::auth::AuthStrategy;
use crate::types::ModelInfo;

pub struct ZaiProvider;

impl ZaiProvider {
    pub fn new(auth: AuthStrategy) -> AnthropicProvider {
        let url = Url::parse("https://api.z.ai/api/anthropic/").expect("valid url");
        AnthropicProvider::with_options("zai", "Z.ai (coding plan)", url, auth, default_models())
    }
}

fn default_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "glm-4.6".into(),
            display_name: "GLM-4.6".into(),
            context_tokens: 128_000,
        },
        ModelInfo {
            id: "glm-4.5".into(),
            display_name: "GLM-4.5".into(),
            context_tokens: 128_000,
        },
    ]
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::provider::LlmProvider;
    use secrecy::SecretString;

    #[test]
    fn test_zai_provider_id() {
        let provider =
            ZaiProvider::new(AuthStrategy::ApiKey(SecretString::new("test".into())));
        assert_eq!(provider.id(), "zai");
        assert_eq!(provider.display_name(), "Z.ai (coding plan)");
    }
}
