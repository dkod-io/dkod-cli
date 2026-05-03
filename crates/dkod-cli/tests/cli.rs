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
