//! Integration tests for `cmd::capture::hook::_route_and_buffer_for_test`.
//!
//! These cover the route_session ↔ pending-buffer wiring without going
//! through stdin or `current_dir()` (both of which are awkward to drive
//! from a test). The public `route_and_buffer` is a thin wrapper around
//! the same code path; its contract (always returns Ok) is covered by the
//! shape of the function itself rather than a stdin-fed end-to-end test.

use dkod_cli::cmd::capture::hook::_route_and_buffer_for_test;
use dkod_core::capture::route::RouteDestination;
use std::path::PathBuf;
use tempfile::TempDir;

fn mkrepo(root: &std::path::Path) {
    std::fs::create_dir_all(root.join(".git")).unwrap();
}

fn mkdkod(root: &std::path::Path) {
    let dir = root.join(".dkod");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), "").unwrap();
}

fn list_pending(root: &std::path::Path) -> Vec<PathBuf> {
    let dir = root.join(".dkod").join("pending");
    if !dir.exists() {
        return Vec::new();
    }
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s == "json")
                .unwrap_or(false)
        })
        .collect();
    v.sort();
    v
}

#[test]
fn orphan_cwd_buffers_into_vault() {
    let cwd = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    let dest = _route_and_buffer_for_test(
        "claude-code",
        "SessionStart",
        cwd.path(),
        vault.path(),
        br#"{"session_id":"s1"}"#,
    )
    .unwrap();
    assert_eq!(dest, RouteDestination::Vault(vault.path().to_path_buf()));
    let pending = list_pending(vault.path());
    assert_eq!(pending.len(), 1, "expected one pending envelope");
    let body = std::fs::read_to_string(&pending[0]).unwrap();
    assert!(body.contains("\"agent\": \"claude-code\""));
    assert!(body.contains("\"event\": \"SessionStart\""));
    assert!(
        body.contains("\"payload_b64\""),
        "envelope must carry payload"
    );
}

#[test]
fn initialised_repo_buffers_into_repo() {
    let repo = TempDir::new().unwrap();
    mkrepo(repo.path());
    mkdkod(repo.path());
    let vault = TempDir::new().unwrap();

    let dest = _route_and_buffer_for_test(
        "codex",
        "session_start",
        repo.path(),
        vault.path(),
        br#"{"cwd":"ignored"}"#,
    )
    .unwrap();
    assert_eq!(dest, RouteDestination::Repo(repo.path().to_path_buf()));
    assert_eq!(list_pending(repo.path()).len(), 1);
    assert!(
        list_pending(vault.path()).is_empty(),
        "vault must not receive anything for an initialised repo"
    );
}

#[test]
fn uninited_repo_under_user_scope_routes_to_auto_init() {
    let repo = TempDir::new().unwrap();
    mkrepo(repo.path());
    let vault = TempDir::new().unwrap();

    let dest = _route_and_buffer_for_test(
        "claude-code",
        "SessionStart",
        repo.path(),
        vault.path(),
        b"{}",
    )
    .unwrap();
    assert_eq!(
        dest,
        RouteDestination::AutoInitThenRepo(repo.path().to_path_buf())
    );
    // The pending file lands in the repo's `.dkod/pending/`, ready to be
    // drained once the wizard auto-runs `dkod init` here.
    assert_eq!(list_pending(repo.path()).len(), 1);
}

#[test]
fn multiple_events_each_get_their_own_pending_file() {
    let cwd = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    for event in ["SessionStart", "PreToolUse", "Stop", "SessionEnd"] {
        _route_and_buffer_for_test(
            "claude-code",
            event,
            cwd.path(),
            vault.path(),
            format!(r#"{{"event":"{event}"}}"#).as_bytes(),
        )
        .unwrap();
    }
    assert_eq!(list_pending(vault.path()).len(), 4);
}

#[test]
fn invalid_agent_name_is_an_error_but_writes_nothing() {
    let cwd = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    let res = _route_and_buffer_for_test(
        "../etc/passwd",
        "SessionStart",
        cwd.path(),
        vault.path(),
        b"{}",
    );
    assert!(res.is_err());
    assert!(list_pending(vault.path()).is_empty());
}

#[test]
fn invalid_event_name_is_an_error_but_writes_nothing() {
    let cwd = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    let res =
        _route_and_buffer_for_test("claude-code", "with space", cwd.path(), vault.path(), b"{}");
    assert!(res.is_err());
    assert!(list_pending(vault.path()).is_empty());
}

#[test]
fn pending_filename_is_unique_under_rapid_burst() {
    // 100 hooks fired back-to-back must produce 100 distinct files.
    let cwd = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    for _ in 0..100 {
        _route_and_buffer_for_test("claude-code", "PreToolUse", cwd.path(), vault.path(), b"{}")
            .unwrap();
    }
    assert_eq!(list_pending(vault.path()).len(), 100);
}
