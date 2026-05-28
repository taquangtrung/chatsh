use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;

use crate::types::ChatMessage;

const DEFAULT_DIR: &str = ".config/chatsh";
const DEFAULT_FILE: &str = "conversation.jsonl";

pub struct Conversation {
    messages: Vec<ChatMessage>,
}

impl Default for Conversation {
    fn default() -> Self {
        Self::new()
    }
}

impl Conversation {
    pub fn new() -> Self {
        Self { messages: Vec::new() }
    }

    pub fn load() -> Self {
        let mut messages = Vec::new();
        if let Ok(text) = std::fs::read_to_string(Self::path()) {
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(m) = serde_json::from_str::<ChatMessage>(trimmed) {
                    messages.push(m);
                }
            }
        }
        Self { messages }
    }

    pub fn append(&mut self, user: ChatMessage, assistant: ChatMessage) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, "{}", serde_json::to_string(&user)?)?;
        writeln!(file, "{}", serde_json::to_string(&assistant)?)?;
        self.messages.push(user);
        self.messages.push(assistant);
        Ok(())
    }

    pub fn clear(&mut self) -> Result<()> {
        self.messages.clear();
        let path = Self::path();
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    pub fn recent(&self, max_pairs: usize) -> Vec<ChatMessage> {
        let max_msgs = max_pairs * 2;
        let start = self.messages.len().saturating_sub(max_msgs);
        self.messages[start..].to_vec()
    }

    pub fn turn_count(&self) -> usize {
        self.messages.len() / 2
    }

    fn path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(DEFAULT_DIR).join(DEFAULT_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChatRole;

    #[test]
    fn test_new_is_empty() {
        let c = Conversation::new();
        assert_eq!(c.turn_count(), 0);
        assert!(c.recent(10).is_empty());
    }

    #[test]
    fn test_recent_caps_to_max_pairs() {
        let mut c = Conversation::new();
        for i in 0..50 {
            c.messages.push(ChatMessage::user(format!("q{i}")));
            c.messages.push(ChatMessage::assistant(format!("a{i}")));
        }
        let r = c.recent(5);
        assert_eq!(r.len(), 10);
        assert_eq!(r[0].role, ChatRole::User);
        assert_eq!(r[0].content, "q45");
        assert_eq!(r.last().unwrap().content, "a49");
    }

    #[test]
    fn test_recent_returns_all_when_below_cap() {
        let mut c = Conversation::new();
        c.messages.push(ChatMessage::user("hi"));
        c.messages.push(ChatMessage::assistant("hello"));
        let r = c.recent(20);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn test_turn_count_counts_pairs() {
        let mut c = Conversation::new();
        c.messages.push(ChatMessage::user("a"));
        c.messages.push(ChatMessage::assistant("b"));
        c.messages.push(ChatMessage::user("c"));
        c.messages.push(ChatMessage::assistant("d"));
        assert_eq!(c.turn_count(), 2);
    }
}
