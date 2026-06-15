//! Integration test for the shared `finalize_session` helper that every
//! agent wrapper uses to redact, persist, and commit-link a session.

use dkod_cli::cmd::capture::finalize_session;
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

fn fixture_session(prompt: &str) -> dkod_core::Session {
    dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::Codex,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: prompt.into(),
        messages: vec![],
        commits: vec![],
        files_touched: vec![],
        redaction_count: 0,
    }
}

#[test]
fn finalize_links_commits_made_since_head_at_start() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "A"]);
    let a = head(repo.path());

    std::fs::write(repo.path().join("f.txt"), "a\nb\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "B"]);
    let b = head(repo.path());

    let cfg = dkod_core::config::Config::default();
    let mut session = fixture_session("do the thing");
    let linked = finalize_session(repo.path(), &mut session, Some(&a), &cfg).unwrap();

    assert_eq!(linked, vec![b.clone()]);
    assert_eq!(session.commits, vec![b.clone()]);

    let back = dkod_core::store::read_session(repo.path(), &session.id).unwrap();
    assert_eq!(back.commits, vec![b.clone()]);

    let r = gix::open(repo.path()).unwrap();
    let sref = r
        .find_reference(&dkod_core::refs::session_ref(&session.id))
        .unwrap();
    let cref = r.find_reference(&dkod_core::refs::commit_ref(&b)).unwrap();
    assert_eq!(sref.id(), cref.id());
}

#[test]
fn finalize_with_none_head_links_nothing_but_writes_session() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "A"]);

    let cfg = dkod_core::config::Config::default();
    let mut session = fixture_session("no head");
    let linked = finalize_session(repo.path(), &mut session, None, &cfg).unwrap();

    assert!(linked.is_empty());
    assert!(session.commits.is_empty());
    assert_eq!(
        dkod_core::store::read_session(repo.path(), &session.id)
            .unwrap()
            .id,
        session.id
    );
}

#[test]
fn finalize_writes_one_index_commit_covering_session_links_and_patchids() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "A"]);
    let a = head(repo.path());
    std::fs::write(repo.path().join("f.txt"), "a\nb\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "B"]);
    let b = head(repo.path());

    let cfg = dkod_core::config::Config::default();
    let mut session = fixture_session("one batched commit");
    finalize_session(repo.path(), &mut session, Some(&a), &cfg).unwrap();

    // exactly ONE commit on refs/dkod/index (root, no parent)
    let gx = gix::open(repo.path()).unwrap();
    let tip = gx
        .try_find_reference("refs/dkod/index")
        .unwrap()
        .expect("index ref")
        .id()
        .detach();
    let c = gx.find_object(tip).unwrap().try_into_commit().unwrap();
    assert_eq!(
        c.parent_ids().count(),
        0,
        "session + commit link + patchid link batched"
    );
    let msg = c.message_raw().unwrap().to_string();
    assert!(msg.contains("1 commit(s)"), "message: {msg}");

    // legacy patchid ref still exists (dual-write ON, finalize parity)
    let out = std::process::Command::new("git")
        .args(["for-each-ref", "refs/dkod/patchid/"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "legacy patchid link missing"
    );
    let _ = b;
}
