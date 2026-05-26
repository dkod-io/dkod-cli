//! Cheap drift check that runs at the top of every `dkod` subcommand.
//!
//! The wizard's "self-healing on every dkod call" promise lives here: stat
//! a fixed list of well-known agent config paths, compare against the set
//! recorded in `~/.dkod/config.toml`, and report drift. No parsing, no
//! shell-outs, no network — sub-millisecond on a warm cache so it's safe
//! to call on the hot path of every CLI invocation.

use crate::cmd::setup::state::DkodConfig;
use std::path::{Path, PathBuf};

/// Outcome of a drift check.
#[derive(Debug, PartialEq, Eq)]
pub enum SelfHealResult {
    /// Wizard has never been run — config file empty or all-default.
    NeverInstalled,
    /// Installed agent set matches what's on disk; nothing to do.
    Clean,
    /// One or more agents have appeared or disappeared since the last
    /// wizard run. Names are the keys we use throughout the codebase
    /// (e.g. `"claude-code"`, `"codex"`).
    DriftDetected(Vec<String>),
}

/// Map of agent name → list of well-known absolute paths whose presence
/// implies that agent is installed for the current user. We only consider
/// an agent "present" if at least one path exists. The dollar-substitution
/// (`$HOME`) is resolved against `home`, keeping this function pure for
/// tests that point `home` at a TempDir.
fn well_known_agent_paths(home: &Path) -> Vec<(&'static str, Vec<PathBuf>)> {
    vec![
        (
            "claude-code",
            vec![
                home.join(".claude/settings.json"),
                home.join(".claude/settings.local.json"),
            ],
        ),
        (
            "codex",
            vec![home.join(".codex/config.toml"), home.join(".codex")],
        ),
        (
            "copilot-cli",
            vec![
                home.join(".copilot/hooks"),
                home.join(".copilot/settings.json"),
                home.join(".config/copilot"),
            ],
        ),
        (
            "cursor",
            vec![
                home.join(".cursor/hooks.json"),
                home.join(".cursor/settings.json"),
                home.join(".cursor"),
            ],
        ),
        (
            "factory-ai",
            vec![home.join(".factory/settings.json"), home.join(".factory")],
        ),
        (
            "gemini-cli",
            vec![home.join(".gemini/settings.json"), home.join(".gemini")],
        ),
        (
            "opencode",
            vec![
                home.join(".config/opencode/opencode.json"),
                home.join(".opencode"),
            ],
        ),
    ]
}

/// Compute the set of agents currently present on disk, by stat alone.
fn detect_present(home: &Path) -> Vec<&'static str> {
    well_known_agent_paths(home)
        .into_iter()
        .filter(|(_, paths)| paths.iter().any(|p| p.exists()))
        .map(|(name, _)| name)
        .collect()
}

/// Inspect `config` against the live filesystem under `home` and report
/// drift. Pure function with respect to its inputs — no I/O beyond
/// `Path::exists()` on a small fixed list of paths.
pub fn ensure_setup_current(config: &DkodConfig, home: &Path) -> SelfHealResult {
    let present = detect_present(home);
    let tracked: Vec<&str> = config
        .agents
        .iter()
        .filter(|(_, st)| st.installed)
        .map(|(name, _)| name.as_str())
        .collect();

    let mut drift: Vec<String> = Vec::new();

    // New agents present on disk but not tracked as installed yet.
    for name in &present {
        if !tracked.iter().any(|t| t == name) {
            drift.push((*name).to_string());
        }
    }
    // Agents we used to track but whose config paths have disappeared.
    for name in &tracked {
        if !present.iter().any(|p| p == name) {
            drift.push((*name).to_string());
        }
    }

    if drift.is_empty() {
        if config.agents.is_empty() {
            // Empty config + no drift means nothing is installed and
            // nothing is present — the wizard has never run here.
            SelfHealResult::NeverInstalled
        } else {
            SelfHealResult::Clean
        }
    } else {
        drift.sort();
        drift.dedup();
        SelfHealResult::DriftDetected(drift)
    }
}
