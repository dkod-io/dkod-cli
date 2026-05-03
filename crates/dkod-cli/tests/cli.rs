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
