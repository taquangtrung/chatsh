use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

const DEFAULT_CONFIG_DIR: &str = ".config/chatsh";
const DEFAULT_CONFIG_FILE: &str = "config.toml";
const DEFAULT_BUFFER_LINES: usize = 200;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub ai: AiConfig,
    #[serde(default)]
    pub context: ContextConfig,
    #[serde(default)]
    pub ui: UiConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AiConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextConfig {
    #[serde(default = "default_buffer_lines")]
    pub buffer_lines: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UiConfig {
    #[serde(default)]
    pub overlay_height: Option<u16>,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: None,
        }
    }
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            buffer_lines: default_buffer_lines(),
        }
    }
}

fn default_provider() -> String {
    "auto".to_string()
}

fn default_buffer_lines() -> usize {
    DEFAULT_BUFFER_LINES
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        let config: Self = toml::from_str(&text)?;
        Ok(config)
    }

    pub fn save(&mut self, provider: Option<&str>, model: Option<&str>) -> Result<()> {
        if let Some(p) = provider {
            self.ai.provider = p.to_string();
        }
        if model.is_some() {
            self.ai.model = model.map(|s| s.to_string());
        }
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text)?;
        Ok(())
    }

    pub fn config_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(DEFAULT_CONFIG_DIR)
            .join(DEFAULT_CONFIG_FILE)
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert_eq!(config.ai.provider, "auto");
        assert_eq!(config.context.buffer_lines, 200);
        assert!(config.ai.model.is_none());
    }

    #[test]
    fn test_config_parse_toml() {
        let toml = r#"
[ai]
provider = "z.ai-coding-plan"
model = "glm-4.6"

[context]
buffer_lines = 100
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ai.provider, "z.ai-coding-plan");
        assert_eq!(config.ai.model.as_deref(), Some("glm-4.6"));
        assert_eq!(config.context.buffer_lines, 100);
    }
}
