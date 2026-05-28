#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Command,
    Output,
    Stderr,
    Prompt,
}

pub struct ParsedOutput {
    pub cwd: Option<String>,
    pub exit_code: Option<i32>,
    pub kind: EntryKind,
    pub line: String,
}

pub fn parse_output_line(raw: &str) -> ParsedOutput {
    let cwd = parse_osc7_cwd(raw);
    ParsedOutput {
        cwd,
        exit_code: None,
        kind: EntryKind::Output,
        line: raw.to_string(),
    }
}

fn parse_osc7_cwd(raw: &str) -> Option<String> {
    let prefix = "\x1b]7;file://";
    if let Some(start) = raw.find(prefix) {
        let rest = &raw[start + prefix.len()..];
        if let Some(end) = rest.find('\x07').or_else(|| rest.find("\x1b\\")) {
            let url_path = &rest[..end];
            let decoded = url_path.replace("%2F", "/").replace("%20", " ");
            let path = if decoded.starts_with('/') {
                decoded
            } else if let Some(slash) = decoded.find('/') {
                decoded[slash..].to_string()
            } else {
                decoded
            };
            return Some(path);
        }
    }
    None
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_osc7_cwd_valid() {
        let raw = "\x1b]7;file://localhost/home/trung/proj\x07";
        let result = parse_osc7_cwd(raw);
        assert_eq!(result, Some("/home/trung/proj".to_string()));
    }

    #[test]
    fn test_parse_osc7_cwd_absent() {
        let raw = "normal output line";
        let result = parse_osc7_cwd(raw);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_output_line_defaults() {
        let parsed = parse_output_line("hello");
        assert!(parsed.cwd.is_none());
        assert!(parsed.exit_code.is_none());
        assert_eq!(parsed.kind, EntryKind::Output);
    }
}
