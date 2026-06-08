use clap::{Parser, Subcommand, ValueEnum};
use dkod_cli::cmd;

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

/// CLI-side mirror of `setup::state::Scope`. Kept separate so clap's
/// `ValueEnum` derive doesn't leak into the state module.
#[derive(Copy, Clone, Debug, ValueEnum)]
enum ScopeArg {
    User,
    PerRepo,
}

impl From<ScopeArg> for cmd::setup::state::Scope {
    fn from(s: ScopeArg) -> Self {
        match s {
            ScopeArg::User => cmd::setup::state::Scope::User,
            ScopeArg::PerRepo => cmd::setup::state::Scope::PerRepo,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Initialize dkod in the current repo
    Init,
    /// Capture a session by wrapping an agent invocation
    Capture {
        /// Agent name (e.g. "codex", "claude-code", "copilot-cli", "gemini-cli", "factory-ai")
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
    /// Show, per line of a file, the AI agent session that produced it.
    Blame {
        /// Path to the file to annotate (relative to the repo).
        path: String,
    },
    /// Run the seamless capture wizard: detect installed AI agents and
    /// wire their hook config / PATH shim so dkod captures every session
    /// automatically. Re-run any time to refresh hooks; idempotent.
    Setup {
        /// Wizard scope: "user" writes to `~/.<agent>/...`; "per-repo"
        /// writes to `<repo>/.<agent>/...`. Defaults to "user".
        #[arg(long, value_enum, default_value_t = ScopeArg::User)]
        scope: ScopeArg,
        /// Skip every consent prompt (record `consent = skipped-noninteractive`
        /// for shim agents). Implied when stdin isn't a TTY.
        #[arg(long)]
        non_interactive: bool,
        /// Repo root for `--scope per-repo`. Ignored under user scope;
        /// defaults to the current working directory.
        #[arg(long)]
        repo_root: Option<std::path::PathBuf>,
    },
    /// Internal: invoked by agent hooks. Not for direct use.
    ///
    /// Two forms are accepted:
    ///
    /// * Legacy (Claude Code per-repo install written by `dkod init`):
    ///   `dkod capture-hook <repo_hash> <event_name>`
    /// * Wizard (seamless user-scope install):
    ///   `dkod capture-hook --agent <name> --event <event>`
    ///
    /// Both are supported because in-flight installs at upgrade time may
    /// have written either format.
    #[command(hide = true)]
    CaptureHook {
        /// Agent name (e.g. "claude-code", "codex"). Use with `--event`.
        #[arg(long)]
        agent: Option<String>,
        /// Hook event name (e.g. "SessionStart"). Use with `--agent`.
        #[arg(long)]
        event: Option<String>,
        /// Legacy positional repo hash + event name. Mutually exclusive
        /// with `--agent`/`--event`; presence is detected at runtime.
        legacy_args: Vec<String>,
    },
    /// Internal: invoked by the git post-rewrite hook to re-link sessions
    /// after a history rewrite. Not for direct use.
    #[command(hide = true)]
    Relink,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // Self-heal runs on every dispatch except `Setup` itself
    // (chicken-and-egg) and `CaptureHook` (must stay on its own fast
    // path — the hook is hot-spot code that fires once per tool use,
    // and even a single TOML parse on top adds avoidable latency).
    if !matches!(
        cli.cmd,
        Cmd::Setup { .. } | Cmd::CaptureHook { .. } | Cmd::Relink
    ) {
        maybe_warn_drift();
    }
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
            "cursor" | "cursor-agent" => {
                cmd::capture::cursor::run(&std::env::current_dir()?, args)
            }
            "factory-ai" | "factory" | "droid" => {
                cmd::capture::factory_ai::run(&std::env::current_dir()?, args)
            }
            "gemini-cli" | "gemini" => {
                cmd::capture::gemini_cli::run(&std::env::current_dir()?, args)
            }
            "opencode" => cmd::capture::opencode::run(&std::env::current_dir()?, args),
            other => Err(anyhow::anyhow!(
                "unknown agent: {other} (supported: codex, claude-code, copilot-cli/copilot, cursor/cursor-agent, factory-ai/factory/droid, gemini-cli/gemini, opencode)"
            )),
        },
        Cmd::Log => cmd::log::run(&std::env::current_dir()?),
        Cmd::Show { id } => cmd::show::run(&std::env::current_dir()?, &id),
        Cmd::Blame { path } => cmd::blame::run(&std::env::current_dir()?, &path),
        Cmd::Setup {
            scope,
            non_interactive,
            repo_root,
        } => cmd::setup::orchestrator::run_cli(
            scope.into(),
            non_interactive,
            repo_root.as_deref(),
        ),
        Cmd::CaptureHook {
            agent,
            event,
            legacy_args,
        } => match (agent, event, legacy_args.as_slice()) {
            (Some(agent), Some(event), []) => cmd::capture::hook::route_and_buffer(&agent, &event),
            (None, None, [repo_hash, event_name]) => {
                cmd::capture::claude_code::hook_command(repo_hash, event_name)
            }
            // Misuse: log + exit 0 so the hook never breaks the agent.
            _ => Ok(()),
        },
        Cmd::Relink => cmd::relink::run(&std::env::current_dir()?),
    }
}

/// Best-effort drift warning. Stats a fixed list of well-known agent
/// config paths against `~/.dkod/config.toml` and prints a one-line
/// notice on stderr if any agent appeared or disappeared. Cheap by
/// construction (~20 `Path::exists` calls + one small TOML parse on a
/// cold start, less on a warm cache), but no formal latency guarantee —
/// any error is swallowed so a `dkod log` is never blocked by unreadable
/// state.
fn maybe_warn_drift() {
    use cmd::setup::{
        selfheal::{ensure_setup_current, SelfHealResult},
        state::DkodConfig,
    };
    let config_path = match DkodConfig::default_path() {
        Ok(p) => p,
        Err(_) => return,
    };
    let cfg = match DkodConfig::load_or_default(&config_path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };
    if let SelfHealResult::DriftDetected(agents) = ensure_setup_current(&cfg, &home) {
        eprintln!(
            "dkod: agent state has drifted ({}) — run `dkod setup` to refresh hooks",
            agents.join(", ")
        );
    }
}
