//! Storage v2 rollup index (`dkod-index/1`, design 2026-06-10): one
//! `refs/dkod/index` ref pointing at a commit chain whose tree holds every
//! session's metadata + body and the commit/patch-id lookup tables. This
//! module owns the tree layout, the tree codec, and the batched, CAS-guarded
//! write path. `store.rs` composes it with the legacy-ref dual-write and the
//! permanent legacy read fallback.

use anyhow::{anyhow, Context, Result};

/// The single v2 ref (§4.1). Points at a commit; parent = previous tip.
pub const INDEX_REF: &str = "refs/dkod/index";
/// Contents of the root `version` blob.
pub const INDEX_VERSION: &str = "1\n";

/// Unix milliseconds embedded in a UUIDv7's first 48 bits, or `None` for
/// anything that is not a v7 UUID (foreign imports, hand-rolled test ids).
pub(crate) fn uuid_v7_unix_ms(id: &str) -> Option<u64> {
    let u = uuid::Uuid::parse_str(id).ok()?;
    if u.get_version_num() != 7 {
        return None;
    }
    let b = u.as_bytes();
    Some(
        ((b[0] as u64) << 40)
            | ((b[1] as u64) << 32)
            | ((b[2] as u64) << 24)
            | ((b[3] as u64) << 16)
            | ((b[4] as u64) << 8)
            | (b[5] as u64),
    )
}

/// `YYYY-MM-DD` (UTC) for a unix-milliseconds timestamp. Civil-from-days
/// algorithm (Howard Hinnant) — dependency-free, valid for all dates ≥ 1970.
pub(crate) fn date_from_unix_ms(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

/// `sessions/<date>/<id>` for a v7 id (date computable from the id alone,
/// §4.2), `None` otherwise.
pub fn session_dir(id: &str) -> Option<String> {
    uuid_v7_unix_ms(id).map(|ms| format!("sessions/{}/{id}", date_from_unix_ms(ms)))
}

/// Write-side directory: v7 timestamp when available, else the session's
/// `created_at` (clamped to epoch) — §10's rule for non-v7 ids.
pub fn session_dir_for(id: &str, created_at: i64) -> String {
    match session_dir(id) {
        Some(d) => d,
        None => {
            let ms = (created_at.max(0) as u64).saturating_mul(1000);
            format!("sessions/{}/{id}", date_from_unix_ms(ms))
        }
    }
}

pub fn meta_path(id: &str) -> Option<String> {
    session_dir(id).map(|d| format!("{d}/meta.json"))
}

pub fn body_path(id: &str) -> Option<String> {
    session_dir(id).map(|d| format!("{d}/body.json"))
}

/// `commits/<aa>/<remaining-38>` (§4.2 hex fan-out). `sha` is always a full
/// git object hash (40 hex) by contract; panics on inputs shorter than 2.
pub fn commit_pointer_path(sha: &str) -> String {
    assert!(sha.len() >= 2, "commit sha must be at least 2 chars");
    format!("commits/{}/{}", &sha[..2], &sha[2..])
}

/// `patchid/<aa>/<remaining-38>`. `pid` is always a full patch-id hash by
/// contract; panics on inputs shorter than 2.
pub fn patchid_pointer_path(pid: &str) -> String {
    assert!(pid.len() >= 2, "patch-id must be at least 2 chars");
    format!("patchid/{}/{}", &pid[..2], &pid[2..])
}

/// Decode a tree object into owned entries. Empty Vec for `None`.
#[allow(dead_code)] // wired up by the batch/commit path in the next task
fn tree_entries(
    repo: &gix::Repository,
    tree: Option<gix::ObjectId>,
) -> Result<Vec<gix::objs::tree::Entry>> {
    let Some(oid) = tree else {
        return Ok(Vec::new());
    };
    let obj = repo.find_object(oid).context("find tree object")?;
    let tree = obj
        .try_into_tree()
        .map_err(|e| anyhow!("not a tree: {e}"))?;
    let decoded = tree.decode().context("decode tree")?;
    // gix 0.66 has no `From<&EntryRef>` for `Entry` — build owned entries
    // manually (executor note in the plan for this gix API drift).
    Ok(decoded
        .entries
        .iter()
        .map(|e| gix::objs::tree::Entry {
            mode: e.mode,
            filename: e.filename.to_owned(),
            oid: e.oid.to_owned(),
        })
        .collect())
}

/// Write a tree from entries (sorted into git's canonical tree order via
/// `Entry: Ord`, which honors the directory-sorts-as-`name/` rule).
#[allow(dead_code)] // wired up by the batch/commit path in the next task
fn write_tree(
    repo: &gix::Repository,
    mut entries: Vec<gix::objs::tree::Entry>,
) -> Result<gix::ObjectId> {
    entries.sort();
    Ok(repo
        .write_object(&gix::objs::Tree { entries })
        .context("write tree")?
        .detach())
}

/// Read-modify-write a single `path` (slash-separated, all intermediate
/// components trees) to point at `blob`, returning the new ROOT tree oid.
/// `root = None` starts from an empty tree. A non-tree in an intermediate
/// position is an error (corrupt index — never silently overwritten).
#[allow(dead_code)] // wired up by the batch/commit path in the next task
pub(crate) fn upsert_path(
    repo: &gix::Repository,
    root: Option<gix::ObjectId>,
    path: &str,
    blob: gix::ObjectId,
) -> Result<gix::ObjectId> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err(anyhow!("empty index path"));
    }
    upsert_segments(repo, root, &segments, blob)
}

#[allow(dead_code)] // wired up by the batch/commit path in the next task
fn upsert_segments(
    repo: &gix::Repository,
    tree: Option<gix::ObjectId>,
    segments: &[&str],
    blob: gix::ObjectId,
) -> Result<gix::ObjectId> {
    use gix::objs::tree::{Entry, EntryKind};
    let mut entries = tree_entries(repo, tree)?;
    let name = segments[0];
    let existing = entries.iter().position(|e| e.filename == name);
    if segments.len() == 1 {
        let entry = Entry {
            mode: EntryKind::Blob.into(),
            filename: name.into(),
            oid: blob,
        };
        match existing {
            Some(i) => entries[i] = entry,
            None => entries.push(entry),
        }
    } else {
        let child = match existing {
            Some(i) => {
                if !entries[i].mode.is_tree() {
                    return Err(anyhow!(
                        "index path conflict at {name:?}: blob where tree expected"
                    ));
                }
                Some(entries[i].oid)
            }
            None => None,
        };
        let new_child = upsert_segments(repo, child, &segments[1..], blob)?;
        let entry = Entry {
            mode: EntryKind::Tree.into(),
            filename: name.into(),
            oid: new_child,
        };
        match existing {
            Some(i) => entries[i] = entry,
            None => entries.push(entry),
        }
    }
    write_tree(repo, entries)
}

/// Resolve `path` under the ROOT TREE `root` to its blob oid. `None` when any
/// component is absent or the leaf is not a blob.
#[allow(dead_code)] // wired up by the batch/commit path in the next task
pub(crate) fn blob_oid_at(
    repo: &gix::Repository,
    root: gix::ObjectId,
    path: &str,
) -> Option<gix::ObjectId> {
    let mut current = root;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    for (i, seg) in segments.iter().enumerate() {
        let entries = tree_entries(repo, Some(current)).ok()?;
        let entry = entries.iter().find(|e| e.filename == *seg)?;
        if i == segments.len() - 1 {
            return entry.mode.is_blob().then_some(entry.oid);
        }
        if !entry.mode.is_tree() {
            return None;
        }
        current = entry.oid;
    }
    None
}

/// Blob bytes at `path` under root tree `root`.
#[allow(dead_code)] // wired up by the batch/commit path in the next task
pub(crate) fn read_tree_blob(
    repo: &gix::Repository,
    root: gix::ObjectId,
    path: &str,
) -> Option<Vec<u8>> {
    let oid = blob_oid_at(repo, root, path)?;
    Some(repo.find_object(oid).ok()?.detach().data)
}

/// Names of the SUBTREE entries directly under `dir` ("" = root), sorted.
/// Used to walk `sessions/<date>/<id>` (§6.1) and gc candidates (§8).
#[allow(dead_code)] // wired up by the batch/commit path in the next task
pub(crate) fn list_tree_dir(
    repo: &gix::Repository,
    root: gix::ObjectId,
    dir: &str,
) -> Result<Vec<String>> {
    let mut current = root;
    for seg in dir.split('/').filter(|s| !s.is_empty()) {
        let entries = tree_entries(repo, Some(current))?;
        match entries
            .iter()
            .find(|e| e.filename == seg && e.mode.is_tree())
        {
            Some(e) => current = e.oid,
            None => return Ok(Vec::new()),
        }
    }
    let mut names: Vec<String> = tree_entries(repo, Some(current))?
        .into_iter()
        .filter(|e| e.mode.is_tree())
        .map(|e| e.filename.to_string())
        .collect();
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod path_tests {
    use super::*;

    /// UUIDv7 generated at exactly 2025-01-01T00:00:00Z (unix 1735689600).
    fn v7_at(secs: u64) -> String {
        uuid::Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, secs, 0)).to_string()
    }

    #[test]
    fn v7_id_maps_to_its_embedded_date() {
        let id = v7_at(1735689600);
        assert_eq!(uuid_v7_unix_ms(&id), Some(1735689600000));
        assert_eq!(session_dir(&id), Some(format!("sessions/2025-01-01/{id}")));
        assert_eq!(
            meta_path(&id),
            Some(format!("sessions/2025-01-01/{id}/meta.json"))
        );
        assert_eq!(
            body_path(&id),
            Some(format!("sessions/2025-01-01/{id}/body.json"))
        );
    }

    #[test]
    fn non_v7_id_yields_none_paths() {
        assert_eq!(uuid_v7_unix_ms("not-a-uuid"), None);
        // v4 uuid: version nibble is 4, not 7.
        assert_eq!(
            uuid_v7_unix_ms("123e4567-e89b-42d3-a456-426614174000"),
            None
        );
        assert_eq!(session_dir("not-a-uuid"), None);
    }

    #[test]
    fn session_dir_for_falls_back_to_created_at() {
        // non-v7 id + created_at 2025-01-01 → date from created_at
        let d = session_dir_for("legacy-id-1", 1735689600);
        assert_eq!(d, "sessions/2025-01-01/legacy-id-1");
        // v7 id wins over created_at
        let id = v7_at(1735689600);
        assert_eq!(session_dir_for(&id, 0), format!("sessions/2025-01-01/{id}"));
        // negative created_at clamps to epoch
        assert_eq!(session_dir_for("x", -5), "sessions/1970-01-01/x");
    }

    #[test]
    fn date_math_handles_leap_years_and_epoch() {
        assert_eq!(date_from_unix_ms(0), "1970-01-01");
        // 2024-02-29T12:00:00Z = 1709208000
        assert_eq!(date_from_unix_ms(1709208000 * 1000), "2024-02-29");
        // 2026-06-10T00:00:00Z = 1781049600
        assert_eq!(date_from_unix_ms(1781049600 * 1000), "2026-06-10");
    }

    #[test]
    fn pointer_paths_use_two_hex_fanout() {
        let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        assert_eq!(
            commit_pointer_path(sha),
            format!("commits/de/{}", &sha[2..])
        );
        assert_eq!(
            patchid_pointer_path(sha),
            format!("patchid/de/{}", &sha[2..])
        );
    }
}

#[cfg(test)]
mod tree_tests {
    use super::*;
    use tempfile::TempDir;

    fn repo() -> (TempDir, gix::Repository) {
        let tmp = TempDir::new().unwrap();
        let mut r = gix::init(tmp.path()).unwrap();
        crate::store::ensure_committer(&mut r).unwrap();
        (tmp, r)
    }

    #[test]
    fn upsert_creates_nested_path_and_read_blob_round_trips() {
        let (_tmp, repo) = repo();
        let blob = repo.write_blob(b"hello").unwrap().detach();
        let root = upsert_path(&repo, None, "sessions/2025-01-01/abc/body.json", blob).unwrap();
        assert_eq!(
            read_tree_blob(&repo, root, "sessions/2025-01-01/abc/body.json").unwrap(),
            b"hello".to_vec()
        );
        assert!(read_tree_blob(&repo, root, "sessions/2025-01-01/abc/meta.json").is_none());
        assert!(read_tree_blob(&repo, root, "nope/nope").is_none());
    }

    #[test]
    fn upsert_preserves_siblings_and_overwrites_same_path() {
        let (_tmp, repo) = repo();
        let a = repo.write_blob(b"a").unwrap().detach();
        let b = repo.write_blob(b"b").unwrap().detach();
        let root = upsert_path(&repo, None, "commits/de/adbeef", a).unwrap();
        let root = upsert_path(&repo, Some(root), "commits/de/other", b).unwrap();
        let root = upsert_path(&repo, Some(root), "commits/de/adbeef", b).unwrap();
        assert_eq!(
            read_tree_blob(&repo, root, "commits/de/adbeef").unwrap(),
            b"b".to_vec()
        );
        assert_eq!(
            read_tree_blob(&repo, root, "commits/de/other").unwrap(),
            b"b".to_vec()
        );
    }

    #[test]
    fn blob_oid_at_returns_oid_for_existing_path() {
        let (_tmp, repo) = repo();
        let blob = repo.write_blob(b"x").unwrap().detach();
        let root = upsert_path(&repo, None, "version", blob).unwrap();
        assert_eq!(blob_oid_at(&repo, root, "version"), Some(blob));
        assert_eq!(blob_oid_at(&repo, root, "epoch"), None);
    }

    #[test]
    fn list_tree_dir_names_subdirectories() {
        let (_tmp, repo) = repo();
        let blob = repo.write_blob(b"x").unwrap().detach();
        let root = upsert_path(&repo, None, "sessions/2025-01-01/a/meta.json", blob).unwrap();
        let root = upsert_path(&repo, Some(root), "sessions/2025-01-02/b/meta.json", blob).unwrap();
        let dates = list_tree_dir(&repo, root, "sessions").unwrap();
        assert_eq!(
            dates,
            vec!["2025-01-01".to_string(), "2025-01-02".to_string()]
        );
        let ids = list_tree_dir(&repo, root, "sessions/2025-01-01").unwrap();
        assert_eq!(ids, vec!["a".to_string()]);
    }
}
