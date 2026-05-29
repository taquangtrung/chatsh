use std::sync::Arc;

use anyhow::{anyhow, Result};
use futures::stream::BoxStream;

use crate::ai::LlmProvider;
use crate::ai::{ChatEvent, ChatRequest};

#[derive(Default)]
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn LlmProvider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, provider: Arc<dyn LlmProvider>) {
        let id = provider.id().to_string();
        if let Some(slot) = self.providers.iter_mut().find(|p| p.id() == id) {
            *slot = provider;
        } else {
            self.providers.push(provider);
        }
    }

    pub fn get(&self, id: &str) -> Result<Arc<dyn LlmProvider>> {
        self.providers
            .iter()
            .find(|p| p.id() == id)
            .cloned()
            .ok_or_else(|| anyhow!("provider '{id}' not found"))
    }

    pub fn list(&self) -> Vec<Arc<dyn LlmProvider>> {
        self.providers.clone()
    }

    pub async fn chat(&self, req: ChatRequest) -> Result<BoxStream<'static, ChatEvent>> {
        let provider = self.get(&req.provider_id)?;
        provider.chat(req).await
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ModelInfo;
    use async_trait::async_trait;
    use futures::stream::{self, StreamExt};

    struct FakeProvider {
        id: String,
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        fn id(&self) -> &str {
            &self.id
        }
        fn display_name(&self) -> &str {
            &self.id
        }
        async fn list_models(&self) -> Result<Vec<ModelInfo>> {
            Ok(vec![])
        }
        async fn chat(&self, _req: ChatRequest) -> Result<BoxStream<'static, ChatEvent>> {
            Ok(stream::iter(vec![ChatEvent::Done]).boxed())
        }
    }

    #[tokio::test]
    async fn test_registry_dispatch() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(FakeProvider {
            id: "test".into(),
        }));
        let req = ChatRequest {
            provider_id: "test".into(),
            model: "m".into(),
            messages: vec![],
        };
        let mut stream = reg.chat(req).await.unwrap();
        let event = stream.next().await.unwrap();
        assert_eq!(event, ChatEvent::Done);
    }

    #[tokio::test]
    async fn test_registry_missing_provider() {
        let reg = ProviderRegistry::new();
        let req = ChatRequest {
            provider_id: "missing".into(),
            model: "m".into(),
            messages: vec![],
        };
        assert!(reg.chat(req).await.is_err());
    }
}
