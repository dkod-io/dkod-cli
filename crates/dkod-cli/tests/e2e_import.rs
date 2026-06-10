//! End-to-end tests for `dkod import` — the transcript-rescue importers.
//!
//! Claude Code's local transcripts expire after ~30 days; `dkod import`
//! parses them into Session blobs under `refs/dkod/sessions/<id>` so they
//! survive. These tests seed fake source trees and drive the real binary.
//!
//! Source-root override for tests: `DKOD_CLAUDE_DIR` points the Claude Code
//! importer at an alternative `~/.claude`-shaped root (it must contain a
//! `projects/` subdirectory). The Codex importer takes `--dir` directly.

use assert_cmd::Command;
use std::path::Path;
use std::process::Command as StdCommand;

fn init_git_repo(path: &Path) {
    StdCommand::new("git")
        .arg("init")
        .arg(path)
        .output()
        .unwrap();
}

/// Mirror of the importer's cwd -> Claude Code project-dir mapping:
/// every non-alphanumeric char becomes `-` (observed on-disk behavior of
/// Claude Code's `~/.claude/projects/` encoding).
fn claude_project_dir_name(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

const CLAUDE_SESSION_ID: &str = "11111111-2222-4333-8444-555555555555";

/// Minimal Claude Code transcript JSONL matching what
/// `dkod_core::capture::claude_code::parse_transcript` expects. Contains a
/// fake AWS key (AWS's own documented example placeholder) so redaction is
/// observable in the stored blob.
fn claude_transcript_jsonl() -> String {
    let aws = "AKIAIOSFODNN7EXAMPLE"; // gitleaks:allow
    format!(
        concat!(
            r#"{{"type":"user","timestamp":"2026-05-03T12:00:01.000Z","message":{{"content":"fix the login bug {aws}"}}}}"#,
            "\n",
            r#"{{"type":"assistant","timestamp":"2026-05-03T12:00:03.000Z","message":{{"content":[{{"type":"text","text":"done"}}]}}}}"#,
            "\n",
        ),
        aws = aws
    )
}

/// Seed `<claude_root>/projects/<mapped-repo-path>/<session-id>.jsonl`.
fn seed_claude_transcript(claude_root: &Path, repo: &Path, session_id: &str, body: &str) {
    // The importer canonicalizes the repo path before mapping (macOS tempdirs
    // are symlinks under /var -> /private/var), so the seed must too.
    let canonical = repo.canonicalize().unwrap();
    let project_dir = claude_root
        .join("projects")
        .join(claude_project_dir_name(&canonical));
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join(format!("{session_id}.jsonl")), body).unwrap();
}

#[test]
fn import_claude_code_rescues_transcripts_idempotently() {
    let repo = tempfile::TempDir::new().unwrap();
    init_git_repo(repo.path());
    let claude_root = tempfile::TempDir::new().unwrap();
    seed_claude_transcript(
        claude_root.path(),
        repo.path(),
        CLAUDE_SESSION_ID,
        &claude_transcript_jsonl(),
    );

    // First import: 1 in, 0 skipped. Works without `dkod init` — only a
    // plain git repo is required.
    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&repo)
        .env("DKOD_CLAUDE_DIR", claude_root.path())
        .args(["import", "claude-code"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("imported 1 session(s), skipped 0"),
        "unexpected import summary: {stdout}"
    );

    // The session ref exists under the transcript's own UUID and the blob
    // deserializes back into a Session.
    let session = dkod_core::store::read_session(repo.path(), CLAUDE_SESSION_ID)
        .expect("imported session readable by id");
    assert!(matches!(session.agent, dkod_core::Agent::ClaudeCode));
    assert!(
        session.prompt_summary.starts_with("fix the login bug"),
        "prompt_summary from first user message; got: {}",
        session.prompt_summary
    );
    assert_eq!(session.created_at, 1777809601, "created_at from entry ts");

    // Redaction was applied like a live capture: the fake AWS key must not
    // survive into the stored blob.
    let json = serde_json::to_string(&session).unwrap();
    assert!(
        !json.contains("AKIAIOSFODNN7EXAMPLE"),
        "AWS key leaked into stored session: {json}"
    );
    assert!(
        json.contains("[REDACTED"),
        "expected a [REDACTED marker in stored session: {json}"
    );

    // `dkod log` lists it.
    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&repo)
        .arg("log")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains(CLAUDE_SESSION_ID),
        "dkod log missing imported session: {stdout}"
    );

    // Re-import: idempotent — 0 imported, 1 skipped (already captured).
    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&repo)
        .env("DKOD_CLAUDE_DIR", claude_root.path())
        .args(["import", "claude-code"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("imported 0 session(s), skipped 1"),
        "re-import should skip everything: {stdout}"
    );
}

#[test]
fn import_claude_code_project_flag_overrides_source_dir() {
    let repo = tempfile::TempDir::new().unwrap();
    init_git_repo(repo.path());

    // Transcripts live in an arbitrary directory, not the mapped one.
    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(
        src.path().join(format!("{CLAUDE_SESSION_ID}.jsonl")),
        claude_transcript_jsonl(),
    )
    .unwrap();

    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&repo)
        .args(["import", "claude-code", "--project"])
        .arg(src.path())
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("imported 1 session(s), skipped 0"),
        "unexpected import summary: {stdout}"
    );
    assert!(dkod_core::store::read_session(repo.path(), CLAUDE_SESSION_ID).is_ok());
}

#[test]
fn import_claude_code_warns_and_continues_on_unparseable_files() {
    let repo = tempfile::TempDir::new().unwrap();
    init_git_repo(repo.path());
    let src = tempfile::TempDir::new().unwrap();
    // One good transcript + one .jsonl whose name is not a session UUID.
    std::fs::write(
        src.path().join(format!("{CLAUDE_SESSION_ID}.jsonl")),
        claude_transcript_jsonl(),
    )
    .unwrap();
    std::fs::write(src.path().join("not-a-session.jsonl"), "{}\n").unwrap();

    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&repo)
        .args(["import", "claude-code", "--project"])
        .arg(src.path())
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("imported 1 session(s), skipped 1"),
        "bad file should be skipped, not fatal: {stdout}"
    );
}

#[test]
fn import_claude_code_with_missing_source_dir_imports_zero() {
    let repo = tempfile::TempDir::new().unwrap();
    init_git_repo(repo.path());
    let claude_root = tempfile::TempDir::new().unwrap(); // no projects/ subdir at all

    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&repo)
        .env("DKOD_CLAUDE_DIR", claude_root.path())
        .args(["import", "claude-code"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("imported 0 session(s), skipped 0"),
        "missing source dir should be a clean no-op: {stdout}"
    );
}

#[test]
fn import_outside_a_repo_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&tmp)
        .args(["import", "claude-code"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a git repo"));
}

#[test]
fn import_is_a_visible_subcommand() {
    let out = Command::cargo_bin("dkod")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("import"),
        "import should be visible in --help: {stdout}"
    );
}

// --- Codex ----------------------------------------------------------------

const CODEX_THREAD_ID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

/// Minimal Codex rollout JSONL matching what
/// `dkod_core::capture::codex::parse_rollout` expects. `cwd` scopes the
/// rollout to a project; the importer only rescues rollouts recorded inside
/// the current repo.
fn codex_rollout_jsonl(thread_id: &str, cwd: &Path) -> String {
    let aws = "AKIAIOSFODNN7EXAMPLE"; // gitleaks:allow
    format!(
        concat!(
            r#"{{"timestamp":"2026-05-03T12:00:00.000Z","type":"session_meta","payload":{{"id":"{id}","cwd":"{cwd}","originator":"codex_cli_rs","cli_version":"0.34.0"}}}}"#,
            "\n",
            r#"{{"timestamp":"2026-05-03T12:00:02.000Z","type":"event_msg","payload":{{"type":"user_message","message":"add hello.txt {aws}"}}}}"#,
            "\n",
            r#"{{"timestamp":"2026-05-03T12:00:05.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"created hello.txt"}}]}}}}"#,
            "\n",
        ),
        id = thread_id,
        cwd = cwd.display(),
        aws = aws
    )
}

/// Seed `<dir>/2026/05/03/rollout-2026-05-03T12-00-00-<thread_id>.jsonl`.
fn seed_codex_rollout(dir: &Path, thread_id: &str, body: &str) {
    let day = dir.join("2026/05/03");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(
        day.join(format!("rollout-2026-05-03T12-00-00-{thread_id}.jsonl")),
        body,
    )
    .unwrap();
}

#[test]
fn import_codex_rescues_rollouts_idempotently() {
    let repo = tempfile::TempDir::new().unwrap();
    init_git_repo(repo.path());
    let sessions = tempfile::TempDir::new().unwrap();
    let canonical_repo = repo.path().canonicalize().unwrap();
    seed_codex_rollout(
        sessions.path(),
        CODEX_THREAD_ID,
        &codex_rollout_jsonl(CODEX_THREAD_ID, &canonical_repo),
    );
    // A rollout from a DIFFERENT project must be ignored entirely.
    let other_id = "99999999-8888-4777-8666-555555555555";
    seed_codex_rollout(
        sessions.path(),
        other_id,
        &codex_rollout_jsonl(other_id, Path::new("/somewhere/else")),
    );

    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&repo)
        .args(["import", "codex", "--dir"])
        .arg(sessions.path())
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("imported 1 session(s), skipped 0"),
        "unexpected import summary: {stdout}"
    );

    let session = dkod_core::store::read_session(repo.path(), CODEX_THREAD_ID)
        .expect("imported codex session readable by thread id");
    assert!(matches!(session.agent, dkod_core::Agent::Codex));
    assert!(
        session.prompt_summary.starts_with("add hello.txt"),
        "prompt_summary from first user message; got: {}",
        session.prompt_summary
    );

    // Redaction applied.
    let json = serde_json::to_string(&session).unwrap();
    assert!(
        !json.contains("AKIAIOSFODNN7EXAMPLE"),
        "AWS key leaked into stored session: {json}"
    );

    // The other-project rollout was not imported under any id.
    assert!(dkod_core::store::read_session(repo.path(), other_id).is_err());

    // `dkod log` lists the imported one.
    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&repo)
        .arg("log")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains(CODEX_THREAD_ID), "dkod log: {stdout}");

    // Re-import: idempotent.
    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&repo)
        .args(["import", "codex", "--dir"])
        .arg(sessions.path())
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("imported 0 session(s), skipped 1"),
        "re-import should skip the already-captured rollout: {stdout}"
    );
}

// --- subdirectory invocation (worktree-root resolution) ---------------------

#[test]
fn import_claude_code_from_subdirectory_resolves_repo_root() {
    // `dkod import` run from a nested path must behave exactly like a run
    // from the repo root: the Claude project-dir mapping is computed from
    // the WORKTREE ROOT, not the caller's cwd.
    let repo = tempfile::TempDir::new().unwrap();
    init_git_repo(repo.path());
    let subdir = repo.path().join("src/deeply/nested");
    std::fs::create_dir_all(&subdir).unwrap();

    let claude_root = tempfile::TempDir::new().unwrap();
    // Seeded under the ROOT's mapped name — a cwd-based mapping would miss it.
    seed_claude_transcript(
        claude_root.path(),
        repo.path(),
        CLAUDE_SESSION_ID,
        &claude_transcript_jsonl(),
    );

    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&subdir)
        .env("DKOD_CLAUDE_DIR", claude_root.path())
        .args(["import", "claude-code"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("imported 1 session(s), skipped 0"),
        "subdir run should import like a root run: {stdout}"
    );
    assert!(
        dkod_core::store::read_session(repo.path(), CLAUDE_SESSION_ID).is_ok(),
        "session ref must land in the repo regardless of invocation cwd"
    );
}

#[test]
fn import_codex_from_subdirectory_keeps_repo_root_scope() {
    // The Codex cwd-inside-repo filter must compare against the WORKTREE
    // ROOT: a rollout recorded at the repo root is in scope even when the
    // import runs from a subdirectory.
    let repo = tempfile::TempDir::new().unwrap();
    init_git_repo(repo.path());
    let subdir = repo.path().join("src");
    std::fs::create_dir_all(&subdir).unwrap();

    let sessions = tempfile::TempDir::new().unwrap();
    let canonical_repo = repo.path().canonicalize().unwrap();
    seed_codex_rollout(
        sessions.path(),
        CODEX_THREAD_ID,
        &codex_rollout_jsonl(CODEX_THREAD_ID, &canonical_repo),
    );

    let out = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&subdir)
        .args(["import", "codex", "--dir"])
        .arg(sessions.path())
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("imported 1 session(s), skipped 0"),
        "root-recorded rollout must be in scope from a subdir run: {stdout}"
    );
    assert!(dkod_core::store::read_session(repo.path(), CODEX_THREAD_ID).is_ok());
}
