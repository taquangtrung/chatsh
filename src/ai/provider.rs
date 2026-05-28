use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::types::{ChatEvent, ChatRequest, ModelInfo, QuotaSnapshot};

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>>;
    async fn quota(&self) -> anyhow::Result<QuotaSnapshot>;
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<BoxStream<'static, ChatEvent>>;
}
