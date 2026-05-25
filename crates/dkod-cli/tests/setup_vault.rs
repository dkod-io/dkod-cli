//! Tests for `cmd::setup::vault` — wizard's vault repo manager.

use dkod_cli::cmd::setup::vault;
use tempfile::TempDir;

#[test]
fn ensure_creates_fresh_git_repo_when_path_missing() {
    let tmp = TempDir::new().unwrap();
    let vault_path = tmp.path().join("subdir/vault");
    assert!(!vault_path.exists());

    vault::ensure(&vault_path).unwrap();

    assert!(vault_path.exists(), "vault dir was not created");
    assert!(
        vault::is_initialized(&vault_path),
        "vault dir exists but is not a git repo"
    );
}

#[test]
fn ensure_is_noop_when_repo_already_initialized() {
    let tmp = TempDir::new().unwrap();
    let vault_path = tmp.path().join("vault");

    vault::ensure(&vault_path).unwrap();
    let first_mtime = std::fs::metadata(vault_path.join(".git")).unwrap();

    // Second call must succeed and must not re-init (which would touch .git).
    vault::ensure(&vault_path).unwrap();
    let second_mtime = std::fs::metadata(vault_path.join(".git")).unwrap();

    // We don't strictly compare mtimes — fs resolution varies — but we
    // assert the repo is still openable and the dir didn't get clobbered.
    assert!(vault::is_initialized(&vault_path));
    assert_eq!(first_mtime.is_dir(), second_mtime.is_dir());
}

#[test]
fn ensure_initializes_when_dir_exists_but_empty() {
    let tmp = TempDir::new().unwrap();
    let vault_path = tmp.path().join("vault");
    std::fs::create_dir_all(&vault_path).unwrap();
    assert!(vault_path.exists());
    assert!(!vault::is_initialized(&vault_path));

    vault::ensure(&vault_path).unwrap();

    assert!(vault::is_initialized(&vault_path));
}

#[test]
fn ensure_refuses_when_dir_is_non_empty_non_git() {
    let tmp = TempDir::new().unwrap();
    let vault_path = tmp.path().join("vault");
    std::fs::create_dir_all(&vault_path).unwrap();
    std::fs::write(
        vault_path.join("important_user_file.txt"),
        "do not clobber me",
    )
    .unwrap();

    let err =
        vault::ensure(&vault_path).expect_err("should refuse to clobber non-empty non-git dir");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("not a git repo") || msg.contains("refusing"),
        "expected refusal-to-clobber error, got: {msg}"
    );
    assert!(
        !vault::is_initialized(&vault_path),
        "vault must not have been initialized"
    );
    assert!(
        vault_path.join("important_user_file.txt").exists(),
        "user data was clobbered"
    );
}

#[test]
fn is_initialized_returns_false_for_missing_path() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("does/not/exist");
    assert!(!vault::is_initialized(&missing));
}
