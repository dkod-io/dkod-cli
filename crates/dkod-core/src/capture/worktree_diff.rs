//! Snapshot the working tree and diff two snapshots.
//!
//! Used as a fallback signal for `files_touched`: some agents edit files
//! through plain shell commands (e.g. `bash -c 'echo hi > x.txt'`, `mv`,
//! `sed -i`) that the transcript adapter cannot see. By snapshotting before
//! the agent runs and again after it exits, we can union the symmetric
//! difference into whatever the transcript-derived signal already produced.
//!
//! The snapshot honors `.gitignore`. We use `gix` (no `git2`, no shell-out
//! to `git`) to walk the working tree, classify each entry as
//! tracked/untracked/ignored, and hash file contents with the same SHA-1
//! function git uses for blob OIDs. That keeps the diff identity-aligned
//! with what `git diff` would produce.
//!
//! Performance note: large files are unlikely to be agent-touched and
//! re-hashing them on every capture is expensive — files larger than
//! [`MAX_FILE_BYTES`] are deliberately skipped from the snapshot. They will
//! never appear in `files_touched` via this fallback. The transcript adapter
//! still picks them up if the agent edited them via `apply_patch`.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Files larger than this are skipped from the snapshot (heuristic — large
/// files are rarely edited by agents and re-hashing them on every capture
/// is expensive).
pub(crate) const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// 20-byte git SHA-1 blob hash.
pub(crate) type BlobHash = [u8; 20];

/// Walk the working tree at `repo_path`, honoring `.gitignore`, and return
/// a map of repo-relative forward-slash UTF-8 path → git blob SHA-1.
///
/// Behavior:
///
/// - Ignored files (per `.gitignore`) are excluded.
/// - Files larger than [`MAX_FILE_BYTES`] are skipped.
/// - Symlinks and non-regular files are skipped.
/// - Paths whose UTF-8 encoding is invalid are skipped (rare).
/// - Hashes are computed with `gix_object::compute_hash` so they match
///   the blob OIDs git would produce for the same content.
pub(crate) fn snapshot(repo_path: &Path) -> Result<BTreeMap<PathBuf, BlobHash>> {
    use gix::bstr::ByteSlice;

    let repo =
        gix::open(repo_path).with_context(|| format!("open repo at {}", repo_path.display()))?;
    let work_dir = repo
        .work_dir()
        .ok_or_else(|| anyhow::anyhow!("repo {} has no working directory", repo_path.display()))?
        .to_path_buf();

    let index = repo.index_or_empty().context("open or synthesize index")?;

    // Configure the dirwalk to emit BOTH tracked and untracked files. We
    // leave `emit_ignored = None` so `.gitignore`d entries are filtered out
    // entirely, and we don't ask for collapsed/pruned entries — we want one
    // entry per actual file path.
    let opts = repo
        .dirwalk_options()
        .context("derive dirwalk options")?
        .emit_tracked(true)
        .emit_untracked(gix::dir::walk::EmissionMode::Matching);

    let iter = repo
        .dirwalk_iter(index, std::iter::empty::<&str>(), Default::default(), opts)
        .context("start dirwalk iter")?;

    let object_hash = repo.object_hash();
    let mut out: BTreeMap<PathBuf, BlobHash> = BTreeMap::new();

    for item in iter {
        let item = match item {
            Ok(it) => it,
            Err(e) => {
                eprintln!("dkod: worktree-diff: dirwalk error: {e}");
                continue;
            }
        };

        // Only files that actually exist on disk and are tracked or
        // untracked (NOT ignored, NOT pruned) are interesting.
        match item.entry.status {
            gix::dir::entry::Status::Tracked | gix::dir::entry::Status::Untracked => {}
            gix::dir::entry::Status::Ignored(_) | gix::dir::entry::Status::Pruned => continue,
        }
        match item.entry.disk_kind {
            // Regular files are the only thing we hash. Symlinks, dirs,
            // submodule repos, and unknown types are skipped — they would
            // either confuse the diff (symlink target changes) or aren't
            // representable as a single blob (directories, submodules).
            Some(gix::dir::entry::Kind::File) => {}
            _ => continue,
        }

        let rela = match item.entry.rela_path.to_str() {
            Ok(s) => s,
            Err(_) => continue, // non-UTF-8 path — skip per spec
        };
        let abs = work_dir.join(rela);

        // Stat first so we can cheaply skip large files.
        let meta = match std::fs::metadata(&abs) {
            Ok(m) => m,
            Err(_) => continue, // raced with a delete, or unreadable — skip
        };
        if !meta.is_file() {
            continue;
        }
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }

        // TOCTOU-safe read: the file may have grown between the stat and
        // the read, so cap the read at MAX_FILE_BYTES + 1 and reject
        // anything that fills the cap. We only ever hash content we know
        // is at or below the limit.
        let bytes = {
            use std::io::Read;
            let f = match std::fs::File::open(&abs) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("dkod: worktree-diff: open {} failed: {e}", abs.display());
                    continue;
                }
            };
            let mut buf = Vec::with_capacity(meta.len() as usize);
            // `take` reads at most N bytes; we ask for one extra so we can
            // detect a file that grew past the cap after the stat above.
            if let Err(e) = f.take(MAX_FILE_BYTES + 1).read_to_end(&mut buf) {
                eprintln!("dkod: worktree-diff: read {} failed: {e}", abs.display());
                continue;
            }
            if buf.len() as u64 > MAX_FILE_BYTES {
                continue;
            }
            buf
        };
        let oid = gix::objs::compute_hash(object_hash, gix::objs::Kind::Blob, &bytes);
        let mut hash = [0u8; 20];
        let slice = oid.as_slice();
        if slice.len() != 20 {
            // SHA-256 repos: not supported in V1 since BlobHash is fixed at
            // 20 bytes. Skip rather than truncate.
            continue;
        }
        hash.copy_from_slice(slice);
        out.insert(PathBuf::from(rela), hash);
    }

    Ok(out)
}

/// Symmetric difference of two snapshots: every path that is present in
/// only one map, or present in both with a different hash.
///
/// The result is a sorted, deduplicated list of forward-slash UTF-8 paths
/// (the same form the snapshot stored).
pub(crate) fn symmetric_diff(
    before: &BTreeMap<PathBuf, BlobHash>,
    after: &BTreeMap<PathBuf, BlobHash>,
) -> Vec<String> {
    let mut changed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for (path, hash) in before {
        match after.get(path) {
            Some(after_hash) if after_hash == hash => {} // unchanged
            _ => {
                if let Some(s) = path.to_str() {
                    changed.insert(s.to_string());
                }
            }
        }
    }
    for path in after.keys() {
        if !before.contains_key(path) {
            if let Some(s) = path.to_str() {
                changed.insert(s.to_string());
            }
        }
    }

    changed.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_repo(p: &std::path::Path) {
        gix::init(p).unwrap();
    }

    #[test]
    fn snapshot_then_diff_detects_added_file() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let before = snapshot(tmp.path()).unwrap();

        std::fs::write(tmp.path().join("hello.txt"), "hi").unwrap();
        let after = snapshot(tmp.path()).unwrap();

        let diff = symmetric_diff(&before, &after);
        assert!(diff.iter().any(|p| p == "hello.txt"));
    }

    #[test]
    fn snapshot_then_diff_detects_modified_file() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "one").unwrap();
        let before = snapshot(tmp.path()).unwrap();

        std::fs::write(tmp.path().join("a.txt"), "two").unwrap();
        let after = snapshot(tmp.path()).unwrap();

        let diff = symmetric_diff(&before, &after);
        assert!(diff.iter().any(|p| p == "a.txt"));
    }

    #[test]
    fn snapshot_then_diff_detects_deleted_file() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "one").unwrap();
        let before = snapshot(tmp.path()).unwrap();

        std::fs::remove_file(tmp.path().join("a.txt")).unwrap();
        let after = snapshot(tmp.path()).unwrap();

        let diff = symmetric_diff(&before, &after);
        assert!(diff.iter().any(|p| p == "a.txt"));
    }

    #[test]
    fn snapshot_skips_gitignored_paths() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(tmp.path().join("ignored.txt"), "x").unwrap();

        let snap = snapshot(tmp.path()).unwrap();
        // .gitignore itself is fine to include; ignored.txt must not appear.
        assert!(!snap.contains_key(std::path::Path::new("ignored.txt")));
    }

    #[test]
    fn snapshot_skips_files_over_max_size() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        // Just over the 1 MiB limit.
        let big = vec![b'a'; (MAX_FILE_BYTES + 1) as usize];
        std::fs::write(tmp.path().join("big.bin"), &big).unwrap();
        std::fs::write(tmp.path().join("small.txt"), "ok").unwrap();

        let snap = snapshot(tmp.path()).unwrap();
        assert!(snap.contains_key(std::path::Path::new("small.txt")));
        assert!(!snap.contains_key(std::path::Path::new("big.bin")));
    }

    #[test]
    fn symmetric_diff_dedupes_and_sorts() {
        let mut before: BTreeMap<PathBuf, BlobHash> = BTreeMap::new();
        let mut after: BTreeMap<PathBuf, BlobHash> = BTreeMap::new();

        before.insert(PathBuf::from("z.txt"), [1u8; 20]);
        after.insert(PathBuf::from("z.txt"), [2u8; 20]); // modified
        after.insert(PathBuf::from("a.txt"), [3u8; 20]); // added

        let diff = symmetric_diff(&before, &after);
        assert_eq!(diff, vec!["a.txt".to_string(), "z.txt".to_string()]);
    }
}
