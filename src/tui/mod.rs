pub mod markdown;
pub mod syntax;

// Re-exports so consumers use `crate::tui::Item` instead of deep submodule paths.
pub use markdown::MarkdownRenderer;
pub use syntax::Highlighter;

/// If `bytes[i..]` begins a CSI escape sequence (`ESC [ … final-byte`), return
/// the index just past it; otherwise return `None`. Used to skip color codes
/// when measuring or stripping rendered terminal output.
pub(crate) fn skip_csi_escape(bytes: &[u8], i: usize) -> Option<usize> {
    if bytes.get(i) != Some(&0x1B) || bytes.get(i + 1) != Some(&b'[') {
        return None;
    }
    let mut j = i + 2;
    while j < bytes.len() && !matches!(bytes[j], 0x40..=0x7E) {
        j += 1;
    }
    if j < bytes.len() {
        j += 1;
    }
    Some(j)
}
