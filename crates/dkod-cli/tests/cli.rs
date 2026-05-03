use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn shows_help() {
    let mut cmd = Command::cargo_bin("dkod").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(contains("init"))
        .stdout(contains("capture"))
        .stdout(contains("log"))
        .stdout(contains("show"));
}

#[test]
fn version_flag_works() {
    let mut cmd = Command::cargo_bin("dkod").unwrap();
    cmd.arg("--version").assert().success();
}

use std::process::Command as StdCommand;

fn init_git_repo(path: &std::path::Path) {
    StdCommand::new("git")
        .arg("init")
        .arg(path)
        .output()
        .unwrap();
}

#[test]
fn init_writes_config_in_a_repo() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_git_repo(tmp.path());

    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .arg("init")
        .assert()
        .success();

    let cfg = tmp.path().join(".dkod/config.toml");
    assert!(cfg.exists(), ".dkod/config.toml not created");
    let body = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        body.contains("[redact]"),
        "config missing [redact] section: {body}"
    );
    assert!(
        body.contains("enabled = true"),
        "redact not enabled by default: {body}"
    );
}

#[test]
fn init_is_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_git_repo(tmp.path());

    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .arg("init")
        .assert()
        .success();

    // Take a snapshot of the file's bytes
    let cfg = tmp.path().join(".dkod/config.toml");
    let body_before = std::fs::read(&cfg).unwrap();

    // Run init a second time — should succeed and NOT overwrite existing config
    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .arg("init")
        .assert()
        .success();

    let body_after = std::fs::read(&cfg).unwrap();
    assert_eq!(
        body_before, body_after,
        "init overwrote existing .dkod/config.toml"
    );
}

#[test]
fn init_outside_a_repo_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .arg("init")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a git repo"));
}

fn write_fixture_session(
    repo: &std::path::Path,
    id_override: Option<String>,
    summary: &str,
) -> String {
    let s = dkod_core::Session {
        id: id_override.unwrap_or_else(dkod_core::Session::new_id),
        agent: dkod_core::Agent::Codex,
        created_at: 1735689600,
        duration_ms: 0,
        prompt_summary: summary.to_string(),
        messages: vec![
            dkod_core::Message::user(summary),
            dkod_core::Message::assistant("done"),
        ],
        commits: vec![],
        files_touched: vec![],
    };
    let id = s.id.clone();
    dkod_core::store::write_session(repo, &s).unwrap_or_else(|e| panic!("write_session: {e}"));
    id
}

#[test]
fn log_lists_sessions_written_directly() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_git_repo(tmp.path());

    let id = write_fixture_session(tmp.path(), None, "hello");

    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .arg("log")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains(&id),
        "log stdout missing session id; got: {stdout}"
    );
    assert!(
        stdout.contains("hello"),
        "log stdout missing prompt summary; got: {stdout}"
    );
}

#[test]
fn log_outside_a_repo_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .arg("log")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a git repo"));
}

#[test]
fn show_prints_session_transcript() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_git_repo(tmp.path());
    let id = write_fixture_session(tmp.path(), None, "fix bug");

    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .args(["show", &id])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("fix bug"),
        "show should contain prompt; got: {stdout}"
    );
    assert!(
        stdout.contains("done"),
        "show should contain assistant content; got: {stdout}"
    );
}

#[test]
fn show_unknown_id_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_git_repo(tmp.path());
    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .args(["show", "nonexistent-id-1234"])
        .assert()
        .failure();
}

#[test]
fn capture_unknown_agent_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_git_repo(tmp.path());
    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .args(["capture", "made-up-agent", "--", "noop"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown agent"));
}

#[test]
fn capture_claude_code_outside_repo_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .args(["capture", "claude-code"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a git repo"));
}

#[test]
fn capture_claude_code_refuses_when_global_hooks_disabled() {
    // Build a fake $HOME with `.claude/settings.json` containing
    // `disableAllHooks: true`, then run `dkod capture claude-code` against
    // a fresh git repo. Expect failure with `disableAllHooks` in stderr.
    let home = tempfile::TempDir::new().unwrap();
    let claude_dir = home.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"disableAllHooks": true}"#,
    )
    .unwrap();

    let repo = tempfile::TempDir::new().unwrap();
    init_git_repo(repo.path());

    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&repo)
        .env("HOME", home.path())
        .env_remove("XDG_DATA_HOME") // force dirs to use HOME
        .args(["capture", "claude-code"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("disableAllHooks"));
}

#[test]
fn capture_hook_unknown_event_exits_zero() {
    // Hidden subcommand. Even an unknown event must exit 0 — never break
    // Claude Code.
    Command::cargo_bin("dkod")
        .unwrap()
        .args(["capture-hook", "deadbeefcafe", "NotARealEvent"])
        .write_stdin("")
        .assert()
        .success();
}

#[test]
fn capture_hook_with_no_socket_exits_zero() {
    // A valid hook event name, but the socket doesn't exist for the
    // supplied repo_hash. Must still exit 0.
    let payload = serde_json::json!({
        "session_id": "00000000-0000-0000-0000-000000000000",
        "transcript_path": "/tmp/never.jsonl",
        "cwd": "/tmp",
        "hook_event_name": "SessionStart",
        "source": "startup",
    })
    .to_string();
    Command::cargo_bin("dkod")
        .unwrap()
        .args(["capture-hook", "deadbeefcafe", "SessionStart"])
        .write_stdin(payload)
        .assert()
        .success();
}

#[test]
fn capture_hook_with_malformed_repo_hash_exits_zero() {
    // Defence-in-depth: a tampered settings.local.json could pass us a
    // path-like repo_hash. We must silently exit 0 — never touch the
    // filesystem and never break Claude Code.
    Command::cargo_bin("dkod")
        .unwrap()
        .args(["capture-hook", "../../etc/passwd", "SessionStart"])
        .write_stdin("{}")
        .assert()
        .success();
}

#[test]
fn capture_hook_is_hidden_in_help() {
    // The internal capture-hook subcommand must NOT appear in --help.
    let out = Command::cargo_bin("dkod")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("capture-hook"),
        "capture-hook should be hidden, got: {stdout}"
    );
}

#[test]
fn capture_outside_a_repo_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .args(["capture", "codex", "--", "noop"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a git repo"));
}

#[test]
fn init_rejects_invalid_custom_regex_in_existing_config() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_git_repo(tmp.path());

    // Write a pre-existing config with a deliberately bad custom regex
    let dkod_dir = tmp.path().join(".dkod");
    std::fs::create_dir_all(&dkod_dir).unwrap();
    let bad_cfg = r#"
[redact]
enabled = true
patterns = ["builtin:aws"]
custom = ["bad-pattern["]
"#;
    std::fs::write(dkod_dir.join("config.toml"), bad_cfg).unwrap();

    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .arg("init")
        .assert()
        .failure()
        .stderr(predicates::str::contains("invalid custom redact pattern"))
        .stderr(predicates::str::contains("bad-pattern["));
}
