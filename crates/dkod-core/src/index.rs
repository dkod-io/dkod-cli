//! Storage v2 rollup index (`dkod-index/1`, design 2026-06-10): one
//! `refs/dkod/index` ref pointing at a commit chain whose tree holds every
//! session's metadata + body and the commit/patch-id lookup tables. This
//! module owns the tree layout, the tree codec, and the batched, CAS-guarded
//! write path. `store.rs` composes it with the legacy-ref dual-write and the
//! permanent legacy read fallback.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

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

/// All session ids in the index (sorted date-dir walk → chronological for v7
/// ids). Empty when there is no index ref.
pub fn list_index_session_ids(repo: &gix::Repository) -> Vec<String> {
    let Some(tip) = index_tip(repo) else {
        return Vec::new();
    };
    let Ok(root) = commit_tree(repo, tip) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for date in list_tree_dir(repo, root, "sessions").unwrap_or_default() {
        for id in list_tree_dir(repo, root, &format!("sessions/{date}")).unwrap_or_default() {
            ids.push(id);
        }
    }
    ids
}

/// Locate a session dir by scanning every date directory — the non-v7-id
/// fallback (§6.2). O(date-dirs); only hit for foreign/hand-rolled ids.
pub(crate) fn find_session_dir_by_scan(
    repo: &gix::Repository,
    root: gix::ObjectId,
    id: &str,
) -> Option<String> {
    for date in list_tree_dir(repo, root, "sessions").ok()? {
        let dir = format!("sessions/{date}/{id}");
        if blob_oid_at(repo, root, &format!("{dir}/body.json")).is_some() {
            return Some(dir);
        }
    }
    None
}

/// One logical index write: ordered `(tree_path, blob_bytes)` inserts applied
/// to the current tip's tree in a single read-modify-write, producing one
/// child commit (§5.1).
pub struct IndexBatch {
    pub message: String,
    inserts: Vec<(String, Vec<u8>)>,
}

impl IndexBatch {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            inserts: Vec::new(),
        }
    }
    pub fn insert(&mut self, path: String, bytes: Vec<u8>) {
        self.inserts.push((path, bytes));
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inserts.is_empty()
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.inserts.len()
    }
}

/// Current `refs/dkod/index` tip commit, if the ref exists.
pub fn index_tip(repo: &gix::Repository) -> Option<gix::ObjectId> {
    let r = repo.try_find_reference(INDEX_REF).ok()??;
    Some(r.id().detach())
}

/// ROOT TREE oid of an index commit.
pub(crate) fn commit_tree(repo: &gix::Repository, tip: gix::ObjectId) -> Result<gix::ObjectId> {
    let commit = repo
        .find_object(tip)
        .context("find index commit")?
        .try_into_commit()
        .map_err(|e| anyhow!("index tip is not a commit: {e}"))?;
    Ok(commit.tree_id().context("index commit tree id")?.detach())
}

/// The repo's committer signature (guaranteed present after
/// `store::ensure_committer`) as an owned `gix::actor::Signature`.
fn signature(repo: &gix::Repository) -> Result<gix::actor::Signature> {
    let sig = repo
        .committer()
        .ok_or_else(|| anyhow!("no committer configured (ensure_committer not called)"))?
        .map_err(|e| anyhow!("committer time: {e}"))?;
    Ok(sig.to_owned())
}

/// Write the COMMIT OBJECT applying `inserts` (path → existing blob oid) on
/// top of `parent`'s tree, WITHOUT touching any ref. Seeds `version`/`epoch`
/// blobs when `parent` is `None` (new root). Returns `Ok(None)` when every
/// insert is already present with the same oid (no object written — the
/// idempotence rule of §10.3).
pub(crate) fn build_commit(
    repo: &gix::Repository,
    parent: Option<gix::ObjectId>,
    inserts: &[(String, gix::ObjectId)],
    message: &str,
) -> Result<Option<gix::ObjectId>> {
    let old_root = match parent {
        Some(p) => Some(commit_tree(repo, p)?),
        None => None,
    };
    let mut root = old_root;
    if parent.is_none() {
        let version = repo
            .write_blob(INDEX_VERSION.as_bytes())
            .context("write version blob")?
            .detach();
        let epoch = repo
            .write_blob(b"0\n")
            .context("write epoch blob")?
            .detach();
        root = Some(upsert_path(repo, root, "version", version)?);
        root = Some(upsert_path(repo, root.take(), "epoch", epoch)?);
    }
    for (path, oid) in inserts {
        let already = root.and_then(|r| blob_oid_at(repo, r, path));
        if already == Some(*oid) {
            continue; // idempotent skip
        }
        root = Some(upsert_path(repo, root, path, *oid)?);
    }
    let Some(new_root) = root else {
        return Ok(None);
    };
    if old_root == Some(new_root) {
        return Ok(None); // nothing changed → no empty commit
    }
    let sig = signature(repo)?;
    let commit = gix::objs::Commit {
        tree: new_root,
        parents: parent.into_iter().collect(),
        author: sig.clone(),
        committer: sig,
        encoding: None,
        message: message.into(),
        extra_headers: Vec::new(),
    };
    Ok(Some(
        repo.write_object(&commit)
            .context("write index commit")?
            .detach(),
    ))
}

/// `build_commit` + move `refs/dkod/index` from `expected_tip` to the new
/// commit with compare-and-swap semantics (§5.3 layer 2). `expected_tip =
/// None` requires the ref to not exist yet.
pub(crate) fn commit_inserts(
    repo: &gix::Repository,
    parent: Option<gix::ObjectId>,
    expected_tip: Option<gix::ObjectId>,
    inserts: &[(String, gix::ObjectId)],
    message: &str,
) -> Result<Option<gix::ObjectId>> {
    use gix::refs::{
        transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
        Target,
    };
    let Some(new_tip) = build_commit(repo, parent, inserts, message)? else {
        return Ok(None);
    };
    let expected = match expected_tip {
        Some(t) => PreviousValue::MustExistAndMatch(Target::Object(t)),
        None => PreviousValue::MustNotExist,
    };
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
            },
            expected,
            new: Target::Object(new_tip),
        },
        name: INDEX_REF.try_into().context("invalid index ref name")?,
        deref: false,
    })
    .context("CAS edit of refs/dkod/index")?;
    Ok(Some(new_tip))
}

/// Advisory lock file serializing local index writers (§5.3 layer 1).
/// `O_CREAT|O_EXCL`; waits up to ~10 s in 100 ms steps; a lock older than
/// 30 s is treated as stale and removed. Released on Drop.
struct IndexLock {
    path: std::path::PathBuf,
}

impl IndexLock {
    fn acquire(repo: &gix::Repository) -> Result<Self> {
        let dir = repo.path().join("dkod");
        std::fs::create_dir_all(&dir).context("create .git/dkod")?;
        let path = dir.join("index.lock");
        for _ in 0..100 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|m| std::time::SystemTime::now().duration_since(m).ok())
                        .is_some_and(|age| age > std::time::Duration::from_secs(30));
                    if stale {
                        let _ = std::fs::remove_file(&path); // stale takeover
                        continue;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => return Err(e).context("create index lock"),
            }
        }
        Err(anyhow!(
            "timed out waiting for index lock at {}",
            path.display()
        ))
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Apply one batch as one index commit: lock → write blobs → CAS loop
/// (5 bounded attempts with jitter, §5.3). Returns the new tip, or `Ok(None)`
/// when the batch was already fully present (idempotent no-op). Errors only
/// after retry exhaustion — callers on the capture path spill to the outbox
/// (`store::spill_to_outbox`) instead of failing the session.
pub fn apply_batch(repo_path: &Path, batch: &IndexBatch) -> Result<Option<gix::ObjectId>> {
    let mut repo = gix::open(repo_path).context("open repo")?;
    crate::store::ensure_committer(&mut repo)?;
    let _lock = IndexLock::acquire(&repo)?;
    let mut inserts = Vec::with_capacity(batch.inserts.len());
    for (path, bytes) in &batch.inserts {
        let oid = repo
            .write_blob(bytes.as_slice())
            .context("write index blob")?
            .detach();
        inserts.push((path.clone(), oid));
    }
    let mut last_err = None;
    for attempt in 0..5 {
        let tip = index_tip(&repo);
        match commit_inserts(&repo, tip, tip, &inserts, &batch.message) {
            Ok(res) => return Ok(res),
            Err(e) => {
                last_err = Some(e);
                // jitter: 10–50 ms scaled by attempt
                let ms = 10 + (attempt as u64 * 10) + (std::process::id() as u64 % 10);
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("index CAS retries exhausted")))
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

#[cfg(test)]
mod batch_tests {
    use super::*;
    use tempfile::TempDir;

    fn repo() -> (TempDir, gix::Repository) {
        let tmp = TempDir::new().unwrap();
        let mut r = gix::init(tmp.path()).unwrap();
        crate::store::ensure_committer(&mut r).unwrap();
        (tmp, r)
    }

    fn batch(msg: &str, paths: &[(&str, &[u8])]) -> IndexBatch {
        let mut b = IndexBatch::new(msg);
        for (p, bytes) in paths {
            b.insert(p.to_string(), bytes.to_vec());
        }
        b
    }

    #[test]
    fn first_apply_creates_root_with_version_and_epoch() {
        let (tmp, repo) = repo();
        let tip = apply_batch(
            tmp.path(),
            &batch("dkod: test", &[("commits/aa/bb", b"x\n")]),
        )
        .unwrap()
        .expect("a commit must be written");
        assert_eq!(index_tip(&repo), Some(tip));
        let root = commit_tree(&repo, tip).unwrap();
        assert_eq!(
            read_tree_blob(&repo, root, "version").unwrap(),
            b"1\n".to_vec()
        );
        assert_eq!(
            read_tree_blob(&repo, root, "epoch").unwrap(),
            b"0\n".to_vec()
        );
        assert_eq!(
            read_tree_blob(&repo, root, "commits/aa/bb").unwrap(),
            b"x\n".to_vec()
        );
        // root commit has no parent
        let commit = repo.find_object(tip).unwrap().try_into_commit().unwrap();
        assert_eq!(commit.parent_ids().count(), 0);
    }

    #[test]
    fn second_apply_chains_onto_first() {
        let (tmp, repo) = repo();
        let t1 = apply_batch(tmp.path(), &batch("m1", &[("a", b"1")]))
            .unwrap()
            .unwrap();
        let t2 = apply_batch(tmp.path(), &batch("m2", &[("b", b"2")]))
            .unwrap()
            .unwrap();
        let commit = repo.find_object(t2).unwrap().try_into_commit().unwrap();
        let parents: Vec<_> = commit.parent_ids().map(|p| p.detach()).collect();
        assert_eq!(parents, vec![t1]);
        let root = commit_tree(&repo, t2).unwrap();
        assert!(
            read_tree_blob(&repo, root, "a").is_some(),
            "older path must persist"
        );
        assert!(read_tree_blob(&repo, root, "b").is_some());
    }

    #[test]
    fn reapplying_identical_batch_is_a_noop() {
        let (tmp, _repo) = repo();
        let b = batch("m", &[("a", b"1")]);
        let t1 = apply_batch(tmp.path(), &b).unwrap().unwrap();
        assert_eq!(
            apply_batch(tmp.path(), &b).unwrap(),
            None,
            "no empty commit on no-op"
        );
        let repo2 = gix::open(tmp.path()).unwrap();
        assert_eq!(index_tip(&repo2), Some(t1), "tip unchanged");
    }

    #[test]
    fn build_commit_without_ref_edit_leaves_index_ref_alone() {
        let (tmp, repo) = repo();
        let t1 = apply_batch(tmp.path(), &batch("m", &[("a", b"1")]))
            .unwrap()
            .unwrap();
        let blob = repo.write_blob(b"side").unwrap().detach();
        let side = build_commit(&repo, Some(t1), &[("b".to_string(), blob)], "side").unwrap();
        assert!(side.is_some());
        assert_eq!(index_tip(&repo), Some(t1), "ref must not move");
    }

    #[test]
    fn commit_inserts_cas_rejects_stale_expected() {
        let (tmp, repo) = repo();
        let t1 = apply_batch(tmp.path(), &batch("m1", &[("a", b"1")]))
            .unwrap()
            .unwrap();
        let t2 = apply_batch(tmp.path(), &batch("m2", &[("b", b"2")]))
            .unwrap()
            .unwrap();
        assert_ne!(t1, t2);
        let blob = repo.write_blob(b"3").unwrap().detach();
        // expected tip is stale (t1) while the ref is at t2 → must error
        let err = commit_inserts(&repo, Some(t2), Some(t1), &[("c".to_string(), blob)], "m3");
        assert!(err.is_err(), "stale CAS must be rejected");
    }

    #[test]
    fn concurrent_apply_batch_keeps_every_insert() {
        let (tmp, _repo) = repo();
        let path = tmp.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let p = path.clone();
                std::thread::spawn(move || {
                    let b = {
                        let mut b = IndexBatch::new(format!("t{i}"));
                        b.insert(format!("commits/aa/{i:038}"), vec![b'0' + i as u8]);
                        b
                    };
                    apply_batch(&p, &b).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let repo = gix::open(&path).unwrap();
        let tip = index_tip(&repo).unwrap();
        let root = commit_tree(&repo, tip).unwrap();
        for i in 0..8 {
            assert!(
                read_tree_blob(&repo, root, &format!("commits/aa/{i:038}")).is_some(),
                "insert {i} lost under contention"
            );
        }
    }
}
