# chatsh

[![Crates.io](https://img.shields.io/crates/v/chatsh.svg)](https://crates.io/crates/chatsh)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A PTY-level, shell-agnostic AI chat tool that lives inline in any terminal. Invisible when idle; type `/chat <question>` to stream an answer with your recent terminal output as context.

```
❯ /chat how do I find open ports?
```

## Install

From crates.io:

```bash
cargo install chatsh
```

From source:

```bash
git clone https://github.com/taquangtrung/chatsh
cd chatsh
cargo install --path .
```

Run it (or `exec` from your rc file):

```bash
exec chatsh $SHELL
```

## Commands

| Input | Action |
|---|---|
| `/chat <question>` | Stream an AI answer using recent terminal output as context |
| `/connect` | Show the currently connected provider |
| `/connect <provider>` | Switch provider: `anthropic`, `copilot`, `openai`, or `zai` |
| `/model` | List available models for the current provider |
| `/model <name>` | Select a model on the current provider |
| `/exit` | Quit chatsh |
| `/` then Tab / arrows | Cycle command suggestions |
| `/<path><Tab>` | Falls through to shell path completion (e.g. `/bin/<Tab>`) |
| Anything else | Passed through to your shell untouched |

Examples:

```
❯ /connect anthropic
❯ /model claude-sonnet-4-20250514
❯ /chat summarize the last error
```

## Configuration

### API keys

Set one of:

| Variable | Backend |
|---|---|
| `ANTHROPIC_API_KEY` | Anthropic (Claude) |
| `GITHUB_COPILOT_TOKEN` | GitHub Copilot |
| `ZAI_API_KEY` | Z.ai |
| `OPENAI_API_KEY` | OpenAI |

Override via CLI: `chatsh --provider copilot --model gpt-4o`.

### `~/.config/chatsh/config.toml` (optional)

```toml
[ai]
provider = "auto"   # auto | anthropic | copilot | zai | openai
model = "claude-sonnet-4-20250514"

[context]
buffer_lines = 200
```

### Session marker in your prompt

`chatsh` sets `CHATSH_SESSION=1` on the wrapped shell. Hook it for a visual "you're in chatsh" cue. Example for Starship — adds a cyan `✦` before the prompt character:

```toml
[custom.chatsh_badge]
when = '[ -n "$CHATSH_SESSION" ]'
format = "[✦]($style) "
style = "bold cyan"
```

Place `${custom.chatsh_badge}` right before `$character` in your `format`.

For plain shells:

```sh
# zsh
[[ -n "$CHATSH_SESSION" ]] && PROMPT="%F{8}(chatsh)%f $PROMPT"

# bash
[ -n "$CHATSH_SESSION" ] && PS1='\[\e[2m\](chatsh)\[\e[0m\] '"$PS1"
```

## License

Licensed under the [MIT License](LICENSE).
