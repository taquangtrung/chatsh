//! Runs the shell bridge and chatsh command loop.
//!
//! Buffered `/` commands are edited locally before they are executed or flushed.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;

use crate::chat::handle_chat;
use crate::chat::commands::{
    build_registry, fetch_all_model_completions, handle_connect, handle_model, handle_reauth,
};
use crate::input::repl::{
    clear_completion, command_prefix_len, history_back, history_forward, print_greeting,
    redraw_completion, redraw_input, show_arg_hint, step_completion, write_stdout,
};
use crate::input::trigger::{
    Command, InputAction, classify_input, command_for_line, complete, complete_providers,
    hint_text,
};
use crate::ai::ProviderRegistry;
use crate::shell::{ContextBuffer, PtyBridge};
use crate::storage::{keyring, Config, Conversation};

const ESC: u8 = 27;
const DEL: u8 = 127;
const STDIN_BUF_SIZE: usize = 256;
const STARTUP_SETTLE: Duration = Duration::from_millis(50);

/// Number of characters to remove from the end of a buffered `/`-command line
/// for a word-delete (Ctrl+Backspace / Alt+Backspace / Ctrl+W).
///
/// • While editing a non-empty argument (a space followed by text): removes
///   trailing blanks + the last argument word, but never touches the command name.
/// • While still in the command name, or once the arguments are blank (e.g.
///   `/chat ` with only a trailing space): removes everything so the user lands
///   back at the bare-prompt prefix.
fn word_delete_chop(input_line: &str) -> usize {
    if let Some(space_pos) = input_line.find(' ') {
        let args = &input_line[space_pos + 1..];
        let trimmed = args.trim_end();
        if !trimmed.is_empty() {
            let word_len = trimmed.len() - trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
            return (args.len() - trimmed.len()) + word_len;
        }
        // No argument typed yet (just the command name plus trailing blanks):
        // fall through so the whole command is removed instead of getting stuck
        // on the blank trailing space.
    }
    // Still typing the command name (or just '/'): delete everything so the
    // user can start over with a shell command.
    input_line.len()
}

/// Resolves the shell command line that should be launched in the PTY.
pub fn resolve_shell(args: &[String]) -> Vec<String> {
    if args.is_empty() {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        vec![shell]
    } else {
        args.to_vec()
    }
}

fn extra_shell_env() -> Vec<(String, String)> {
    vec![("CHATSH_SESSION".to_string(), "1".to_string())]
}

#[derive(Clone, Copy)]
enum EscapeState {
    Normal,
    GotEsc,
    GotIntroducer(u8),
}

struct InputState {
    line: String,
    cursor: usize,
    selected: usize,
    tab_prefix: Option<String>,
    escape_state: EscapeState,
    flushed: bool,
    flushed_len: usize,
    cached_models: Vec<(String, Option<String>)>,
    completion_count: usize,
    history: Vec<String>,
    history_pos: Option<usize>,
    pending_buffer: String,
}

impl InputState {
    fn new() -> Self {
        Self {
            line: String::new(),
            cursor: 0,
            selected: 0,
            tab_prefix: None,
            escape_state: EscapeState::Normal,
            flushed: false,
            flushed_len: 0,
            cached_models: Vec::new(),
            completion_count: 0,
            history: Vec::new(),
            history_pos: None,
            pending_buffer: String::new(),
        }
    }

    fn is_buffered(&self) -> bool {
        self.line.starts_with('/')
    }

    fn refresh_completion(&mut self, model_id: Option<&str>) {
        self.completion_count = redraw_completion(
            &self.line,
            self.selected,
            self.completion_count,
            &self.cached_models,
            model_id,
        );
    }

    /// Escape from chatsh buffered mode to shell mode: flush the current
    /// `/`-command text to the PTY and reset all buffered-mode state.
    fn escape_to_shell(&mut self, pty: &mut PtyBridge) {
        let shell_input = self.line.clone();
        redraw_input(self.cursor, "", 0);
        clear_completion(self.completion_count);
        self.completion_count = 0;
        if !shell_input.is_empty() {
            let _ = pty.write(shell_input.as_bytes());
        }
        self.line.clear();
        self.cursor = 0;
        self.selected = 0;
        self.flushed = true;
        self.flushed_len = shell_input.len();
        self.tab_prefix = None;
        self.history_pos = None;
        self.pending_buffer.clear();
    }

    /// Navigate history or completion (Up/Down/Ctrl-N/P, ESC+p/n).
    ///
    /// When `try_completion_first` is `true`, attempts to step through the
    /// active completion list before falling back to history navigation.
    fn navigate(&mut self, forward: bool, try_completion_first: bool, model_id: Option<&str>) {
        let stepped = try_completion_first
            && step_completion(
                &mut self.line,
                &mut self.tab_prefix,
                &mut self.selected,
                &mut self.completion_count,
                &self.cached_models,
                forward,
                self.cursor,
                model_id,
            );
        if !stepped {
            if forward {
                history_forward(
                    &mut self.line,
                    &self.history,
                    &mut self.history_pos,
                    &mut self.pending_buffer,
                    self.cursor,
                );
            } else {
                history_back(
                    &mut self.line,
                    &self.history,
                    &mut self.history_pos,
                    &mut self.pending_buffer,
                    self.cursor,
                );
            }
            self.selected = 0;
            self.tab_prefix = None;
            self.refresh_completion(model_id);
        }
        self.cursor = self.line.len();
    }

    /// If the command requires an argument and none was provided, show the
    /// hint/completion and return `true` (caller should `continue` the event loop).
    fn check_missing_arg(&mut self, cmd: &Command, model_id: Option<&str>) -> bool {
        let has_args = self.line.len() > cmd.name.len()
            && !self.line[cmd.name.len()..].trim_start().is_empty();
        if cmd.takes_args && !has_args && cmd.requires_args {
            if !self.line.ends_with(' ') {
                self.line.push(' ');
                redraw_input(self.cursor, &self.line, self.line.len());
                self.cursor = self.line.len();
            }
            self.refresh_completion(model_id);
            if self.completion_count == 0 && !cmd.arg_hint.is_empty() {
                self.completion_count = show_arg_hint(cmd.arg_hint, 0);
            }
            return true;
        }
        false
    }

    /// Handle a single byte while in flushed (pass-through) mode.
    /// Returns `true` when the byte was consumed and the caller should `continue`.
    fn handle_flushed_byte(&mut self, byte: u8, pty: &mut PtyBridge) -> bool {
        if !self.flushed {
            return false;
        }
        match byte {
            DEL | 8 => {
                if self.flushed_len > 0 {
                    let _ = pty.write(&[byte]);
                    self.flushed_len -= 1;
                }
                if self.flushed_len == 0 {
                    self.flushed = false;
                }
            }
            21 => {
                let _ = pty.write(&[byte]);
                self.flushed = false;
                self.flushed_len = 0;
            }
            13 | 10 => {
                let _ = pty.write(&[byte]);
                self.flushed = false;
                self.flushed_len = 0;
                self.line.clear();
                self.cursor = 0;
                self.selected = 0;
            }
            _ => {
                let _ = pty.write(&[byte]);
                if byte >= 32 {
                    self.flushed_len += 1;
                }
            }
        }
        true
    }

    /// Push the current line to history (dedup-last, skip empty lines).
    fn record_history(&mut self) {
        if !self.line.is_empty()
            && self.history.last().map(String::as_str) != Some(self.line.as_str())
        {
            self.history.push(self.line.clone());
        }
    }

    /// Reset per-command mutable state after an Enter is processed.
    fn reset_after_enter(&mut self) {
        self.line.clear();
        self.cursor = 0;
        self.selected = 0;
        self.tab_prefix = None;
        self.history_pos = None;
        self.pending_buffer.clear();
    }

    /// Erase the active completion popup and reset its row counter.
    fn clear_completion_popup(&mut self) {
        clear_completion(self.completion_count);
        self.completion_count = 0;
    }

    /// Resolve one byte against the active escape-sequence state. Returns `true`
    /// when the byte was consumed as part of an escape sequence and the caller
    /// should `continue`.
    fn process_escape(&mut self, byte: u8, pty: &mut PtyBridge, model_id: Option<&str>) -> bool {
        match self.escape_state {
            EscapeState::GotEsc if byte == b'[' || byte == b'O' => {
                self.escape_state = EscapeState::GotIntroducer(byte);
                true
            }
            EscapeState::GotEsc => {
                let buffered = self.is_buffered();
                let mut consumed = false;
                if byte == DEL || byte == 8 || byte == 23 {
                    if buffered {
                        let chop = word_delete_chop(&self.line[..self.cursor]);
                        if chop > 0 {
                            let old_cp = self.cursor;
                            self.line.drain((self.cursor - chop)..self.cursor);
                            self.cursor -= chop;
                            redraw_input(old_cp, &self.line, self.cursor);
                        }
                        self.selected = 0;
                        self.tab_prefix = None;
                        self.refresh_completion(model_id);
                    } else {
                        // ESC + DEL/BS/Ctrl+W = Meta-Backspace = backward-kill-word.
                        // Best-effort mirror to keep line in sync.
                        while self.line.ends_with(' ') {
                            self.line.pop();
                        }
                        while !self.line.is_empty() && !self.line.ends_with(' ') {
                            self.line.pop();
                        }
                        let _ = pty.write(&[ESC, byte]);
                    }
                    consumed = true;
                } else if buffered && matches!(byte, b'p' | b'P') {
                    self.navigate(false, false, model_id);
                    consumed = true;
                } else if buffered && matches!(byte, b'n' | b'N') {
                    self.navigate(true, false, model_id);
                    consumed = true;
                } else if buffered {
                    self.escape_to_shell(pty);
                } else {
                    let _ = pty.write(&[ESC]);
                }
                self.escape_state = EscapeState::Normal;
                consumed
            }
            EscapeState::GotIntroducer(intro) => {
                let buffered = self.is_buffered();
                if buffered && byte == b'A' {
                    self.navigate(false, true, model_id);
                } else if buffered && byte == b'B' {
                    self.navigate(true, true, model_id);
                } else if buffered && byte == b'C' {
                    if self.cursor < self.line.len() {
                        self.cursor += 1;
                        write_stdout(b"\x1b[C");
                    }
                } else if buffered && byte == b'D' {
                    if self.cursor > 0 {
                        self.cursor -= 1;
                        write_stdout(b"\x1b[D");
                    }
                } else if buffered && byte == b'H' {
                    if self.cursor > 0 {
                        write_stdout(format!("\x1b[{}D", self.cursor).as_bytes());
                        self.cursor = 0;
                    }
                } else if buffered && byte == b'F' {
                    if self.cursor < self.line.len() {
                        let diff = self.line.len() - self.cursor;
                        write_stdout(format!("\x1b[{diff}C").as_bytes());
                        self.cursor = self.line.len();
                    }
                } else if buffered && byte == b'Z' {
                    // Shift+Tab (back-tab): cycle completion backward.
                    step_completion(
                        &mut self.line,
                        &mut self.tab_prefix,
                        &mut self.selected,
                        &mut self.completion_count,
                        &self.cached_models,
                        false,
                        self.cursor,
                        model_id,
                    );
                    self.cursor = self.line.len();
                } else if !buffered {
                    let _ = pty.write(&[ESC, intro, byte]);
                }
                self.escape_state = EscapeState::Normal;
                true
            }
            EscapeState::Normal if byte == ESC => {
                self.escape_state = EscapeState::GotEsc;
                true
            }
            EscapeState::Normal => false,
        }
    }

    /// Resolve a lone ESC left pending at the end of a read batch: in buffered
    /// mode escape to the shell, otherwise forward the ESC byte as-is.
    fn resolve_dangling_escape(&mut self, pty: &mut PtyBridge) {
        if matches!(self.escape_state, EscapeState::GotEsc) {
            if self.is_buffered() {
                self.escape_to_shell(pty);
            } else {
                let _ = pty.write(&[ESC]);
            }
            self.escape_state = EscapeState::Normal;
        }
    }

    /// Auto-complete and validate a buffered `/`-command on Enter. Returns `true`
    /// when the caller should `continue` (the command name was completed from a
    /// prefix that takes args, or a required argument is still missing).
    fn resolve_buffered_command(&mut self, model_id: Option<&str>) -> bool {
        if let Some(arg) = self.line.strip_prefix("/connect ") {
            let providers = complete_providers(arg);
            if providers.len() == 1 && providers[0] != arg {
                let remainder = providers[0][arg.len()..].to_string();
                self.line.push_str(&remainder);
                redraw_input(self.cursor, &self.line, self.line.len());
                self.cursor = self.line.len();
            }
        }
        let matches = complete(&self.line);
        if !matches.is_empty() {
            let idx = self.selected % matches.len();
            let cmd = matches[idx];
            let was_prefix = self.line.len() < cmd.name.len();
            if was_prefix {
                let prev_len = self.line.len();
                self.line.push_str(&cmd.name[prev_len..]);
                // Only append a space for commands that accept arguments;
                // no-arg commands will execute immediately below.
                if cmd.takes_args {
                    self.line.push(' ');
                }
                redraw_input(self.cursor, &self.line, self.line.len());
                self.cursor = self.line.len();
            }
            if self.check_missing_arg(cmd, model_id) {
                return true;
            }
            // Completed the command name from a prefix — for commands that take
            // arguments, wait for the user to supply them. For no-arg commands,
            // fall through and execute immediately.
            if was_prefix && cmd.takes_args {
                self.tab_prefix = None;
                self.selected = 0;
                self.refresh_completion(model_id);
                return true;
            }
        }
        // Fallback: complete() returned empty (e.g. line is "/chat " — command
        // name with trailing space). Check if the line still maps to a command
        // that requires an argument; if so, show the hint and stay.
        if matches.is_empty() {
            if let Some(cmd) = command_for_line(&self.line) {
                if self.check_missing_arg(cmd, model_id) {
                    return true;
                }
            }
        }
        false
    }

    /// Handle a byte that matched no control key: Tab completion, cursor
    /// home/end, printable insertion, and the reset/abort keys, in both buffered
    /// and pass-through modes.
    async fn handle_default_byte(
        &mut self,
        byte: u8,
        pty: &mut PtyBridge,
        registry: &Arc<Mutex<ProviderRegistry>>,
        model_id: Option<&str>,
    ) {
        let entering_buffered = self.line.is_empty() && byte == b'/';
        if !self.is_buffered() && !entering_buffered {
            if byte >= 32 {
                // Tab (9) is intentionally excluded: it triggers shell completion
                // or autosuggestion acceptance, which may expand the visible line
                // by more characters than the single Tab byte — tracking it would
                // cause self.line to overcount and prevent entering buffered mode.
                self.line.push(byte as char);
            } else if byte == 3 || byte == 4 {
                // Ctrl+C / Ctrl+D abort the shell line; keep tracking in sync.
                self.line.clear();
                self.cursor = 0;
            }
            let _ = pty.write(&[byte]);
            return;
        }

        if byte == 9 {
            // Fetch all providers' models on first Tab for `/model` or `/model `.
            if (self.line.starts_with("/model ") || self.line == "/model")
                && self.cached_models.is_empty()
            {
                self.cached_models = fetch_all_model_completions(registry).await;
            }
            if step_completion(
                &mut self.line,
                &mut self.tab_prefix,
                &mut self.selected,
                &mut self.completion_count,
                &self.cached_models,
                true,
                self.cursor,
                model_id,
            ) {
                self.cursor = self.line.len();
                // A completion popup handled the keypress.
            } else if !self.line.is_empty()
                && matches!(classify_input(&self.line), InputAction::Passthrough)
            {
                redraw_input(self.cursor, "", 0);
                self.clear_completion_popup();
                let _ = pty.write(self.line.as_bytes());
                let _ = pty.write(&[byte]);
                self.line.clear();
                self.cursor = 0;
                self.selected = 0;
                self.flushed = true;
                self.tab_prefix = None;
                self.history_pos = None;
                self.pending_buffer.clear();
            }
        } else if byte == 1 {
            if self.cursor > 0 {
                write_stdout(format!("\x1b[{}D", self.cursor).as_bytes());
                self.cursor = 0;
            }
        } else if byte == 5 {
            if self.cursor < self.line.len() {
                let diff = self.line.len() - self.cursor;
                write_stdout(format!("\x1b[{diff}C").as_bytes());
                self.cursor = self.line.len();
            }
        } else if byte >= 32 {
            let old_cp = self.cursor;
            self.line.insert(self.cursor, byte as char);
            self.cursor += 1;
            redraw_input(old_cp, &self.line, self.cursor);
            self.selected = 0;
            self.tab_prefix = None;
            self.refresh_completion(model_id);
        } else if byte == 3 {
            // Ctrl+C: clear the completion popup, reset buffered state, then
            // forward Ctrl+C to the shell. The PTY echoes `^C` right after the
            // half-typed command and the shell redraws a fresh empty prompt.
            self.clear_completion_popup();
            self.reset_after_enter();
            let _ = pty.write(&[3u8]);
        } else {
            write_stdout(b"\r\x1b[K\x1b[0m");
            self.clear_completion_popup();
            self.reset_after_enter();
        }
    }
}

/// Execute a resolved chatsh command action (chat, connect, reauth, model, …).
#[allow(clippy::too_many_arguments)]
async fn dispatch_command_action(
    action: InputAction,
    registry: &Arc<Mutex<ProviderRegistry>>,
    buffer: &Arc<Mutex<ContextBuffer>>,
    conversation: &mut Conversation,
    config: &mut Config,
    provider_id: &mut Option<String>,
    model_id: &mut Option<String>,
    cached_models: &mut Vec<(String, Option<String>)>,
) {
    match action {
        InputAction::ShowHint => {
            write_stdout(format!("\r\n{}\r\n", hint_text()).as_bytes());
        }
        InputAction::TriggerChat { query } => {
            if query.is_empty() {
                write_stdout(b"\r\n\x1b[2mWhat do you want to ask?\x1b[0m\r\n\r\n");
            } else {
                let prov = provider_id.clone();
                let mdl = model_id.clone();
                if let Err(e) =
                    handle_chat(registry, buffer, conversation, &query, &prov, &mdl).await
                {
                    write_stdout(format!("\r\n\x1b[31mchatsh: {e}\x1b[0m\r\n\r\n").as_bytes());
                }
            }
        }
        InputAction::NewChat => {
            let prior = conversation.turn_count();
            let _ = conversation.clear();
            write_stdout(
                format!(
                    "\r\n\x1b[2mchatsh: cleared {prior} prior turn(s); starting fresh.\x1b[0m\r\n\r\n"
                )
                .as_bytes(),
            );
        }
        InputAction::Connect { provider } => {
            let _ = handle_connect(registry, &provider, provider_id, model_id).await;
            cached_models.clear();
            let _ = config.save(provider_id.as_deref(), model_id.as_deref());
        }
        InputAction::Reauth { provider } => {
            let _ = handle_reauth(registry, &provider, provider_id, model_id).await;
            cached_models.clear();
            let _ = config.save(provider_id.as_deref(), model_id.as_deref());
        }
        InputAction::Model { name } => {
            if handle_model(registry, provider_id, model_id, name.as_deref())
                .await
                .is_ok()
            {
                // Clear so the cache is re-fetched with fresh rates on next Tab.
                cached_models.clear();
            }
            let _ = config.save(provider_id.as_deref(), model_id.as_deref());
        }
        InputAction::Passthrough | InputAction::Exit => unreachable!(),
    }
}

/// Runs the interactive shell bridge and buffered command loop.
pub async fn run(
    shell_args: Vec<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
) -> Result<()> {
    print_greeting();
    keyring::init();
    let mut config = Config::load().unwrap_or_default();

    let mut provider_id = provider_override.or_else(|| {
        if config.ai.provider != "auto" {
            Some(config.ai.provider.clone())
        } else {
            None
        }
    });
    let mut model_id = model_override.or(config.ai.model.clone());
    let extra_env = extra_shell_env();
    let (mut pty, pty_rx) = PtyBridge::spawn(&shell_args, &extra_env)?;
    let buffer = Arc::new(Mutex::new(ContextBuffer::new()));
    let registry = Arc::new(std::sync::Mutex::new(build_registry()));

    let writer_buffer = buffer.clone();
    let _pty_output_handle = std::thread::Builder::new()
        .name("pty-output".into())
        .spawn(move || {
            loop {
                match pty_rx.recv() {
                    Ok(data) => {
                        {
                            let stdout = std::io::stdout();
                            let mut out = stdout.lock();
                            let _ = out.write_all(&data);
                            let _ = out.flush();
                        }

                        let text = String::from_utf8_lossy(&data);
                        for line in text.lines() {
                            if let Ok(mut buf) = writer_buffer.lock() {
                                buf.push(line.to_string());
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        })?;

    let initial_size = crossterm::terminal::size()?;
    let _ = pty.resize(initial_size.1, initial_size.0);

    std::thread::sleep(STARTUP_SETTLE);

    let mut state = InputState::new();
    let mut stdin_buf = [0u8; STDIN_BUF_SIZE];
    let mut conversation = Conversation::load();
    let mut clean_exit = false;

    'main_loop: loop {
        let n = {
            let mut stdin_lock = std::io::stdin().lock();
            stdin_lock.read(&mut stdin_buf)?
        };
        if n == 0 {
            break;
        }

        let input = &stdin_buf[..n];

        for &byte in input {
            if state.process_escape(byte, &mut pty, model_id.as_deref()) {
                continue;
            }

            if state.handle_flushed_byte(byte, &mut pty) {
                continue;
            }

            let buffered = state.is_buffered();

            match byte {
                13 | 10 => {
                    if buffered && state.resolve_buffered_command(model_id.as_deref()) {
                        continue;
                    }
                    let action = classify_input(&state.line);
                    match action {
                        InputAction::Passthrough => {
                            if buffered {
                                state.clear_completion_popup();
                                write_stdout(b"\r\x1b[K\x1b[0m");
                                let _ = pty.write(state.line.as_bytes());
                            }
                            let _ = pty.write(&[byte]);
                        }
                        InputAction::Exit => {
                            if buffered {
                                clear_completion(state.completion_count);
                                write_stdout(b"\r\n\x1b[0m");
                            }
                            write_stdout(b"\r\nchatsh: bye!\r\n");
                            clean_exit = true;
                            break 'main_loop;
                        }
                        _ => {
                            if buffered {
                                state.clear_completion_popup();
                                write_stdout(b"\r\n\x1b[0m");
                            }
                            dispatch_command_action(
                                action,
                                &registry,
                                &buffer,
                                &mut conversation,
                                &mut config,
                                &mut provider_id,
                                &mut model_id,
                                &mut state.cached_models,
                            )
                            .await;
                            let _ = pty.write(b"\n");
                        }
                    }
                    if buffered {
                        state.record_history();
                    }
                    state.reset_after_enter();
                }
                14 => {
                    if buffered {
                        state.navigate(true, true, model_id.as_deref());
                    } else {
                        let _ = pty.write(&[byte]);
                    }
                }
                16 => {
                    if buffered {
                        state.navigate(false, true, model_id.as_deref());
                    } else {
                        let _ = pty.write(&[byte]);
                    }
                }
                DEL => {
                    if buffered {
                        if state.cursor > 0 {
                            let old_cp = state.cursor;
                            state.cursor -= 1;
                            state.line.remove(state.cursor);
                            redraw_input(old_cp, &state.line, state.cursor);
                            state.selected = 0;
                            state.tab_prefix = None;
                            if state.line.starts_with('/') {
                                state.refresh_completion(model_id.as_deref());
                            } else {
                                state.clear_completion_popup();
                            }
                        }
                    } else {
                        state.line.pop();
                        let _ = pty.write(&[byte]);
                    }
                }
                8 | 23 => {
                    if buffered {
                        let chop = word_delete_chop(&state.line[..state.cursor]);
                        if chop > 0 {
                            let old_cp = state.cursor;
                            state.line.drain((state.cursor - chop)..state.cursor);
                            state.cursor -= chop;
                            redraw_input(old_cp, &state.line, state.cursor);
                        }
                        state.selected = 0;
                        state.tab_prefix = None;
                        state.refresh_completion(model_id.as_deref());
                    } else {
                        if byte == 8 {
                            state.line.pop();
                        } else {
                            // Ctrl+W: best-effort mirror of shell word-delete
                            while state.line.ends_with(' ') {
                                state.line.pop();
                            }
                            while !state.line.is_empty() && !state.line.ends_with(' ') {
                                state.line.pop();
                            }
                        }
                        let _ = pty.write(&[byte]);
                    }
                }
                21 => {
                    if buffered {
                        let prefix_len = command_prefix_len(&state.line);
                        let old_cp = state.cursor;
                        if old_cp > prefix_len {
                            state.line.drain(prefix_len..state.cursor);
                            state.cursor = prefix_len;
                            redraw_input(old_cp, &state.line, state.cursor);
                        }
                        state.selected = 0;
                        state.tab_prefix = None;
                        state.refresh_completion(model_id.as_deref());
                    } else {
                        state.line.clear();
                        state.cursor = 0;
                        let _ = pty.write(&[byte]);
                    }
                }
                7 => {
                    if buffered {
                        state.escape_to_shell(&mut pty);
                    } else {
                        let _ = pty.write(&[byte]);
                    }
                }
                _ => {
                    state
                        .handle_default_byte(byte, &mut pty, &registry, model_id.as_deref())
                        .await;
                }
            }
        }

        // A lone ESC (no follow-up bytes in the same read batch): resolve it now
        // rather than waiting for the next keypress.
        state.resolve_dangling_escape(&mut pty);
    }

    if !clean_exit {
        write_stdout(b"\r\n");
    }
    Ok(())
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_word_delete_chop_deletes_last_argument_word() {
        // Deletes the final word, leaving earlier args and the command intact.
        assert_eq!(word_delete_chop("/chat foo bar"), 3);
        assert_eq!(word_delete_chop("/chat hello"), 5);
    }

    #[test]
    fn test_word_delete_chop_consumes_trailing_blanks_with_word() {
        // Trailing blanks after a word are removed together with that word.
        assert_eq!(word_delete_chop("/chat hello  "), 7);
    }

    #[test]
    fn test_word_delete_chop_command_plus_trailing_space_deletes_all() {
        // Regression: "/chat " (command name + lone trailing space, no argument)
        // must not get stuck — the whole command is removed.
        assert_eq!(word_delete_chop("/chat "), 6);
        assert_eq!(word_delete_chop("/chat   "), 8);
    }

    #[test]
    fn test_word_delete_chop_command_name_only_deletes_all() {
        assert_eq!(word_delete_chop("/chat"), 5);
        assert_eq!(word_delete_chop("/"), 1);
    }
}
