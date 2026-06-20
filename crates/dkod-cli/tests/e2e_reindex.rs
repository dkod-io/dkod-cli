//! End-to-end: `dkod reindex` folds plumbing-seeded legacy refs (the exact
//! pattern benchmarks/drift/run.sh uses) into the index, after which the
//! session reads back even if the legacy ref is deleted.

use assert_cmd::Command as AssertCommand;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.com")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `git hash-object -w --stdin` — run.sh line for line.
fn hash_object(repo: &Path, bytes: &[u8]) -> String {
    let mut child = Command::new("git")
        .args(["hash-object", "-w", "--stdin"])
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn reindex_folds_plumbing_seeded_legacy_session() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);

    // 1. Seed a session via `git hash-object -w --stdin` + `git update-ref`
    //    (NO dkod writer) — the benchmark pattern.
    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 1735689600,
        duration_ms: 0,
        prompt_summary: "reindex-me legacy session".into(),
        messages: vec![dkod_core::Message::user("reindex-me legacy session")],
        commits: vec![],
        files_touched: vec!["src/lib.rs".into()],
        redaction_count: 0,
    };
    let blob = hash_object(repo.path(), &serde_json::to_vec(&s).unwrap());
    git(
        repo.path(),
        &["update-ref", &format!("refs/dkod/sessions/{}", s.id), &blob],
    );

    // 2. `dkod reindex` → folds exactly 1 session.
    let out = AssertCommand::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .arg("reindex")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("folded 1 session(s)"),
        "reindex stdout: {stdout}"
    );

    // 3. Delete the legacy ref — only the index remains.
    git(
        repo.path(),
        &["update-ref", "-d", &format!("refs/dkod/sessions/{}", s.id)],
    );

    // 4. `dkod show <id>` succeeds and prints the prompt summary — the index
    //    alone serves it.
    let show = AssertCommand::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["show", s.id.as_str()])
        .assert()
        .success();
    let show_out = String::from_utf8(show.get_output().stdout.clone()).unwrap();
    assert!(
        show_out.contains("reindex-me legacy session"),
        "dkod show lost the reindexed session:\n{show_out}"
    );

    // 5. Re-running reindex is idempotent — nothing left to fold.
    let again = AssertCommand::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .arg("reindex")
        .assert()
        .success();
    let again_out = String::from_utf8(again.get_output().stdout.clone()).unwrap();
    assert!(
        again_out.contains("folded 0 session(s)"),
        "second reindex should fold nothing: {again_out}"
    );
}
