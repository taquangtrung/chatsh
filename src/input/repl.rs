//! Renders and edits buffered chatsh command input.
//!
//! This module owns completion, history, and inline prompt redraw behavior.

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

/// Split a `provider/model` completion item into its `(provider, model)` parts.
/// When there is no `/`, the provider is empty and the whole string is the model.
fn split_provider_model(item: &str) -> (&str, &str) {
    match item.split_once('/') {
        Some((provider, model)) => (provider, model),
        None => ("", item),
    }
}

fn render_completion_items(items: &[&str], selected: usize, old_count: usize) -> usize {
    let sel = if items.is_empty() {
        0
    } else {
        selected % items.len()
    };
    let lines = old_count.max(items.len());
    if lines == 0 {
        write_stdout(b"\x1b[K");
        return 0;
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
    items.len()
}

// Render a grouped model picker: items are `(provider/model, rate_label)` pairs.
// Headers (provider names) are inserted automatically and are not selectable.
// Providers are separated by a blank line. `selected` indexes into `items`.
// Returns the total visual line count for the caller to store as `completion_count`.
fn render_model_completion(
    items: &[(&str, Option<&str>)],
    selected: usize,
    old_visual_lines: usize,
    current_model: Option<&str>,
) -> usize {
    if items.is_empty() && old_visual_lines == 0 {
        write_stdout(b"\x1b[K");
        return 0;
    }

    // Build visual rows.
    enum Row<'a> {
        Blank,
        Header(&'a str),
        Item {
            display: &'a str,
            rate: Option<&'a str>,
            idx: usize,
            is_current: bool,
        },
    }

    let mut rows: Vec<Row<'_>> = Vec::new();
    let mut last_provider = "";
    let mut first = true;
    for (i, (item, rate)) in items.iter().enumerate() {
        let (provider, display) = split_provider_model(item);
        if provider != last_provider {
            if !first {
                rows.push(Row::Blank);
            }
            rows.push(Row::Header(provider));
            last_provider = provider;
            first = false;
        }
        let is_current = current_model.is_some_and(|c| c == *item);
        rows.push(Row::Item {
            display,
            rate: *rate,
            idx: i,
            is_current,
        });
    }

    let visual_lines = rows.len();
    let total = old_visual_lines.max(visual_lines);

    let mut out = String::from("\x1b[K");
    for _ in 0..total {
        out.push('\n');
    }
    out.push_str(&format!("\x1b[{total}A"));
    out.push_str("\x1b7");

    for row in &rows {
        out.push_str("\x1b[B\x1b[2K\r");
        match row {
            Row::Blank => {}
            Row::Header(name) => {
                out.push_str("\x1b[2;3m ");
                out.push_str(name);
                out.push_str("\x1b[0m");
            }
            Row::Item {
                display,
                rate,
                idx,
                is_current,
            } => {
                let marker = if *is_current { " ✓ " } else { "   " };
                if *idx == selected {
                    out.push_str("\x1b[34m");
                    out.push_str(marker);
                    out.push_str(display);
                    if let Some(r) = rate {
                        out.push_str("\x1b[2m [");
                        out.push_str(r);
                        out.push(']');
                    }
                    out.push_str("\x1b[0m");
                } else {
                    out.push_str("\x1b[2m");
                    out.push_str(marker);
                    out.push_str(display);
                    if let Some(r) = rate {
                        out.push_str(" [");
                        out.push_str(r);
                        out.push(']');
                    }
                    out.push_str("\x1b[22m");
                }
            }
        }
    }
    // Blank out any leftover lines from a previous longer list.
    for _ in visual_lines..total {
        out.push_str("\x1b[B\x1b[2K\r");
    }

    out.push_str("\x1b8");
    write_stdout(out.as_bytes());
    visual_lines
}

// The candidates a completion popup offers for the current input, plus the
// command token used to splice a chosen candidate back into the input line.
// `command` is `None` for top-level command-name completion, where each
// candidate is itself the full input line. `grouped` requests the provider-
// grouped model picker renderer instead of the plain flat list.
// For grouped completions, `rates` is a parallel slice of rate labels (same
// length as `items`); for flat completions it is empty.
struct Completion {
    items: Vec<String>,
    rates: Vec<Option<String>>,
    command: Option<&'static str>,
    grouped: bool,
}

impl Completion {
    fn input_for(&self, item: &str) -> String {
        match self.command {
            Some(command) => format!("{command} {item}"),
            None => item.to_string(),
        }
    }

    /// Draw this completion popup, choosing the grouped model picker or the flat
    /// list renderer, and return the visual line count to store as the count.
    fn render(&self, selected: usize, old_count: usize, current_model: Option<&str>) -> usize {
        if self.grouped {
            let items_ref: Vec<(&str, Option<&str>)> = self
                .items
                .iter()
                .zip(self.rates.iter())
                .map(|(s, r)| (s.as_str(), r.as_deref()))
                .collect();
            render_model_completion(&items_ref, selected, old_count, current_model)
        } else {
            let items_ref: Vec<&str> = self.items.iter().map(String::as_str).collect();
            render_completion_items(&items_ref, selected, old_count)
        }
    }
}

// Build a provider-list completion for a single command (e.g. `/connect` or
// `/reauth`). Matches both the exact command name and the `command <arg>` form.
fn provider_completion_for(command: &'static str, input: &str) -> Option<Completion> {
    let prefix = format!("{command} ");
    let arg = if let Some(a) = input.strip_prefix(prefix.as_str()) {
        a
    } else if input == command {
        // Exact command name with no space yet: show all providers immediately.
        ""
    } else {
        return None;
    };
    let items = complete_providers(arg)
        .into_iter()
        .map(String::from)
        .collect();
    Some(Completion {
        items,
        rates: Vec::new(),
        command: Some(command),
        grouped: false,
    })
}

// Resolve the completion popup for `input`, or `None` when no chatsh command is
// being completed. Covers `/connect` and `/model` argument completion as well as
// top-level command-name completion (`/`, `/ch`, ...).
fn resolve_completion(
    input: &str,
    cached_models: &[(String, Option<String>)],
) -> Option<Completion> {
    if let Some(c) = provider_completion_for("/connect", input) {
        return Some(c);
    }
    if let Some(c) = provider_completion_for("/reauth", input) {
        return Some(c);
    }
    if let Some(arg) = input.strip_prefix("/model ") {
        let (items, rates) = cached_models
            .iter()
            .filter(|(m, _)| {
                // Match `provider/model` prefix OR bare model name after the `/`.
                m.starts_with(arg)
                    || m.rsplit_once('/')
                        .map(|(_, model)| model.starts_with(arg))
                        .unwrap_or(false)
            })
            .map(|(m, r)| (m.clone(), r.clone()))
            .unzip();
        Some(Completion {
            items,
            rates,
            command: Some("/model"),
            grouped: true,
        })
    } else if input == "/model" && !cached_models.is_empty() {
        // Exact command name with cache already populated: show all models immediately.
        let (items, rates) = cached_models
            .iter()
            .map(|(m, r)| (m.clone(), r.clone()))
            .unzip();
        Some(Completion {
            items,
            rates,
            command: Some("/model"),
            grouped: true,
        })
    } else if input.starts_with('/') {
        let items = complete(input).iter().map(|c| c.name.to_string()).collect();
        Some(Completion {
            items,
            rates: Vec::new(),
            command: None,
            grouped: false,
        })
    } else {
        None
    }
}

pub(crate) fn redraw_completion(
    input_line: &str,
    selected: usize,
    old_count: usize,
    cached_models: &[(String, Option<String>)],
    current_model: Option<&str>,
) -> usize {
    match resolve_completion(input_line, cached_models) {
        Some(completion) => completion.render(selected, old_count, current_model),
        None => render_completion_items(&[], 0, old_count),
    }
}

// Index of the completion candidate to highlight after a navigation keypress.
// `fresh` is true on the first keypress, before any candidate has been applied:
// it selects the candidate at the leading edge of the list (top for `forward`,
// bottom otherwise) instead of skipping past it. Requires `len > 0`.
fn next_completion_index(selected: usize, len: usize, fresh: bool, forward: bool) -> usize {
    if fresh {
        if forward { 0 } else { len - 1 }
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
    cached_models: &[(String, Option<String>)],
    forward: bool,
    cursor_col: usize,
    current_model: Option<&str>,
) -> bool {
    let fresh = tab_prefix.is_none();
    let prefix = tab_prefix.clone().unwrap_or_else(|| input_line.clone());
    let completion = match resolve_completion(&prefix, cached_models) {
        Some(completion) if !completion.items.is_empty() => completion,
        _ => return false,
    };
    let idx = next_completion_index(*selected, completion.items.len(), fresh, forward);

    let new_input = completion.input_for(&completion.items[idx]);
    if new_input != *input_line {
        redraw_input(cursor_col, &new_input, new_input.len());
        *input_line = new_input;
    }

    // When a command NAME is completed (not an argument) and there is only one
    // candidate, the input is now the full command name with no trailing space.
    // Release the prefix lock so the very next Tab resolves argument completions
    // from that new input instead of cycling the name.
    if completion.command.is_none() && completion.items.len() == 1 {
        *tab_prefix = None;
    } else {
        *tab_prefix = Some(prefix);
    }

    *completion_count = completion.render(idx, *completion_count, current_model);
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

pub(crate) fn redraw_input(cursor_col: usize, input: &str, cursor_pos: usize) {
    let mut out = String::new();
    if cursor_col > 0 {
        out.push_str(&format!("\x1b[{cursor_col}D"));
    }
    out.push_str("\x1b[K");
    out.push_str(&colorize_command(input));
    if cursor_pos < input.len() {
        let back = input.len() - cursor_pos;
        out.push_str(&format!("\x1b[{back}D"));
    }
    write_stdout(out.as_bytes());
}

pub(crate) fn history_back(
    input_line: &mut String,
    history: &[String],
    history_pos: &mut Option<usize>,
    pending_buffer: &mut String,
    cursor_col: usize,
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
    redraw_input(cursor_col, &new_input, new_input.len());
    *input_line = new_input;
}

pub(crate) fn history_forward(
    input_line: &mut String,
    history: &[String],
    history_pos: &mut Option<usize>,
    pending_buffer: &mut String,
    cursor_col: usize,
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
    redraw_input(cursor_col, &new_input, new_input.len());
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
/// Displays a dim prompt below a blank line asking the user to supply the missing
/// argument. Returns 2 so the caller can pass it to `clear_completion` later.
pub(crate) fn show_arg_hint(hint: &str, old_count: usize) -> usize {
    // Reserve at least 2 lines: one blank separator, one for the message.
    let lines = old_count.max(2);
    let mut out = String::from("\x1b[K");
    for _ in 0..lines {
        out.push('\n');
    }
    out.push_str(&format!("\x1b[{lines}A\x1b7"));
    // Line 1: blank separator.
    out.push_str("\x1b[B\x1b[2K\r");
    // Line 2: dim hint message.
    out.push_str("\x1b[B\x1b[2K\r");
    out.push_str(&format!("\x1b[2m{hint}\x1b[22m"));
    // Clear any extra lines left over from a previous larger popup.
    for _ in 2..lines {
        out.push_str("\x1b[B\x1b[2K");
    }
    out.push_str("\x1b8");
    write_stdout(out.as_bytes());
    2
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
    fn test_resolve_completion_connect_with_space_lists_providers() {
        let completion = resolve_completion("/connect ", &[]).unwrap();
        assert_eq!(completion.command, Some("/connect"));
        assert_eq!(
            completion.items,
            vec!["anthropic", "github-copilot", "openai", "z.ai-coding-plan"]
        );
    }

    #[test]
    fn test_resolve_completion_connect_exact_lists_providers() {
        // Typing "/connect" (no trailing space) + Tab should show providers immediately.
        let completion = resolve_completion("/connect", &[]).unwrap();
        assert_eq!(completion.command, Some("/connect"));
        assert_eq!(
            completion.items,
            vec!["anthropic", "github-copilot", "openai", "z.ai-coding-plan"]
        );
    }

    #[test]
    fn test_resolve_completion_reauth_exact_lists_providers() {
        // Typing "/reauth" (no trailing space) + Tab should show providers immediately.
        let completion = resolve_completion("/reauth", &[]).unwrap();
        assert_eq!(completion.command, Some("/reauth"));
        assert_eq!(
            completion.items,
            vec!["anthropic", "github-copilot", "openai", "z.ai-coding-plan"]
        );
    }

    #[test]
    fn test_resolve_completion_model_exact_with_cache_lists_all() {
        // Typing "/model" (no trailing space) + Tab should show all models when cache populated.
        let ms = models(&["openai/gpt-4o", "anthropic/claude-3-5"]);
        let completion = resolve_completion("/model", &ms).unwrap();
        assert_eq!(completion.command, Some("/model"));
        assert_eq!(
            completion.items,
            vec!["openai/gpt-4o", "anthropic/claude-3-5"]
        );
        assert!(completion.grouped);
    }

    #[test]
    fn test_resolve_completion_model_exact_empty_cache_falls_through_to_command_name() {
        // With empty cache, "/model" falls through to command-name completion (no regression).
        let completion = resolve_completion("/model", &[]).unwrap();
        assert_eq!(completion.command, None);
        assert_eq!(completion.items, vec!["/model"]);
    }

    fn models(ids: &[&str]) -> Vec<(String, Option<String>)> {
        ids.iter().map(|s| (s.to_string(), None)).collect()
    }

    #[test]
    fn test_resolve_completion_model_filters_cached() {
        // Plain model IDs still match by prefix.
        let ms = models(&["gpt-4o", "glm-4.6", "o3"]);
        let completion = resolve_completion("/model g", &ms).unwrap();
        assert_eq!(completion.command, Some("/model"));
        assert_eq!(completion.items, vec!["gpt-4o", "glm-4.6"]);
    }

    #[test]
    fn test_resolve_completion_model_provider_slash_format() {
        let ms = models(&[
            "openai/gpt-4o",
            "openai/gpt-4.1",
            "z.ai-coding-plan/glm-5.1",
        ]);
        // Full provider/model prefix match.
        let c = resolve_completion("/model openai/gpt", &ms).unwrap();
        assert_eq!(c.items, vec!["openai/gpt-4o", "openai/gpt-4.1"]);
        // Bare model name matches across providers.
        let c2 = resolve_completion("/model gpt", &ms).unwrap();
        assert_eq!(c2.items, vec!["openai/gpt-4o", "openai/gpt-4.1"]);
    }

    #[test]
    fn test_resolve_completion_model_carries_rates() {
        let ms: Vec<(String, Option<String>)> = vec![
            ("github-copilot/gpt-4o".to_string(), None),
            ("github-copilot/o3-mini".to_string(), Some("2x".to_string())),
        ];
        let c = resolve_completion("/model github-copilot/", &ms).unwrap();
        assert_eq!(
            c.items,
            vec!["github-copilot/gpt-4o", "github-copilot/o3-mini"]
        );
        assert_eq!(c.rates, vec![None, Some("2x".to_string())]);
    }

    #[test]
    fn test_resolve_completion_none_for_non_command() {
        assert!(resolve_completion("ls -la", &[]).is_none());
        assert!(resolve_completion("hello world", &[]).is_none());
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

    fn run_step(input: &str, tab_prefix: &mut Option<String>, count: &mut usize) -> String {
        let mut line = input.to_string();
        let mut selected = 0;
        step_completion(
            &mut line,
            tab_prefix,
            &mut selected,
            count,
            &[],
            true,
            0,
            None,
        );
        line
    }

    #[test]
    fn test_step_completion_partial_then_arg_tab() {
        // Simulates: type "/rea", Tab → "/reauth", Tab → "/reauth anthropic"
        let mut tab_prefix: Option<String> = None;
        let mut count = 0;

        // First Tab on "/rea": completes to "/reauth" (no trailing space).
        let result1 = run_step("/rea", &mut tab_prefix, &mut count);
        assert_eq!(result1, "/reauth");
        // tab_prefix must be cleared (single-match command name) so next Tab
        // resolves argument completions from the new input_line.
        assert!(
            tab_prefix.is_none(),
            "tab_prefix should be reset after single-match command-name completion"
        );

        // Second Tab on "/reauth": exact-match resolves provider completions.
        let result2 = run_step("/reauth", &mut tab_prefix, &mut count);
        assert_eq!(result2, "/reauth anthropic");
    }

    #[test]
    fn test_step_completion_multi_match_prefix_stays_locked() {
        // Simulates: type "/c", Tab → "/chat", Tab → "/connect" (cycles, not advanced).
        let mut tab_prefix: Option<String> = None;
        let mut count = 0;

        // First Tab: multiple matches (/chat, /connect) — lock is kept so cycling works.
        let result1 = run_step("/c", &mut tab_prefix, &mut count);
        assert_eq!(result1, "/chat");
        assert!(
            tab_prefix.is_some(),
            "tab_prefix should stay set when multiple command names match"
        );

        // Second Tab: cycles to /connect.
        let result2 = run_step("/chat", &mut tab_prefix, &mut count);
        // The prefix is still "/c", so completion resolves to the second item.
        assert_eq!(result2, "/connect");
    }
}
