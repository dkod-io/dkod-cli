//! Claude Code installer.
//!
//! Claude Code has the most mature hook surface of the seven agents. The
//! per-repo hook installer already lives in
//! [`crate::cmd::capture::claude_code::install_hooks_at_init`] and powers
//! `dkod init`'s phase-1 install. This wizard module wraps that for the
//! per-repo case and adds the user-scope path (`~/.claude/settings.json`)
//! that the seamless wizard needs so capture works in every repo without
//! a per-repo `dkod init` first.

use crate::cmd::setup::agents::{AgentInstaller, DetectedAgent, InstallContext, InstallOutcome};
use crate::cmd::setup::state::{fingerprint, AgentState, Scope};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;

pub struct ClaudeCode;

impl AgentInstaller for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn detect(&self, home: &Path) -> Option<DetectedAgent> {
        // The presence of either the user-scope settings file OR the
        // `.claude/` directory itself is enough to confirm Claude Code is
        // installed. We don't probe the binary — that would require
        // shelling out and the wizard's self-heal must stay sub-50µs.
        let user_settings = home.join(".claude/settings.json");
        if user_settings.exists() {
            return Some(DetectedAgent {
                name: "claude-code",
                config_path: user_settings,
                version: None,
            });
        }
        let claude_dir = home.join(".claude");
        if claude_dir.exists() {
            return Some(DetectedAgent {
                name: "claude-code",
                config_path: claude_dir,
                version: None,
            });
        }
        None
    }

    fn install(&self, ctx: &mut InstallContext, state: &mut AgentState) -> Result<InstallOutcome> {
        let detected = match self.detect(ctx.home) {
            Some(d) => d,
            None => return Ok(InstallOutcome::AgentNotPresent),
        };

        match ctx.scope {
            Scope::User => install_user_scope(ctx.home, state)?,
            Scope::PerRepo => {
                // The per-repo install is already wired into `dkod init`;
                // surface a clean delegation here so the wizard can be
                // invoked outside of init too.
                let repo = ctx
                    .repo_root
                    .context("per-repo claude-code install requires a repo_root")?;
                crate::cmd::capture::claude_code::install_hooks_at_init(repo)
                    .with_context(|| format!("install per-repo hooks in {}", repo.display()))?;
                let settings_path = repo.join(".claude/settings.local.json");
                // Record the fingerprint so self-heal can detect drift
                // (e.g., a hand-edit) the same way it does for the
                // user-scope install above. Propagate the read error
                // rather than silently skipping — install_hooks_at_init
                // just wrote this file, so a read failure is a real bug
                // we want to surface, not a soft fallback.
                let body = std::fs::read(&settings_path).with_context(|| {
                    format!(
                        "read just-written {} for fingerprint",
                        settings_path.display()
                    )
                })?;
                state.fingerprint = Some(fingerprint(&body));
                state.installed = true;
                state.scope = Some(Scope::PerRepo);
                state.hook_path = Some(settings_path);
            }
        }

        let _ = detected; // detection metadata reserved for future state fields
        Ok(InstallOutcome::Installed)
    }
}

/// Write user-scope dkod hooks into `~/.claude/settings.json`. Preserves
/// every existing key; only touches the `hooks` block and only the entries
/// marked with the dkod sentinel.
fn install_user_scope(home: &Path, state: &mut AgentState) -> Result<()> {
    let path = home.join(".claude/settings.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let mut root: Value = if path.exists() {
        let body =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        if body.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&body).with_context(|| format!("parse {}", path.display()))?
        }
    } else {
        json!({})
    };

    let hooks = root
        .as_object_mut()
        .context("~/.claude/settings.json root is not a JSON object")?
        .entry("hooks")
        .or_insert_with(|| json!({}));

    let hooks_obj = hooks
        .as_object_mut()
        .context("settings.json `hooks` block is not an object")?;

    for (event, version) in dkod_hook_events() {
        let arr = hooks_obj
            .entry((*event).to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("settings.json hooks.{event} is not an array"))?;

        // Replace any existing dkod-marked entry for this event; preserve
        // everything else. The sentinel key `_dkod` is what makes our
        // entries surgically removable later.
        arr.retain(|entry| {
            entry
                .as_object()
                .and_then(|o| o.get("_dkod"))
                .and_then(|v| v.as_bool())
                != Some(true)
        });

        arr.push(json!({
            "_dkod": true,
            "version": version,
            "matcher": "",
            "hooks": [
                {
                    "type": "command",
                    "command": "dkod capture-hook --agent claude-code --event ".to_string() + event,
                    "timeout": 5,
                }
            ],
        }));
    }

    let serialized = serde_json::to_string_pretty(&root).context("serialize settings.json")?;
    let bytes = serialized.as_bytes();
    state.fingerprint = Some(fingerprint(bytes));
    let tmp = tmp_sibling(&path);
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;

    state.installed = true;
    state.scope = Some(Scope::User);
    state.hook_path = Some(path);
    Ok(())
}

fn tmp_sibling(path: &Path) -> std::path::PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(name),
        _ => std::path::PathBuf::from(name),
    }
}

/// Hook events dkod installs at the user scope. Keep the order stable so
/// diffs against `~/.claude/settings.json` stay clean. Mirrors the per-repo
/// list in `cmd::capture::claude_code` but parameterised so future events
/// can be added in one place.
fn dkod_hook_events() -> &'static [(&'static str, u32)] {
    &[
        ("SessionStart", 1),
        ("UserPromptSubmit", 1),
        ("PreToolUse", 1),
        ("PostToolUse", 1),
        ("Stop", 1),
        ("SessionEnd", 2),
    ]
}
