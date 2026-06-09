//! End-to-end: `dkod drift` flags a session that touched a sensitive path from
//! a small-sounding prompt, hides a clean session by default, and shows it
//! under --all.

use assert_cmd::Command as AssertCommand;
use std::path::Path;
use std::process::Command;
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

fn seed(repo: &Path, prompt: &str, files: &[&str], commits: &[String]) -> String {
    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: prompt.into(),
        messages: vec![dkod_core::Message::user(prompt)],
        commits: commits.to_vec(),
        files_touched: files.iter().map(|s| s.to_string()).collect(),
    };
    dkod_core::store::write_session(repo, &s).unwrap();
    s.id
}

#[test]
fn drift_flags_sensitive_path_and_hides_clean() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    // Real commit touching a sensitive path (gives the drifting session a commit).
    std::fs::create_dir_all(repo.path().join(".github/workflows")).unwrap();
    std::fs::write(repo.path().join(".github/workflows/ci.yml"), "on: push\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "ci"]);
    let ci_sha = head(repo.path());

    // Drifting: "fix typo" prompt, touched the workflow file.
    let drift_id = seed(
        repo.path(),
        "fix the typo in the readme",
        &[".github/workflows/ci.yml"],
        &[ci_sha],
    );
    // Clean: small ask, but the change is small (2 files < large_change_files=5,
    // no commits → no line magnitude), touches no sensitive path, and names no
    // path-shaped tokens — so no rule fires.
    let clean_id = seed(
        repo.path(),
        "refactor the parser module and update its tests across the codebase",
        &["src/parser.rs", "src/parser_tests.rs"],
        &[],
    );

    // Default: drifting shown with the sensitive-path reason; clean hidden.
    let out = AssertCommand::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .arg("drift")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains(&drift_id),
        "drifting session should be listed:\n{stdout}"
    );
    assert!(
        stdout.contains("sensitive path") && stdout.contains("ci.yml"),
        "expected the sensitive-path reason:\n{stdout}"
    );
    assert!(
        !stdout.contains(&clean_id),
        "clean session must be hidden by default:\n{stdout}"
    );

    // Single clean session → "clean".
    let out = AssertCommand::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["drift", &clean_id])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("clean"),
        "single clean session should report clean:\n{stdout}"
    );

    // --all shows both.
    let out = AssertCommand::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["drift", "--all"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains(&drift_id) && stdout.contains(&clean_id),
        "--all should show both sessions:\n{stdout}"
    );
}
