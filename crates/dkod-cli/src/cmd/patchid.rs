//! Compute a commit's stable `git patch-id`, used by the `dkod blame` patch-id
//! fallback. Kept in the CLI layer (not `dkod-core`) because it shells out to
//! git, like `init.rs` and `blame.rs` already do.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// True iff `s` is exactly 40 lowercase hex chars (a git SHA-1 / patch-id).
fn is_hex40(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The stable `git patch-id` of `sha`'s diff, or `None` when it can't be
/// computed or is empty (empty-diff commits, merges with no combined diff).
///
/// Runs `git -C <cwd> diff-tree -p --root <sha>` and pipes it into
/// `git patch-id --stable`. The `--root` flag is required so a root commit
/// (no parent) still produces a diff. Best-effort: any spawn/IO failure → None.
pub(crate) fn compute_patch_id(cwd: &Path, sha: &str) -> Option<String> {
    let diff = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["diff-tree", "-p", "--root", sha])
        .output()
        .ok()?;
    if !diff.status.success() || diff.stdout.is_empty() {
        return None;
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(&diff.stdout).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8(out.stdout).ok()?;
    let pid = stdout.split_whitespace().next()?;
    if is_hex40(pid) {
        Some(pid.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
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

    #[test]
    fn normal_commit_yields_40_hex() {
        let repo = TempDir::new().unwrap();
        git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("f.txt"), "a\nb\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "c"]);
        let pid = compute_patch_id(repo.path(), &head(repo.path())).expect("patch-id");
        assert_eq!(pid.len(), 40);
        assert!(pid
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    }
    #[test]
    fn root_commit_yields_a_patch_id() {
        let repo = TempDir::new().unwrap();
        git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("f.txt"), "hello\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "root"]);
        assert!(compute_patch_id(repo.path(), &head(repo.path())).is_some());
    }
    #[test]
    fn stable_across_reword() {
        let repo = TempDir::new().unwrap();
        git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("f.txt"), "a\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "orig"]);
        let before = compute_patch_id(repo.path(), &head(repo.path())).unwrap();
        git(repo.path(), &["commit", "--amend", "-qm", "reworded"]);
        let after = compute_patch_id(repo.path(), &head(repo.path())).unwrap();
        assert_eq!(before, after);
    }
    #[test]
    fn empty_commit_yields_none() {
        let repo = TempDir::new().unwrap();
        git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("f.txt"), "a\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "base"]);
        git(repo.path(), &["commit", "--allow-empty", "-qm", "empty"]);
        assert!(compute_patch_id(repo.path(), &head(repo.path())).is_none());
    }
}
