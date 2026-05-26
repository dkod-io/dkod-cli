//! End-to-end smoke tests for the `dkod setup` and `dkod capture-hook`
//! binary entry points.
//!
//! These spawn the actual `dkod` binary built by cargo against a tempdir
//! `HOME` and verify the wiring at the CLI boundary — complementing the
//! library-level orchestrator tests in `setup_orchestrator.rs`.

use assert_cmd::Command;
use std::path::PathBuf;
use tempfile::TempDir;

fn dkod_bin() -> Command {
    Command::cargo_bin("dkod").expect("dkod binary built")
}

fn touch(path: &std::path::Path) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, "{}").unwrap();
}

#[test]
fn setup_in_empty_home_writes_config_and_vault() {
    let home = TempDir::new().unwrap();
    let out = dkod_bin()
        .env("HOME", home.path())
        .args(["setup", "--non-interactive"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();

    assert!(
        stdout.contains("config:"),
        "summary missing config: line:\n{stdout}"
    );
    assert!(
        stdout.contains("agents:"),
        "summary missing agents: line:\n{stdout}"
    );

    let config_path = home.path().join(".dkod/config.toml");
    assert!(config_path.exists(), "config.toml should be written");
    let vault_path = home.path().join(".dkod/vault");
    assert!(
        vault_path.join(".git").exists(),
        "vault should be a git repo"
    );
}

#[test]
fn setup_with_claude_code_settings_wires_hooks() {
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".claude/settings.json"));

    dkod_bin()
        .env("HOME", home.path())
        .args(["setup", "--non-interactive"])
        .assert()
        .success();

    let settings_body = std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    assert!(
        settings_body.contains("_dkod"),
        "claude-code settings.json should carry dkod sentinel after setup"
    );
    assert!(
        settings_body.contains("dkod capture-hook"),
        "claude-code settings.json should reference the capture-hook command"
    );
}

#[test]
fn capture_hook_flag_form_buffers_pending_in_vault() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();

    dkod_bin()
        .env("HOME", home.path())
        .env("DKOD_VAULT_PATH", vault.path())
        .current_dir(cwd.path())
        .args([
            "capture-hook",
            "--agent",
            "claude-code",
            "--event",
            "SessionStart",
        ])
        .write_stdin(r#"{"session_id":"s1"}"#)
        .assert()
        .success();

    let pending_dir = vault.path().join(".dkod/pending");
    let entries: Vec<PathBuf> = std::fs::read_dir(&pending_dir)
        .expect("pending dir should exist")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one pending envelope expected");
    let body = std::fs::read_to_string(&entries[0]).unwrap();
    assert!(body.contains("\"agent\": \"claude-code\""));
    assert!(body.contains("\"event\": \"SessionStart\""));
}

#[test]
fn capture_hook_with_bad_args_still_exits_zero() {
    // The hook MUST never break the agent — even when called with
    // garbage args it must return success.
    let home = TempDir::new().unwrap();
    dkod_bin()
        .env("HOME", home.path())
        .args(["capture-hook", "--agent", "../etc", "--event", "X"])
        .write_stdin("not json")
        .assert()
        .success();
}

#[test]
fn dkod_log_emits_drift_warning_when_state_differs_from_disk() {
    // Pre-seed config saying claude-code is installed, but DON'T create
    // any agent files in the tempdir HOME. dkod log should print a one-
    // line drift notice on stderr and still succeed.
    let home = TempDir::new().unwrap();
    let dkod_dir = home.path().join(".dkod");
    std::fs::create_dir_all(&dkod_dir).unwrap();
    std::fs::write(
        dkod_dir.join("config.toml"),
        r#"schema_version = 1

[scope]
default = "user"

[vault]
path = ""

[agents.claude-code]
installed = true
"#,
    )
    .unwrap();

    // Need a git repo for `dkod log` to succeed; use a tempdir.
    let repo = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(repo.path())
        .output()
        .unwrap();

    let out = dkod_bin()
        .env("HOME", home.path())
        .current_dir(repo.path())
        .arg("log")
        .assert()
        .success();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("drifted") && stderr.contains("claude-code"),
        "expected drift warning mentioning claude-code on stderr, got:\n{stderr}"
    );
}

#[test]
fn dkod_init_hints_at_setup_when_wizard_has_never_run() {
    // Fresh tempdir HOME (no ~/.dkod/config.toml) + fresh git repo.
    let home = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(repo.path())
        .output()
        .unwrap();

    let out = dkod_bin()
        .env("HOME", home.path())
        .current_dir(repo.path())
        .arg("init")
        .assert()
        .success();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("dkod setup"),
        "expected dkod init to hint at `dkod setup` when wizard has never run:\n{stderr}"
    );
}

#[test]
fn dkod_init_stays_quiet_when_wizard_has_run() {
    let home = TempDir::new().unwrap();
    // Pre-seed the wizard config so the hint is suppressed.
    let dkod_dir = home.path().join(".dkod");
    std::fs::create_dir_all(&dkod_dir).unwrap();
    std::fs::write(dkod_dir.join("config.toml"), "schema_version = 1\n").unwrap();

    let repo = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(repo.path())
        .output()
        .unwrap();

    let out = dkod_bin()
        .env("HOME", home.path())
        .current_dir(repo.path())
        .arg("init")
        .assert()
        .success();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        !stderr.contains("tip — run `dkod setup`"),
        "init should NOT print the setup tip once the wizard has run:\n{stderr}"
    );
}
