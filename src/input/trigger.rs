const TRIGGER_PREFIX: &str = "/chat ";
const EXIT_COMMAND: &str = "/exit";
const NEW_COMMAND: &str = "/new";
const HINT_PREFIX: char = '/';
const HINT_TEXT: &str = concat!(
    "\x1b[1;36m/chat\x1b[0m   : Chat with AI\r\n",
    "\x1b[1;36m/new\x1b[0m    : Start a fresh conversation\r\n",
    "\x1b[1;36m/connect\x1b[0m: Connect to provider\r\n",
    "\x1b[1;36m/reauth\x1b[0m : Re-authenticate a provider\r\n",
    "\x1b[1;36m/model\x1b[0m  : List or select model\r\n",
    "\x1b[1;36m/exit\x1b[0m   : Quit chatsh",
);

pub struct Command {
    pub name: &'static str,
    pub takes_args: bool,
    pub requires_args: bool,
    /// Shown below the prompt when Enter is pressed without a required argument.
    pub arg_hint: &'static str,
}

const COMMANDS: &[Command] = &[
    Command {
        name: "/chat",
        takes_args: true,
        requires_args: true,
        arg_hint: "Please type your message",
    },
    Command {
        name: "/connect",
        takes_args: true,
        requires_args: false,
        arg_hint: "<provider>",
    },
    Command {
        name: "/reauth",
        takes_args: true,
        requires_args: true,
        arg_hint: "Please type the provider name",
    },
    Command {
        name: "/model",
        takes_args: true,
        requires_args: false,
        arg_hint: "[provider/model]",
    },
    Command {
        name: "/new",
        takes_args: false,
        requires_args: false,
        arg_hint: "",
    },
    Command {
        name: "/exit",
        takes_args: false,
        requires_args: false,
        arg_hint: "",
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputAction {
    Connect { provider: String },
    Exit,
    Model { name: Option<String> },
    NewChat,
    Passthrough,
    Reauth { provider: String },
    ShowHint,
    TriggerChat { query: String },
}

pub fn complete(prefix: &str) -> Vec<&'static Command> {
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(prefix))
        .collect()
}

const PROVIDERS: &[&str] = &["anthropic", "github-copilot", "openai", "z.ai-coding-plan"];

pub fn complete_providers(prefix: &str) -> Vec<&'static str> {
    PROVIDERS
        .iter()
        .filter(|p| p.starts_with(prefix))
        .copied()
        .collect()
}

/// Find the command whose name exactly prefixes `line`.
/// Matches when `line` equals the command name or starts with `"<name> "`.
pub fn command_for_line(line: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| {
        line == c.name
            || (line.len() > c.name.len()
                && line.starts_with(c.name)
                && line.as_bytes()[c.name.len()] == b' ')
    })
}

pub fn classify_input(line: &str) -> InputAction {
    let trimmed = line.trim();
    if let Some(query) = line.strip_prefix(TRIGGER_PREFIX) {
        InputAction::TriggerChat {
            query: query.trim().to_string(),
        }
    } else if trimmed == "/connect" || trimmed.starts_with("/connect ") {
        let provider = trimmed
            .strip_prefix("/connect ")
            .map(|rest| rest.trim().to_string())
            .unwrap_or_default();
        InputAction::Connect { provider }
    } else if trimmed.starts_with("/reauth ") {
        let provider = trimmed
            .strip_prefix("/reauth ")
            .map(|rest| rest.trim().to_string())
            .unwrap_or_default();
        InputAction::Reauth { provider }
    } else if trimmed == "/model" || trimmed.starts_with("/model ") {
        let name = trimmed
            .strip_prefix("/model ")
            .map(str::trim)
            .filter(|rest| !rest.is_empty())
            .map(str::to_string);
        InputAction::Model { name }
    } else if trimmed == EXIT_COMMAND {
        InputAction::Exit
    } else if trimmed == NEW_COMMAND {
        InputAction::NewChat
    } else if line == HINT_PREFIX.to_string() {
        InputAction::ShowHint
    } else {
        InputAction::Passthrough
    }
}

pub fn hint_text() -> &'static str {
    HINT_TEXT
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_passthrough_normal_text() {
        assert_eq!(classify_input("ls -la"), InputAction::Passthrough);
    }

    #[test]
    fn test_show_hint_on_bare_slash() {
        assert_eq!(classify_input("/"), InputAction::ShowHint);
    }

    #[test]
    fn test_trigger_chat_with_query() {
        let result = classify_input("/chat how do I find open ports?");
        assert_eq!(
            result,
            InputAction::TriggerChat {
                query: "how do I find open ports?".to_string()
            }
        );
    }

    #[test]
    fn test_passthrough_partial_match() {
        assert_eq!(classify_input("/chatting"), InputAction::Passthrough);
        assert_eq!(classify_input("/chat"), InputAction::Passthrough);
    }

    #[test]
    fn test_hint_text_contains_chat() {
        assert!(hint_text().contains("/chat"));
    }

    #[test]
    fn test_reauth_with_provider() {
        assert_eq!(
            classify_input("/reauth copilot"),
            InputAction::Reauth {
                provider: "copilot".to_string()
            }
        );
    }

    #[test]
    fn test_exit_command() {
        assert_eq!(classify_input("/exit"), InputAction::Exit);
    }

    #[test]
    fn test_connect_with_provider() {
        assert_eq!(
            classify_input("/connect github-copilot"),
            InputAction::Connect {
                provider: "github-copilot".to_string()
            }
        );
    }

    #[test]
    fn test_model_no_args() {
        assert_eq!(classify_input("/model"), InputAction::Model { name: None });
    }

    #[test]
    fn test_model_with_name() {
        assert_eq!(
            classify_input("/model glm-4.6"),
            InputAction::Model {
                name: Some("glm-4.6".to_string())
            }
        );
    }

    #[test]
    fn test_complete_slash_lists_all() {
        let m = complete("/");
        assert_eq!(m.len(), 6);
    }

    #[test]
    fn test_complete_partial_one_match() {
        let m = complete("/c");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name, "/chat");
        assert!(m[0].takes_args);
        assert!(m[0].requires_args);
    }

    #[test]
    fn test_complete_connect() {
        let m = complete("/connect");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "/connect");
        assert!(m[0].takes_args);
        assert!(!m[0].requires_args);
    }

    #[test]
    fn test_complete_no_match() {
        assert!(complete("/zzz").is_empty());
    }

    #[test]
    fn test_complete_providers_empty_prefix() {
        let m = complete_providers("");
        assert_eq!(
            m,
            vec!["anthropic", "github-copilot", "openai", "z.ai-coding-plan"]
        );
    }

    #[test]
    fn test_complete_providers_no_match() {
        assert!(complete_providers("xyz").is_empty());
    }

    #[test]
    fn test_command_for_line_exact_name() {
        let cmd = command_for_line("/chat").unwrap();
        assert_eq!(cmd.name, "/chat");
    }

    #[test]
    fn test_command_for_line_with_trailing_space() {
        let cmd = command_for_line("/chat ").unwrap();
        assert_eq!(cmd.name, "/chat");
    }

    #[test]
    fn test_command_for_line_with_args() {
        let cmd = command_for_line("/reauth github-copilot").unwrap();
        assert_eq!(cmd.name, "/reauth");
    }

    #[test]
    fn test_command_for_line_partial_name_no_match() {
        assert!(command_for_line("/ch").is_none());
        assert!(command_for_line("/cha").is_none());
    }

    #[test]
    fn test_command_for_line_no_match() {
        assert!(command_for_line("/unknown").is_none());
        assert!(command_for_line("hello").is_none());
    }
}
