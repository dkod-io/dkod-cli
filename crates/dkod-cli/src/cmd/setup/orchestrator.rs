//! `dkod setup` orchestrator: walks every supported agent and invokes its
//! installer.
//!
//! The orchestrator's job is the boring glue: load `~/.dkod/config.toml`,
//! ensure the vault exists, build the right [`Prompter`] for the context,
//! iterate over each [`AgentInstaller`], record the outcome in
//! `DkodConfig::agents`, and atomically write the result back.
//!
//! All per-agent specifics (where to write, what consent question to ask,
//! whether a shim is needed) live in `agents/*.rs` — this module never
//! special-cases an agent by name.

use crate::cmd::setup::agents::{
    claude_code::ClaudeCode, codex::Codex, copilot_cli::CopilotCli, cursor::Cursor,
    factory_ai::FactoryAi, gemini_cli::GeminiCli, opencode::OpenCode, AgentInstaller,
    InstallContext, InstallOutcome,
};
use crate::cmd::setup::consent::{is_tty, NonInteractivePrompter, Prompter, StdinPrompter};
use crate::cmd::setup::state::{AgentState, DkodConfig, Scope};
use crate::cmd::setup::vault;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Per-agent outcome surfaced to callers (the CLI wraps this in a printable
/// summary). Kept structured so the test suite can assert on the shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentResult {
    pub name: &'static str,
    pub outcome: InstallOutcome,
}

/// Aggregate result of one wizard run. The CLI prints this as a per-agent
/// table; the orchestrator returns it instead of printing so tests can
/// drive it without capturing stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WizardSummary {
    pub agents: Vec<AgentResult>,
    /// Path to the on-disk config that was loaded / written.
    pub config_path: PathBuf,
    /// Path to the vault that was initialised (or confirmed). Always under
    /// `home/.dkod/vault` unless overridden via `Options::vault_path`.
    pub vault_path: PathBuf,
}

/// Inputs to `run`. All-defaultable so callers in the live CLI usually pass
/// `Options::default()`; tests override `home`, `prompter`, etc.
pub struct Options<'a> {
    /// User home dir. Tests pass a `TempDir`.
    pub home: PathBuf,
    /// Wizard scope: user-wide vs per-repo. Per-repo requires `repo_root`.
    pub scope: Scope,
    /// Optional repo root for `Scope::PerRepo`. Ignored under `Scope::User`.
    pub repo_root: Option<PathBuf>,
    /// `true` for `install.sh | sh` / CI contexts. Consent-required agents
    /// short-circuit to `SkippedNoninteractive` here instead of blocking on
    /// stdin.
    pub non_interactive: bool,
    /// Override the default `~/.dkod/config.toml` path. Tests use this to
    /// keep state out of `$HOME`.
    pub config_path: Option<PathBuf>,
    /// Override the default `<home>/.dkod/vault` path. Tests use this to
    /// keep the vault out of `$HOME` too.
    pub vault_path: Option<PathBuf>,
    /// Custom prompter for tests. `None` → `StdinPrompter` (interactive) or
    /// `NonInteractivePrompter("n")` (when `non_interactive` is true).
    pub prompter: Option<&'a mut dyn Prompter>,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Self {
            home: dirs::home_dir()
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from(".")),
            scope: Scope::User,
            repo_root: None,
            non_interactive: !is_tty(),
            config_path: None,
            vault_path: None,
            prompter: None,
        }
    }
}

/// The complete fleet of agent installers. New agents go here once their
/// `agents/<name>.rs` module is in place; the orchestrator picks them up
/// automatically without further wiring.
fn all_installers() -> Vec<Box<dyn AgentInstaller>> {
    vec![
        Box::new(ClaudeCode),
        Box::new(Codex),
        Box::new(CopilotCli),
        Box::new(Cursor),
        Box::new(FactoryAi),
        Box::new(GeminiCli),
        Box::new(OpenCode),
    ]
}

/// Drive one wizard run end-to-end. Returns the per-agent summary; the CLI
/// formats it as a table.
pub fn run(opts: Options) -> Result<WizardSummary> {
    let config_path = opts
        .config_path
        .clone()
        .unwrap_or(DkodConfig::default_path()?);
    let vault_path = opts
        .vault_path
        .clone()
        .unwrap_or_else(|| opts.home.join(".dkod").join("vault"));

    vault::ensure(&vault_path)
        .with_context(|| format!("ensure vault at {}", vault_path.display()))?;

    let mut cfg = DkodConfig::load_or_default(&config_path)
        .with_context(|| format!("load {}", config_path.display()))?;
    cfg.scope.default = opts.scope;
    cfg.vault.path = vault_path.clone();

    // Build the prompter once and pass `&mut dyn Prompter` to every
    // installer. Tests can inject a scripted prompter via opts.prompter.
    let mut fallback_stdin = StdinPrompter;
    let mut fallback_noninteractive = NonInteractivePrompter::new("n");
    let prompter: &mut dyn Prompter = match opts.prompter {
        Some(p) => p,
        None if opts.non_interactive => &mut fallback_noninteractive,
        None => &mut fallback_stdin,
    };

    let installers = all_installers();
    let mut results: Vec<AgentResult> = Vec::with_capacity(installers.len());
    let mut new_agents: BTreeMap<String, AgentState> = std::mem::take(&mut cfg.agents);

    for installer in &installers {
        let name = installer.name();
        // Take the existing state (preserving prior `consent = Never`, etc.)
        // or seed a fresh one. We re-insert it after install regardless of
        // outcome so we don't lose prior history on partial runs.
        let mut state = new_agents.remove(name).unwrap_or_else(blank_agent_state);

        let outcome = if installer.detect(&opts.home).is_none() {
            InstallOutcome::AgentNotPresent
        } else {
            let mut ctx = InstallContext {
                home: &opts.home,
                scope: opts.scope,
                repo_root: opts.repo_root.as_deref(),
                prompter,
                non_interactive: opts.non_interactive,
            };
            installer
                .install(&mut ctx, &mut state)
                .with_context(|| format!("install agent {name}"))?
        };

        new_agents.insert(name.to_string(), state);
        results.push(AgentResult { name, outcome });
    }

    cfg.agents = new_agents;
    cfg.save(&config_path)
        .with_context(|| format!("save {}", config_path.display()))?;

    Ok(WizardSummary {
        agents: results,
        config_path,
        vault_path,
    })
}

/// Convenience for callers that just want a one-line summary string per
/// agent. Used by the live CLI; tests assert on `WizardSummary` directly.
pub fn format_summary(summary: &WizardSummary) -> String {
    let mut out = String::new();
    out.push_str(&format!("config: {}\n", summary.config_path.display()));
    out.push_str(&format!("vault:  {}\n", summary.vault_path.display()));
    out.push_str("agents:\n");
    for r in &summary.agents {
        out.push_str(&format!("  {:<12} {:?}\n", r.name, r.outcome));
    }
    out
}

fn blank_agent_state() -> AgentState {
    AgentState {
        installed: false,
        scope: None,
        hook_path: None,
        installed_version: None,
        fingerprint: None,
        last_check: None,
        consent: None,
        detected_at: None,
        extra: Default::default(),
    }
}

/// Entry point for `dkod setup` from `main.rs`. Builds default `Options`,
/// runs the orchestrator, prints the summary. The CLI argv is parsed
/// upstream; this fn only takes the resolved flags.
pub fn run_cli(scope: Scope, non_interactive_flag: bool, repo_root: Option<&Path>) -> Result<()> {
    // The CLI flag overrides the TTY heuristic — users explicitly passing
    // `--non-interactive` want it even on a TTY.
    let non_interactive = non_interactive_flag || !is_tty();
    let opts = Options {
        home: dirs::home_dir()
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .context("HOME unset and dirs::home_dir() returned None")?,
        scope,
        repo_root: repo_root.map(|p| p.to_path_buf()),
        non_interactive,
        ..Default::default()
    };
    let summary = run(opts)?;
    print!("{}", format_summary(&summary));
    Ok(())
}
