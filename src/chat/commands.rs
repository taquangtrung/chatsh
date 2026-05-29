// `/connect` and `/model` command handlers plus provider registry construction.

use std::io::{Read, Write};
use std::sync::Arc;

use anyhow::Result;
use secrecy::SecretString;

use crate::ai::AnthropicProvider;
use crate::ai::{AuthStrategy, OAuthState};
use crate::ai::{CopilotProvider, DeviceFlowPoll};
use crate::ai::OpenAiProvider;
use crate::ai::ProviderRegistry;
use crate::ai::ZaiProvider;
use crate::ai::{PROVIDER_ANTHROPIC, PROVIDER_COPILOT, PROVIDER_OPENAI, PROVIDER_ZAI};
use crate::storage::keyring;

const KEYRING_ZAI_API_KEY: &str = "zai_api_key";

const AVAILABLE_PROVIDERS: &str =
    "z.ai-coding-plan (Z.ai Coding Plan), github-copilot (GitHub Copilot), anthropic (Anthropic), openai (OpenAI)";

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

pub(crate) fn build_registry() -> ProviderRegistry {
    let mut reg = ProviderRegistry::new();

    let copilot_token = std::env::var("GITHUB_COPILOT_TOKEN")
        .ok()
        .filter(|k| !k.is_empty())
        .or_else(|| keyring::get_secret("github_copilot_token").ok().flatten());

    if let Some(key) = copilot_token {
        let state = Arc::new(OAuthState::new());
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
            reg.register(Arc::new(AnthropicProvider::new(PROVIDER_ANTHROPIC, url, auth)));
        }
    }

    let zai_key = std::env::var("ZAI_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
        .or_else(|| keyring::get_secret(KEYRING_ZAI_API_KEY).ok().flatten());

    if let Some(key) = zai_key {
        let auth = AuthStrategy::ApiKey(SecretString::new(key));
        reg.register(Arc::new(ZaiProvider::build(auth)));
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

const PROVIDER_ENV_MAP: &[(&str, &str, &str)] = &[
    (PROVIDER_ANTHROPIC, "Anthropic", "ANTHROPIC_API_KEY"),
    (PROVIDER_OPENAI, "OpenAI", "OPENAI_API_KEY"),
];

pub(crate) async fn handle_connect(
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
                format!("  Use /connect <provider> to connect. Available: {AVAILABLE_PROVIDERS}\r\n").as_bytes(),
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
                .write_all(format!("\r\n  /connect <name> to switch. Available: {AVAILABLE_PROVIDERS}\r\n").as_bytes());
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
                format!("\r\n\x1b[32mSwitched to {provider_name}.\x1b[0m  (Use /reauth {provider_name} to re-authenticate.)\r\n").as_bytes(),
            );
            let _ = out.flush();
            return Ok(());
        }
    }

    match provider_name {
        PROVIDER_COPILOT => handle_connect_copilot(registry, provider_id, model_id, &mut out).await,
        PROVIDER_ZAI => handle_connect_zai(registry, provider_id, model_id, &mut out, false).await,
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
                            PROVIDER_ZAI => reg.register(Arc::new(ZaiProvider::build(auth))),
                            PROVIDER_ANTHROPIC => {
                                let url = url::Url::parse("https://api.anthropic.com/").unwrap();
                                reg.register(Arc::new(AnthropicProvider::new(
                                    PROVIDER_ANTHROPIC, url, auth,
                                )));
                            }
                            PROVIDER_OPENAI => {
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
                            "\r\n\x1b[31mUnknown provider '{name}'. Available: {AVAILABLE_PROVIDERS}\x1b[0m\r\n"
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

pub(crate) async fn handle_reauth(
    registry: &Arc<std::sync::Mutex<ProviderRegistry>>,
    provider_name: &str,
    provider_id: &mut Option<String>,
    model_id: &mut Option<String>,
) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if provider_name.is_empty() {
        let _ = out.write_all(
            format!("\r\nUsage: /reauth <provider>  (Available: {AVAILABLE_PROVIDERS})\r\n")
                .as_bytes(),
        );
        let _ = out.flush();
        return Ok(());
    }

    match provider_name {
        PROVIDER_COPILOT => {
            // Always starts a fresh device flow, overwriting the stored token.
            handle_connect_copilot(registry, provider_id, model_id, &mut out).await
        }
        PROVIDER_ZAI => {
            // force_prompt = true: skip keyring, always ask for a new key.
            handle_connect_zai(registry, provider_id, model_id, &mut out, true).await
        }
        name if PROVIDER_ENV_MAP.iter().any(|(id, _, _)| *id == name) => {
            let env_var = PROVIDER_ENV_MAP
                .iter()
                .find(|(id, _, _)| *id == name)
                .map(|(_, _, env)| *env)
                .unwrap();
            let _ = out.write_all(
                format!(
                    "\r\nTo re-authenticate {name}, update the environment variable and reconnect:\r\n  export {env_var}=<new_key>\r\n  /connect {name}\r\n"
                )
                .as_bytes(),
            );
            let _ = out.flush();
            Ok(())
        }
        name => {
            let _ = out.write_all(
                format!(
                    "\r\n\x1b[31mUnknown provider '{name}'. Available: {AVAILABLE_PROVIDERS}\x1b[0m\r\n"
                )
                .as_bytes(),
            );
            let _ = out.flush();
            Ok(())
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
                *provider_id = Some(PROVIDER_COPILOT.to_string());
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

async fn handle_connect_zai(
    registry: &Arc<std::sync::Mutex<ProviderRegistry>>,
    provider_id: &mut Option<String>,
    model_id: &mut Option<String>,
    out: &mut std::io::StdoutLock<'_>,
    force_prompt: bool,
) -> Result<()> {
    // Prefer env var; when force_prompt is false also try keyring; otherwise always prompt.
    let from_env = std::env::var("ZAI_API_KEY").ok().filter(|k| !k.is_empty());

    let (key, save_to_keyring) = if let Some(k) = from_env {
        if force_prompt {
            let _ = out.write_all(
                b"\r\n\x1b[33mNote: ZAI_API_KEY env var is set and takes precedence.\x1b[0m\r\n  To use a different key, unset it first: unset ZAI_API_KEY\r\n",
            );
            let _ = out.flush();
        }
        (k, false)
    } else if !force_prompt {
        match keyring::get_secret(KEYRING_ZAI_API_KEY) {
            Ok(Some(k)) => (k, false),
            Ok(None) => match prompt_api_key("Z.ai Coding Plan API key", out) {
                None => {
                    let _ = out.write_all(b"\r\n\x1b[33mCancelled.\x1b[0m\r\n");
                    let _ = out.flush();
                    return Ok(());
                }
                Some(k) => (k, true),
            },
            Err(e) => {
                let _ = out.write_all(
                    format!("\r\n\x1b[33mWarning: keyring read failed ({e}); enter key manually.\x1b[0m\r\n").as_bytes(),
                );
                let _ = out.flush();
                match prompt_api_key("Z.ai Coding Plan API key", out) {
                    None => {
                        let _ = out.write_all(b"\r\n\x1b[33mCancelled.\x1b[0m\r\n");
                        let _ = out.flush();
                        return Ok(());
                    }
                    Some(k) => (k, true),
                }
            }
        }
    } else {
        // force_prompt: always ask for a new key and overwrite keyring.
        match prompt_api_key("Z.ai Coding Plan API key (leave empty to cancel)", out) {
            None => {
                let _ = out.write_all(b"\r\n\x1b[33mCancelled.\x1b[0m\r\n");
                let _ = out.flush();
                return Ok(());
            }
            Some(k) => (k, true),
        }
    };

    // Register the provider before attempting keyring write so a save failure
    // does not prevent connectivity.
    let auth = AuthStrategy::ApiKey(SecretString::new(key.clone()));
    let provider = Arc::new(ZaiProvider::build(auth));

    // Validate the key with a quick models API call.
    let _ = out.write_all(b"\r\n  Verifying API key...");
    let _ = out.flush();
    if let Err(e) = provider.validate_key().await {
        let _ = out.write_all(
            format!("\r\n\x1b[31mError: API key rejected by Z.ai ({e}).\x1b[0m\r\n  Check your key and try again.\r\n").as_bytes(),
        );
        let _ = out.flush();
        return Ok(());
    }

    registry.lock().unwrap().register(provider);
    *provider_id = Some(PROVIDER_ZAI.to_string());
    *model_id = None;
    let _ = out.write_all(b"\r\n\x1b[32mConnected to Z.ai Coding Plan.\x1b[0m\r\n");

    if save_to_keyring {
        match keyring::set_secret(KEYRING_ZAI_API_KEY, &key) {
            Ok(()) => {
                let _ = out.write_all(b"  API key saved to keyring.\r\n");
            }
            Err(e) => {
                let _ = out.write_all(
                    format!("  \x1b[33mWarning: could not save key ({e}). Connected for this session only.\x1b[0m\r\n").as_bytes(),
                );
            }
        }
    }

    let _ = out.flush();
    Ok(())
}

/// Reads an API key from stdin while in raw terminal mode.
/// Characters are masked with `*`; Backspace erases; Enter confirms; Ctrl+C/D cancels.
/// Handles pasted input correctly by stripping bracketed-paste escape sequences.
fn prompt_api_key(label: &str, out: &mut std::io::StdoutLock<'_>) -> Option<String> {
    let _ = out.write_all(format!("\r\n  {label}: ").as_bytes());
    // Disable bracketed paste mode so pasted text arrives as plain bytes.
    let _ = out.write_all(b"\x1b[?2004l");
    let _ = out.flush();

    let stdin = std::io::stdin();
    let mut key = String::new();
    let mut stdin_lock = stdin.lock();
    let mut buf = [0u8; 1];
    let mut escape_buf: Vec<u8> = Vec::new(); // accumulates bytes after ESC

    loop {
        match stdin_lock.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }

        // If we're inside an escape sequence, keep consuming until terminator.
        if !escape_buf.is_empty() {
            escape_buf.push(buf[0]);
            // CSI sequences end at a byte in 0x40–0x7E.
            if buf[0] >= 0x40 && buf[0] <= 0x7E {
                escape_buf.clear(); // discard entire escape sequence
            }
            continue;
        }

        match buf[0] {
            27 => {
                // ESC — start of an escape sequence; buffer it and discard.
                escape_buf.push(27);
            }
            13 | 10 => break,                  // Enter (CR/LF)
            3 | 4 => { key.clear(); break; }   // Ctrl+C / Ctrl+D → cancel
            127 | 8 => {                        // DEL / Backspace
                if !key.is_empty() {
                    key.pop();
                    let _ = out.write_all(b"\x08 \x08");
                    let _ = out.flush();
                }
            }
            b if b >= 32 && b < 127 => {        // printable ASCII
                key.push(b as char);
                let _ = out.write_all(b"*");
                let _ = out.flush();
            }
            _ => {}
        }
    }

    // Re-enable bracketed paste mode.
    let _ = out.write_all(b"\x1b[?2004h\r\n");
    let _ = out.flush();

    if key.is_empty() { None } else { Some(key) }
}

pub(crate) async fn handle_model(
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
                    let color = if active { "\x1b[32m" } else { "\x1b[2m" };
                    let _ = out.write_all(
                        format!(
                            "{color}{marker}\x1b[0m{} - {} [{rate}]{ctx}\r\n",
                            m.id, m.display_name
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
    fn test_build_registry_with_env() {
        let reg = build_registry();
        assert!(reg.list().len() <= 4);
    }
}
