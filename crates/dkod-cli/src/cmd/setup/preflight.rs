//! Wizard preflight: detect a stale `dkod` on PATH before writing hooks.
//!
//! The incident behind issue #24: install.sh put a new binary at
//! `$DKOD_PREFIX` while an old cargo-installed dkod still shadowed it on
//! PATH. The wizard wrote hooks invoking bare `dkod`, every hook resolved
//! the old binary, and its clap rejected `--agent` with exit 2 — bricking
//! every Claude Code session on the machine.
//!
//! Before writing any hooks, the wizard now:
//!
//! 1. Resolves `dkod` on PATH (which-equivalent).
//! 2. Probes it: `<resolved> capture-hook --agent probe --event Preflight`
//!    must exit 0 (the probe agent is handled side-effect-free by any
//!    0.2.1-or-newer binary; a stale binary fails clap parsing and exits
//!    2 — exactly the signal we want).
//! 3. Compares `<resolved> --version` against the running binary's
//!    `CARGO_PKG_VERSION`.
//!
//! Any anomaly produces a loud warning naming both paths and versions.
//! Hooks are still written — but only in the fail-open form (see
//! [`super::failopen`]), which is safe regardless of what PATH resolves.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Outcome of the preflight checks. `warning()` renders the user-facing
/// message; `None` means everything looks healthy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
    /// The `dkod` binary PATH resolves to, if any.
    pub resolved: Option<PathBuf>,
    /// `<resolved> --version` output, parsed down to the bare version
    /// (e.g. "0.1.1"). `None` if the binary is absent or --version failed.
    pub resolved_version: Option<String>,
    /// Whether the capture-hook probe exited 0. `None` if no binary found.
    pub probe_ok: Option<bool>,
    /// Version of the currently running wizard binary.
    pub running_version: String,
    /// Path of the currently running wizard binary, when resolvable.
    pub running_exe: Option<PathBuf>,
}

impl PreflightReport {
    /// Healthy means: a dkod was found on PATH, the probe exited 0, and
    /// the version matches the running binary.
    pub fn is_healthy(&self) -> bool {
        self.resolved.is_some()
            && self.probe_ok == Some(true)
            && self.resolved_version.as_deref() == Some(self.running_version.as_str())
    }

    /// User-facing warning, or `None` when healthy.
    pub fn warning(&self) -> Option<String> {
        if self.is_healthy() {
            return None;
        }
        let running_exe = self
            .running_exe
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        let Some(resolved) = &self.resolved else {
            return Some(format!(
                "WARNING: no `dkod` found on PATH. Hooks are written in fail-open form, so \
                 nothing will break — but no sessions will be captured until `dkod` is on \
                 PATH. Add the install directory of {running_exe} (version {}) to PATH.",
                self.running_version
            ));
        };
        let resolved = resolved.display();
        let resolved_version = self.resolved_version.as_deref().unwrap_or("<unknown>");
        let mut msg = format!(
            "WARNING: stale dkod binary at {resolved} (version {resolved_version}) would break \
             hooks — this wizard is {} at {running_exe}.",
            self.running_version
        );
        if self.probe_ok == Some(false) {
            msg.push_str(&format!(
                " Probe `{resolved} capture-hook --agent probe --event Preflight` exited \
                 non-zero — hook invocations through PATH would fail.",
            ));
        }
        msg.push_str(&format!(
            " Hooks are written in fail-open form (`command -v dkod ... || true`), so your \
             agent sessions stay safe, but capture will not work until you fix PATH: either \
             reinstall dkod {} over {resolved}, or remove the shadowing binary.",
            self.running_version
        ));
        Some(msg)
    }
}

/// Which-equivalent: scan `path_env` (a PATH-style list) for an executable
/// regular file named `dkod`.
pub fn resolve_dkod_on_path(path_env: &OsStr) -> Option<PathBuf> {
    for dir in std::env::split_paths(path_env) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join("dkod");
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Run the full preflight against `path_env`. `running_version` is the
/// wizard's own `CARGO_PKG_VERSION`. Never fails: every probe error is
/// folded into the report.
pub fn run_preflight(path_env: &OsStr, running_version: &str) -> PreflightReport {
    let resolved = resolve_dkod_on_path(path_env);
    let (resolved_version, probe_ok) = match &resolved {
        None => (None, None),
        Some(bin) => {
            let version = Command::new(bin)
                .arg("--version")
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| parse_version_output(&String::from_utf8_lossy(&o.stdout)));
            let probe = Command::new(bin)
                .args(["capture-hook", "--agent", "probe", "--event", "Preflight"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            (version, Some(probe))
        }
    };
    PreflightReport {
        resolved,
        resolved_version,
        probe_ok,
        running_version: running_version.to_string(),
        running_exe: std::env::current_exe().ok(),
    }
}

/// Extract the bare version from `dkod --version` output ("dkod 0.2.0\n"
/// → "0.2.0"). Tolerates a bare version with no binary name.
fn parse_version_output(out: &str) -> Option<String> {
    out.lines()
        .next()?
        .split_whitespace()
        .last()
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_version_line() {
        assert_eq!(
            parse_version_output("dkod 0.2.0\n").as_deref(),
            Some("0.2.0")
        );
        assert_eq!(parse_version_output("0.2.0").as_deref(), Some("0.2.0"));
        assert_eq!(parse_version_output(""), None);
    }

    #[test]
    fn empty_path_resolves_nothing() {
        assert_eq!(resolve_dkod_on_path(OsStr::new("")), None);
    }
}
