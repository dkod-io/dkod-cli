//! `dkod init` discovers existing session refs on origin (the clone-side half
//! of the in-repo viral loop): counts `refs/dkod/sessions/*` on the remote,
//! fetches them, and drops a `.dkod.toml` breadcrumb at the repo root so
//! teammates browsing the repo learn dkod is in use.

use assert_cmd::Command;
use std::path::Path;
use tempfile::TempDir;

const BREADCRUMB_MARKER: &str = "This repository uses dkod";
const DISCOVERY_MARKER: &str = "captured agent session";

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

/// Run `dkod init` in `repo`, asserting success, and return captured stdout.
fn dkod_init(repo: &Path) -> String {
    let assert = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo)
        .arg("init")
        .assert()
        .success();
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

/// `git show-ref` output for `repo` (empty string when there are no refs).
fn show_ref(repo: &Path) -> String {
    let out = std::process::Command::new("git")
        .args(["show-ref"])
        .current_dir(repo)
        .output()
        .expect("git show-ref");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Seed a bare "origin" with one commit on main plus one dkod session ref,
/// then return (scratch dir, path of a fresh clone of that bare repo).
fn seeded_clone() -> (TempDir, std::path::PathBuf, String) {
    let scratch = TempDir::new().unwrap();
    let bare = scratch.path().join("origin.git");
    let source = scratch.path().join("source");
    let clone = scratch.path().join("clone");

    git(
        scratch.path(),
        &["init", "--bare", "-q", bare.to_str().unwrap()],
    );

    std::fs::create_dir_all(&source).unwrap();
    git(&source, &["init", "-q"]);
    std::fs::write(source.join("README.md"), "# seeded\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-qm", "initial"]);
    git(&source, &["branch", "-M", "main"]);
    git(
        &source,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );

    // Write a session ref via dkod_core and push it (plus main) to the bare.
    let session = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: "seed the discovery test".into(),
        messages: vec![],
        commits: vec![],
        files_touched: vec![],
    };
    dkod_core::store::write_session(&source, &session).unwrap();
    let sid = session.id.clone();
    git(&source, &["push", "-q", "origin", "main"]);
    git(
        &source,
        &["push", "-q", "origin", "+refs/dkod/*:refs/dkod/*"],
    );

    git(
        scratch.path(),
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    (scratch, clone, sid)
}

#[test]
fn init_discovers_and_fetches_sessions_from_origin() {
    let (_scratch, clone, sid) = seeded_clone();

    // Default clone must NOT have brought the dkod refs down.
    assert!(
        !show_ref(&clone).contains("refs/dkod/sessions/"),
        "precondition: clone should not have dkod refs before init"
    );

    let stdout = dkod_init(&clone);

    // Discovery line mentions the session count and that we're fetching.
    assert!(
        stdout.contains("1 captured agent session(s) on origin"),
        "stdout missing discovery line:\n{stdout}"
    );

    // The fetch actually happened: the session ref is now local.
    let refs = show_ref(&clone);
    assert!(
        refs.contains(&format!("refs/dkod/sessions/{sid}")),
        "local clone missing fetched session ref:\n{refs}"
    );

    // Breadcrumb written and announced.
    let crumb = clone.join(".dkod.toml");
    assert!(crumb.exists(), ".dkod.toml breadcrumb should be written");
    let body = std::fs::read_to_string(&crumb).unwrap();
    assert!(
        body.contains(BREADCRUMB_MARKER),
        "breadcrumb content wrong:\n{body}"
    );
    assert!(
        stdout.contains("wrote .dkod.toml breadcrumb"),
        "stdout missing breadcrumb announcement:\n{stdout}"
    );
}

#[test]
fn init_stays_silent_when_origin_has_no_sessions() {
    let scratch = TempDir::new().unwrap();
    let bare = scratch.path().join("origin.git");
    let repo = scratch.path().join("repo");

    git(
        scratch.path(),
        &["init", "--bare", "-q", bare.to_str().unwrap()],
    );
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);

    let stdout = dkod_init(&repo);

    assert!(
        !stdout.contains(DISCOVERY_MARKER),
        "no discovery line expected with zero dkod refs:\n{stdout}"
    );
    // Breadcrumb still written.
    let crumb = repo.join(".dkod.toml");
    assert!(crumb.exists(), ".dkod.toml breadcrumb should be written");
    let body = std::fs::read_to_string(&crumb).unwrap();
    assert!(
        body.contains(BREADCRUMB_MARKER),
        "breadcrumb content wrong:\n{body}"
    );
}

#[test]
fn init_succeeds_with_no_remote_and_writes_breadcrumb() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);

    let stdout = dkod_init(repo.path());

    assert!(
        !stdout.contains(DISCOVERY_MARKER),
        "no discovery line expected without a remote:\n{stdout}"
    );
    let crumb = repo.path().join(".dkod.toml");
    assert!(crumb.exists(), ".dkod.toml breadcrumb should be written");
    let body = std::fs::read_to_string(&crumb).unwrap();
    assert!(
        body.contains(BREADCRUMB_MARKER),
        "breadcrumb content wrong:\n{body}"
    );
}

#[test]
fn rerun_is_idempotent_and_never_clobbers_existing_dkod_toml() {
    let scratch = TempDir::new().unwrap();
    let bare = scratch.path().join("origin.git");
    let repo = scratch.path().join("repo");

    git(
        scratch.path(),
        &["init", "--bare", "-q", bare.to_str().unwrap()],
    );
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);

    // Pre-existing .dkod.toml with custom content must be left untouched.
    let custom = "# my custom dkod settings\ncustom = true\n";
    std::fs::write(repo.join(".dkod.toml"), custom).unwrap();

    let stdout1 = dkod_init(&repo);
    let stdout2 = dkod_init(&repo);

    assert_eq!(
        std::fs::read_to_string(repo.join(".dkod.toml")).unwrap(),
        custom,
        "existing .dkod.toml must never be clobbered"
    );
    for (i, out) in [&stdout1, &stdout2].iter().enumerate() {
        assert!(
            !out.contains("wrote .dkod.toml breadcrumb"),
            "run {} must not announce a breadcrumb it didn't write:\n{out}",
            i + 1
        );
    }

    // Refspec not duplicated across re-runs (existing init contract).
    let out = std::process::Command::new("git")
        .args(["config", "--get-all", "remote.origin.fetch"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let fetch_lines = String::from_utf8_lossy(&out.stdout);
    let n = fetch_lines
        .lines()
        .filter(|l| l.trim() == "+refs/dkod/*:refs/dkod/*")
        .count();
    assert_eq!(n, 1, "dkod refspec duplicated:\n{fetch_lines}");
}
