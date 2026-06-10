//! Fail-open hook tests (issue #24).
//!
//! A stale `dkod` binary on PATH must never be able to brick the user's
//! agent sessions: Claude Code treats a PreToolUse hook exiting 2 as
//! BLOCK, so every hook command the wizard writes has to guarantee
//! exit 0 whether dkod is missing, stale, or crashing.
//!
//! Covers:
//! 1. The hook-command template (`command -v dkod ... || true`).
//! 2. Idempotency: running setup twice never duplicates entries.
//! 3. Self-heal: old-form (non-fail-open) entries get rewritten to the
//!    fail-open form on re-run, preserving the user's other hooks.
//! 4. Preflight: a stale `dkod` shim on PATH (wrong version or failing
//!    probe) is detected and reported, naming both paths and versions.

use dkod_cli::cmd::setup::agents::{claude_code::ClaudeCode, AgentInstaller, InstallContext};
use dkod_cli::cmd::setup::consent::{NonInteractivePrompter, Prompter};
use dkod_cli::cmd::setup::failopen::{fail_open_hook_command, heal_command, is_fail_open};
use dkod_cli::cmd::setup::preflight::run_preflight;
use dkod_cli::cmd::setup::state::{AgentState, Scope};
use serde_json::Value;
use tempfile::TempDir;

fn empty_agent_state() -> AgentState {
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

fn touch(path: &std::path::Path) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, "{}").unwrap();
}

fn install_user_scope(home: &std::path::Path) {
    let mut state = empty_agent_state();
    let mut prompter = NonInteractivePrompter::new("y");
    let mut ctx = InstallContext {
        home,
        scope: Scope::User,
        repo_root: None,
        prompter: &mut prompter as &mut dyn Prompter,
        non_interactive: true,
    };
    ClaudeCode.install(&mut ctx, &mut state).unwrap();
}

/// Collect every `hooks[].command` string anywhere under the settings
/// `hooks` object.
fn all_hook_commands(settings: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(events) = settings.get("hooks").and_then(|h| h.as_object()) {
        for arr in events.values() {
            for entry in arr.as_array().into_iter().flatten() {
                for hook in entry
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .into_iter()
                    .flatten()
                {
                    if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                        out.push(cmd.to_string());
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 1. Template
// ---------------------------------------------------------------------------

#[test]
fn fail_open_template_guards_missing_binary_and_nonzero_exit() {
    let cmd = fail_open_hook_command("dkod capture-hook --agent claude-code --event Stop");
    assert!(
        cmd.starts_with("command -v dkod >/dev/null 2>&1 && "),
        "missing command -v guard: {cmd}"
    );
    assert!(cmd.ends_with(" || true"), "missing || true tail: {cmd}");
    assert!(
        cmd.contains("dkod capture-hook --agent claude-code --event Stop"),
        "inner command lost: {cmd}"
    );
    assert!(is_fail_open(&cmd));
}

#[test]
fn heal_command_rewrites_old_forms_and_leaves_others_alone() {
    // Wizard old form.
    let healed = heal_command("dkod capture-hook --agent claude-code --event PreToolUse")
        .expect("old wizard form should be healed");
    assert!(is_fail_open(&healed));
    assert!(healed.contains("dkod capture-hook --agent claude-code --event PreToolUse"));

    // Legacy per-repo positional form.
    let healed = heal_command("dkod capture-hook deadbeefcafe Stop")
        .expect("legacy positional form should be healed");
    assert!(is_fail_open(&healed));
    assert!(healed.contains("dkod capture-hook deadbeefcafe Stop"));

    // Already fail-open: nothing to do.
    let already = fail_open_hook_command("dkod capture-hook --agent claude-code --event Stop");
    assert_eq!(heal_command(&already), None);

    // Not a dkod capture hook: untouched.
    assert_eq!(heal_command("echo user-hook"), None);
    assert_eq!(heal_command("my-other-tool capture-hook"), None);
}

// ---------------------------------------------------------------------------
// 2. Wizard writes only fail-open commands; idempotent.
// ---------------------------------------------------------------------------

#[test]
fn user_scope_install_writes_only_fail_open_commands() {
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".claude/settings.json"));
    install_user_scope(home.path());

    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap(),
    )
    .unwrap();
    let commands = all_hook_commands(&settings);
    assert!(!commands.is_empty(), "no hook commands written");
    for cmd in &commands {
        assert!(
            is_fail_open(cmd),
            "wizard wrote a non-fail-open hook command: {cmd}"
        );
    }
}

#[test]
fn running_setup_twice_does_not_duplicate_entries() {
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".claude/settings.json"));
    install_user_scope(home.path());
    let first = std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    install_user_scope(home.path());
    let second = std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();

    let count = |s: &str| s.matches("\"_dkod\": true").count();
    assert_eq!(
        count(&first),
        count(&second),
        "re-running setup must not duplicate dkod entries"
    );
    let expected = dkod_cli::cmd::capture::claude_code::HOOK_EVENTS.len();
    assert_eq!(count(&second), expected);
}

// ---------------------------------------------------------------------------
// 3. Self-heal: old-form entries become fail-open, other hooks survive.
// ---------------------------------------------------------------------------

#[test]
fn setup_self_heals_old_form_entries_to_fail_open() {
    let home = TempDir::new().unwrap();
    let settings_path = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();

    // Seed the exact shape an old (v0.2.0) wizard wrote — the bricked-user
    // scenario — plus a user hook dkod must not touch, plus a stray
    // hand-copied dkod entry WITHOUT the sentinel.
    let seeded = serde_json::json!({
        "otherSetting": 42,
        "hooks": {
            "PreToolUse": [{
                "_dkod": true,
                "version": 1,
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": "dkod capture-hook --agent claude-code --event PreToolUse",
                    "timeout": 5,
                }]
            }],
            "Stop": [
                {
                    "matcher": "*",
                    "hooks": [{"type": "command", "command": "echo user-hook"}]
                },
                {
                    "_dkod": true,
                    "version": 1,
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "dkod capture-hook --agent claude-code --event Stop",
                        "timeout": 5,
                    }]
                }
            ],
            "Notification": [{
                // No sentinel — e.g. hand-copied from a blog post. Must be
                // healed in place (made harmless) but not duplicated.
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": "dkod capture-hook --agent claude-code --event Notification"
                }]
            }]
        }
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&seeded).unwrap(),
    )
    .unwrap();

    install_user_scope(home.path());

    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();

    // Every dkod capture-hook command anywhere is now fail-open.
    for cmd in all_hook_commands(&settings) {
        if cmd.contains("dkod capture-hook") {
            assert!(is_fail_open(&cmd), "unhealed dkod command survived: {cmd}");
        }
    }

    // The user's own hook is untouched.
    let stop = settings
        .pointer("/hooks/Stop")
        .and_then(|v| v.as_array())
        .unwrap();
    assert!(
        stop.iter().any(|e| {
            e.pointer("/hooks/0/command").and_then(|c| c.as_str()) == Some("echo user-hook")
        }),
        "user hook lost: {settings}"
    );
    // Exactly one dkod entry on Stop (replaced, not duplicated).
    let dkod_on_stop = stop
        .iter()
        .filter(|e| e.get("_dkod").and_then(|v| v.as_bool()) == Some(true))
        .count();
    assert_eq!(dkod_on_stop, 1);

    // The non-sentinel stray was healed in place, not duplicated.
    let notification = settings
        .pointer("/hooks/Notification")
        .and_then(|v| v.as_array())
        .unwrap();
    assert_eq!(notification.len(), 1);

    // Unrelated settings preserved.
    assert_eq!(
        settings.get("otherSetting").unwrap(),
        &serde_json::json!(42)
    );
}

// ---------------------------------------------------------------------------
// 4. Preflight: stale binary on PATH is detected.
// ---------------------------------------------------------------------------

/// Write a fake `dkod` shim into `dir` that reports `version` for
/// `--version` and exits `probe_exit` for everything else.
fn write_fake_dkod(dir: &std::path::Path, version: &str, probe_exit: i32) -> std::path::PathBuf {
    let path = dir.join("dkod");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo \"dkod {version}\"; exit 0; fi\nexit {probe_exit}\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

#[test]
fn preflight_flags_version_mismatch_on_path() {
    let bin = TempDir::new().unwrap();
    let shim = write_fake_dkod(bin.path(), "0.1.1", 0);

    let report = run_preflight(bin.path().as_os_str(), "9.9.9");
    assert_eq!(report.resolved.as_deref(), Some(shim.as_path()));
    assert_eq!(report.resolved_version.as_deref(), Some("0.1.1"));
    assert!(!report.is_healthy());
    let warning = report.warning().expect("mismatch must produce a warning");
    assert!(
        warning.contains("0.1.1"),
        "warning missing stale version: {warning}"
    );
    assert!(
        warning.contains("9.9.9"),
        "warning missing running version: {warning}"
    );
    assert!(
        warning.contains(&shim.display().to_string()),
        "warning missing stale path: {warning}"
    );
}

#[test]
fn preflight_flags_failing_probe() {
    let bin = TempDir::new().unwrap();
    // Same version, but capture-hook exits 2 — exactly the clap-rejection
    // failure mode from the incident.
    write_fake_dkod(bin.path(), "9.9.9", 2);

    let report = run_preflight(bin.path().as_os_str(), "9.9.9");
    assert_eq!(report.probe_ok, Some(false));
    assert!(!report.is_healthy());
    assert!(report.warning().is_some());
}

#[test]
fn preflight_with_healthy_matching_binary_is_quiet() {
    let bin = TempDir::new().unwrap();
    write_fake_dkod(bin.path(), "9.9.9", 0);

    let report = run_preflight(bin.path().as_os_str(), "9.9.9");
    assert!(report.is_healthy());
    assert_eq!(report.warning(), None);
}

#[test]
fn preflight_with_no_dkod_on_path_warns_but_is_not_fatal() {
    let bin = TempDir::new().unwrap(); // empty dir — no dkod
    let report = run_preflight(bin.path().as_os_str(), "9.9.9");
    assert_eq!(report.resolved, None);
    // Missing from PATH is worth a heads-up (hooks will no-op) but it is
    // not the stale-binary failure mode.
    assert!(report.warning().is_some());
}

// ---------------------------------------------------------------------------
// 5. Probe agent is side-effect free and always exits 0.
// ---------------------------------------------------------------------------

#[test]
fn capture_hook_probe_agent_exits_zero_without_writing() {
    use assert_cmd::Command;
    let home = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    Command::cargo_bin("dkod")
        .unwrap()
        .env("HOME", home.path())
        .env("DKOD_VAULT_PATH", vault.path())
        .args(["capture-hook", "--agent", "probe", "--event", "Preflight"])
        .write_stdin("")
        .assert()
        .success();
    assert!(
        !vault.path().join(".dkod/pending").exists(),
        "probe must not buffer pending events"
    );
}

// ---------------------------------------------------------------------------
// 6. Per-repo install (dkod init path) is fail-open too.
// ---------------------------------------------------------------------------

#[test]
fn per_repo_init_writes_fail_open_hooks() {
    use assert_cmd::Command;
    let home = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let out = std::process::Command::new("git")
        .args(["init"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(out.status.success());

    Command::cargo_bin("dkod")
        .unwrap()
        .env("HOME", home.path())
        .current_dir(repo.path())
        .arg("init")
        .assert()
        .success();

    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".claude/settings.local.json")).unwrap(),
    )
    .unwrap();
    let commands = all_hook_commands(&settings);
    assert!(!commands.is_empty());
    for cmd in &commands {
        assert!(
            is_fail_open(cmd),
            "per-repo install wrote non-fail-open command: {cmd}"
        );
    }
}
