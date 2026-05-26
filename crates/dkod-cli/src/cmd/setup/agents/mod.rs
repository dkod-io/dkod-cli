//! Per-agent install/detect routines for the seamless capture wizard.
//!
//! Each submodule owns one agent's:
//! * `detect()` — returns `Option<DetectedAgent>` based on well-known config
//!   paths and version probes.
//! * `install(...)` — writes the agent's native hook config (silent path) or
//!   a PATH shim (explicit-consent path). Records the result in the wizard's
//!   `AgentState`.
//!
//! The Codex submodule exercises the explicit-consent shim flow; every other
//! submodule writes a native hook entry without prompting.
//!
//! Per-agent strategy is documented in
//! `docs/plans/2026-05-25-seamless-capture-wizard-design.md` (Wave 0 spike
//! confirmed six of seven agents support native hooks; only Codex needs a
//! PATH shim).

pub mod claude_code;
pub mod codex;
pub mod copilot_cli;
pub mod cursor;
pub mod factory_ai;
pub mod gemini_cli;
pub mod opencode;

use crate::cmd::setup::consent::Prompter;
use crate::cmd::setup::state::{AgentState, Scope};
use anyhow::Result;
use std::path::Path;

/// Outcome of a single-agent install attempt. Reported back to the wizard
/// orchestrator so the user-facing summary can distinguish "did it" from
/// "skipped because the user said no" from "skipped because the agent
/// isn't installed."
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// Hook (or shim) written. `AgentState` populated with hook_path /
    /// fingerprint / installed_version.
    Installed,
    /// Agent isn't present on this machine; nothing to install.
    AgentNotPresent,
    /// User explicitly declined consent (shim agents only). Recorded as
    /// `consent = "no"` (one-time) or `"never"` (permanent) by the caller.
    DeclinedByUser,
    /// Non-interactive context (CI / pipe) and consent is required for
    /// this agent. Recorded as `consent = "skipped-noninteractive"`.
    SkippedNoninteractive,
    /// Agent's install path is recognized but the implementation hasn't
    /// landed yet (Wave 2 follow-up). Wizard prints a clear notice and
    /// continues — the agent will pick up in a later release.
    NotYetImplemented,
}

/// What `detect()` returns when an agent is present. Used by the wizard to
/// decide whether to call `install()` and what to record in `AgentState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedAgent {
    /// User-facing agent name, matches the keys in `state::DkodConfig::agents`.
    pub name: &'static str,
    /// Path to the config file (or directory) that proved the agent is
    /// installed. Stored in `AgentState::hook_path` after install.
    pub config_path: std::path::PathBuf,
    /// Best-effort version string from the agent's config or CLI. `None`
    /// if we couldn't determine one cheaply.
    pub version: Option<String>,
}

/// Inputs an installer needs that aren't agent-specific.
pub struct InstallContext<'a> {
    /// User home directory. Tests point this at a `TempDir`.
    pub home: &'a Path,
    /// Whether the wizard was launched with `--scope user` or `--scope per-repo`.
    pub scope: Scope,
    /// `None` in user-scope mode; `Some(repo_root)` in per-repo mode.
    pub repo_root: Option<&'a Path>,
    /// Source of consent answers for shim agents.
    pub prompter: &'a mut dyn Prompter,
    /// Whether we're running in a non-interactive context (`install.sh` piped
    /// from curl, CI, etc.). Shim agents short-circuit to
    /// `SkippedNoninteractive` when this is true.
    pub non_interactive: bool,
}

/// Trait every per-agent installer implements. Kept narrow so each module
/// stays focused on its agent's quirks and the orchestrator can iterate
/// over a `Vec<&dyn AgentInstaller>` without sprouting branches.
pub trait AgentInstaller {
    /// Name used in `state::DkodConfig::agents` and in the wizard's
    /// user-facing summary. Must match what `selfheal::well_known_agent_paths`
    /// keys on.
    fn name(&self) -> &'static str;

    /// Cheap presence check: stat well-known paths. Pure with respect to
    /// `home` so tests can route it at a temp dir.
    fn detect(&self, home: &Path) -> Option<DetectedAgent>;

    /// Perform the install. Updates `state` in place on success. Returns
    /// the outcome so the orchestrator can summarize per-agent results.
    fn install(&self, ctx: &mut InstallContext, state: &mut AgentState) -> Result<InstallOutcome>;
}
