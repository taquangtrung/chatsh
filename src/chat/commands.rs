// `/connect` and `/model` command handlers plus provider registry construction.

use std::io::{Read, Write};
use std::sync::Arc;

use anyhow::Result;
use secrecy::SecretString;

use crate::ai::AnthropicProvider;
use crate::ai::LlmProvider;
use crate::ai::OpenAiProvider;
use crate::ai::ProviderRegistry;
use crate::ai::ZaiProvider;
use crate::ai::ModelInfo;
use crate::ai::{AuthStrategy, OAuthState};
use crate::ai::{CopilotProvider, DeviceFlowPoll};
use crate::ai::{PROVIDER_ANTHROPIC, PROVIDER_COPILOT, PROVIDER_OPENAI, PROVIDER_ZAI};
use crate::storage::keyring;

const KEYRING_ZAI_API_KEY: &str = "zai_api_key";

const MSG_CANCELLED: &str = "\r\n\x1b[33mCancelled.\x1b[0m\r\n";

/// Terminal output convenience: write a string and flush in one call, ignoring
/// I/O errors since terminal writes are best-effort. Replaces the repeated
/// `let _ = out.write_all(...); let _ = out.flush();` pairs throughout this module.
trait WriteExt: Write {
    fn put(&mut self, text: &str) {
        let _ = self.write_all(text.as_bytes());
        let _ = self.flush();
    }
}

impl<W: Write + ?Sized> WriteExt for W {}

const AVAILABLE_PROVIDERS: &str = "z.ai-coding-plan (Z.ai Coding Plan), github-copilot (GitHub Copilot), anthropic (Anthropic), openai (OpenAI)";

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

fn model_rate_label(model_id: &str) -> Option<String> {
    MODEL_RATES
        .iter()
        .find(|(id, _)| model_id.contains(id) || id.contains(model_id))
        .map(|(_, rate)| rate.to_string())
}

/// Format the trailing ` [rate]` suffix shown after a model name, preferring the
/// provider-reported rate and falling back to the static `MODEL_RATES` table.
/// Returns an empty string when no rate is known.
fn format_rate_label(model: &ModelInfo) -> String {
    let static_rate = model_rate_label(&model.id);
    model
        .rate_label
        .as_deref()
        .or(static_rate.as_deref())
        .map(|r| format!(" [{r}]"))
        .unwrap_or_default()
}

/// Construct an API-key provider from its canonical id. Returns `None` for ids
/// that require a specialized connection flow (e.g. copilot's device flow) or
/// that are unknown. Shared by `build_registry` and `handle_connect` so the
/// base URLs and constructors live in one place.
fn create_provider(name: &str, auth: AuthStrategy) -> Option<Arc<dyn LlmProvider>> {
    match name {
        PROVIDER_ZAI => Some(Arc::new(ZaiProvider::build(auth))),
        PROVIDER_ANTHROPIC => {
            let url = url::Url::parse("https://api.anthropic.com/").unwrap();
            Some(Arc::new(AnthropicProvider::new(PROVIDER_ANTHROPIC, url, auth)))
        }
        PROVIDER_OPENAI => {
            let url = url::Url::parse("https://api.openai.com/").unwrap();
            Some(Arc::new(OpenAiProvider::new(url, auth)))
        }
        _ => None,
    }
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

    let api_key_env = |var: &str| std::env::var(var).ok().filter(|k| !k.is_empty());

    if let Some(key) = api_key_env("ANTHROPIC_API_KEY") {
        if let Some(p) = create_provider(PROVIDER_ANTHROPIC, AuthStrategy::ApiKey(SecretString::new(key))) {
            reg.register(p);
        }
    }

    let zai_key = api_key_env("ZAI_API_KEY")
        .or_else(|| keyring::get_secret(KEYRING_ZAI_API_KEY).ok().flatten());
    if let Some(key) = zai_key {
        if let Some(p) = create_provider(PROVIDER_ZAI, AuthStrategy::ApiKey(SecretString::new(key))) {
            reg.register(p);
        }
    }

    if let Some(key) = api_key_env("OPENAI_API_KEY") {
        if let Some(p) = create_provider(PROVIDER_OPENAI, AuthStrategy::ApiKey(SecretString::new(key))) {
            reg.register(p);
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
            out.put("\r\nNo providers connected.\r\n");
            out.put(&format!(
                "  Use /connect <provider> to connect. Available: {AVAILABLE_PROVIDERS}\r\n"
            ));
        } else {
            out.put("\r\n\x1b[1mConnected providers:\x1b[0m\r\n");
            for p in &providers {
                let active = provider_id.as_deref() == Some(p.id());
                let marker = if active {
                    " \x1b[32m(active)\x1b[0m"
                } else {
                    ""
                };
                out.put(&format!("  {} - {}{}\r\n", p.id(), p.display_name(), marker));
            }
            out.put(&format!(
                "\r\n  /connect <name> to switch. Available: {AVAILABLE_PROVIDERS}\r\n"
            ));
        }
        return Ok(());
    }

    {
        let reg = registry.lock().unwrap();
        if reg.get(provider_name).is_ok() {
            drop(reg);
            *provider_id = Some(provider_name.to_string());
            *model_id = None;
            out.put(&format!(
                "\r\n\x1b[32mSwitched to {provider_name}.\x1b[0m  (Use /reauth {provider_name} to re-authenticate.)\r\n"
            ));
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
                        out.put(&format!(
                            "\r\n\x1b[31m{env_var} not set. Run: export {env_var}=<key>\x1b[0m\r\n"
                        ));
                        return Ok(());
                    }
                    let auth = AuthStrategy::ApiKey(SecretString::new(key));
                    if let Some(provider) = create_provider(name, auth) {
                        registry.lock().unwrap().register(provider);
                    }
                    *provider_id = Some(name.to_string());
                    *model_id = None;
                    out.put(&format!("\r\n\x1b[32mConnected to {display}.\x1b[0m\r\n"));
                    Ok(())
                }
                None => {
                    out.put(&format!(
                        "\r\n\x1b[31mUnknown provider '{name}'. Available: {AVAILABLE_PROVIDERS}\x1b[0m\r\n"
                    ));
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
        out.put(&format!(
            "\r\nUsage: /reauth <provider>  (Available: {AVAILABLE_PROVIDERS})\r\n"
        ));
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
            out.put(&format!(
                "\r\nTo re-authenticate {name}, update the environment variable and reconnect:\r\n  export {env_var}=<new_key>\r\n  /connect {name}\r\n"
            ));
            Ok(())
        }
        name => {
            out.put(&format!(
                "\r\n\x1b[31mUnknown provider '{name}'. Available: {AVAILABLE_PROVIDERS}\x1b[0m\r\n"
            ));
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
    out.put(&format!(
        "\r\n\x1b[1mGitHub Copilot Device Flow\x1b[0m\r\n\r\n  Code: \x1b[33m{}\x1b[0m\r\n  URL:  \x1b[4m{}\x1b[0m\r\n\r\n  Waiting for authorization...\r\n",
        challenge.user_code, challenge.verification_uri
    ));

    let mut interval = challenge.interval_seconds.max(5);
    let mut elapsed = 0u32;
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(interval as u64)).await;
        elapsed += interval;

        if elapsed >= challenge.expires_in_seconds {
            out.put("\r\n\x1b[31mDevice flow expired. Try again.\x1b[0m\r\n");
            return Ok(());
        }

        match provider.device_flow_poll(&challenge.device_code).await? {
            DeviceFlowPoll::Authorized(token) => {
                let _ = keyring::set_secret("github_copilot_token", &token);
                registry.lock().unwrap().register(Arc::new(provider));
                *provider_id = Some(PROVIDER_COPILOT.to_string());
                *model_id = None;
                out.put("\r\n\x1b[32mGitHub Copilot connected!\x1b[0m\r\n");
                return Ok(());
            }
            DeviceFlowPoll::Pending => {}
            DeviceFlowPoll::SlowDown => {
                interval += 5;
            }
            DeviceFlowPoll::Expired => {
                out.put("\r\n\x1b[31mDevice flow expired. Try again.\x1b[0m\r\n");
                return Ok(());
            }
            DeviceFlowPoll::Denied => {
                out.put("\r\n\x1b[31mAuthorization denied.\x1b[0m\r\n");
                return Ok(());
            }
            DeviceFlowPoll::Other(msg) => {
                out.put(&format!("\r\n\x1b[31mError: {msg}\x1b[0m\r\n"));
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
            out.put(
                "\r\n\x1b[33mNote: ZAI_API_KEY env var is set and takes precedence.\x1b[0m\r\n  To use a different key, unset it first: unset ZAI_API_KEY\r\n",
            );
        }
        (k, false)
    } else if !force_prompt {
        match keyring::get_secret(KEYRING_ZAI_API_KEY) {
            Ok(Some(k)) => (k, false),
            Ok(None) => match prompt_api_key("Z.ai Coding Plan API key", out) {
                None => {
                    out.put(MSG_CANCELLED);
                    return Ok(());
                }
                Some(k) => (k, true),
            },
            Err(e) => {
                out.put(&format!(
                    "\r\n\x1b[33mWarning: keyring read failed ({e}); enter key manually.\x1b[0m\r\n"
                ));
                match prompt_api_key("Z.ai Coding Plan API key", out) {
                    None => {
                        out.put(MSG_CANCELLED);
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
                out.put(MSG_CANCELLED);
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
    out.put("\r\n  Verifying API key...");
    if let Err(e) = provider.validate_key().await {
        out.put(&format!(
            "\r\n\x1b[31mError: API key rejected by Z.ai ({e}).\x1b[0m\r\n  Check your key and try again.\r\n"
        ));
        return Ok(());
    }

    registry.lock().unwrap().register(provider);
    *provider_id = Some(PROVIDER_ZAI.to_string());
    *model_id = None;
    out.put("\r\n\x1b[32mConnected to Z.ai Coding Plan.\x1b[0m\r\n");

    if save_to_keyring {
        match keyring::set_secret(KEYRING_ZAI_API_KEY, &key) {
            Ok(()) => {
                out.put("  API key saved to keyring.\r\n");
            }
            Err(e) => {
                out.put(&format!(
                    "  \x1b[33mWarning: could not save key ({e}). Connected for this session only.\x1b[0m\r\n"
                ));
            }
        }
    }

    Ok(())
}

/// Reads an API key from stdin while in raw terminal mode.
/// Characters are masked with `*`; Backspace erases; Enter confirms; Ctrl+C/D cancels.
/// Handles pasted input correctly by stripping bracketed-paste escape sequences.
fn prompt_api_key(label: &str, out: &mut std::io::StdoutLock<'_>) -> Option<String> {
    // Disable bracketed paste mode so pasted text arrives as plain bytes.
    out.put(&format!("\r\n  {label}: \x1b[?2004l"));

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
            13 | 10 => break, // Enter (CR/LF)
            3 | 4 => {
                key.clear();
                break;
            } // Ctrl+C / Ctrl+D → cancel
            127 | 8 => {
                // DEL / Backspace
                if !key.is_empty() {
                    key.pop();
                    out.put("\x08 \x08");
                }
            }
            b if b >= 32 && b < 127 => {
                // printable ASCII
                key.push(b as char);
                out.put("*");
            }
            _ => {}
        }
    }

    // Re-enable bracketed paste mode.
    out.put("\x1b[?2004h\r\n");

    if key.is_empty() { None } else { Some(key) }
}

pub(crate) async fn handle_model(
    registry: &Arc<std::sync::Mutex<ProviderRegistry>>,
    provider_id: &mut Option<String>,
    model_id: &mut Option<String>,
    name: Option<&str>,
) -> Result<Vec<String>> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // If name is "provider/model", validate first then switch atomically.
    if let Some(combined) = name {
        let registered_ids: Vec<String> = registry
            .lock()
            .unwrap()
            .list()
            .iter()
            .map(|p| p.id().to_string())
            .collect();
        if let Some(slash_pos) = combined.find('/') {
            let pname = &combined[..slash_pos];
            if registered_ids.iter().any(|id| id == pname) {
                let mname = &combined[slash_pos + 1..];
                let provider = registry.lock().unwrap().get(pname).ok();
                match provider {
                    None => {
                        out.put(&format!(
                            "\r\n\x1b[31mProvider '{pname}' not found.\x1b[0m\r\n"
                        ));
                        return Ok(Vec::new());
                    }
                    Some(p) => {
                        let models = p.list_models().await.unwrap_or_default();
                        if let Some(m) = models.iter().find(|m| m.id == mname) {
                            // Validate succeeded — update both atomically.
                            *provider_id = Some(pname.to_string());
                            *model_id = Some(mname.to_string());
                            let rate_str = format_rate_label(m);
                            out.put(&format!(
                                "\r\n\x1b[32mSwitched to {pname} · {mname}{rate_str}.\x1b[0m\r\n"
                            ));
                        } else {
                            let available: Vec<&str> =
                                models.iter().map(|m| m.id.as_str()).collect();
                            out.put(&format!(
                                "\r\n\x1b[31mModel '{mname}' not found in '{pname}'. Available: {}\x1b[0m\r\n",
                                available.join(", ")
                            ));
                        }
                        return Ok(Vec::new());
                    }
                }
            }
        }
    }

    // Plain model name or no name: operate on current provider.
    let pid = match provider_id.as_ref() {
        Some(id) => id.clone(),
        None => {
            out.put("\r\n\x1b[31mNo provider selected. Use /connect first.\x1b[0m\r\n");
            return Ok(Vec::new());
        }
    };

    let provider = {
        let reg = registry.lock().unwrap();
        match reg.get(&pid) {
            Ok(p) => p,
            Err(_) => {
                drop(reg);
                out.put(&format!("\r\n\x1b[31mProvider '{pid}' not found.\x1b[0m\r\n"));
                return Ok(Vec::new());
            }
        }
    };

    let models = provider.list_models().await.unwrap_or_default();
    let model_ids: Vec<String> = models.iter().map(|m| m.id.clone()).collect();

    match name {
        Some(model_name) => {
            if let Some(m) = models.iter().find(|m| m.id == model_name) {
                *model_id = Some(model_name.to_string());
                let rate_str = format_rate_label(m);
                out.put(&format!(
                    "\r\n\x1b[32mModel set to {model_name}{rate_str}.\x1b[0m\r\n"
                ));
            } else {
                let available: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
                out.put(&format!(
                    "\r\n\x1b[31mModel '{model_name}' not found. Available: {}\x1b[0m\r\n",
                    available.join(", ")
                ));
            }
        }
        None => {
            // List all providers' models grouped.
            let all_providers = registry.lock().unwrap().list();
            drop(out);
            let mut any = false;
            for p in all_providers {
                let pmodels = p.list_models().await.unwrap_or_default();
                if pmodels.is_empty() {
                    continue;
                }
                any = true;
                let mut out = std::io::stdout().lock();
                out.put(&format!("\r\n\x1b[1m{}:\x1b[0m\r\n", p.id()));
                for m in &pmodels {
                    let active = provider_id.as_deref() == Some(p.id())
                        && model_id.as_deref() == Some(&m.id);
                    let marker = if active { " *" } else { "  " };
                    let ctx = if m.context_tokens > 0 {
                        format!(" ({}k ctx)", m.context_tokens / 1000)
                    } else {
                        String::new()
                    };
                    let rate_str = format_rate_label(m);
                    let color = if active { "\x1b[32m" } else { "\x1b[2m" };
                    out.put(&format!(
                        "{color}{marker}\x1b[0m{}/{} - {}{rate_str}{ctx}\r\n",
                        p.id(),
                        m.id,
                        m.display_name
                    ));
                }
            }
            if !any {
                std::io::stdout().lock().put("\r\nNo models available. Use /connect first.\r\n");
            }
            return Ok(model_ids);
        }
    }
    Ok(model_ids)
}

/// Fetch `(provider/model, rate_label)` completion items from all registered providers
/// concurrently. The rate label is `Some("2x")` etc. when known, `None` otherwise.
pub(crate) async fn fetch_all_model_completions(
    registry: &Arc<std::sync::Mutex<ProviderRegistry>>,
) -> Vec<(String, Option<String>)> {
    let providers = registry.lock().unwrap().list();
    let futures: Vec<_> = providers
        .iter()
        .map(|p| {
            let p = p.clone();
            async move {
                let pid = p.id().to_string();
                let models = p.list_models().await.unwrap_or_default();
                models
                    .into_iter()
                    .map(move |m| {
                        let value = format!("{pid}/{}", m.id);
                        let rate = m.rate_label.clone();
                        (value, rate)
                    })
                    .collect::<Vec<_>>()
            }
        })
        .collect();
    futures::future::join_all(futures)
        .await
        .into_iter()
        .flatten()
        .collect()
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
