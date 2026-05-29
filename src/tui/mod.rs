pub mod markdown;
pub mod syntax;

// Re-exports so consumers use `crate::tui::Item` instead of deep submodule paths.
pub use markdown::MarkdownRenderer;
pub use syntax::Highlighter;
