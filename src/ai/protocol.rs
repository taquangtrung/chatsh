use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatRequest {
    pub provider_id: String,
    pub model: String,
    pub messages: Vec<ChatMessage>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatEvent {
    Token { text: String },
    Thinking { text: String },
    Done,
    Error { message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub context_tokens: u32,
    /// Human-readable rate label, e.g. "1x" or "0.1x". None when unknown.
    pub rate_label: Option<String>,
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_message_constructors() {
        let sys = ChatMessage::system("you are helpful");
        assert_eq!(sys.role, ChatRole::System);
        assert_eq!(sys.content, "you are helpful");

        let usr = ChatMessage::user("hello");
        assert_eq!(usr.role, ChatRole::User);
    }

    #[test]
    fn test_chat_event_equality() {
        let a = ChatEvent::Token {
            text: "hi".to_string(),
        };
        let b = ChatEvent::Token {
            text: "hi".to_string(),
        };
        assert_eq!(a, b);
    }
}
