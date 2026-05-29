// Interactive line editing for chatsh `/` commands: completion popups, history
// navigation, and input rendering.

use std::io::Write;

use super::trigger::{complete, complete_providers};
use crate::VERSION;

pub(crate) fn write_stdout(bytes: &[u8]) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

pub(crate) fn command_prefix_len(line: &str) -> usize {
    line.find(' ').map(|i| i + 1).unwrap_or(line.len())
}

fn render_completion_items(items: &[&str], selected: usize, old_count: usize) {
    let sel = if items.is_empty() {
        0
    } else {
        selected % items.len()
    };
    let lines = old_count.max(items.len());
    if lines == 0 {
        write_stdout(b"\x1b[K");
        return;
    }
    let mut out = String::from("\x1b[K");
    // CUD (\x1b[B) does not scroll at the bottom margin, so when the prompt
    // sits on the last visible row the completion items would overwrite the
    // input line. LF does scroll, so use it to reserve space first and then
    // jump the cursor back up to the input row.
    for _ in 0..lines {
        out.push('\n');
    }
    out.push_str(&format!("\x1b[{lines}A"));
    out.push_str("\x1b7");

    for i in 0..lines {
        out.push_str("\x1b[B\x1b[2K\r");
        if i < items.len() {
            if i == sel {
                out.push_str("\x1b[34m");
                out.push_str(items[i]);
                out.push_str("\x1b[39m");
            } else {
                out.push_str("\x1b[2m");
                out.push_str(items[i]);
                out.push_str("\x1b[22m");
            }
        }
    }

    out.push_str("\x1b8");
    write_stdout(out.as_bytes());
}

// The candidates a completion popup offers for the current input, plus the
// command token used to splice a chosen candidate back into the input line.
// `command` is `None` for top-level command-name completion, where each
// candidate is itself the full input line.
struct Completion {
    items: Vec<String>,
    command: Option<&'static str>,
}

impl Completion {
    fn input_for(&self, item: &str) -> String {
        match self.command {
            Some(command) => format!("{command} {item}"),
            None => item.to_string(),
        }
    }
}

// Resolve the completion popup for `input`, or `None` when no chatsh command is
// being completed. Covers `/connect` and `/model` argument completion as well as
// top-level command-name completion (`/`, `/ch`, ...).
fn resolve_completion(input: &str, cached_models: &[String]) -> Option<Completion> {
    if let Some(arg) = input.strip_prefix("/connect ") {
        let items = complete_providers(arg).into_iter().map(String::from).collect();
        Some(Completion { items, command: Some("/connect") })
    } else if let Some(arg) = input.strip_prefix("/reauth ") {
        let items = complete_providers(arg).into_iter().map(String::from).collect();
        Some(Completion { items, command: Some("/reauth") })
    } else if let Some(arg) = input.strip_prefix("/model ") {
        let items = cached_models
            .iter()
            .filter(|m| m.starts_with(arg))
            .cloned()
            .collect();
        Some(Completion { items, command: Some("/model") })
    } else if input.starts_with('/') {
        let items = complete(input).iter().map(|c| c.name.to_string()).collect();
        Some(Completion { items, command: None })
    } else {
        None
    }
}

pub(crate) fn redraw_completion(
    input_line: &str,
    selected: usize,
    old_count: usize,
    cached_models: &[String],
) -> usize {
    let items = match resolve_completion(input_line, cached_models) {
        Some(completion) => completion.items,
        None => Vec::new(),
    };
    let items_ref: Vec<&str> = items.iter().map(String::as_str).collect();
    render_completion_items(&items_ref, selected, old_count);
    items.len()
}

// Index of the completion candidate to highlight after a navigation keypress.
// `fresh` is true on the first keypress, before any candidate has been applied:
// it selects the candidate at the leading edge of the list (top for `forward`,
// bottom otherwise) instead of skipping past it. Requires `len > 0`.
fn next_completion_index(selected: usize, len: usize, fresh: bool, forward: bool) -> usize {
    if fresh {
        if forward {
            0
        } else {
            len - 1
        }
    } else if forward {
        (selected + 1) % len
    } else {
        (selected + len - 1) % len
    }
}

// Move the completion selection one step in the given direction and splice the
// chosen candidate into the input line. Handles every command that offers
// autocompletion: `/connect` and `/model` arguments and top-level command names.
//
// Returns `true` when a completion popup is active and the keypress was
// consumed, or `false` when there is nothing to complete so the caller can fall
// back to history navigation.
pub(crate) fn step_completion(
    input_line: &mut String,
    tab_prefix: &mut Option<String>,
    selected: &mut usize,
    completion_count: &mut usize,
    cached_models: &[String],
    forward: bool,
) -> bool {
    let fresh = tab_prefix.is_none();
    let prefix = tab_prefix.clone().unwrap_or_else(|| input_line.clone());
    let completion = match resolve_completion(&prefix, cached_models) {
        Some(completion) if !completion.items.is_empty() => completion,
        _ => return false,
    };
    let idx = next_completion_index(*selected, completion.items.len(), fresh, forward);

    *tab_prefix = Some(prefix);
    let new_input = completion.input_for(&completion.items[idx]);
    if new_input != *input_line {
        redraw_input(input_line.len(), &new_input);
        *input_line = new_input;
    }
    let items_ref: Vec<&str> = completion.items.iter().map(String::as_str).collect();
    render_completion_items(&items_ref, idx, *completion_count);
    *completion_count = completion.items.len();
    *selected = idx;
    true
}

fn colorize_command(input: &str) -> String {
    if !input.starts_with('/') {
        return input.to_string();
    }
    match input.find(' ') {
        Some(idx) => format!(
            "\x1b[1;36m{}\x1b[22;39m{}\x1b[0m",
            &input[..idx],
            &input[idx..]
        ),
        None => format!("\x1b[1;36m{}\x1b[0m", input),
    }
}

pub(crate) fn redraw_input(prev_len: usize, input: &str) {
    let mut out = String::new();
    if prev_len > 0 {
        out.push_str(&format!("\x1b[{prev_len}D"));
    }
    out.push_str("\x1b[K");
    out.push_str(&colorize_command(input));
    write_stdout(out.as_bytes());
}

pub(crate) fn history_back(
    input_line: &mut String,
    history: &[String],
    history_pos: &mut Option<usize>,
    pending_buffer: &mut String,
) {
    if history.is_empty() {
        return;
    }
    let new_pos = match *history_pos {
        None => {
            *pending_buffer = input_line.clone();
            history.len() - 1
        }
        Some(0) => return,
        Some(p) => p - 1,
    };
    *history_pos = Some(new_pos);
    let new_input = history[new_pos].clone();
    redraw_input(input_line.len(), &new_input);
    *input_line = new_input;
}

pub(crate) fn history_forward(
    input_line: &mut String,
    history: &[String],
    history_pos: &mut Option<usize>,
    pending_buffer: &mut String,
) {
    let new_input = match *history_pos {
        None => return,
        Some(p) if p + 1 < history.len() => {
            *history_pos = Some(p + 1);
            history[p + 1].clone()
        }
        Some(_) => {
            *history_pos = None;
            std::mem::take(pending_buffer)
        }
    };
    redraw_input(input_line.len(), &new_input);
    *input_line = new_input;
}

pub(crate) fn clear_completion(count: usize) {
    if count == 0 {
        write_stdout(b"\x1b[K");
        return;
    }
    let mut out = String::from("\x1b7\x1b[K");
    for _ in 0..count {
        out.push_str("\x1b[B\x1b[2K");
    }
    out.push_str("\x1b8");
    write_stdout(out.as_bytes());
}

/// Show a dim argument-hint line below the prompt (e.g. `<query>`) when a
/// command requires an argument but the user pressed Enter without one.
/// Returns 1 so the caller can pass it to `clear_completion` later.
pub(crate) fn show_arg_hint(hint: &str, old_count: usize) -> usize {
    let lines = old_count.max(1);
    let mut out = String::from("\x1b[K");
    for _ in 0..lines {
        out.push('\n');
    }
    out.push_str(&format!("\x1b[{lines}A\x1b7"));
    out.push_str("\x1b[B\x1b[2K\r");
    out.push_str(&format!("\x1b[2m{hint}\x1b[22m"));
    for _ in 1..lines {
        out.push_str("\x1b[B\x1b[2K");
    }
    out.push_str("\x1b8");
    write_stdout(out.as_bytes());
    1
}

pub(crate) fn print_greeting() {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let body = format!(
        "\r\n\x1b[1;36mchatsh\x1b[0m v{VERSION}\r\n\
         AI chat embedded inline in your shell session.\r\n\
         \r\n\
         \x1b[2mType / to start a command.\x1b[22m\r\n\r\n"
    );
    let _ = out.write_all(body.as_bytes());
    let _ = out.flush();
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_completion_connect_lists_providers() {
        let completion = resolve_completion("/connect ", &[]).unwrap();
        assert_eq!(completion.command, Some("/connect"));
        assert_eq!(completion.items, vec!["anthropic", "github-copilot", "openai", "z.ai-coding-plan"]);
    }

    #[test]
    fn test_resolve_completion_connect_filters_by_prefix() {
        let completion = resolve_completion("/connect g", &[]).unwrap();
        assert_eq!(completion.items, vec!["github-copilot"]);
    }

    #[test]
    fn test_resolve_completion_model_filters_cached() {
        let models = vec!["gpt-4o".to_string(), "glm-4.6".to_string(), "o3".to_string()];
        let completion = resolve_completion("/model g", &models).unwrap();
        assert_eq!(completion.command, Some("/model"));
        assert_eq!(completion.items, vec!["gpt-4o", "glm-4.6"]);
    }

    #[test]
    fn test_resolve_completion_command_names() {
        let completion = resolve_completion("/c", &[]).unwrap();
        assert_eq!(completion.command, None);
        assert_eq!(completion.items, vec!["/chat", "/connect"]);
    }

    #[test]
    fn test_resolve_completion_none_for_non_command() {
        assert!(resolve_completion("ls -la", &[]).is_none());
        assert!(resolve_completion("hello world", &[]).is_none());
    }

    #[test]
    fn test_completion_input_for() {
        let arg = Completion { items: vec![], command: Some("/connect") };
        assert_eq!(arg.input_for("github-copilot"), "/connect github-copilot");
        let command = Completion { items: vec![], command: None };
        assert_eq!(command.input_for("/chat"), "/chat");
    }

    #[test]
    fn test_next_completion_index_fresh_enters_at_edge() {
        // First keypress selects the leading edge without skipping.
        assert_eq!(next_completion_index(0, 4, true, true), 0);
        assert_eq!(next_completion_index(0, 4, true, false), 3);
    }

    #[test]
    fn test_next_completion_index_forward_wraps() {
        assert_eq!(next_completion_index(0, 4, false, true), 1);
        assert_eq!(next_completion_index(3, 4, false, true), 0);
    }

    #[test]
    fn test_next_completion_index_backward_wraps() {
        assert_eq!(next_completion_index(1, 4, false, false), 0);
        assert_eq!(next_completion_index(0, 4, false, false), 3);
    }
}
