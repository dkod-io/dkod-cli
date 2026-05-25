//! Capture-time routing: decide where a session blob should land.
//!
//! The seamless capture wizard fires a hook on every agent session. That
//! hook's job is to invoke `dkod capture-hook --agent <name>`, which
//! consults this module to pick a destination:
//!
//! * The current repo's `refs/dkod/sessions/*` if the repo is already
//!   `dkod init`'d.
//! * A silent auto-init of the current repo (when the user opted into
//!   user-scope install and didn't explicitly opt out per-repo).
//! * The personal vault at `~/.dkod/vault` when there's no git repo at
//!   all.
//!
//! No I/O beyond `Path::exists` and walking parent directories — the
//! caller performs the actual write through `store::write_session` (or
//! the per-repo init for the auto-init case).

use std::path::{Path, PathBuf};

/// Inputs to `route_session`, kept narrow so the function is pure with
/// respect to its arguments and easy to test against synthetic dir trees.
pub struct RouteConfig<'a> {
    /// `true` if the wizard ran with `--scope user`. The auto-init branch
    /// only fires in user-scope mode (per-repo mode means the user has
    /// already explicitly opted into each repo individually).
    pub user_scope: bool,
    /// Where orphan sessions land when no git repo is found.
    pub vault_path: &'a Path,
    /// Per-repo override: a path users can drop at `<repo>/.dkod/no-auto-init`
    /// to keep the auto-init from firing in that repo even under
    /// user-scope. Implemented as a sentinel path so it doesn't require
    /// parsing config we may not own.
    pub auto_init_disabled_sentinel: &'a str,
}

/// What `route_session` returns to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDestination {
    /// Repo is already initialized for dkod; write directly to it.
    Repo(PathBuf),
    /// Repo found but not dkod-init'd; caller must run `dkod init` here
    /// before writing. Only returned in user-scope mode unless the
    /// per-repo opt-out sentinel is present.
    AutoInitThenRepo(PathBuf),
    /// No git repo found (or auto-init disabled and not yet init'd);
    /// write to the vault instead.
    Vault(PathBuf),
}

/// Decide where the session from `cwd` should be written.
///
/// 1. Walk up from `cwd` looking for `.git`. If none found → `Vault`.
/// 2. If the repo has `.dkod/config.toml` → `Repo(repo_root)`.
/// 3. Else, if `user_scope` and the per-repo opt-out sentinel is absent →
///    `AutoInitThenRepo(repo_root)`.
/// 4. Else → `Vault`.
pub fn route_session(cwd: &Path, config: &RouteConfig) -> RouteDestination {
    match find_repo_root(cwd) {
        None => RouteDestination::Vault(config.vault_path.to_path_buf()),
        Some(repo_root) => {
            let dkod_config = repo_root.join(".dkod/config.toml");
            if dkod_config.exists() {
                return RouteDestination::Repo(repo_root);
            }
            let opt_out = repo_root.join(config.auto_init_disabled_sentinel);
            if config.user_scope && !opt_out.exists() {
                RouteDestination::AutoInitThenRepo(repo_root)
            } else {
                RouteDestination::Vault(config.vault_path.to_path_buf())
            }
        }
    }
}

/// Walk up from `start` looking for a directory containing `.git`. Stops
/// at the filesystem root. Returns the directory itself, not its `.git`.
fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };
    loop {
        if cur.join(".git").exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}
