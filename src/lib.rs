//! chatsh: PTY-level, shell-agnostic AI chat that lives inline in any terminal.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// AI providers
pub mod ai;

// Shell integration
pub mod shell;

// Persistence
pub mod storage;

// Commands and chat
pub mod chat;
pub mod input;

// Terminal UI
pub mod tui;

// Application orchestration
pub mod app;
