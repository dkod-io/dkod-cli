use assert_cmd::Command;
use tempfile::TempDir;

fn git(repo: &std::path::Path, args: &[&str]) {
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

#[test]
fn blame_annotates_ai_lines_and_passes_through_others() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "ai line\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "ai commit"]);

    let sha = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: "add greeting".into(),
        messages: vec![],
        commits: vec![sha.clone()],
        files_touched: vec!["f.txt".into()],
    };
    dkod_core::store::write_session(repo.path(), &s).unwrap();
    dkod_core::store::link_session_to_commit(repo.path(), &s.id, &sha).unwrap();

    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["blame", "f.txt"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("claude_code"),
        "AI annotation missing:\n{stdout}"
    );
    assert!(
        stdout.contains("add greeting"),
        "prompt summary missing:\n{stdout}"
    );
    assert!(stdout.contains("ai line"), "source line missing:\n{stdout}");
}
