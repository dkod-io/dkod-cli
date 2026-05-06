use clap::{Parser, Subcommand};

mod cmd;

#[derive(Parser)]
#[command(
    name = "dkod",
    version,
    about = "Capture AI agent sessions into git refs"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Initialize dkod in the current repo
    Init,
    /// Capture a session by wrapping an agent invocation
    Capture {
        /// Agent name (e.g. "codex", "claude-code", "copilot-cli", "gemini-cli")
        agent: String,
        /// Args forwarded to the agent (after `--`)
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// List sessions in this repo
    Log,
    /// Show a session by id
    Show {
        /// Session id to display
        id: String,
    },
    /// Internal: invoked by Claude Code hooks. Not for direct use.
    #[command(hide = true)]
    CaptureHook {
        /// Repo hash that selects the per-repo socket.
        repo_hash: String,
        /// Hook event name (e.g. "SessionStart", "PreToolUse").
        event_name: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init => cmd::init::run(&std::env::current_dir()?),
        Cmd::Capture { agent, args } => match agent.as_str() {
            "codex" => cmd::capture::codex::run(&std::env::current_dir()?, args),
            "claude-code" => {
                cmd::capture::claude_code::run_server_command(&std::env::current_dir()?, args)
            }
            "copilot-cli" | "copilot" => {
                cmd::capture::copilot_cli::run(&std::env::current_dir()?, args)
            }
            "gemini-cli" | "gemini" => {
                cmd::capture::gemini_cli::run(&std::env::current_dir()?, args)
            }
            other => Err(anyhow::anyhow!(
                "unknown agent: {other} (supported: codex, claude-code, copilot-cli/copilot, gemini-cli/gemini)"
            )),
        },
        Cmd::Log => cmd::log::run(&std::env::current_dir()?),
        Cmd::Show { id } => cmd::show::run(&std::env::current_dir()?, &id),
        Cmd::CaptureHook {
            repo_hash,
            event_name,
        } => cmd::capture::claude_code::hook_command(&repo_hash, &event_name),
    }
}
