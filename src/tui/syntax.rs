use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

const THEME_NAME: &str = "base16-mocha.dark";

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    THEME.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        ts.themes
            .get(THEME_NAME)
            .cloned()
            .or_else(|| ts.themes.values().next().cloned())
            .expect("syntect default theme set is empty")
    })
}

pub struct Highlighter {
    inner: HighlightLines<'static>,
}

impl Highlighter {
    pub fn new(lang: &str) -> Self {
        let set = syntax_set();
        let lang_lower = lang.to_ascii_lowercase();
        let syntax = set
            .find_syntax_by_token(&lang_lower)
            .or_else(|| set.find_syntax_by_extension(&lang_lower))
            .or_else(|| set.find_syntax_by_name(lang))
            .unwrap_or_else(|| set.find_syntax_plain_text());
        Self {
            inner: HighlightLines::new(syntax, theme()),
        }
    }

    pub fn highlight_line(&mut self, line: &str) -> String {
        let with_newline;
        let input = if line.ends_with('\n') {
            line
        } else {
            with_newline = format!("{line}\n");
            with_newline.as_str()
        };
        let ranges = match self.inner.highlight_line(input, syntax_set()) {
            Ok(r) => r,
            Err(_) => return line.to_string(),
        };
        let mut out = as_24_bit_terminal_escaped(&ranges[..], false);
        while matches!(out.chars().last(), Some('\n' | '\r')) {
            out.pop();
        }
        out.push_str("\x1b[0m");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1B && bytes.get(i + 1) == Some(&b'[') {
                i += 2;
                while i < bytes.len() && !matches!(bytes[i], 0x40..=0x7E) {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
                continue;
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    #[test]
    fn test_preserves_content_bash() {
        let mut h = Highlighter::new("bash");
        let out = h.highlight_line("ls -la");
        let plain = strip_ansi(&out);
        assert_eq!(plain, "ls -la");
    }

    #[test]
    fn test_emits_ansi_and_preserves_comment() {
        let mut h = Highlighter::new("bash");
        let out = h.highlight_line("# a comment");
        assert!(out.contains("\x1b["));
        assert_eq!(strip_ansi(&out), "# a comment");
    }

    #[test]
    fn test_preserves_content_python() {
        let mut h = Highlighter::new("python");
        let out = h.highlight_line("def greet(name):");
        assert_eq!(strip_ansi(&out), "def greet(name):");
    }

    #[test]
    fn test_preserves_content_rust() {
        let mut h = Highlighter::new("rust");
        let out = h.highlight_line("fn main() {}");
        assert_eq!(strip_ansi(&out), "fn main() {}");
    }

    #[test]
    fn test_unknown_language_falls_back_to_plain() {
        let mut h = Highlighter::new("nonexistent-lang");
        let out = h.highlight_line("just some text");
        assert_eq!(strip_ansi(&out), "just some text");
    }

    #[test]
    fn test_lang_aliases_resolve() {
        let _ = Highlighter::new("py");
        let _ = Highlighter::new("rs");
        let _ = Highlighter::new("sh");
        let _ = Highlighter::new("ts");
        let _ = Highlighter::new("yml");
    }

    #[test]
    fn test_streaming_multiline_state() {
        let mut h = Highlighter::new("rust");
        let _ = h.highlight_line("/*");
        let mid = h.highlight_line("inside block comment");
        let _ = h.highlight_line("*/");
        assert_eq!(strip_ansi(&mid), "inside block comment");
    }

    #[test]
    fn test_empty_line_does_not_crash() {
        let mut h = Highlighter::new("rust");
        let _ = h.highlight_line("");
    }

    #[test]
    fn test_string_content_preserved() {
        let mut h = Highlighter::new("bash");
        let out = h.highlight_line("echo \"hello world\"");
        assert_eq!(strip_ansi(&out), "echo \"hello world\"");
    }

    #[test]
    fn test_number_content_preserved() {
        let mut h = Highlighter::new("python");
        let out = h.highlight_line("x = 42");
        assert_eq!(strip_ansi(&out), "x = 42");
    }
}
