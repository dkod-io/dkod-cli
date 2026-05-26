//! Tests for `capture::route::route_session`.

use dkod_core::capture::route::{route_session, RouteConfig, RouteDestination};
use std::path::PathBuf;
use tempfile::TempDir;

const SENTINEL: &str = ".dkod/no-auto-init";

fn config<'a>(user_scope: bool, vault: &'a std::path::Path) -> RouteConfig<'a> {
    RouteConfig {
        user_scope,
        vault_path: vault,
        auto_init_disabled_sentinel: SENTINEL,
    }
}

fn mkrepo(root: &std::path::Path) {
    std::fs::create_dir_all(root.join(".git")).unwrap();
}

fn mkdkod(root: &std::path::Path) {
    let dir = root.join(".dkod");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), "").unwrap();
}

#[test]
fn no_repo_routes_to_vault() {
    let cwd = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    let cfg = config(true, vault.path());
    assert_eq!(
        route_session(cwd.path(), &cfg),
        RouteDestination::Vault(PathBuf::from(vault.path()))
    );
}

#[test]
fn dkod_initialized_repo_routes_directly_to_repo() {
    let repo = TempDir::new().unwrap();
    mkrepo(repo.path());
    mkdkod(repo.path());
    let vault = TempDir::new().unwrap();
    let cfg = config(true, vault.path());
    assert_eq!(
        route_session(repo.path(), &cfg),
        RouteDestination::Repo(PathBuf::from(repo.path()))
    );
}

#[test]
fn user_scope_in_uninited_repo_routes_to_auto_init() {
    let repo = TempDir::new().unwrap();
    mkrepo(repo.path());
    let vault = TempDir::new().unwrap();
    let cfg = config(true, vault.path());
    assert_eq!(
        route_session(repo.path(), &cfg),
        RouteDestination::AutoInitThenRepo(PathBuf::from(repo.path()))
    );
}

#[test]
fn per_repo_scope_in_uninited_repo_routes_to_vault() {
    let repo = TempDir::new().unwrap();
    mkrepo(repo.path());
    let vault = TempDir::new().unwrap();
    // user_scope = false → no auto-init; orphans go to vault.
    let cfg = config(false, vault.path());
    assert_eq!(
        route_session(repo.path(), &cfg),
        RouteDestination::Vault(PathBuf::from(vault.path()))
    );
}

#[test]
fn per_repo_opt_out_blocks_auto_init_even_under_user_scope() {
    let repo = TempDir::new().unwrap();
    mkrepo(repo.path());
    // Drop the opt-out sentinel: even with user_scope, this repo is off-limits.
    let dkod = repo.path().join(".dkod");
    std::fs::create_dir_all(&dkod).unwrap();
    std::fs::write(repo.path().join(SENTINEL), "").unwrap();

    let vault = TempDir::new().unwrap();
    let cfg = config(true, vault.path());
    assert_eq!(
        route_session(repo.path(), &cfg),
        RouteDestination::Vault(PathBuf::from(vault.path()))
    );
}

#[test]
fn walks_up_from_subdir_to_repo_root() {
    let repo = TempDir::new().unwrap();
    mkrepo(repo.path());
    mkdkod(repo.path());
    let nested = repo.path().join("a/b/c");
    std::fs::create_dir_all(&nested).unwrap();
    let vault = TempDir::new().unwrap();
    let cfg = config(true, vault.path());
    assert_eq!(
        route_session(&nested, &cfg),
        RouteDestination::Repo(PathBuf::from(repo.path()))
    );
}
