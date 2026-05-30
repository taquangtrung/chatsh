use crate::tui::{Highlighter, skip_csi_escape};

// Atom One Dark palette (24-bit truecolor escapes).
const C_DIM: &str = "\x1b[38;2;92;99;112m";
const C_RED: &str = "\x1b[38;2;224;108;117m";
const C_YELLOW: &str = "\x1b[38;2;229;192;123m";
const C_BLUE: &str = "\x1b[38;2;97;175;239m";
const C_PURPLE: &str = "\x1b[38;2;198;120;221m";
const C_CYAN: &str = "\x1b[38;2;86;182;194m";
const CODE_BG: &str = "\x1b[48;2;58;63;75m";
const RESET: &str = "\x1b[0m";
const RESET_FG: &str = "\x1b[39m";
const RESET_BG: &str = "\x1b[49m";

pub struct MarkdownRenderer {
    line_buffer: String,
    in_code_block: bool,
    highlighter: Option<Highlighter>,
    last_emitted_blank: bool,
}

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownRenderer {
    pub fn new() -> Self {
        Self {
            line_buffer: String::new(),
            in_code_block: false,
            highlighter: None,
            last_emitted_blank: true,
        }
    }

    pub fn push(&mut self, text: &str) -> String {
        self.line_buffer.push_str(text);
        let mut out = String::new();
        while let Some(idx) = self.line_buffer.find('\n') {
            let line: String = self.line_buffer.drain(..=idx).collect();
            let trimmed = line.trim_end_matches(['\n', '\r']);
            let rendered = self.render_line(trimmed);
            let is_blank = is_visually_blank(&rendered);
            if is_blank && self.last_emitted_blank {
                continue;
            }
            out.push_str(&rendered);
            out.push_str("\r\n");
            self.last_emitted_blank = is_blank;
        }
        out
    }

    pub fn flush(&mut self) -> String {
        if self.line_buffer.is_empty() {
            return String::new();
        }
        let line = std::mem::take(&mut self.line_buffer);
        let rendered = self.render_line(&line);
        if is_visually_blank(&rendered) && self.last_emitted_blank {
            return String::new();
        }
        rendered
    }

    fn render_line(&mut self, line: &str) -> String {
        let trimmed_start = line.trim_start();
        let indent_len = line.len() - trimmed_start.len();
        let indent = &line[..indent_len];
        let trimmed = trimmed_start;

        if trimmed.starts_with("```") {
            if self.in_code_block {
                self.in_code_block = false;
                self.highlighter = None;
                return String::new();
            }
            self.in_code_block = true;
            let lang = trimmed.trim_start_matches('`').trim();
            self.highlighter = Some(Highlighter::new(lang));
            return String::new();
        }

        if self.in_code_block {
            let highlighted = match self.highlighter.as_mut() {
                Some(h) => h.highlight_line(line),
                None => line.to_string(),
            };
            return format!("  {highlighted}");
        }

        if let Some(rest) = trimmed.strip_prefix("# ") {
            return format!("{indent}\x1b[1m{C_PURPLE}{}{RESET}", strip_inline(rest));
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            return format!("{indent}\x1b[1m{C_BLUE}{}{RESET}", strip_inline(rest));
        }
        if let Some(rest) = trimmed.strip_prefix("### ") {
            return format!("{indent}\x1b[1m{C_CYAN}{}{RESET}", strip_inline(rest));
        }
        if let Some(rest) = trimmed.strip_prefix("#### ") {
            return format!("{indent}\x1b[1m{C_YELLOW}{}{RESET}", strip_inline(rest));
        }

        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            return format!("{C_DIM}──────────────────────────────{RESET_FG}");
        }

        if let Some(rest) = trimmed.strip_prefix("> ") {
            return format!(
                "{indent}{C_DIM}│{RESET_FG} \x1b[3m{}\x1b[23m",
                render_inline(rest)
            );
        }
        if trimmed == ">" {
            return format!("{indent}{C_DIM}│{RESET_FG}");
        }

        for marker in ["- ", "* ", "+ "] {
            if let Some(rest) = trimmed.strip_prefix(marker) {
                return format!("{indent}{C_CYAN}•{RESET_FG} {}", render_inline(rest));
            }
        }

        if let Some((num, rest)) = parse_numbered_list(trimmed) {
            return format!("{indent}{C_CYAN}{num}.{RESET_FG} {}", render_inline(rest));
        }

        format!("{indent}{}", render_inline(trimmed))
    }
}

fn parse_numbered_list(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 || i > 3 {
        return None;
    }
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1) == Some(&b' ') {
        Some((&s[..i], &s[i + 2..]))
    } else {
        None
    }
}

fn render_inline(line: &str) -> String {
    process_inline(line, true)
}

fn strip_inline(line: &str) -> String {
    process_inline(line, false)
}

/// Walk `line` recognizing inline markdown spans (`**bold**`, `~~strike~~`,
/// `` `code` ``, `*`/`_` italics, and `[text](url)` links). When `render` is
/// true each span is wrapped in its ANSI styling; when false the decoration is
/// dropped and only the inner text is kept. Both modes share one scanner so the
/// two behaviors can never drift apart.
fn process_inline(line: &str, render: bool) -> String {
    let mut out = String::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let remaining = &line[i..];

        if remaining.starts_with("**") {
            if let Some(end) = line[i + 2..].find("**") {
                let content = &line[i + 2..i + 2 + end];
                if render {
                    out.push_str("\x1b[1m");
                    out.push_str(content);
                    out.push_str("\x1b[22m");
                } else {
                    out.push_str(content);
                }
                i += 2 + end + 2;
                continue;
            }
        }

        if remaining.starts_with("~~") {
            if let Some(end) = line[i + 2..].find("~~") {
                let content = &line[i + 2..i + 2 + end];
                if render {
                    out.push_str("\x1b[9m");
                    out.push_str(content);
                    out.push_str("\x1b[29m");
                } else {
                    out.push_str(content);
                }
                i += 2 + end + 2;
                continue;
            }
        }

        let c = bytes[i];

        if c == b'`' {
            if let Some(end) = line[i + 1..].find('`') {
                let content = &line[i + 1..i + 1 + end];
                if render {
                    out.push_str(CODE_BG);
                    out.push_str(C_RED);
                    out.push(' ');
                    out.push_str(content);
                    out.push(' ');
                    out.push_str(RESET_FG);
                    out.push_str(RESET_BG);
                } else {
                    out.push_str(content);
                }
                i += 1 + end + 1;
                continue;
            }
        }

        if c == b'*' || c == b'_' {
            if let Some(end) = line[i + 1..].find(c as char) {
                let content = &line[i + 1..i + 1 + end];
                if !content.is_empty() {
                    if render {
                        out.push_str("\x1b[3m");
                        out.push_str(content);
                        out.push_str("\x1b[23m");
                    } else {
                        out.push_str(content);
                    }
                    i += 1 + end + 1;
                    continue;
                }
            }
        }

        if c == b'[' {
            if let Some(close_b) = line[i + 1..].find(']') {
                let after = i + 1 + close_b + 1;
                if bytes.get(after) == Some(&b'(') {
                    if let Some(close_p) = line[after + 1..].find(')') {
                        let text = &line[i + 1..i + 1 + close_b];
                        let url = &line[after + 1..after + 1 + close_p];
                        if render {
                            out.push_str("\x1b[4m");
                            out.push_str(C_BLUE);
                            out.push_str(text);
                            out.push_str(RESET_FG);
                            out.push_str("\x1b[24m");
                            if !url.is_empty() {
                                out.push(' ');
                                out.push_str(C_DIM);
                                out.push('(');
                                out.push_str(url);
                                out.push(')');
                                out.push_str(RESET_FG);
                            }
                        } else {
                            out.push_str(text);
                        }
                        i = after + 1 + close_p + 1;
                        continue;
                    }
                }
            }
        }

        let ch_end = next_char_boundary(line, i);
        out.push_str(&line[i..ch_end]);
        i = ch_end;
    }
    out
}

fn next_char_boundary(s: &str, start: usize) -> usize {
    let mut i = start + 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn is_visually_blank(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(next) = skip_csi_escape(bytes, i) {
            i = next;
            continue;
        }
        if !(bytes[i] as char).is_whitespace() {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if let Some(next) = skip_csi_escape(bytes, i) {
                i = next;
                continue;
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    #[test]
    fn test_heading_h1_strips_hash() {
        let mut r = MarkdownRenderer::new();
        let out = r.push("# Title\n");
        assert!(out.contains("Title"));
        assert!(!out.contains("# Title"));
    }

    #[test]
    fn test_bold_strips_asterisks() {
        let mut r = MarkdownRenderer::new();
        let out = r.push("Hello **world** there\n");
        assert!(out.contains("world"));
        assert!(!out.contains("**world**"));
    }

    #[test]
    fn test_dash_list_with_inline_code() {
        let mut r = MarkdownRenderer::new();
        let out = r.push("- `--oneline` — Show one commit per line\n");
        assert!(strip_ansi(&out).contains("--oneline"));
        assert!(!strip_ansi(&out).contains("`--oneline`"));
    }

    #[test]
    fn test_strikethrough() {
        let mut r = MarkdownRenderer::new();
        let out = r.push("Old ~~deprecated~~ way\n");
        assert!(out.contains("deprecated"));
        assert!(!out.contains("~~"));
    }

    #[test]
    fn test_inline_code_strips_backticks() {
        let mut r = MarkdownRenderer::new();
        let out = r.push("Use `git status` for status\n");
        assert!(out.contains("git status"));
        assert!(!out.contains("`git status`"));
    }

    #[test]
    fn test_code_fence_renders_body() {
        let mut r = MarkdownRenderer::new();
        let out = r.push("```bash\nls -la\n```\n");
        assert!(strip_ansi(&out).contains("ls -la"));
        assert!(!out.contains("```bash"));
    }

    #[test]
    fn test_code_fence_applies_syntax_highlighting() {
        let mut r = MarkdownRenderer::new();
        let out = r.push("```bash\n# comment line\n```\n");
        assert!(out.contains("\x1b["));
        assert!(strip_ansi(&out).contains("# comment line"));
    }

    #[test]
    fn test_unordered_list_dash() {
        let mut r = MarkdownRenderer::new();
        let out = r.push("- item one\n");
        assert!(out.contains("•"));
        assert!(out.contains("item one"));
        assert!(!out.contains("- item"));
    }

    #[test]
    fn test_block_quote() {
        let mut r = MarkdownRenderer::new();
        let out = r.push("> quoted text\n");
        assert!(out.contains("│"));
        assert!(out.contains("quoted text"));
        assert!(!out.contains("> quoted"));
    }

    #[test]
    fn test_link_renders_text_and_url() {
        let mut r = MarkdownRenderer::new();
        let out = r.push("See [docs](https://example.com) for more\n");
        assert!(out.contains("docs"));
        assert!(out.contains("https://example.com"));
        assert!(!out.contains("[docs]"));
    }

    #[test]
    fn test_streaming_split_mid_pattern() {
        let mut r = MarkdownRenderer::new();
        let mut combined = String::new();
        combined.push_str(&r.push("Hello "));
        combined.push_str(&r.push("**wor"));
        combined.push_str(&r.push("ld**\n"));
        assert!(combined.contains("world"));
        assert!(!combined.contains("**world**"));
    }

    #[test]
    fn test_flush_emits_partial_line() {
        let mut r = MarkdownRenderer::new();
        let _ = r.push("partial line no newline");
        let out = r.flush();
        assert!(out.contains("partial line no newline"));
    }

    #[test]
    fn test_code_block_preserves_content_unaltered() {
        let mut r = MarkdownRenderer::new();
        let out = r.push("```\n**not bold**\n```\n");
        assert!(out.contains("**not bold**"));
    }

    #[test]
    fn test_indented_list_keeps_indent() {
        let mut r = MarkdownRenderer::new();
        let out = r.push("  - nested\n");
        let pos_bullet = out.find('•').expect("bullet present");
        let pos_text = out.find("nested").expect("text present");
        assert!(pos_bullet < pos_text);
        assert!(out.starts_with("  "));
    }
}
