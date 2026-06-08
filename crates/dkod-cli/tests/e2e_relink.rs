//! End-to-end: after a history rewrite, `dkod relink` (fed the old→new pair
//! git's post-rewrite hook would emit) restores `dkod blame` provenance on the
//! rewritten commit.

use assert_cmd::Command;
use std::path::Path;
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
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
        std::process::Command::new("git")
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

#[test]
fn relink_restores_blame_after_amend() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "ai line\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "original"]);
    let old = head(repo.path());

    // Seed a session linked to the original commit.
    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: "write the greeting".into(),
        messages: vec![],
        commits: vec![old.clone()],
        files_touched: vec!["f.txt".into()],
    };
    dkod_core::store::write_session(repo.path(), &s).unwrap();
    dkod_core::store::link_session_to_commit(repo.path(), &s.id, &old).unwrap();

    // Rewrite history: amend changes the commit SHA.
    git(repo.path(), &["commit", "--amend", "-qm", "reworded"]);
    let new = head(repo.path());
    assert_ne!(old, new, "amend must change the sha");

    // Before relink, the rewritten commit has no session link yet, so its line
    // is not attributed to the session.
    let before = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["blame", "f.txt"])
        .assert()
        .success();
    let before_out = String::from_utf8(before.get_output().stdout.clone()).unwrap();
    assert!(
        !before_out.contains("write the greeting"),
        "line should not be attributed before relink:\n{before_out}"
    );

    // Feed the post-rewrite pair to `dkod relink` via stdin (what the hook does).
    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .arg("relink")
        .write_stdin(format!("{old} {new}\n"))
        .assert()
        .success();

    // After relink, blame attributes the rewritten line to the session.
    let after = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["blame", "f.txt"])
        .assert()
        .success();
    let after_out = String::from_utf8(after.get_output().stdout.clone()).unwrap();
    assert!(
        after_out.contains("claude_code"),
        "expected agent label after relink:\n{after_out}"
    );
    assert!(
        after_out.contains("write the greeting"),
        "expected prompt summary after relink:\n{after_out}"
    );
}
