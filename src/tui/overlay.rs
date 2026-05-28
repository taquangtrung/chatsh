use std::io::{self, Write};

use anyhow::Result;
use crossterm::cursor::MoveTo;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{
    self, Clear, ClearType, DisableLineWrap, EnableLineWrap, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::execute;
use futures::{pin_mut, FutureExt, StreamExt};

use crate::trigger::engine;
use crate::types::ChatEvent;

const BORDER_HORIZONTAL: char = '\u{2500}';
const BORDER_TOP_LEFT: char = '\u{250c}';
const BORDER_TOP_RIGHT: char = '\u{2510}';
const BORDER_BOTTOM_LEFT: char = '\u{2514}';
const BORDER_BOTTOM_RIGHT: char = '\u{2518}';
const BORDER_VERTICAL: char = '\u{2502}';
const PROMPT_MARKER: char = '\u{25b8}';
const SPINNER_FRAMES: &[&str] = &["\u{28b7}", "\u{28d9}", "\u{2899}", "\u{284e}"];
const STATUS_CONTINUATION: &str = "\u{25cc} esc to close";

const POLL_TIMEOUT_MS: u64 = 100;

pub struct ChatOverlay {
    response: String,
    cancelled: bool,
}

impl ChatOverlay {
    pub fn new() -> Self {
        Self {
            response: String::new(),
            cancelled: false,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn into_response(self) -> String {
        self.response
    }

    pub fn render_hint() -> String {
        format!("\r\n{}\r\n", engine::hint_text())
    }

    pub async fn run(
        &mut self,
        query: &str,
        stream: futures::stream::BoxStream<'static, ChatEvent>,
    ) -> Result<()> {
        let mut stdout = io::stdout();
        let (cols, rows) = terminal::size()?;

        execute!(
            stdout,
            EnterAlternateScreen,
            DisableLineWrap,
            Clear(ClearType::All)
        )?;

        let result = self.run_inner(query, stream, cols, rows, &mut stdout).await;

        execute!(stdout, LeaveAlternateScreen, EnableLineWrap)?;
        stdout.flush()?;

        result
    }

    async fn run_inner(
        &mut self,
        query: &str,
        stream: futures::stream::BoxStream<'static, ChatEvent>,
        cols: u16,
        rows: u16,
        stdout: &mut io::Stdout,
    ) -> Result<()> {
        pin_mut!(stream);

        let mut spinner_idx = 0usize;
        let mut is_thinking = false;
        let mut done = false;

        self.draw(query, cols, rows, stdout, spinner_idx, is_thinking, done)?;
        stdout.flush()?;

        loop {
            if done {
                if event::poll(std::time::Duration::from_millis(50))? {
                    let ev = event::read()?;
                    if let Event::Key(key) = ev {
                        if key.kind == KeyEventKind::Press {
                            match key.code {
                                KeyCode::Esc | KeyCode::Enter => break,
                                _ => {}
                            }
                        }
                    }
                }
                continue;
            }

            let has_event = event::poll(std::time::Duration::from_millis(POLL_TIMEOUT_MS))?;

            if has_event {
                let ev = event::read()?;
                match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                        KeyCode::Esc => {
                            self.cancelled = true;
                            break;
                        }
                        KeyCode::Char('c')
                            if key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            self.cancelled = true;
                            break;
                        }
                        _ => {}
                    },
                    Event::Resize(new_cols, new_rows) => {
                        let _ = self.draw(
                            query,
                            new_cols,
                            new_rows,
                            stdout,
                            spinner_idx,
                            is_thinking,
                            done,
                        );
                        let _ = stdout.flush();
                    }
                    _ => {}
                }
            }

            if !done {
                match stream.next().now_or_never() {
                    Some(Some(ChatEvent::Token { text })) => {
                        is_thinking = false;
                        self.response.push_str(&text);
                        spinner_idx = (spinner_idx + 1) % SPINNER_FRAMES.len();
                        self.draw(query, cols, rows, stdout, spinner_idx, is_thinking, done)?;
                        stdout.flush()?;
                    }
                    Some(Some(ChatEvent::Thinking { text })) => {
                        is_thinking = true;
                        self.response.push_str(&text);
                        spinner_idx = (spinner_idx + 1) % SPINNER_FRAMES.len();
                        self.draw(query, cols, rows, stdout, spinner_idx, is_thinking, done)?;
                        stdout.flush()?;
                    }
                    Some(Some(ChatEvent::Done)) | None => {
                        done = true;
                        self.draw(query, cols, rows, stdout, 0, false, done)?;
                        stdout.flush()?;
                    }
                    Some(Some(ChatEvent::Error { message })) => {
                        self.response = format!("Error: {message}");
                        done = true;
                        self.draw(query, cols, rows, stdout, 0, false, done)?;
                        stdout.flush()?;
                    }
                    Some(None) => {
                        done = true;
                        self.draw(query, cols, rows, stdout, 0, false, done)?;
                        stdout.flush()?;
                    }
                }
            }
        }

        Ok(())
    }

    fn draw(
        &self,
        query: &str,
        cols: u16,
        rows: u16,
        stdout: &mut io::Stdout,
        spinner_idx: usize,
        is_thinking: bool,
        done: bool,
    ) -> Result<()> {
        execute!(stdout, Clear(ClearType::All))?;

        let content_width = cols.saturating_sub(2) as usize;

        let half = (cols as usize).saturating_sub(12) / 2;
        let banner_left = format!(
            "{}{}",
            BORDER_TOP_LEFT,
            repeat_char(BORDER_HORIZONTAL, half)
        );
        let banner_right = format!(
            "{} chatsh {}{}",
            BORDER_HORIZONTAL,
            BORDER_HORIZONTAL,
            BORDER_TOP_RIGHT
        );
        execute!(
            stdout,
            MoveTo(0, 0),
            Print(banner_left),
            SetForegroundColor(Color::Cyan),
            Print("chatsh"),
            ResetColor,
            Print(banner_right)
        )?;

        let query_display = truncate_str(query, content_width.saturating_sub(3));
        execute!(
            stdout,
            MoveTo(0, 1),
            Print(BORDER_VERTICAL),
            Print(" "),
            SetForegroundColor(Color::Green),
            Print(PROMPT_MARKER),
            Print(" "),
            Print(&query_display),
            ResetColor,
            pad_to(query_display.len() + 3, cols as usize - 1),
            Print(BORDER_VERTICAL)
        )?;

        execute!(
            stdout,
            MoveTo(0, 2),
            Print(BORDER_VERTICAL),
            Print(repeat_char(BORDER_HORIZONTAL, cols as usize - 2)),
            Print(BORDER_VERTICAL)
        )?;

        let response_start = 3u16;
        let response_rows = rows.saturating_sub(5) as usize;
        let wrapped = wrap_text(&self.response, content_width.saturating_sub(1));

        for i in 0..response_rows {
            let line = wrapped.get(i).map(|s| s.as_str()).unwrap_or("");
            let display = truncate_str(line, content_width.saturating_sub(1));
            execute!(
                stdout,
                MoveTo(0, response_start + i as u16),
                Print(BORDER_VERTICAL),
                Print(" "),
                Print(&display),
                pad_to(display.len() + 1, cols as usize - 1),
                Print(BORDER_VERTICAL)
            )?;
        }

        let status_row = rows.saturating_sub(2);
        execute!(
            stdout,
            MoveTo(0, status_row),
            Print(BORDER_BOTTOM_LEFT),
            Print(repeat_char(BORDER_HORIZONTAL, cols as usize - 2)),
            Print(BORDER_BOTTOM_RIGHT)
        )?;

        let status = if done {
            STATUS_CONTINUATION.to_string()
        } else {
            let spinner = SPINNER_FRAMES[spinner_idx];
            if is_thinking {
                format!("{spinner} thinking...")
            } else if self.response.is_empty() {
                format!("{spinner} waiting...")
            } else {
                format!("{spinner} {STATUS_CONTINUATION}")
            }
        };
        execute!(
            stdout,
            MoveTo(2, status_row),
            SetForegroundColor(Color::DarkGrey),
            Print(truncate_str(&status, cols as usize - 4)),
            ResetColor
        )?;

        Ok(())
    }
}

fn repeat_char(c: char, n: usize) -> String {
    c.to_string().repeat(n)
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(1)).collect();
        format!("{truncated}\u{2026}")
    }
}

fn pad_to(current_len: usize, target_len: usize) -> Print<String> {
    let padding = target_len.saturating_sub(current_len);
    Print(" ".repeat(padding))
}

fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![];
    }
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                current = word.to_string();
            } else if current.len() + 1 + word.len() <= max_width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(current);
                current = word.to_string();
            }
        }
        lines.push(current);
    }
    lines
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_text_short() {
        assert_eq!(wrap_text("hello world", 20), vec!["hello world"]);
    }

    #[test]
    fn test_wrap_text_wraps() {
        assert_eq!(wrap_text("aaa bbb ccc", 5), vec!["aaa", "bbb", "ccc"]);
    }

    #[test]
    fn test_wrap_text_newlines() {
        assert_eq!(wrap_text("hello\n\nworld", 20), vec!["hello", "", "world"]);
        assert_eq!(wrap_text("hello\nworld", 20), vec!["hello", "world"]);
    }

    #[test]
    fn test_truncate_str_short() {
        assert_eq!(truncate_str("hi", 10), "hi");
    }

    #[test]
    fn test_truncate_str_long() {
        let result = truncate_str("abcdefghij", 5);
        assert_eq!(result, "abcd\u{2026}");
    }

    #[test]
    fn test_repeat_char() {
        assert_eq!(repeat_char('x', 3), "xxx");
    }

    #[test]
    fn test_wrap_text_zero_width() {
        assert_eq!(wrap_text("hello", 0), Vec::<String>::new());
    }

    #[test]
    fn test_overlay_new() {
        let overlay = ChatOverlay::new();
        assert!(!overlay.is_cancelled());
        assert!(overlay.into_response().is_empty());
    }

    #[test]
    fn test_render_hint_contains_chat() {
        let hint = ChatOverlay::render_hint();
        assert!(hint.contains("/chat"));
    }
}
