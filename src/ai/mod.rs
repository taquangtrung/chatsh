pub mod anthropic;
pub mod auth;
pub mod copilot;
pub mod openai;
pub mod protocol;
pub mod provider;
pub mod registry;
pub mod zai;

// Canonical provider identifiers, shared across the registry, the `/connect`
// dispatch, and each provider's `id()`.
pub const PROVIDER_ANTHROPIC: &str = "anthropic";
pub const PROVIDER_COPILOT: &str = "github-copilot";
pub const PROVIDER_OPENAI: &str = "openai";
pub const PROVIDER_ZAI: &str = "z.ai-coding-plan";

// Re-exports so consumers use `crate::ai::Item` instead of deep submodule paths.
pub use anthropic::AnthropicProvider;
pub use auth::{AuthStrategy, CachedToken, OAuthState};
pub use protocol::{ChatEvent, ChatMessage, ChatRequest, ChatRole, ModelInfo};
pub use copilot::{CopilotProvider, DeviceFlowPoll};
pub use openai::{OpenAiProvider, OpenAiStream};
pub use provider::LlmProvider;
pub use registry::ProviderRegistry;
pub use zai::ZaiProvider;
