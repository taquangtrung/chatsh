use anyhow::Result;
use clap::Parser;
use crossterm::terminal;

use chatsh::VERSION;

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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let shell_args = chatsh::app::resolve_shell(&cli.args);

    let _guard = RawGuard::init()?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(chatsh::app::run(shell_args, cli.provider, cli.model))
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
        let cli = Cli::parse_from(["chatsh", "--provider", "github-copilot", "--model", "gpt-4o", "/bin/bash"]);
        assert_eq!(cli.provider.as_deref(), Some("github-copilot"));
        assert_eq!(cli.model.as_deref(), Some("gpt-4o"));
        assert_eq!(cli.args, vec!["/bin/bash"]);
    }
}
