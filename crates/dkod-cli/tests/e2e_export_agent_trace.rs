//! E2E: `dkod export agent-trace` — Agent Trace interop export.

use assert_cmd::Command;
use dkod_core::{Agent, Message, Session};
use predicates::str::contains;
use std::process::Command as StdCommand;

fn init_git_repo(path: &std::path::Path) {
    let output = StdCommand::new("git")
        .arg("init")
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture_session(id: &str) -> Session {
    Session {
        id: id.into(),
        agent: Agent::Codex,
        created_at: 1_735_689_600, // 2025-01-01T00:00:00Z
        duration_ms: 42,
        prompt_summary: "fix bug".into(),
        messages: vec![Message::user("fix bug")],
        commits: vec!["1111111111111111111111111111111111111111".into()],
        files_touched: vec!["src/lib.rs".into()],
        redaction_count: 0,
    }
}

#[test]
fn exports_single_session_to_stdout_with_out_dash() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_git_repo(tmp.path());
    let s = fixture_session("0192f8e2-7b3a-7000-8a3e-00000000aa01");
    dkod_core::store::write_session(tmp.path(), &s).unwrap();

    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .args(["export", "agent-trace", &s.id, "--out", "-"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let v: serde_json::Value = serde_json::from_slice(&out).expect("stdout is a JSON record");
    assert_eq!(v["version"], "0.1.0");
    assert_eq!(v["id"], s.id);
    assert_eq!(v["timestamp"], "2025-01-01T00:00:00Z");
    assert_eq!(v["vcs"]["type"], "git");
    assert_eq!(
        v["vcs"]["revision"],
        "1111111111111111111111111111111111111111"
    );
    assert_eq!(v["tool"]["name"], "dkod");
    assert_eq!(v["files"][0]["path"], "src/lib.rs");
    assert_eq!(
        v["files"][0]["conversations"][0]["contributor"]["type"],
        "ai"
    );
    assert_eq!(
        v["files"][0]["conversations"][0]["ranges"],
        serde_json::json!([])
    );
    assert_eq!(v["metadata"]["io.dkod"]["agent"], "codex");
}

#[test]
fn exports_all_sessions_to_default_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_git_repo(tmp.path());
    let a = fixture_session("0192f8e2-7b3a-7000-8a3e-00000000bb01");
    let b = fixture_session("0192f8e2-7b3a-7000-8a3e-00000000bb02");
    dkod_core::store::write_session(tmp.path(), &a).unwrap();
    dkod_core::store::write_session(tmp.path(), &b).unwrap();

    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .args(["export", "agent-trace"])
        .assert()
        .success()
        .stdout(contains("2"));

    for id in [&a.id, &b.id] {
        let path = tmp.path().join("agent-traces").join(format!("{id}.json"));
        assert!(path.exists(), "missing trace file: {}", path.display());
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["id"], *id);
        assert_eq!(v["version"], "0.1.0");
    }
}

#[test]
fn exports_single_session_to_custom_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_git_repo(tmp.path());
    let s = fixture_session("0192f8e2-7b3a-7000-8a3e-00000000cc01");
    dkod_core::store::write_session(tmp.path(), &s).unwrap();

    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .args(["export", "agent-trace", &s.id, "--out", "traces-out"])
        .assert()
        .success();

    let path = tmp.path().join("traces-out").join(format!("{}.json", s.id));
    assert!(path.exists(), "missing trace file: {}", path.display());
}

#[test]
fn out_dash_without_session_id_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_git_repo(tmp.path());

    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .args(["export", "agent-trace", "--out", "-"])
        .assert()
        .failure()
        .stderr(contains("session id"));
}

#[test]
fn export_with_no_sessions_succeeds_quietly() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_git_repo(tmp.path());

    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .args(["export", "agent-trace"])
        .assert()
        .success()
        .stdout(contains("no sessions"));
}

#[test]
fn export_outside_a_git_repo_fails() {
    let tmp = tempfile::TempDir::new().unwrap();

    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .args(["export", "agent-trace"])
        .assert()
        .failure()
        .stderr(contains("not a git repo"));
}
