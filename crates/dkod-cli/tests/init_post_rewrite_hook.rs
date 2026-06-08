//! `dkod init` installs a sentinel-guarded post-rewrite hook, idempotently,
//! and never clobbers a foreign hook.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
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

fn hooks_dir(repo: &Path) -> PathBuf {
    let out = std::process::Command::new("git")
        .args([
            "-C",
            repo.to_str().unwrap(),
            "rev-parse",
            "--git-path",
            "hooks",
        ])
        .output()
        .unwrap();
    let rel = String::from_utf8(out.stdout).unwrap().trim().to_string();
    let p = PathBuf::from(&rel);
    if p.is_absolute() {
        p
    } else {
        repo.join(p)
    }
}

fn dkod_init(repo: &Path) {
    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo)
        .arg("init")
        .assert()
        .success();
}

#[test]
#[cfg(unix)]
fn init_writes_executable_post_rewrite_hook() {
    use std::os::unix::fs::PermissionsExt;
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    dkod_init(repo.path());

    let hook = hooks_dir(repo.path()).join("post-rewrite");
    assert!(hook.exists(), "hook should be written");
    let body = std::fs::read_to_string(&hook).unwrap();
    assert!(body.contains("dkod-managed"), "missing sentinel:\n{body}");
    assert!(
        body.contains("exec dkod relink"),
        "missing relink call:\n{body}"
    );
    let mode = std::fs::metadata(&hook).unwrap().permissions().mode();
    assert!(mode & 0o111 != 0, "hook must be executable, mode={mode:o}");
}

#[test]
#[cfg(unix)]
fn init_is_idempotent() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    dkod_init(repo.path());
    let hook = hooks_dir(repo.path()).join("post-rewrite");
    let first = std::fs::read_to_string(&hook).unwrap();
    dkod_init(repo.path());
    let second = std::fs::read_to_string(&hook).unwrap();
    assert_eq!(
        first, second,
        "re-running init must not change the managed hook"
    );
}

#[test]
#[cfg(unix)]
fn init_does_not_clobber_foreign_hook() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    let hook = hooks_dir(repo.path()).join("post-rewrite");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\necho i was here first\n").unwrap();
    dkod_init(repo.path());
    let body = std::fs::read_to_string(&hook).unwrap();
    assert_eq!(
        body, "#!/bin/sh\necho i was here first\n",
        "foreign hook must be left intact"
    );
}

#[test]
#[cfg(unix)]
fn init_skips_hook_when_hookspath_is_set() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    // Redirect hooks elsewhere; our .git/hooks/post-rewrite must NOT be written.
    let custom = repo.path().join("my-hooks");
    std::fs::create_dir_all(&custom).unwrap();
    git(
        repo.path(),
        &["config", "core.hooksPath", custom.to_str().unwrap()],
    );

    dkod_init(repo.path());

    // No post-rewrite under the default .git/hooks dir (we resolve via rev-parse,
    // but with hooksPath set the install must early-return without writing).
    let default_hook = repo.path().join(".git/hooks/post-rewrite");
    assert!(
        !default_hook.exists(),
        "must not write .git/hooks/post-rewrite when core.hooksPath is set"
    );
    // And nothing written into the custom dir either (we warn, don't install).
    assert!(
        !custom.join("post-rewrite").exists(),
        "must not install into the custom hooksPath dir"
    );
}
