//! finalize_session writes a patch-id provenance ref for each produced commit.

use dkod_cli::cmd::capture::finalize_session;
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

#[test]
fn finalize_writes_patchid_ref_for_produced_commit() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "base"]);
    let base = head(repo.path());
    std::fs::write(repo.path().join("f.txt"), "a\nb\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "produced"]);
    let produced = head(repo.path());

    let cfg = dkod_core::config::Config::default();
    let mut session = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: "do it".into(),
        messages: vec![],
        commits: vec![],
        files_touched: vec![],
    };
    let linked = finalize_session(repo.path(), &mut session, Some(&base), &cfg).unwrap();
    assert!(
        linked.contains(&produced),
        "expected the produced commit to be linked"
    );

    let pid = patch_id(repo.path(), &produced);
    let r = gix::open(repo.path()).unwrap();
    let pid_ref = r
        .find_reference(&dkod_core::refs::patchid_ref(&pid))
        .unwrap();
    let sess_ref = r
        .find_reference(&dkod_core::refs::session_ref(&session.id))
        .unwrap();
    assert_eq!(pid_ref.id(), sess_ref.id());
}
