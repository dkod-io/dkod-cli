//! End-to-end: the patch-id fallback restores `dkod blame` provenance after a
//! diff-preserving rewrite the post-rewrite hook never saw — and does NOT
//! mis-attribute after a content-changing rewrite.

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

fn patch_id(repo: &Path, sha: &str) -> String {
    let diff = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["diff-tree", "-p", "--root", sha])
        .output()
        .unwrap();
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&diff.stdout).unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn seed_session(repo: &Path, prompt: &str, old_sha: &str) -> dkod_core::Session {
    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: prompt.into(),
        messages: vec![],
        commits: vec![old_sha.to_string()],
        files_touched: vec!["f.txt".into()],
        redaction_count: 0,
    };
    dkod_core::store::write_session(repo, &s).unwrap();
    dkod_core::store::link_session_to_commit(repo, &s.id, old_sha).unwrap();
    // The patch-id ref is what survives the rewrite (the commit-ref becomes stale).
    dkod_core::store::link_session_to_patchid(repo, &s.id, &patch_id(repo, old_sha)).unwrap();
    s
}

#[test]
fn patchid_fallback_recovers_blame_after_diff_preserving_rewrite() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "ai line\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "original"]);
    let old = head(repo.path());

    seed_session(repo.path(), "write the greeting", &old);

    // Diff-preserving rewrite (reword). commit-ref for `new` does NOT exist and
    // we deliberately do NOT run relink — only the patch-id ref can save us.
    git(repo.path(), &["commit", "--amend", "-qm", "reworded"]);
    let new = head(repo.path());
    assert_ne!(old, new, "amend must change the sha");
    assert_eq!(
        patch_id(repo.path(), &old),
        patch_id(repo.path(), &new),
        "message-only amend must preserve patch-id"
    );

    let out = AssertCommand::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["blame", "f.txt"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("claude_code"),
        "patch-id fallback should attribute the agent:\n{stdout}"
    );
    assert!(
        stdout.contains("write the greeting"),
        "patch-id fallback should show the prompt:\n{stdout}"
    );
}

#[test]
fn patchid_fallback_does_not_misattribute_after_content_change() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "ai line\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "original"]);
    let old = head(repo.path());

    seed_session(repo.path(), "write the greeting", &old);

    // Content-CHANGING amend → different patch-id → fallback must NOT match.
    std::fs::write(repo.path().join("f.txt"), "ai line\nextra\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "--amend", "-qm", "changed"]);
    let new = head(repo.path());
    assert_ne!(
        patch_id(repo.path(), &old),
        patch_id(repo.path(), &new),
        "content-changing amend must produce different patch-id"
    );

    let out = AssertCommand::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["blame", "f.txt"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("write the greeting"),
        "a content change must NOT be attributed to the old session:\n{stdout}"
    );
}
