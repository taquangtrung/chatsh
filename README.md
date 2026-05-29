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
| `/connect` | List connected providers and the active one |
| `/connect <provider>` | Connect or switch provider |
| `/reauth <provider>` | Re-authenticate a provider (refresh token or re-enter API key) |
| `/model` | List available models for the current provider |
| `/model <name>` | Select a model |
| `/new` | Start a fresh conversation (clears history) |
| `/exit` | Quit chatsh |
| `/` then Tab / ↑↓ | Cycle command completions |
| `/<path><Tab>` | Falls through to shell path completion (e.g. `/bin/<Tab>`) |
| Anything else | Passed through to your shell untouched |

Available providers: `github-copilot`, `z.ai-coding-plan`, `anthropic`, `openai`.

### Tab completion

Pressing Tab on a command with arguments shows completions inline:

- `/connect <Tab>` — cycles through providers
- `/reauth <Tab>` — cycles through providers
- `/model <Tab>` — fetches and cycles through models from the active provider in real time

If you press Enter on a command that requires an argument (e.g. `/chat` or `/reauth`) without typing one, chatsh reminds you with a dim hint below the prompt instead of executing.

## Examples

### GitHub Copilot (OAuth device flow)

```
❯ /connect github-copilot

  Open https://github.com/login/device in your browser
  and enter code: ABCD-1234

  Waiting...
  GitHub Copilot connected!

❯ /model <Tab>          # fetches models live
❯ /model claude-sonnet-4
  Model set to claude-sonnet-4 [1x].

❯ /chat why is my build failing?
```

### Z.ai Coding Plan (API key)

```
❯ export ZAI_API_KEY=<your-key>

  (chatsh reads the key automatically on startup)

❯ /connect z.ai-coding-plan
  Z.ai Coding Plan API key (leave empty to cancel): ****
  Verifying API key...
  Connected to Z.ai Coding Plan.

❯ /model
  Models (z.ai-coding-plan):
    glm-5.1 - GLM-5.1 [?] (128k ctx)
  * glm-4.6 - GLM-4.6 [?] (128k ctx)
    glm-4.5 - GLM-4.5 [?] (128k ctx)

❯ /chat explain this error
```

## Configuration

### API keys

Set one of the following environment variables before starting chatsh:

| Variable | Provider |
|---|---|
| `GITHUB_COPILOT_TOKEN` | GitHub Copilot (alternatively use `/connect github-copilot` for the device flow) |
| `ZAI_API_KEY` | Z.ai Coding Plan |
| `ANTHROPIC_API_KEY` | Anthropic (Claude) |
| `OPENAI_API_KEY` | OpenAI |

Override provider and model at launch:

```bash
chatsh --provider github-copilot --model gpt-4o
```

### `~/.config/chatsh/config.toml` (optional)

```toml
[ai]
provider = "auto"   # auto | github-copilot | z.ai-coding-plan | anthropic | openai
model = "claude-sonnet-4"

[context]
buffer_lines = 200
```

### Session marker in your prompt

`chatsh` sets `CHATSH_SESSION=1` on the wrapped shell. Hook it for a visual "you're in chatsh" cue. Example for Starship — adds the ✨ symbol before the prompt character:

```toml
[custom.chatsh_badge]
when = '[ -n "$CHATSH_SESSION" ]'
format = "[✨]($style) "
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
