//! Permanent-compat guard (storage-v2 §6.2): a repo whose sessions were
//! seeded with raw git plumbing against legacy refs — exactly what
//! benchmarks/drift/run.sh does — must keep working with NO refs/dkod/index
//! present, in every storage-v2 phase, forever. A companion test seeds a
//! session index-ONLY (dual-write off) and proves the same commands resolve it
//! through the index alone, so the fallback chain is exercised from both ends.

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

fn head(repo: &Path) -> String {
    String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string()
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

/// True when this repo has no `refs/dkod/index` ref.
fn no_index_ref(repo: &Path) -> bool {
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/dkod/index"])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&refs.stdout).trim().is_empty()
}

#[test]
fn plumbing_seeded_legacy_repo_works_without_index_ref() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);

    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 1735689600,
        duration_ms: 0,
        prompt_summary: "benchmark-style legacy session".into(),
        messages: vec![dkod_core::Message::user("benchmark-style legacy session")],
        commits: vec![],
        files_touched: vec!["src/lib.rs".into()],
        redaction_count: 0,
    };
    let blob = hash_object(repo.path(), &serde_json::to_vec(&s).unwrap());
    git(
        repo.path(),
        &["update-ref", &format!("refs/dkod/sessions/{}", s.id), &blob],
    );

    // No index ref may exist.
    assert!(no_index_ref(repo.path()));

    for args in [
        vec!["log"],
        vec!["show", s.id.as_str()],
        vec!["drift", s.id.as_str()],
    ] {
        let out = AssertCommand::cargo_bin("dkod")
            .unwrap()
            .current_dir(repo.path())
            .args(&args)
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert!(
            stdout.contains(&s.id) || stdout.contains("benchmark-style"),
            "dkod {args:?} lost the legacy session:\n{stdout}"
        );
    }

    // And reading must NOT have created an index ref as a side effect.
    assert!(
        no_index_ref(repo.path()),
        "read path must never write the index"
    );
}

#[test]
fn index_only_session_resolves_through_read_and_blame() {
    // Companion to the legacy-only guard: with dual-write OFF, a session lives
    // ONLY in refs/dkod/index (no refs/dkod/sessions|commits ref at all). The
    // read path (show) and blame must resolve it through the index alone.
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::create_dir_all(repo.path().join(".dkod")).unwrap();
    std::fs::write(
        repo.path().join(".dkod/config.toml"),
        "[storage]\nwrite_legacy_refs = false\n",
    )
    .unwrap();

    std::fs::write(repo.path().join("f.txt"), "ai authored line\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "ai commit"]);
    let sha = head(repo.path());

    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::Codex,
        created_at: 1735689600,
        duration_ms: 0,
        prompt_summary: "index-only session".into(),
        messages: vec![dkod_core::Message::user("index-only session")],
        commits: vec![],
        files_touched: vec!["f.txt".into()],
        redaction_count: 0,
    };
    // Index-only write + index-only commit pointer (no legacy refs).
    let mut s = s;
    dkod_core::store::write_session_full(repo.path(), &mut s, std::slice::from_ref(&sha), &[])
        .unwrap();

    // No legacy session/commit ref exists — only the index.
    let r = gix::open(repo.path()).unwrap();
    assert!(
        r.try_find_reference(&dkod_core::refs::session_ref(&s.id))
            .unwrap()
            .is_none(),
        "no legacy session ref under dual-write OFF"
    );
    assert!(
        r.try_find_reference(&dkod_core::refs::commit_ref(&sha))
            .unwrap()
            .is_none(),
        "no legacy commit ref under dual-write OFF"
    );
    assert!(!no_index_ref(repo.path()), "index ref must exist");

    // show resolves the body via the index.
    let show = AssertCommand::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["show", s.id.as_str()])
        .assert()
        .success();
    let show_out = String::from_utf8(show.get_output().stdout.clone()).unwrap();
    assert!(
        show_out.contains("index-only session"),
        "dkod show lost the index-only session:\n{show_out}"
    );

    // blame resolves the commit's annotation via the index pointer.
    let blame = AssertCommand::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["blame", "f.txt"])
        .assert()
        .success();
    let blame_out = String::from_utf8(blame.get_output().stdout.clone()).unwrap();
    assert!(
        blame_out.contains("codex") && blame_out.contains("index-only session"),
        "blame did not resolve the index-only session:\n{blame_out}"
    );
}
