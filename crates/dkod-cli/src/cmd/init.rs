use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::Command;

/// Fetch refspec that pulls every `refs/dkod/*` ref from a remote into
/// the local copy with the same name. Mirrored exactly: a `git fetch`
/// against a remote that has new session refs picks them up
/// automatically once this is wired into `.git/config`.
const DKOD_FETCH_REFSPEC: &str = "+refs/dkod/*:refs/dkod/*";

pub fn run(cwd: &Path) -> Result<()> {
    // 1. Ensure we're inside (or under) a git repo. `gix::discover`
    //    walks up from `cwd` so `dkod init` works whether the user
    //    is at the repo root or in any subdirectory — matching how
    //    `git` itself behaves and what `resolve_repo_root` does
    //    further down the call stack.
    gix::discover(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    // 2. If a config already exists, validate it (via load_config); otherwise
    //    write a default and validate that.
    let dir = cwd.join(".dkod");
    std::fs::create_dir_all(&dir).context("create .dkod/")?;
    let path = dir.join("config.toml");

    if !path.exists() {
        let cfg = dkod_core::config::Config::default();
        let body = toml::to_string_pretty(&cfg).context("serialize default config")?;
        std::fs::write(&path, body).context("write .dkod/config.toml")?;
    }

    // load_config performs the custom-regex validation step.
    let _cfg = super::load_config(cwd)?;

    // 3. Wire `refs/dkod/*` into each configured remote's fetch refspec
    //    so a vanilla `git fetch origin` pulls session refs alongside
    //    the usual heads. Idempotent: re-running `dkod init` after
    //    adding a new remote applies the refspec there too without
    //    duplicating it on remotes that already have it.
    ensure_dkod_refspec(cwd)?;

    // 4. Install Claude Code hooks at init time so the user gets
    //    capture wired up just by running `dkod init` (issue #6 phase
    //    1). The current V1 path still requires a separate
    //    `dkod capture claude-code` to start the long-lived server;
    //    phase 2 will lazy-spawn it from the hook so this becomes the
    //    only setup step. Either way, having the hook ENTRIES in
    //    place at init time is the prerequisite for both modes.
    match super::capture::claude_code::install_hooks_at_init(cwd)? {
        super::capture::claude_code::InitInstallOutcome::Installed => {
            // Stay quiet on the happy path — `dkod init` already
            // implies "set everything up" and a flood of "did X, did
            // Y" lines clutters the user's terminal.
        }
        super::capture::claude_code::InitInstallOutcome::SkippedDisabledGlobally => {
            eprintln!(
                "dkod init: skipping Claude Code hook install — \
                 ~/.claude/settings.json has disableAllHooks=true. \
                 Remove that flag (or scope it) to enable capture."
            );
        }
    }

    Ok(())
}

/// For every remote configured in `cwd`'s `.git/config`, append the
/// dkod fetch refspec if it's not already present. No-op when no
/// remotes are configured (the user is expected to re-run `dkod init`
/// after `git remote add`).
fn ensure_dkod_refspec(cwd: &Path) -> Result<()> {
    let remotes = list_remotes(cwd)?;
    for remote in remotes {
        if !remote_already_has_dkod_refspec(cwd, &remote)? {
            add_fetch_refspec(cwd, &remote)?;
        }
    }
    Ok(())
}

/// Enumerate remote names via `git remote`. Returns an empty Vec on a
/// repo with no remotes configured (the common case immediately after
/// `git init`); errors only on a true git invocation failure.
fn list_remotes(cwd: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("remote")
        .output()
        .context("invoke `git remote`")?;
    if !output.status.success() {
        return Err(anyhow!(
            "`git remote` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

/// Returns true iff the given remote already has the dkod fetch
/// refspec. We deliberately check exact-match — a refspec that's a
/// strict prefix or a different mapping doesn't satisfy the contract.
fn remote_already_has_dkod_refspec(cwd: &Path, remote: &str) -> Result<bool> {
    let key = format!("remote.{remote}.fetch");
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--get-all", &key])
        .output()
        .with_context(|| format!("invoke `git config --get-all {key}`"))?;

    // Exit code 1 from `git config --get-all` means "key not set". Any
    // other non-zero code is a real failure we want to surface.
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(anyhow!(
            "`git config --get-all {key}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.trim() == DKOD_FETCH_REFSPEC))
}

/// Append the dkod fetch refspec to the named remote. Caller MUST
/// have already checked that it isn't there — `git config --add`
/// would happily produce a duplicate line otherwise.
fn add_fetch_refspec(cwd: &Path, remote: &str) -> Result<()> {
    let key = format!("remote.{remote}.fetch");
    let status = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--add", &key, DKOD_FETCH_REFSPEC])
        .status()
        .with_context(|| format!("invoke `git config --add {key}`"))?;
    if !status.success() {
        return Err(anyhow!(
            "`git config --add {key} {DKOD_FETCH_REFSPEC}` exited with {:?}",
            status.code()
        ));
    }
    Ok(())
}
