#![allow(dead_code)]

mod ai;
mod config;
mod context;
mod conversation;
mod keyring;
mod pty;
mod trigger;
mod tui;
mod types;

use std::io::{Read as IoRead, Write as IoWrite};
use std::sync::{Arc, Mutex};

use futures::StreamExt;

use anyhow::Result;
use clap::Parser;
use crossterm::terminal;
use secrecy::SecretString;

use crate::ai::anthropic::AnthropicProvider;
use crate::ai::auth::{AuthStrategy, OAuthState};
use crate::ai::copilot::{CopilotProvider, DeviceFlowPoll};
use crate::ai::openai::OpenAiProvider;
use crate::ai::registry::ProviderRegistry;
use crate::ai::zai::ZaiProvider;
use crate::config::Config;
use crate::context::buffer::ContextBuffer;
use crate::context::parser::EntryKind;
use crate::conversation::Conversation;
use crate::pty::bridge::PtyBridge;
use crate::trigger::engine::{classify_input, complete, complete_providers, hint_text, InputAction};
use crate::tui::markdown::MarkdownRenderer;
use crate::types::{ChatEvent, ChatMessage, ChatRequest};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const ESC: u8 = 27;
const DEL: u8 = 127;
const PLANNER_MODEL: &str = "gpt-4o-mini";

const MODEL_RATES: &[(&str, &str)] = &[
    ("gpt-4o", "1x"),
    ("gpt-4o-mini", "0.1x"),
    ("o3-mini", "0.5x"),
    ("o3", "5x"),
    ("o4-mini", "1x"),
    ("claude-sonnet-4", "1x"),
    ("claude-sonnet-4-20250514", "1x"),
    ("gpt-4.5-preview", "10x"),
    ("gemini-2.0-flash", "0.1x"),
    ("gemini-2.5-pro", "2x"),
];

fn model_rate(model_id: &str) -> &'static str {
    MODEL_RATES
        .iter()
        .find(|(id, _)| model_id.contains(id) || id.contains(model_id))
        .map(|(_, rate)| *rate)
        .unwrap_or("?")
}

#[derive(Parser)]
#[command(name = "chatsh", version = VERSION, about = "PTY-level AI chat in your terminal")]
struct Cli {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,

    #[arg(long, global = true)]
    provider: Option<String>,

    #[arg(long, global = true)]
    model: Option<String>,
}

struct RawGuard;

impl RawGuard {
    fn init() -> Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

fn build_registry() -> ProviderRegistry {
    let mut reg = ProviderRegistry::new();

    let copilot_token = std::env::var("GITHUB_COPILOT_TOKEN")
        .ok()
        .filter(|k| !k.is_empty())
        .or_else(|| keyring::get_secret("github_copilot_token").ok().flatten());

    if let Some(key) = copilot_token {
        let state = std::sync::Arc::new(crate::ai::auth::OAuthState::new());
        *state.github_token.write() = Some(SecretString::new(key));
        let auth = AuthStrategy::OAuthDevice(state);
        if let Ok(provider) = CopilotProvider::new(auth) {
            reg.register(Arc::new(provider));
        }
    }

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            let auth = AuthStrategy::ApiKey(SecretString::new(key));
            let url = url::Url::parse("https://api.anthropic.com/").unwrap();
            reg.register(Arc::new(AnthropicProvider::new("anthropic", url, auth)));
        }
    }

    if let Ok(key) = std::env::var("ZAI_API_KEY") {
        if !key.is_empty() {
            let auth = AuthStrategy::ApiKey(SecretString::new(key));
            reg.register(Arc::new(ZaiProvider::new(auth)));
        }
    }

    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.is_empty() {
            let auth = AuthStrategy::ApiKey(SecretString::new(key));
            let url = url::Url::parse("https://api.openai.com/").unwrap();
            reg.register(Arc::new(OpenAiProvider::new(url, auth)));
        }
    }

    reg
}

fn resolve_shell(args: &[String]) -> Vec<String> {
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let shell_args = resolve_shell(&cli.args);

    let _guard = RawGuard::init()?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run(shell_args, cli.provider, cli.model))
}

fn write_stdout(bytes: &[u8]) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

fn command_prefix_len(line: &str) -> usize {
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

fn redraw_completion(input_line: &str, selected: usize, old_count: usize) -> usize {
    let items: Vec<&str> = if let Some(arg) = input_line.strip_prefix("/connect ") {
        complete_providers(arg)
    } else if input_line.starts_with('/') {
        complete(input_line)
            .iter()
            .map(|c| c.name)
            .collect()
    } else {
        Vec::new()
    };
    render_completion_items(&items, selected, old_count);
    items.len()
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

fn redraw_input(prev_len: usize, input: &str) {
    let mut out = String::new();
    if prev_len > 0 {
        out.push_str(&format!("\x1b[{prev_len}D"));
    }
    out.push_str("\x1b[K");
    out.push_str(&colorize_command(input));
    write_stdout(out.as_bytes());
}

fn history_back(
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

fn history_forward(
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

fn clear_completion(count: usize) {
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

fn print_greeting() {
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

async fn run(
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
                                buf.push(EntryKind::Output, line.to_string());
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        })?;

    let initial_size = crossterm::terminal::size()?;
    let _ = pty.resize(initial_size.1, initial_size.0);

    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut input_line = String::new();
    let mut stdin_buf = [0u8; 256];
    let mut selected: usize = 0;
    let mut tab_prefix: Option<String> = None;
    let mut escape_state = EscapeState::Normal;
    let mut flushed: bool = false;
    let mut cached_models: Vec<String> = Vec::new();
    let mut completion_count: usize = 0;
    let mut history: Vec<String> = Vec::new();
    let mut history_pos: Option<usize> = None;
    let mut pending_buffer: String = String::new();
    let mut conversation = Conversation::load();

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
                            let prefix_len = command_prefix_len(&input_line);
                            let args = &input_line[prefix_len..];
                            let trimmed = args.trim_end();
                            let after_space = trimmed.len() - trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
                            let chop = (args.len() - trimmed.len()) + after_space;
                            let chop = chop.min(input_line.len().saturating_sub(prefix_len));
                            if chop > 0 {
                                let prev_len = input_line.len();
                                input_line.truncate(input_line.len() - chop);
                                redraw_input(prev_len, &input_line);
                            }
                            selected = 0;
                            tab_prefix = None;
                            completion_count = redraw_completion(&input_line, selected, completion_count);
                        } else {
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
                            redraw_completion(&input_line, selected, completion_count);
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
                            redraw_completion(&input_line, selected, completion_count);
                        consumed = true;
                    } else if buffered {
                        input_line.clear();
                        write_stdout(b"\r\x1b[K\x1b[0m");
                        clear_completion(completion_count);
                        completion_count = 0;
                        selected = 0;
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
                        history_back(
                            &mut input_line,
                            &history,
                            &mut history_pos,
                            &mut pending_buffer,
                        );
                        selected = 0;
                        tab_prefix = None;
                        completion_count =
                            redraw_completion(&input_line, selected, completion_count);
                    } else if buffered && byte == b'B' {
                        history_forward(
                            &mut input_line,
                            &history,
                            &mut history_pos,
                            &mut pending_buffer,
                        );
                        selected = 0;
                        tab_prefix = None;
                        completion_count =
                            redraw_completion(&input_line, selected, completion_count);
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
                let _ = pty.write(&[byte]);
                if byte == 13 || byte == 10 {
                    flushed = false;
                    input_line.clear();
                    selected = 0;
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
                                completion_count = redraw_completion(&input_line, selected, completion_count);
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
                        history_forward(
                            &mut input_line,
                            &history,
                            &mut history_pos,
                            &mut pending_buffer,
                        );
                        selected = 0;
                        tab_prefix = None;
                        completion_count =
                            redraw_completion(&input_line, selected, completion_count);
                    } else {
                        let _ = pty.write(&[byte]);
                    }
                }
                16 => {
                    if buffered {
                        history_back(
                            &mut input_line,
                            &history,
                            &mut history_pos,
                            &mut pending_buffer,
                        );
                        selected = 0;
                        tab_prefix = None;
                        completion_count =
                            redraw_completion(&input_line, selected, completion_count);
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
                                completion_count = redraw_completion(&input_line, selected, completion_count);
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
                        let prefix_len = command_prefix_len(&input_line);
                        let args = &input_line[prefix_len..];
                        let trimmed = args.trim_end();
                        let after_space = trimmed.len() - trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
                        let chop = (args.len() - trimmed.len()) + after_space;
                        let chop = chop.min(input_line.len().saturating_sub(prefix_len));
                        if chop > 0 {
                            let prev_len = input_line.len();
                            input_line.truncate(input_line.len() - chop);
                            redraw_input(prev_len, &input_line);
                        }
                        selected = 0;
                        tab_prefix = None;
                        completion_count = redraw_completion(&input_line, selected, completion_count);
                    } else {
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
                        completion_count = redraw_completion(&input_line, selected, completion_count);
                    } else {
                        let _ = pty.write(&[byte]);
                    }
                }
                _ => {
                    let entering_buffered = input_line.is_empty() && byte == b'/';
                    if buffered || entering_buffered {
                        if byte == 9 {
                            if tab_prefix.is_none() {
                                tab_prefix = Some(input_line.clone());
                            }
                            let prefix = tab_prefix.as_ref().unwrap();
                            let (items, make_input): (Vec<String>, Box<dyn Fn(&str) -> String>) =
                                if let Some(arg) = prefix.strip_prefix("/connect ") {
                                    (complete_providers(arg).into_iter().map(String::from).collect(), Box::new(|s: &str| format!("/connect {s}")))
                                } else if let Some(arg) = prefix.strip_prefix("/model ") {
                                    let matches: Vec<String> = cached_models.iter().filter(|m| m.starts_with(arg)).cloned().collect();
                                    (matches, Box::new(|s: &str| format!("/model {s}")))
                                } else {
                                    (complete(prefix).iter().map(|c| c.name.to_string()).collect(), Box::new(|s: &str| s.to_string()))
                                };
                            let items_ref: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
                            if !items_ref.is_empty() {
                                let idx = selected % items_ref.len();
                                let new_input = make_input(items_ref[idx]);
                                if new_input != input_line {
                                    redraw_input(input_line.len(), &new_input);
                                    input_line = new_input;
                                }
                                completion_count = items_ref.len();
                                render_completion_items(&items_ref, idx, completion_count);
                                selected = (selected + 1) % items_ref.len();
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
                            completion_count = redraw_completion(&input_line, selected, completion_count);
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
                        if byte >= 32 || byte == 9 {
                            input_line.push(byte as char);
                        }
                        let _ = pty.write(&[byte]);
                    }
                }
            }
        }
    }

    write_stdout(b"\r\n");
    Ok(())
}

#[derive(Clone, Copy)]
enum EscapeState {
    Normal,
    GotEsc,
    GotIntroducer(u8),
}

async fn plan_needs_context(
    provider: &Arc<dyn crate::ai::provider::LlmProvider>,
    query: &str,
) -> bool {
    let req = ChatRequest {
        provider_id: provider.id().to_string(),
        model: PLANNER_MODEL.to_string(),
        messages: vec![
            ChatMessage::system(
                "You are a classifier. Does this user question need terminal/shell context \
                 (e.g. commands, errors, files, processes, paths, git, docker, build output) \
                 to answer well? Reply ONLY with 'yes' or 'no'.",
            ),
            ChatMessage::user(query),
        ],
    };

    let stream = match provider.chat(req).await {
        Ok(s) => s,
        Err(_) => return false,
    };

    let answer = collect_stream(stream).await;
    answer.to_lowercase().contains("yes")
}

async fn collect_stream(
    mut stream: futures::stream::BoxStream<'static, ChatEvent>,
) -> String {
    let mut out = String::new();
    while let Some(event) = stream.next().await {
        match event {
            ChatEvent::Token { text } => out.push_str(&text),
            ChatEvent::Done | ChatEvent::Error { .. } => break,
            ChatEvent::Thinking { .. } => {}
        }
    }
    out
}

const MAX_HISTORY_PAIRS: usize = 20;

async fn handle_chat(
    registry: &Arc<std::sync::Mutex<ProviderRegistry>>,
    buffer: &Arc<Mutex<ContextBuffer>>,
    conversation: &mut Conversation,
    query: &str,
    provider_override: &Option<String>,
    model_override: &Option<String>,
) -> Result<()> {
    let provider = {
        let reg = registry.lock().unwrap();
        let providers = reg.list();
        if providers.is_empty() {
            drop(reg);
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let _ = out.write_all(b"\r\nchatsh: no providers configured. Set ANTHROPIC_API_KEY, OPENAI_API_KEY, ZAI_API_KEY, or GITHUB_COPILOT_TOKEN.\r\n");
            let _ = out.flush();
            return Ok(());
        }
        if let Some(id) = provider_override {
            reg.get(id)?
        } else {
            providers.into_iter().next().unwrap()
        }
    };

    let model = match model_override {
        Some(m) => m.clone(),
        None => {
            let models = provider.list_models().await.unwrap_or_default();
            models
                .first()
                .map(|m| m.id.clone())
                .unwrap_or_else(|| "default".to_string())
        }
    };

    let needs_context = plan_needs_context(&provider, query).await;

    let system_prompt = if needs_context {
        let context_lines = {
            let buf = buffer.lock().unwrap();
            buf.recent_lines(30)
                .iter()
                .map(|e| e.line.clone())
                .collect::<Vec<_>>()
        };
        format!(
            "You are a helpful AI assistant embedded in the user's shell session.\n\
             Recent terminal output:\n---\n{}\n---\n\
             Use the terminal output to help answer the user's question.",
            context_lines.join("\n")
        )
    } else {
        "You are a helpful AI assistant.".to_string()
    };

    let user_msg = ChatMessage::user(query);
    let mut messages = Vec::with_capacity(conversation.turn_count() * 2 + 2);
    messages.push(ChatMessage::system(system_prompt));
    messages.extend(conversation.recent(MAX_HISTORY_PAIRS));
    messages.push(user_msg.clone());

    let req = ChatRequest {
        provider_id: provider.id().to_string(),
        model,
        messages,
    };

    let stream = provider.chat(req).await?;
    let answer = stream_response(stream).await?;

    if !answer.is_empty() {
        let _ = conversation.append(user_msg, ChatMessage::assistant(answer));
    }

    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamPhase {
    Initial,
    Thinking,
    Answering,
}

async fn stream_response(
    mut stream: futures::stream::BoxStream<'static, ChatEvent>,
) -> Result<String> {
    let mut renderer = MarkdownRenderer::new();
    let mut phase = StreamPhase::Initial;
    let mut answer = String::new();

    write_stdout("\r\n\x1b[2;3m✻ Thinking\x1b[0m".as_bytes());
    let mut placeholder_active = true;
    let mut dots: usize = 0;

    let mut ticker =
        tokio::time::interval(std::time::Duration::from_millis(350));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await;

    'outer: loop {
        tokio::select! {
            biased;
            event = stream.next() => {
                let Some(event) = event else { break 'outer; };
                match event {
                    ChatEvent::Thinking { text } => {
                        if placeholder_active {
                            write_stdout(b"\r\x1b[K");
                            placeholder_active = false;
                        }
                        if phase != StreamPhase::Thinking {
                            write_stdout(
                                "\x1b[2;3m✻ Thinking…\x1b[0m\r\n\x1b[2;3m  "
                                    .as_bytes(),
                            );
                            phase = StreamPhase::Thinking;
                        }
                        let normalized = text.replace('\n', "\r\n  ");
                        write_stdout(normalized.as_bytes());
                    }
                    ChatEvent::Token { text } => {
                        if placeholder_active {
                            write_stdout(b"\r\x1b[K");
                            placeholder_active = false;
                        }
                        if phase == StreamPhase::Thinking {
                            write_stdout(b"\x1b[0m\r\n\r\n");
                        }
                        phase = StreamPhase::Answering;
                        answer.push_str(&text);
                        let rendered = renderer.push(&text);
                        if !rendered.is_empty() {
                            write_stdout(rendered.as_bytes());
                        }
                    }
                    ChatEvent::Done => {
                        if placeholder_active {
                            write_stdout(b"\r\x1b[K");
                        }
                        if phase == StreamPhase::Thinking {
                            write_stdout(b"\x1b[0m\r\n");
                        }
                        let trailing = renderer.flush();
                        if !trailing.is_empty() {
                            write_stdout(trailing.as_bytes());
                        }
                        break 'outer;
                    }
                    ChatEvent::Error { message } => {
                        if placeholder_active {
                            write_stdout(b"\r\x1b[K");
                        }
                        if phase == StreamPhase::Thinking {
                            write_stdout(b"\x1b[0m\r\n");
                        }
                        let trailing = renderer.flush();
                        if !trailing.is_empty() {
                            write_stdout(trailing.as_bytes());
                        }
                        write_stdout(
                            format!("\r\n\x1b[31merror: {message}\x1b[0m").as_bytes(),
                        );
                        break 'outer;
                    }
                }
            }
            _ = ticker.tick() => {
                if placeholder_active {
                    dots = (dots % 3) + 1;
                    let pip = ".".repeat(dots);
                    write_stdout(
                        format!("\r\x1b[K\x1b[2;3m✻ Thinking{pip}\x1b[0m")
                            .as_bytes(),
                    );
                }
            }
        }
    }

    write_stdout(b"\r\n\x1b[0m");
    Ok(answer)
}

const PROVIDER_ENV_MAP: &[(&str, &str, &str)] = &[
    ("zai", "Z.ai (coding plan)", "ZAI_API_KEY"),
    ("anthropic", "Anthropic", "ANTHROPIC_API_KEY"),
    ("openai", "OpenAI", "OPENAI_API_KEY"),
];

async fn handle_connect(
    registry: &Arc<std::sync::Mutex<ProviderRegistry>>,
    provider_name: &str,
    provider_id: &mut Option<String>,
    model_id: &mut Option<String>,
) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if provider_name.is_empty() {
        let reg = registry.lock().unwrap();
        let providers = reg.list();
        drop(reg);

        if providers.is_empty() {
            let _ = out.write_all(b"\r\nNo providers connected.\r\n");
            let _ = out.write_all(
                b"  Use /connect <provider> to connect. Available: zai, copilot, anthropic, openai\r\n",
            );
        } else {
            let _ = out.write_all(b"\r\n\x1b[1mConnected providers:\x1b[0m\r\n");
            for p in &providers {
                let active = provider_id.as_deref() == Some(p.id());
                let marker = if active {
                    " \x1b[32m(active)\x1b[0m"
                } else {
                    ""
                };
                let _ = out.write_all(
                    format!("  {} - {}{}\r\n", p.id(), p.display_name(), marker).as_bytes(),
                );
            }
            let _ = out
                .write_all(b"\r\n  /connect <name> to switch. Available: zai, copilot, anthropic, openai\r\n");
        }
        let _ = out.flush();
        return Ok(());
    }

    {
        let reg = registry.lock().unwrap();
        if reg.get(provider_name).is_ok() {
            drop(reg);
            *provider_id = Some(provider_name.to_string());
            *model_id = None;
            let _ = out.write_all(
                format!("\r\n\x1b[32mSwitched to {provider_name}.\x1b[0m\r\n").as_bytes(),
            );
            let _ = out.flush();
            return Ok(());
        }
    }

    match provider_name {
        "copilot" => handle_connect_copilot(registry, provider_id, model_id, &mut out).await,
        name => {
            let spec = PROVIDER_ENV_MAP.iter().find(|(id, _, _)| *id == name);
            match spec {
                Some(&(_, display, env_var)) => {
                    let key = std::env::var(env_var).unwrap_or_default();
                    if key.is_empty() {
                        let _ = out.write_all(
                            format!(
                                "\r\n\x1b[31m{env_var} not set. Run: export {env_var}=<key>\x1b[0m\r\n"
                            )
                            .as_bytes(),
                        );
                        let _ = out.flush();
                        return Ok(());
                    }
                    let auth = AuthStrategy::ApiKey(SecretString::new(key));
                    {
                        let mut reg = registry.lock().unwrap();
                        match name {
                            "zai" => reg.register(Arc::new(ZaiProvider::new(auth))),
                            "anthropic" => {
                                let url = url::Url::parse("https://api.anthropic.com/").unwrap();
                                reg.register(Arc::new(AnthropicProvider::new(
                                    "anthropic", url, auth,
                                )));
                            }
                            "openai" => {
                                let url = url::Url::parse("https://api.openai.com/").unwrap();
                                reg.register(Arc::new(OpenAiProvider::new(url, auth)));
                            }
                            _ => {}
                        }
                    }
                    *provider_id = Some(name.to_string());
                    *model_id = None;
                    let _ = out.write_all(
                        format!(
                            "\r\n\x1b[32mConnected to {display}.\x1b[0m\r\n"
                        )
                        .as_bytes(),
                    );
                    let _ = out.flush();
                    Ok(())
                }
                None => {
                    let _ = out.write_all(
                        format!(
                            "\r\n\x1b[31mUnknown provider '{name}'. Available: zai, copilot, anthropic, openai\x1b[0m\r\n"
                        )
                        .as_bytes(),
                    );
                    let _ = out.flush();
                    Ok(())
                }
            }
        }
    }
}

async fn handle_connect_copilot(
    registry: &Arc<std::sync::Mutex<ProviderRegistry>>,
    provider_id: &mut Option<String>,
    model_id: &mut Option<String>,
    out: &mut std::io::StdoutLock<'_>,
) -> Result<()> {
    let state = Arc::new(OAuthState::new());
    let auth = AuthStrategy::OAuthDevice(state);
    let provider = CopilotProvider::new(auth)?;

    let challenge = provider.device_flow_start().await?;
    let _ = out.write_all(
        format!(
            "\r\n\x1b[1mGitHub Copilot Device Flow\x1b[0m\r\n\r\n  Code: \x1b[33m{}\x1b[0m\r\n  URL:  \x1b[4m{}\x1b[0m\r\n\r\n  Waiting for authorization...\r\n",
            challenge.user_code, challenge.verification_uri
        )
        .as_bytes(),
    );
    let _ = out.flush();

    let mut interval = challenge.interval_seconds.max(5);
    let mut elapsed = 0u32;
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(interval as u64)).await;
        elapsed += interval;

        if elapsed >= challenge.expires_in_seconds {
            let _ = out.write_all(b"\r\n\x1b[31mDevice flow expired. Try again.\x1b[0m\r\n");
            let _ = out.flush();
            return Ok(());
        }

        match provider.device_flow_poll(&challenge.device_code).await? {
            DeviceFlowPoll::Authorized(token) => {
                let _ = keyring::set_secret("github_copilot_token", &token);
                registry.lock().unwrap().register(Arc::new(provider));
                *provider_id = Some("copilot".to_string());
                *model_id = None;
                let _ = out
                    .write_all(b"\r\n\x1b[32mGitHub Copilot connected!\x1b[0m\r\n");
                let _ = out.flush();
                return Ok(());
            }
            DeviceFlowPoll::Pending => {}
            DeviceFlowPoll::SlowDown => {
                interval += 5;
            }
            DeviceFlowPoll::Expired => {
                let _ = out
                    .write_all(b"\r\n\x1b[31mDevice flow expired. Try again.\x1b[0m\r\n");
                let _ = out.flush();
                return Ok(());
            }
            DeviceFlowPoll::Denied => {
                let _ = out
                    .write_all(b"\r\n\x1b[31mAuthorization denied.\x1b[0m\r\n");
                let _ = out.flush();
                return Ok(());
            }
            DeviceFlowPoll::Other(msg) => {
                let _ = out.write_all(
                    format!("\r\n\x1b[31mError: {msg}\x1b[0m\r\n").as_bytes(),
                );
                let _ = out.flush();
                return Ok(());
            }
        }
    }
}

async fn handle_model(
    registry: &Arc<std::sync::Mutex<ProviderRegistry>>,
    provider_id: &Option<String>,
    model_id: &mut Option<String>,
    name: Option<&str>,
) -> Result<Vec<String>> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let pid = match provider_id {
        Some(id) => id.clone(),
        None => {
            let _ = out.write_all(
                b"\r\n\x1b[31mNo provider selected. Use /connect first.\x1b[0m\r\n",
            );
            let _ = out.flush();
            return Ok(Vec::new());
        }
    };

    let provider = {
        let reg = registry.lock().unwrap();
        match reg.get(&pid) {
            Ok(p) => p,
            Err(_) => {
                drop(reg);
                let _ = out.write_all(
                    format!("\r\n\x1b[31mProvider '{pid}' not found.\x1b[0m\r\n").as_bytes(),
                );
                let _ = out.flush();
                return Ok(Vec::new());
            }
        }
    };

    let models = provider.list_models().await.unwrap_or_default();
    let model_ids: Vec<String> = models.iter().map(|m| m.id.clone()).collect();

    match name {
        Some(model_name) => {
            if models.iter().any(|m| m.id == model_name) {
                *model_id = Some(model_name.to_string());
                let rate = model_rate(model_name);
                let _ = out.write_all(
                    format!("\r\n\x1b[32mModel set to {model_name} [{rate}].\x1b[0m\r\n")
                        .as_bytes(),
                );
            } else {
                let available: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
                let _ = out.write_all(
                    format!(
                        "\r\n\x1b[31mModel '{model_name}' not found. Available: {}\x1b[0m\r\n",
                        available.join(", ")
                    )
                    .as_bytes(),
                );
            }
        }
        None => {
            if models.is_empty() {
                let _ = out.write_all(b"\r\nNo models available.\r\n");
            } else {
                let _ = out.write_all(
                    format!("\r\n\x1b[1mModels ({pid}):\x1b[0m\r\n").as_bytes(),
                );
                for m in &models {
                    let active = model_id.as_deref() == Some(&m.id);
                    let marker = if active { " *" } else { "  " };
                    let ctx = if m.context_tokens > 0 {
                        format!(" ({}k ctx)", m.context_tokens / 1000)
                    } else {
                        String::new()
                    };
                    let rate = model_rate(&m.id);
                    let _ = out.write_all(
                        format!(
                            "{}{}\x1b[0m{}\r\n",
                            if active { "\x1b[32m" } else { "\x1b[2m" },
                            marker,
                            format!("{} - {} [{}]{}", m.id, m.display_name, rate, ctx)
                        )
                        .as_bytes(),
                    );
                }
            }
        }
    }
    let _ = out.flush();
    Ok(model_ids)
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parse() {
        let cli = Cli::parse_from(["chatsh", "/bin/zsh"]);
        assert_eq!(cli.args, vec!["/bin/zsh"]);
        assert!(cli.provider.is_none());
        assert!(cli.model.is_none());
    }

    #[test]
    fn test_cli_no_args() {
        let cli = Cli::parse_from(["chatsh"]);
        assert!(cli.args.is_empty());
    }

    #[test]
    fn test_cli_with_provider() {
        let cli = Cli::parse_from(["chatsh", "--provider", "copilot", "--model", "gpt-4o", "/bin/bash"]);
        assert_eq!(cli.provider.as_deref(), Some("copilot"));
        assert_eq!(cli.model.as_deref(), Some("gpt-4o"));
        assert_eq!(cli.args, vec!["/bin/bash"]);
    }

    #[test]
    fn test_build_registry_with_env() {
        let reg = build_registry();
        assert!(reg.list().len() <= 4);
    }
}
