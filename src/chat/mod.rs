// Chat orchestration: context planning, request assembly, and streaming the
// provider response to the terminal.

pub mod commands;

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;

use crate::ai::{ChatEvent, ChatMessage, ChatRequest, LlmProvider, ProviderRegistry};
use crate::input::repl::write_stdout;
use crate::shell::ContextBuffer;
use crate::storage::Conversation;
use crate::tui::MarkdownRenderer;

const SPINNER_TICK: Duration = Duration::from_millis(350);

async fn plan_needs_context(provider: &Arc<dyn LlmProvider>, model: &str, query: &str) -> bool {
    let req = ChatRequest {
        provider_id: provider.id().to_string(),
        model: model.to_string(),
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

async fn collect_stream(mut stream: futures::stream::BoxStream<'static, ChatEvent>) -> String {
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

pub(crate) async fn handle_chat(
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

    // Show the thinking indicator and start spinner immediately — before any API calls.
    write_stdout(b"\r\n\x1b[2;3m\xe2\x9c\xbb Thinking\x1b[0m");
    let spinning = Arc::new(AtomicBool::new(true));
    let spin_flag = spinning.clone();
    let spinner_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(SPINNER_TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await; // consume the immediate first tick
        let mut dots = 0usize;
        loop {
            interval.tick().await;
            if !spin_flag.load(Ordering::Relaxed) {
                break;
            }
            dots = (dots % 3) + 1;
            let pip = ".".repeat(dots);
            write_stdout(format!("\r\x1b[K\x1b[2;3m\u{273b} Thinking{pip}\x1b[0m").as_bytes());
        }
    });

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

    let needs_context = plan_needs_context(&provider, &model, query).await;

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

    let provider_id_str = provider.id().to_string();
    let req = ChatRequest {
        provider_id: provider_id_str.clone(),
        model: model.clone(),
        messages,
    };

    let answer = async {
        let stream = provider.chat(req).await?;
        stream_response(stream, spinning).await
    }
    .await;
    spinner_handle.abort();
    let answer = answer?;

    write_stdout(
        format!("\r\n\x1b[2mAnswered by {provider_id_str} · {model}\x1b[0m\r\n").as_bytes(),
    );

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
    spinning: Arc<AtomicBool>,
) -> Result<String> {
    let mut renderer = MarkdownRenderer::new();
    let mut phase = StreamPhase::Initial;
    let mut answer = String::new();

    while let Some(event) = stream.next().await {
        match event {
            ChatEvent::Thinking { text } => {
                if spinning.swap(false, Ordering::Relaxed) {
                    write_stdout(b"\r\x1b[K");
                }
                if phase != StreamPhase::Thinking {
                    write_stdout("\x1b[2;3m✻ Thinking…\x1b[0m\r\n\x1b[2;3m  ".as_bytes());
                    phase = StreamPhase::Thinking;
                }
                let normalized = text.replace('\n', "\r\n  ");
                write_stdout(normalized.as_bytes());
            }
            ChatEvent::Token { text } => {
                if spinning.swap(false, Ordering::Relaxed) {
                    write_stdout(b"\r\x1b[K");
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
                if spinning.swap(false, Ordering::Relaxed) {
                    write_stdout(b"\r\x1b[K");
                }
                if phase == StreamPhase::Thinking {
                    write_stdout(b"\x1b[0m\r\n");
                }
                let trailing = renderer.flush();
                if !trailing.is_empty() {
                    write_stdout(trailing.as_bytes());
                }
                break;
            }
            ChatEvent::Error { message } => {
                if spinning.swap(false, Ordering::Relaxed) {
                    write_stdout(b"\r\x1b[K");
                }
                if phase == StreamPhase::Thinking {
                    write_stdout(b"\x1b[0m\r\n");
                }
                let trailing = renderer.flush();
                if !trailing.is_empty() {
                    write_stdout(trailing.as_bytes());
                }
                write_stdout(format!("\r\n\x1b[31merror: {message}\x1b[0m").as_bytes());
                break;
            }
        }
    }

    write_stdout(b"\r\n\x1b[0m");
    Ok(answer)
}
