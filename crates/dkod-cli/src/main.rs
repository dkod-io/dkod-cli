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
        /// Agent name (e.g. "codex", "claude-code")
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init => cmd::init::run(&std::env::current_dir()?),
        Cmd::Capture { .. } => todo!("dkod capture: implemented in Task 15"),
        Cmd::Log => todo!("dkod log: implemented in Task 13"),
        Cmd::Show { .. } => todo!("dkod show: implemented in Task 13"),
    }
}
