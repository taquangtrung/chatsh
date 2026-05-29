// Application entry point: spawns the shell PTY and runs the read-eval loop
// that intercepts `/` commands and forwards everything else to the shell.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;

use crate::chat::handle_chat;
use crate::chat::commands::{build_registry, handle_connect, handle_model, handle_reauth};
use crate::input::repl::{
    clear_completion, command_prefix_len, history_back, history_forward, print_greeting,
    redraw_completion, redraw_input, show_arg_hint, step_completion, write_stdout,
};
use crate::input::trigger::{classify_input, complete, complete_providers, hint_text, InputAction};
use crate::shell::{ContextBuffer, PtyBridge};
use crate::storage::{keyring, Config, Conversation};

const ESC: u8 = 27;
const DEL: u8 = 127;
const STDIN_BUF_SIZE: usize = 256;
const STARTUP_SETTLE: Duration = Duration::from_millis(50);

/// Number of characters to remove from the end of a buffered `/`-command line
/// for a word-delete (Ctrl+Backspace / Alt+Backspace / Ctrl+W).
///
/// • While editing arguments (a space is present): removes trailing blanks +
///   the last argument word, but never touches the command name.
/// • While still in the command name (no space yet): removes everything after
///   the leading `/` so the user lands back at the bare-prompt prefix.
fn word_delete_chop(input_line: &str) -> usize {
    if let Some(space_pos) = input_line.find(' ') {
        let args = &input_line[space_pos + 1..];
        let trimmed = args.trim_end();
        let word_len = trimmed.len()
            - trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
        (args.len() - trimmed.len()) + word_len
    } else {
        // Still typing the command name (or just '/'): delete everything
        // so the user can start over with a shell command.
        input_line.len()
    }
}

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

pub async fn run(
    shell_args: Vec<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
) -> Result<()> {
    print_greeting();
    keyring::init();
    let mut config = Config::load().unwrap_or_default();
    let extra_env = extra_shell_env();
    let (mut pty, pty_rx) = PtyBridge::spawn(&shell_args, &extra_env)?;
    let buffer = Arc::new(Mutex::new(ContextBuffer::new()));
    let registry = Arc::new(std::sync::Mutex::new(build_registry()));

    let mut provider_id = provider_override.or_else(|| {
        if config.ai.provider != "auto" {
            Some(config.ai.provider.clone())
        } else {
            None
        }
    });
    let mut model_id = model_override.or(config.ai.model.clone());

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

    let mut input_line = String::new();
    let mut stdin_buf = [0u8; STDIN_BUF_SIZE];
    let mut selected: usize = 0;
    let mut tab_prefix: Option<String> = None;
    let mut escape_state = EscapeState::Normal;
    let mut flushed: bool = false;
    let mut flushed_len: usize = 0; // byte-count of what the shell currently shows
    let mut cached_models: Vec<String> = Vec::new();
    let mut completion_count: usize = 0;
    let mut history: Vec<String> = Vec::new();
    let mut history_pos: Option<usize> = None;
    let mut pending_buffer: String = String::new();
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
            let consumed = match escape_state {
                EscapeState::GotEsc if byte == b'[' || byte == b'O' => {
                    escape_state = EscapeState::GotIntroducer(byte);
                    true
                }
                EscapeState::GotEsc => {
                    let buffered = input_line.starts_with('/');
                    let mut consumed = false;
                    if byte == DEL || byte == 8 || byte == 23 {
                        if buffered {
                            let chop = word_delete_chop(&input_line);
                            if chop > 0 {
                                let prev_len = input_line.len();
                                input_line.truncate(input_line.len() - chop);
                                redraw_input(prev_len, &input_line);
                            }
                            selected = 0;
                            tab_prefix = None;
                            completion_count = redraw_completion(&input_line, selected, completion_count, &cached_models);
                        } else {
                            // ESC + DEL/BS/Ctrl+W = Meta-Backspace = backward-kill-word.
                            // Best-effort mirror to keep input_line in sync.
                            while input_line.ends_with(' ') {
                                input_line.pop();
                            }
                            while !input_line.is_empty() && !input_line.ends_with(' ') {
                                input_line.pop();
                            }
                            let _ = pty.write(&[ESC, byte]);
                        }
                        consumed = true;
                    } else if buffered && matches!(byte, b'p' | b'P') {
                        history_back(
                            &mut input_line,
                            &history,
                            &mut history_pos,
                            &mut pending_buffer,
                        );
                        selected = 0;
                        tab_prefix = None;
                        completion_count =
                            redraw_completion(&input_line, selected, completion_count, &cached_models);
                        consumed = true;
                    } else if buffered && matches!(byte, b'n' | b'N') {
                        history_forward(
                            &mut input_line,
                            &history,
                            &mut history_pos,
                            &mut pending_buffer,
                        );
                        selected = 0;
                        tab_prefix = None;
                        completion_count =
                            redraw_completion(&input_line, selected, completion_count, &cached_models);
                        consumed = true;
                    } else if buffered {
                        // ESC: escape chatsh command mode — pass typed text
                        // to the shell. The user drives shell completion manually.
                        let shell_input = input_line.clone();
                        redraw_input(input_line.len(), "");
                        clear_completion(completion_count);
                        completion_count = 0;
                        if !shell_input.is_empty() {
                            let _ = pty.write(shell_input.as_bytes());
                        }
                        input_line.clear();
                        selected = 0;
                        flushed = true;
                        flushed_len = shell_input.len();
                        tab_prefix = None;
                        history_pos = None;
                        pending_buffer.clear();
                    } else {
                        let _ = pty.write(&[ESC]);
                    }
                    escape_state = EscapeState::Normal;
                    consumed
                }
                EscapeState::GotIntroducer(intro) => {
                    let buffered = input_line.starts_with('/');
                    if buffered && byte == b'A' {
                        if !step_completion(
                            &mut input_line,
                            &mut tab_prefix,
                            &mut selected,
                            &mut completion_count,
                            &cached_models,
                            false,
                        ) {
                            history_back(
                                &mut input_line,
                                &history,
                                &mut history_pos,
                                &mut pending_buffer,
                            );
                            selected = 0;
                            tab_prefix = None;
                            completion_count =
                                redraw_completion(&input_line, selected, completion_count, &cached_models);
                        }
                    } else if buffered && byte == b'B' {
                        if !step_completion(
                            &mut input_line,
                            &mut tab_prefix,
                            &mut selected,
                            &mut completion_count,
                            &cached_models,
                            true,
                        ) {
                            history_forward(
                                &mut input_line,
                                &history,
                                &mut history_pos,
                                &mut pending_buffer,
                            );
                            selected = 0;
                            tab_prefix = None;
                            completion_count =
                                redraw_completion(&input_line, selected, completion_count, &cached_models);
                        }
                    } else if buffered && byte == b'Z' {
                        // Shift+Tab (back-tab): cycle completion backward.
                        step_completion(
                            &mut input_line,
                            &mut tab_prefix,
                            &mut selected,
                            &mut completion_count,
                            &cached_models,
                            false,
                        );
                    } else if !buffered {
                        let _ = pty.write(&[ESC, intro, byte]);
                    }
                    escape_state = EscapeState::Normal;
                    true
                }
                EscapeState::Normal if byte == ESC => {
                    escape_state = EscapeState::GotEsc;
                    true
                }
                EscapeState::Normal => false,
            };

            if consumed {
                continue;
            }

            if flushed {
                match byte {
                    DEL | 8 => {
                        if flushed_len > 0 {
                            let _ = pty.write(&[byte]);
                            flushed_len -= 1;
                        }
                        if flushed_len == 0 {
                            // Backspaced back to empty — re-enter chatsh mode.
                            flushed = false;
                        }
                    }
                    21 => {
                        // Ctrl+U: clears the shell line, re-enter chatsh mode.
                        let _ = pty.write(&[byte]);
                        flushed = false;
                        flushed_len = 0;
                    }
                    13 | 10 => {
                        let _ = pty.write(&[byte]);
                        flushed = false;
                        flushed_len = 0;
                        input_line.clear();
                        selected = 0;
                    }
                    _ => {
                        let _ = pty.write(&[byte]);
                        // Track printable bytes so we know when the line is
                        // empty again (multi-byte UTF-8 may drift slightly).
                        if byte >= 32 {
                            flushed_len += 1;
                        }
                    }
                }
                continue;
            }

            let buffered = input_line.starts_with('/');

            match byte {
                13 | 10 => {
                    if buffered {
                        if let Some(arg) = input_line.strip_prefix("/connect ") {
                            let providers = complete_providers(arg);
                            if providers.len() == 1 && providers[0] != arg {
                                let remainder = &providers[0][arg.len()..];
                                input_line.push_str(remainder);
                                write_stdout(remainder.as_bytes());
                            }
                        }
                        let matches = complete(&input_line);
                        if !matches.is_empty() {
                            let idx = selected % matches.len();
                            let cmd = matches[idx];
                            if input_line.len() < cmd.name.len() {
                                let remainder = &cmd.name[input_line.len()..];
                                input_line.push_str(remainder);
                                write_stdout(remainder.as_bytes());
                            }
                            let has_args =
                                !input_line[cmd.name.len()..].trim_start().is_empty();
                            if cmd.takes_args && !has_args && cmd.requires_args {
                                if !input_line.ends_with(' ') {
                                    input_line.push(' ');
                                    write_stdout(b" ");
                                }
                                completion_count = redraw_completion(&input_line, selected, completion_count, &cached_models);
                                if completion_count == 0 && !cmd.arg_hint.is_empty() {
                                    completion_count = show_arg_hint(cmd.arg_hint, 0);
                                }
                                continue;
                            }
                        }
                    }
                    let action = classify_input(&input_line);
                    match action {
                        InputAction::Passthrough => {
                            if buffered {
                                clear_completion(completion_count);
                                completion_count = 0;
                                write_stdout(b"\r\x1b[K\x1b[0m");
                                let _ = pty.write(input_line.as_bytes());
                            }
                            let _ = pty.write(&[byte]);
                        }
                        InputAction::Exit => {
                            if buffered {
                                clear_completion(completion_count);
                                write_stdout(b"\r\n\x1b[0m");
                            }
                            write_stdout(b"chatsh: bye.\r\n");
                            clean_exit = true;
                            break 'main_loop;
                        }
                        _ => {
                            if buffered {
                                clear_completion(completion_count);
                                completion_count = 0;
                                write_stdout(b"\r\n\x1b[0m");
                            }
                            match action {
                                InputAction::ShowHint => {
                                    write_stdout(
                                        format!("\r\n{}\r\n", hint_text()).as_bytes(),
                                    );
                                }
                                InputAction::TriggerChat { query } => {
                                    let prov = provider_id.clone();
                                    let mdl = model_id.clone();
                                    if let Err(e) = handle_chat(
                                        &registry,
                                        &buffer,
                                        &mut conversation,
                                        &query,
                                        &prov,
                                        &mdl,
                                    )
                                    .await
                                    {
                                        write_stdout(
                                            format!(
                                                "\r\n\x1b[31mchatsh: {e}\x1b[0m\r\n\r\n"
                                            )
                                            .as_bytes(),
                                        );
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
                                    let _ = handle_connect(
                                        &registry,
                                        &provider,
                                        &mut provider_id,
                                        &mut model_id,
                                    )
                                    .await;
                                    cached_models.clear();
                                    let _ = config.save(
                                        provider_id.as_deref(),
                                        model_id.as_deref(),
                                    );
                                }
                                InputAction::Reauth { provider } => {
                                    let _ = handle_reauth(
                                        &registry,
                                        &provider,
                                        &mut provider_id,
                                        &mut model_id,
                                    )
                                    .await;
                                    cached_models.clear();
                                    let _ = config.save(
                                        provider_id.as_deref(),
                                        model_id.as_deref(),
                                    );
                                }
                                InputAction::Model { name } => {
                                    if let Ok(models) = handle_model(
                                        &registry,
                                        &provider_id,
                                        &mut model_id,
                                        name.as_deref(),
                                    )
                                    .await
                                    {
                                        cached_models = models;
                                    }
                                    let _ = config.save(
                                        provider_id.as_deref(),
                                        model_id.as_deref(),
                                    );
                                }
                                InputAction::Passthrough | InputAction::Exit => {
                                    unreachable!()
                                }
                            }
                            let _ = pty.write(b"\n");
                        }
                    }
                    if buffered && !input_line.is_empty()
                        && history.last().map(String::as_str) != Some(input_line.as_str())
                    {
                        history.push(input_line.clone());
                    }
                    input_line.clear();
                    selected = 0;
                    tab_prefix = None;
                    history_pos = None;
                    pending_buffer.clear();
                }
                14 => {
                    if buffered {
                        if !step_completion(
                            &mut input_line,
                            &mut tab_prefix,
                            &mut selected,
                            &mut completion_count,
                            &cached_models,
                            true,
                        ) {
                            history_forward(
                                &mut input_line,
                                &history,
                                &mut history_pos,
                                &mut pending_buffer,
                            );
                            selected = 0;
                            tab_prefix = None;
                            completion_count =
                                redraw_completion(&input_line, selected, completion_count, &cached_models);
                        }
                    } else {
                        let _ = pty.write(&[byte]);
                    }
                }
                16 => {
                    if buffered {
                        if !step_completion(
                            &mut input_line,
                            &mut tab_prefix,
                            &mut selected,
                            &mut completion_count,
                            &cached_models,
                            false,
                        ) {
                            history_back(
                                &mut input_line,
                                &history,
                                &mut history_pos,
                                &mut pending_buffer,
                            );
                            selected = 0;
                            tab_prefix = None;
                            completion_count =
                                redraw_completion(&input_line, selected, completion_count, &cached_models);
                        }
                    } else {
                        let _ = pty.write(&[byte]);
                    }
                }
                DEL => {
                    if buffered {
                        let prev_len = input_line.len();
                        if input_line.pop().is_some() {
                            redraw_input(prev_len, &input_line);
                            selected = 0;
                            tab_prefix = None;
                            if input_line.starts_with('/') {
                                completion_count = redraw_completion(&input_line, selected, completion_count, &cached_models);
                            } else {
                                clear_completion(completion_count);
                                completion_count = 0;
                            }
                        }
                    } else {
                        input_line.pop();
                        let _ = pty.write(&[byte]);
                    }
                }
                8 | 23 => {
                    if buffered {
                        let chop = word_delete_chop(&input_line);
                        if chop > 0 {
                            let prev_len = input_line.len();
                            input_line.truncate(input_line.len() - chop);
                            redraw_input(prev_len, &input_line);
                        }
                        selected = 0;
                        tab_prefix = None;
                        completion_count = redraw_completion(&input_line, selected, completion_count, &cached_models);
                    } else {
                        if byte == 8 {
                            input_line.pop();
                        } else {
                            // Ctrl+W: best-effort mirror of shell word-delete
                            while input_line.ends_with(' ') { input_line.pop(); }
                            while !input_line.is_empty() && !input_line.ends_with(' ') { input_line.pop(); }
                        }
                        let _ = pty.write(&[byte]);
                    }
                }
                21 => {
                    if buffered {
                        let prefix_len = command_prefix_len(&input_line);
                        let removed = input_line.len().saturating_sub(prefix_len);
                        if removed > 0 {
                            let prev_len = input_line.len();
                            input_line.truncate(prefix_len);
                            redraw_input(prev_len, &input_line);
                        }
                        selected = 0;
                        tab_prefix = None;
                        completion_count = redraw_completion(&input_line, selected, completion_count, &cached_models);
                    } else {
                        input_line.clear();
                        let _ = pty.write(&[byte]);
                    }
                }
                7 => {
                    if buffered {
                        // Ctrl+G: escape chatsh command mode and pass the
                        // typed text (including the leading '/') to the shell.
                        // The user drives shell completion manually with Tab.
                        let shell_input = input_line.clone();
                        redraw_input(input_line.len(), "");
                        clear_completion(completion_count);
                        completion_count = 0;
                        if !shell_input.is_empty() {
                            let _ = pty.write(shell_input.as_bytes());
                        }
                        input_line.clear();
                        selected = 0;
                        flushed = true;
                        flushed_len = shell_input.len();
                        tab_prefix = None;
                        history_pos = None;
                        pending_buffer.clear();
                    } else {
                        let _ = pty.write(&[byte]);
                    }
                }
                _ => {
                    let entering_buffered = input_line.is_empty() && byte == b'/';
                    if buffered || entering_buffered {
                        if byte == 9 {
                            // Fetch models in real time on the first Tab press for `/model `.
                            if input_line.starts_with("/model ") && cached_models.is_empty() {
                                if let Some(ref pid) = provider_id {
                                    let provider = registry.lock().unwrap().get(pid).ok();
                                    if let Some(p) = provider {
                                        if let Ok(models) = p.list_models().await {
                                            cached_models =
                                                models.into_iter().map(|m| m.id).collect();
                                        }
                                    }
                                }
                            }
                            if step_completion(
                                &mut input_line,
                                &mut tab_prefix,
                                &mut selected,
                                &mut completion_count,
                                &cached_models,
                                true,
                            ) {
                                // A completion popup handled the keypress.
                            } else if !input_line.is_empty()
                                && matches!(
                                    classify_input(&input_line),
                                    InputAction::Passthrough
                                )
                            {
                                redraw_input(input_line.len(), "");
                                clear_completion(completion_count);
                                completion_count = 0;
                                let _ = pty.write(input_line.as_bytes());
                                let _ = pty.write(&[byte]);
                                input_line.clear();
                                selected = 0;
                                flushed = true;
                                tab_prefix = None;
                                history_pos = None;
                                pending_buffer.clear();
                            }
                        } else if byte >= 32 {
                            let prev_len = input_line.len();
                            input_line.push(byte as char);
                            redraw_input(prev_len, &input_line);
                            selected = 0;
                            tab_prefix = None;
                            completion_count = redraw_completion(&input_line, selected, completion_count, &cached_models);
                        } else {
                            input_line.clear();
                            write_stdout(b"\r\x1b[K\x1b[0m");
                            clear_completion(completion_count);
                            completion_count = 0;
                            selected = 0;
                            tab_prefix = None;
                            history_pos = None;
                            pending_buffer.clear();
                        }
                    } else {
                        if byte >= 32 {
                            // Tab (9) is intentionally excluded: it triggers shell completion
                            // or autosuggestion acceptance, which may expand the visible line
                            // by more characters than the single Tab byte — tracking it would
                            // cause input_line to overcount and prevent entering buffered mode.
                            input_line.push(byte as char);
                        } else if byte == 3 || byte == 4 {
                            // Ctrl+C / Ctrl+D abort the shell line; keep tracking in sync.
                            input_line.clear();
                        }
                        let _ = pty.write(&[byte]);
                    }
                }
            }
        }

        // A lone ESC (no follow-up bytes in the same read batch): resolve it now
        // rather than waiting for the next keypress.
        if matches!(escape_state, EscapeState::GotEsc) {
            if input_line.starts_with('/') {
                // Buffered chatsh mode: escape to shell without auto-completing.
                let shell_input = input_line.clone();
                redraw_input(input_line.len(), "");
                clear_completion(completion_count);
                completion_count = 0;
                if !shell_input.is_empty() {
                    let _ = pty.write(shell_input.as_bytes());
                }
                input_line.clear();
                selected = 0;
                flushed = true;
                flushed_len = shell_input.len();
                tab_prefix = None;
                history_pos = None;
                pending_buffer.clear();
            } else {
                // Not buffered: forward ESC to the shell as-is.
                let _ = pty.write(&[ESC]);
            }
            escape_state = EscapeState::Normal;
        }
    }

    if !clean_exit {
        write_stdout(b"\r\n");
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum EscapeState {
    Normal,
    GotEsc,
    GotIntroducer(u8),
}
