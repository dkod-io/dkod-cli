use crate::index;
use crate::SessionMeta;
use crate::{refs, Session};
use anyhow::{Context, Result};
use std::path::Path;

/// Inject a synthetic fallback committer into the in-memory repo config when
/// the host environment has no `user.name`/`user.email` configured (CI, fresh
/// boxes, sandboxed runs). gix requires a committer identity to write reflog
/// entries — without this guard, any `edit_reference` call against a
/// reflog-tracked ref (`HEAD`, `refs/heads/*`, `refs/remotes/*`,
/// `refs/notes/*`, `refs/worktree/*`) errors with `MissingCommitter`.
///
/// The dkod ref namespaces (`refs/dkod/sessions/*`, `refs/dkod/commits/*`)
/// don't auto-create reflogs today, but we apply the guard defensively so
/// these helpers stay correct if the namespace policy changes or if a caller
/// flips `force_create_reflog: true`.
pub(crate) fn ensure_committer(repo: &mut gix::Repository) -> Result<()> {
    use gix::config::tree::gitoxide;

    if repo.committer().is_some() {
        return Ok(());
    }

    let mut config = gix::config::File::new(gix::config::file::Metadata::api());
    config
        .set_raw_value(&gitoxide::Committer::NAME_FALLBACK, "dkod")
        .context("set committer name fallback")?;
    config
        .set_raw_value(&gitoxide::Committer::EMAIL_FALLBACK, "noreply@dkod.io")
        .context("set committer email fallback")?;
    // also patch author so any author-requiring code path works the same way
    config
        .set_raw_value(&gitoxide::Author::NAME_FALLBACK, "dkod")
        .context("set author name fallback")?;
    config
        .set_raw_value(&gitoxide::Author::EMAIL_FALLBACK, "noreply@dkod.io")
        .context("set author email fallback")?;

    let mut snapshot = repo.config_snapshot_mut();
    snapshot.append(config);
    snapshot.commit().context("commit committer fallback")?;
    Ok(())
}

/// (meta_path, meta_bytes) + (body_path, body_bytes) inserts for `session`,
/// pushed onto `batch`. Path date: v7 timestamp, else created_at (§10).
fn push_session_inserts(batch: &mut index::IndexBatch, session: &Session) -> Result<()> {
    let body = serde_json::to_vec(session).context("serialize session body")?;
    let meta = serde_json::to_vec(&SessionMeta::derive(session, body.len() as u64))
        .context("serialize session meta")?;
    let dir = index::session_dir_for(&session.id, session.created_at);
    batch.insert(format!("{dir}/meta.json"), meta);
    batch.insert(format!("{dir}/body.json"), body);
    Ok(())
}

/// Pointer blob contents: `<session-id>\n` (§4.4).
fn pointer_blob(session_id: &str) -> Vec<u8> {
    format!("{session_id}\n").into_bytes()
}

/// Spill a session that could not reach the index (CAS exhaustion) to
/// `.git/dkod/outbox/<id>.json` (§5.3). pub(crate) so tests can call it.
pub(crate) fn spill_to_outbox(repo_path: &Path, session: &Session) -> Result<()> {
    let repo = gix::open(repo_path).context("open repo")?;
    let dir = repo.path().join("dkod/outbox");
    std::fs::create_dir_all(&dir).context("create outbox dir")?;
    let bytes = serde_json::to_vec(session).context("serialize outbox session")?;
    std::fs::write(dir.join(format!("{}.json", session.id)), bytes).context("write outbox file")?;
    Ok(())
}

/// Fold every parseable `.git/dkod/outbox/*.json` into `batch` (meta + body +
/// commit pointers from `session.commits`; patch-ids are unknown for spilled
/// sessions and are skipped). Returns the file paths to delete after a
/// successful apply. Unparseable files are left in place and skipped.
fn fold_outbox(repo_path: &Path, batch: &mut index::IndexBatch) -> Vec<std::path::PathBuf> {
    let Ok(repo) = gix::open(repo_path) else {
        return Vec::new();
    };
    let dir = repo.path().join("dkod/outbox");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut consumed = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(session) = serde_json::from_slice::<Session>(&bytes) else {
            continue;
        };
        if push_session_inserts(batch, &session).is_err() {
            continue;
        }
        for sha in &session.commits {
            if sha.len() >= 3 {
                batch.insert(index::commit_pointer_path(sha), pointer_blob(&session.id));
            }
        }
        consumed.push(path);
    }
    consumed
}

/// Apply `batch` to the index; on retry exhaustion spill `session` to the
/// outbox and WARN instead of failing — a capture is never lost to
/// contention (§5.3). Deletes `consumed_outbox` files only on success.
fn apply_or_spill(
    repo_path: &Path,
    batch: &index::IndexBatch,
    session: &Session,
    consumed_outbox: &[std::path::PathBuf],
) {
    match index::apply_batch(repo_path, batch) {
        Ok(_) => {
            for p in consumed_outbox {
                let _ = std::fs::remove_file(p);
            }
        }
        Err(e) => {
            eprintln!("dkod: index write failed ({e:#}); session spilled to outbox");
            let _ = spill_to_outbox(repo_path, session);
        }
    }
}

/// Legacy write: serialize `session` as JSON, write it as a Git blob, and
/// create the `refs/dkod/sessions/<id>` reference pointing directly at that
/// blob. Factored out of `write_session` so both the public single-shot
/// function and `write_session_full` share one legacy code path. The session
/// write is the only fatal step in the dual-write flow.
fn write_session_legacy(repo_path: &Path, session: &Session) -> Result<()> {
    use gix::refs::{
        transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
        Target,
    };

    let mut repo = gix::open(repo_path).context("open repo")?;
    ensure_committer(&mut repo)?;
    let bytes = serde_json::to_vec(session).context("serialize session")?;
    let blob_id = repo.write_blob(&bytes).context("write blob")?.detach();
    let ref_name = refs::session_ref(&session.id);

    // gix 0.66: edit_reference + Target::Object pins a ref directly at a blob.
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("dkod: write session {}", session.id).into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(blob_id),
        },
        name: ref_name.try_into().context("invalid session ref name")?,
        deref: false,
    })
    .context("edit session ref")?;

    Ok(())
}

/// Persist `session` to the rollup index (meta + body) and, when dual-write is
/// on, to the legacy `refs/dkod/sessions/<id>` blob ref. Spilled outbox
/// sessions are folded into the same index commit. The legacy session write
/// is the only fatal step; an index-write failure spills to the outbox and
/// warns rather than failing (§5.3).
pub fn write_session(repo_path: &Path, session: &Session) -> Result<()> {
    let storage = crate::config::load_storage_config(repo_path);

    let mut batch = index::IndexBatch::new(format!(
        "dkod: add session {} ({}, 0 commit(s))",
        session.id,
        crate::agent_label(&session.agent)
    ));
    // Serialize failure stays fatal — same contract as today.
    push_session_inserts(&mut batch, session)?;
    let consumed = fold_outbox(repo_path, &mut batch);
    apply_or_spill(repo_path, &batch, session, &consumed);

    if storage.write_legacy_refs {
        write_session_legacy(repo_path, session)?;
    }
    Ok(())
}

/// Populate `session.commits` from HEAD-watching (commits made since
/// `head_at_start`) and persist everything: write the session blob (which now
/// carries the commit list) and a `refs/dkod/commits/<sha>` link per commit,
/// all pointing at the same blob. `head_at_start` is the repo HEAD recorded
/// when the session began; `None` (or a non-ancestor) links nothing.
///
/// Ordering matters: `session.commits` is set BEFORE `write_session` so the
/// session blob and every commit-link ref resolve to the same final blob.
///
/// Failure policy: writing the session is the ONLY fatal step — if it fails,
/// the session is not persisted and the error propagates so the caller treats
/// the flush as failed. Commit discovery and per-commit linking are
/// best-effort: a bad/unreadable start HEAD yields no commits, and a single
/// link failure mid-loop is skipped rather than propagated. Returns the shas
/// that were successfully linked.
pub fn write_session_with_commit_links(
    repo_path: &Path,
    session: &mut Session,
    head_at_start: Option<&str>,
) -> Result<Vec<String>> {
    // Commit discovery is best-effort: a bad/unreadable start HEAD must never
    // block persisting the session.
    let commits = new_commits_since(repo_path, head_at_start).unwrap_or_default();
    write_session_full(repo_path, session, &commits, &[])
}

/// The batched finalize entry point (§5.2/§16): ONE index commit containing
/// the session, all its commit pointers, and all its patch-id pointers.
/// `commits` is the discovered commit list (caller runs `new_commits_since`);
/// `patch_ids` is `(commit_sha, patch_id)` pairs (best-effort, may be empty).
/// Sets `session.commits = commits` BEFORE serializing so body, meta, and
/// pointers agree. Returns the linked shas. Legacy refs (session + commit
/// links + patchid links) are also written iff dual-write is on; legacy link
/// failures are skipped per-entry (today's best-effort policy), and only the
/// session write itself is fatal.
pub fn write_session_full(
    repo_path: &Path,
    session: &mut Session,
    commits: &[String],
    patch_ids: &[(String, String)],
) -> Result<Vec<String>> {
    session.commits = commits.to_vec();
    let storage = crate::config::load_storage_config(repo_path);

    // Index half: session + commit pointers + patch-id pointers in one batch.
    let mut batch = index::IndexBatch::new(format!(
        "dkod: add session {} ({}, {} commit(s))",
        session.id,
        crate::agent_label(&session.agent),
        commits.len()
    ));
    push_session_inserts(&mut batch, session)?; // serialize failure is fatal
    for sha in commits {
        if sha.len() >= 3 {
            batch.insert(index::commit_pointer_path(sha), pointer_blob(&session.id));
        }
    }
    for (sha, pid) in patch_ids {
        if pid.len() >= 3 && commits.contains(sha) {
            batch.insert(index::patchid_pointer_path(pid), pointer_blob(&session.id));
        }
    }
    let consumed = fold_outbox(repo_path, &mut batch);
    apply_or_spill(repo_path, &batch, session, &consumed);

    // Legacy half: session blob + ref (fatal), then best-effort link refs.
    if storage.write_legacy_refs {
        write_session_legacy(repo_path, session)?;
        for sha in commits {
            let _ = link_commit_legacy(repo_path, &session.id, sha);
        }
        for (sha, pid) in patch_ids {
            if commits.contains(sha) {
                let _ = link_patchid_legacy(repo_path, &session.id, pid);
            }
        }
    }

    Ok(commits.to_vec())
}

/// Read a session by id: the rollup index `body.json` first (§6.2), then the
/// legacy `refs/dkod/sessions/<id>` blob. A parse failure on a located body is
/// a real error, not a silent fallthrough.
pub fn read_session(repo_path: &Path, id: &str) -> Result<Session> {
    let repo = gix::open(repo_path).context("open repo")?;

    // Index-first: locate body.json at the current index tip. v7 ids resolve
    // their date directory directly; non-v7/foreign ids fall back to a
    // date-dir scan (§6.2).
    if let Some(tip) = index::index_tip(&repo) {
        if let Ok(root) = index::commit_tree(&repo, tip) {
            let body = index::body_path(id)
                .and_then(|p| index::read_tree_blob(&repo, root, &p))
                .or_else(|| {
                    index::find_session_dir_by_scan(&repo, root, id).and_then(|dir| {
                        index::read_tree_blob(&repo, root, &format!("{dir}/body.json"))
                    })
                });
            if let Some(bytes) = body {
                return serde_json::from_slice(&bytes).context("deserialize session (index)");
            }
        }
    }

    // Legacy fallback: the original blob-ref read path, unchanged.
    let r = repo
        .find_reference(&refs::session_ref(id))
        .context("find session ref")?;
    let object = repo.find_object(r.id()).context("find object")?.detach();
    let session: Session = serde_json::from_slice(&object.data).context("deserialize session")?;
    Ok(session)
}

/// Session header for `id`: `meta.json` at the index tip (parsed directly),
/// else derived from the legacy session blob (§6.2). `None` only when the
/// session exists nowhere.
fn meta_for_id(repo: &gix::Repository, id: &str) -> Option<SessionMeta> {
    // Index half: prefer the stored meta.json; fall back to body.json (direct
    // v7 path, then a date-dir scan for foreign ids) and derive from it.
    if let Some(tip) = index::index_tip(repo) {
        if let Ok(root) = index::commit_tree(repo, tip) {
            if let Some(bytes) =
                index::meta_path(id).and_then(|p| index::read_tree_blob(repo, root, &p))
            {
                if let Ok(meta) = serde_json::from_slice::<SessionMeta>(&bytes) {
                    return Some(meta);
                }
            }
            let dir = index::session_dir(id)
                .filter(|d| index::read_tree_blob(repo, root, &format!("{d}/body.json")).is_some())
                .or_else(|| index::find_session_dir_by_scan(repo, root, id));
            if let Some(dir) = dir {
                if let Some(body) = index::read_tree_blob(repo, root, &format!("{dir}/body.json")) {
                    if let Ok(s) = serde_json::from_slice::<Session>(&body) {
                        return Some(SessionMeta::derive(&s, body.len() as u64));
                    }
                }
            }
        }
    }

    // Legacy half: derive the header from the session blob.
    let r = repo.try_find_reference(&refs::session_ref(id)).ok()??;
    let data = repo.find_object(r.id()).ok()?.detach().data;
    let s: Session = serde_json::from_slice(&data).ok()?;
    Some(SessionMeta::derive(&s, data.len() as u64))
}

/// Resolve an index pointer path (e.g. `commits/<fanout>`) to the session id it
/// names. `None` when there is no index ref or the path is absent.
fn pointer_session_id(repo: &gix::Repository, path: &str) -> Option<String> {
    let tip = index::index_tip(repo)?;
    let root = index::commit_tree(repo, tip).ok()?;
    let bytes = index::read_tree_blob(repo, root, path)?;
    Some(String::from_utf8_lossy(&bytes).trim_end().to_string())
}

/// Derive a `SessionMeta` from a legacy blob the ref `ref_name` points at.
fn meta_from_legacy_ref(repo: &gix::Repository, ref_name: &str) -> Option<SessionMeta> {
    let r = repo.try_find_reference(ref_name).ok()??;
    let data = repo.find_object(r.id()).ok()?.detach().data;
    let s: Session = serde_json::from_slice(&data).ok()?;
    Some(SessionMeta::derive(&s, data.len() as u64))
}

/// Session header for `id`: `meta.json` at the index tip, else derived from
/// the legacy session blob (§6.2). Errors only when the session exists
/// nowhere.
pub fn read_session_meta(repo_path: &Path, id: &str) -> Result<SessionMeta> {
    let repo = gix::open(repo_path).context("open repo")?;
    meta_for_id(&repo, id).ok_or_else(|| anyhow::anyhow!("session {id} not found"))
}

/// Blame primary lookup (§6.1): `commits/<fanout(sha)>` pointer → meta; legacy
/// `refs/dkod/commits/<sha>` → derived meta. `None` = human commit.
pub fn lookup_commit_session(repo_path: &Path, sha: &str) -> Option<SessionMeta> {
    if sha.len() < 3 {
        return None;
    }
    let repo = gix::open(repo_path).ok()?;
    if let Some(id) = pointer_session_id(&repo, &index::commit_pointer_path(sha)) {
        if let Some(meta) = meta_for_id(&repo, &id) {
            return Some(meta);
        }
    }
    meta_from_legacy_ref(&repo, &refs::commit_ref(sha))
}

/// Blame patch-id fallback: `patchid/<fanout(pid)>` pointer → meta; legacy
/// `refs/dkod/patchid/<pid>` → derived meta.
pub fn lookup_patchid_session(repo_path: &Path, pid: &str) -> Option<SessionMeta> {
    if pid.len() < 3 {
        return None;
    }
    let repo = gix::open(repo_path).ok()?;
    if let Some(id) = pointer_session_id(&repo, &index::patchid_pointer_path(pid)) {
        if let Some(meta) = meta_for_id(&repo, &id) {
            return Some(meta);
        }
    }
    meta_from_legacy_ref(&repo, &refs::patchid_ref(pid))
}

/// Point a dkod ref at `blob_id` (creating or overwriting it). Shared by the
/// commit-ref / patch-id-ref / relink writers — all of which pin a ref in a
/// `refs/dkod/*` namespace directly at a session blob. `PreviousValue::Any`
/// gives overwrite-on-collision (last-writer-wins).
fn write_link_ref(
    repo: &gix::Repository,
    ref_name: String,
    blob_id: gix::ObjectId,
    message: String,
) -> Result<()> {
    use gix::refs::{
        transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
        Target,
    };
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(blob_id),
        },
        name: ref_name.try_into().context("invalid ref name")?,
        deref: false,
    })
    .context("edit ref")?;
    Ok(())
}

/// Legacy commit link: write `refs/dkod/commits/<commit_sha>` pointing at the
/// same blob the session ref points at. Errors when the legacy session ref is
/// absent (preserving today's contract). Factored out so the public function
/// and `write_session_full` share one legacy code path.
fn link_commit_legacy(repo_path: &Path, session_id: &str, commit_sha: &str) -> Result<()> {
    let mut repo = gix::open(repo_path).context("open repo")?;
    ensure_committer(&mut repo)?;
    let session_ref = repo
        .find_reference(&refs::session_ref(session_id))
        .context("find session ref")?;
    let blob_id = session_ref.id().detach();

    write_link_ref(
        &repo,
        refs::commit_ref(commit_sha),
        blob_id,
        format!("dkod: link session {} to commit {}", session_id, commit_sha),
    )
}

/// Legacy patch-id link: mirror of `link_commit_legacy` for
/// `refs/dkod/patchid/<patch_id>`.
fn link_patchid_legacy(repo_path: &Path, session_id: &str, patch_id: &str) -> Result<()> {
    let mut repo = gix::open(repo_path).context("open repo")?;
    ensure_committer(&mut repo)?;
    let session_ref = repo
        .find_reference(&refs::session_ref(session_id))
        .context("find session ref")?;
    let blob_id = session_ref.id().detach();

    write_link_ref(
        &repo,
        refs::patchid_ref(patch_id),
        blob_id,
        format!("dkod: link session {} to patch-id {}", session_id, patch_id),
    )
}

/// Link a commit sha to a session: an index pointer `commits/<fanout>` (§4.4),
/// and — when dual-write is on — the legacy `refs/dkod/commits/<sha>` ref. The
/// index write is best-effort (warn-only, matching today's link policy); the
/// legacy write still errors when the session ref is absent, preserving the
/// existing contract and tests. When dual-write is off, an existence check
/// (`read_session`) keeps "link a never-written session" an error.
pub fn link_session_to_commit(repo_path: &Path, session_id: &str, commit_sha: &str) -> Result<()> {
    if commit_sha.len() >= 3 {
        let mut batch = index::IndexBatch::new(format!(
            "dkod: link session {session_id} to commit {commit_sha}"
        ));
        batch.insert(
            index::commit_pointer_path(commit_sha),
            pointer_blob(session_id),
        );
        if let Err(e) = index::apply_batch(repo_path, &batch) {
            eprintln!("dkod: index commit-link failed ({e:#}); legacy ref still attempted");
        }
    }

    let storage = crate::config::load_storage_config(repo_path);
    if storage.write_legacy_refs {
        link_commit_legacy(repo_path, session_id, commit_sha)
    } else {
        // Linking a never-written session must still error.
        read_session(repo_path, session_id)?;
        Ok(())
    }
}

/// Link a patch-id to a session: an index pointer `patchid/<fanout>`, and —
/// when dual-write is on — the legacy `refs/dkod/patchid/<pid>` ref. Same
/// best-effort/existence-check policy as `link_session_to_commit`.
pub fn link_session_to_patchid(repo_path: &Path, session_id: &str, patch_id: &str) -> Result<()> {
    if patch_id.len() >= 3 {
        let mut batch = index::IndexBatch::new(format!(
            "dkod: link session {session_id} to patch-id {patch_id}"
        ));
        batch.insert(
            index::patchid_pointer_path(patch_id),
            pointer_blob(session_id),
        );
        if let Err(e) = index::apply_batch(repo_path, &batch) {
            eprintln!("dkod: index patch-id-link failed ({e:#}); legacy ref still attempted");
        }
    }

    let storage = crate::config::load_storage_config(repo_path);
    if storage.write_legacy_refs {
        link_patchid_legacy(repo_path, session_id, patch_id)
    } else {
        read_session(repo_path, session_id)?;
        Ok(())
    }
}

/// Re-point a commit link after a history rewrite. Delegates to
/// `relink_commits`; returns `Ok(true)` if the pair resolved (old pointer/ref
/// existed) and the new pointer was written, `Ok(false)` otherwise. The old
/// pointer/ref is left in place (additive) so an undone rewrite
/// (`git reset --hard ORIG_HEAD`) still resolves.
pub fn relink_commit(repo_path: &Path, old_sha: &str, new_sha: &str) -> Result<bool> {
    Ok(relink_commits(repo_path, &[(old_sha.to_string(), new_sha.to_string())])? == 1)
}

/// Batched post-rewrite relink (§5.2): resolve every `(old, new)` pair — index
/// pointer at `commits/<old>` first, legacy `refs/dkod/commits/<old>` second —
/// and write all new pointers as ONE index commit. Old paths/refs are kept
/// (additive). Legacy new refs are written iff dual-write is on AND the pair
/// resolved via a legacy ref or its legacy session ref still exists. Returns
/// how many pairs resolved; pairs that resolve nowhere are skipped.
pub fn relink_commits(repo_path: &Path, pairs: &[(String, String)]) -> Result<usize> {
    let mut repo = gix::open(repo_path).context("open repo")?;
    ensure_committer(&mut repo)?;

    // Resolve each pair to its session id, recording any legacy old-ref blob so
    // the legacy new ref can be written (dual-write) without re-resolving.
    let index_root = index::index_tip(&repo).and_then(|tip| index::commit_tree(&repo, tip).ok());
    let mut resolved: Vec<(String, String, Option<gix::ObjectId>)> = Vec::new();
    for (old, new) in pairs {
        if old.len() < 3 || new.len() < 3 {
            continue;
        }
        // Index pointer first.
        let from_index = index_root.and_then(|root| {
            index::read_tree_blob(&repo, root, &index::commit_pointer_path(old))
                .map(|b| String::from_utf8_lossy(&b).trim_end().to_string())
        });
        if let Some(id) = from_index {
            resolved.push((new.clone(), id, None));
            continue;
        }
        // Legacy ref second.
        if let Ok(Some(old_ref)) = repo.try_find_reference(&refs::commit_ref(old)) {
            let blob_id = old_ref.id().detach();
            let id = repo
                .find_object(blob_id)
                .ok()
                .and_then(|o| serde_json::from_slice::<Session>(&o.detach().data).ok())
                .map(|s| s.id);
            if let Some(id) = id {
                resolved.push((new.clone(), id, Some(blob_id)));
            }
        }
    }

    if resolved.is_empty() {
        return Ok(0);
    }

    // Index half: all new pointers in one batch.
    let mut batch = index::IndexBatch::new(format!(
        "dkod: relink {} commit(s) after history rewrite",
        resolved.len()
    ));
    for (new, id, _) in &resolved {
        batch.insert(index::commit_pointer_path(new), pointer_blob(id));
    }
    index::apply_batch(repo_path, &batch).context("relink index batch")?;

    // Legacy half (dual-write only): write the new `refs/dkod/commits/<new>`
    // pointing at the session blob. The blob is resolved via the legacy session
    // ref `refs/dkod/sessions/<id>` when present, else the recorded legacy
    // old-ref blob; pairs with no legacy provenance are skipped (index-only).
    let storage = crate::config::load_storage_config(repo_path);
    if storage.write_legacy_refs {
        for (new, id, legacy_blob) in &resolved {
            let blob_id = repo
                .try_find_reference(&refs::session_ref(id))
                .ok()
                .flatten()
                .map(|r| r.id().detach())
                .or(*legacy_blob);
            if let Some(blob_id) = blob_id {
                let _ = write_link_ref(
                    &repo,
                    refs::commit_ref(new),
                    blob_id,
                    format!("dkod: relink commit -> {new}"),
                );
            }
        }
    }

    Ok(resolved.len())
}

/// Enumerate every session id in this repo: the union of the rollup index
/// (`refs/dkod/index`) and the legacy `refs/dkod/sessions/*` refs, deduped by
/// id (§6.2). Sorted (BTreeSet) — a session present in both halves under
/// dual-write appears exactly once.
pub fn list_sessions(repo_path: &Path) -> Result<Vec<String>> {
    let repo = gix::open(repo_path).context("open repo")?;
    let mut ids: std::collections::BTreeSet<String> =
        index::list_index_session_ids(&repo).into_iter().collect();
    for r in repo
        .references()
        .context("list refs")?
        .prefixed("refs/dkod/sessions/")
        .context("filter session refs")?
    {
        let r = r
            .map_err(|e| anyhow::anyhow!(e))
            .context("walk session ref")?;
        let name = r.name().as_bstr().to_string();
        if let Some(id) = refs::parse_session_ref(&name) {
            ids.insert(id);
        }
    }
    Ok(ids.into_iter().collect())
}

/// Commit SHAs reachable from the repo's current HEAD but NOT reachable from
/// `start`. When `start` is `None`, returns an empty Vec — we never recorded a
/// session-start HEAD, so attributing any commit to the session would be a
/// guess. Used by the capture flow to link a session to the commits it produced.
///
/// If `start` is not an ancestor of the current HEAD (e.g. a mid-session branch
/// switch, or a stale/foreign SHA), returns an empty Vec rather than
/// over-attributing unrelated history.
///
/// Note: a rebase/squash/amend that rewrites SHAs *after* this runs will leave
/// the `refs/dkod/commits/<old-sha>` links pointing at commits that no longer
/// exist on the branch. Re-linking on observed history rewrites is a future
/// follow-up; V1 accepts the stale links.
pub fn new_commits_since(repo_path: &Path, start: Option<&str>) -> Result<Vec<String>> {
    let start = match start {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };
    let repo = gix::open(repo_path).context("open repo")?;
    let head_id = match repo.head_id() {
        Ok(id) => id,
        Err(_) => return Ok(Vec::new()),
    };
    let start_oid = gix::ObjectId::from_hex(start.as_bytes()).context("parse start commit sha")?;
    let head_oid = head_id.detach();
    if head_oid == start_oid {
        return Ok(Vec::new());
    }

    // Manual ancestor walk: gix's `rev_walk` lives behind the `revision`
    // feature, which this workspace does not enable. Walk parent links from
    // HEAD, stop descending at `start_oid` (exclusive), and dedup with a
    // visited set so merge commits with shared ancestry aren't revisited.
    let mut out = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    let mut found_start = false;
    queue.push_back(head_oid);

    while let Some(oid) = queue.pop_front() {
        if oid == start_oid {
            found_start = true;
            continue;
        }
        if !visited.insert(oid) {
            continue;
        }
        out.push(oid.to_string());
        let commit = repo
            .find_object(oid)
            .context("find commit")?
            .try_into_commit()
            .context("object is not a commit")?;
        for parent in commit.parent_ids() {
            queue.push_back(parent.detach());
        }
    }

    // `start` was a valid OID but never appeared in HEAD's ancestry — it is not
    // an ancestor of HEAD. Attribute nothing rather than every commit reachable
    // from HEAD.
    if !found_start {
        return Ok(Vec::new());
    }
    Ok(out)
}

/// Current HEAD commit SHA of the git repo at `path`, or None if `path`
/// isn't a git repo or HEAD is unborn (no commits yet). Used by the capture
/// flows to record where a session started, so the commits it produces can be
/// linked via `write_session_with_commit_links`.
pub fn head_sha(path: &Path) -> Option<String> {
    gix::open(path)
        .ok()?
        .head_id()
        .ok()
        .map(|id| id.detach().to_string())
}

/// Outcome of folding legacy refs into the index (§10).
#[derive(Debug)]
pub struct ReindexReport {
    pub sessions: usize,
    pub commit_links: usize,
    pub patchid_links: usize,
    pub dry_run_only: bool,
}

/// Blob oid of `body.json` for `id` at the index `root`, or `None` when the
/// session is absent. Resolves the date directory directly for v7 ids and
/// scans the date directories for non-v7/foreign ids (mirroring `read_session`,
/// §6.2) so the reindex idempotency check holds for both.
fn index_body_oid(
    repo: &gix::Repository,
    root: Option<gix::ObjectId>,
    id: &str,
) -> Option<gix::ObjectId> {
    let root = root?;
    let path = index::body_path(id)
        .filter(|p| index::blob_oid_at(repo, root, p).is_some())
        .or_else(|| {
            index::find_session_dir_by_scan(repo, root, id).map(|d| format!("{d}/body.json"))
        })?;
    index::blob_oid_at(repo, root, &path)
}

/// Read the `Session` id a legacy `refs/dkod/{commits,patchid}/*` ref names,
/// caching parses by the target blob oid so a session linked to many commits is
/// only deserialized once.
fn legacy_pointer_session_id(
    repo: &gix::Repository,
    blob_id: gix::ObjectId,
    cache: &mut std::collections::HashMap<gix::ObjectId, Option<String>>,
) -> Option<String> {
    if let Some(hit) = cache.get(&blob_id) {
        return hit.clone();
    }
    let id = repo
        .find_object(blob_id)
        .ok()
        .and_then(|o| serde_json::from_slice::<Session>(&o.detach().data).ok())
        .map(|s| s.id);
    cache.insert(blob_id, id.clone());
    id
}

/// Fold every local legacy ref (`refs/dkod/{sessions,commits,patchid}/*`) and
/// outbox spill into the index as ONE batched commit
/// (`dkod: reindex <n> legacy session(s)`). Body bytes are copied verbatim
/// (same content — no new storage, §10.2); link refs become pointer blobs.
/// Idempotent: already-present paths are skipped at the oid level, and a
/// fully-indexed repo produces no commit. Counts report what was MISSING from
/// the index before the run. On success, `.dkod/config.toml` gains
/// `format = "v2"` (best-effort).
pub fn reindex_legacy_refs(repo_path: &Path, dry_run: bool) -> Result<ReindexReport> {
    let mut repo = gix::open(repo_path).context("open repo")?;
    ensure_committer(&mut repo)?;

    // Snapshot the current index root (if any) so we can skip already-present
    // entries at the oid level.
    let root = index::index_tip(&repo).and_then(|tip| index::commit_tree(&repo, tip).ok());

    let mut batch = index::IndexBatch::new(String::new());
    let mut sessions = 0usize;
    let mut commit_links = 0usize;
    let mut patchid_links = 0usize;
    let mut blob_id_cache: std::collections::HashMap<gix::ObjectId, Option<String>> =
        std::collections::HashMap::new();

    // Sessions: fold each legacy session blob into the index. Skip when the
    // index already holds an identical body for the id. The oid-level check
    // works because `push_session_inserts` re-serializes the same `Session`
    // with serde_json's deterministic output, so a re-run produces the same
    // blob oid the legacy ref points at — idempotent within a codebase version
    // (verified by the e2e reindex test).
    for r in repo
        .references()
        .context("list refs")?
        .prefixed("refs/dkod/sessions/")
        .context("filter session refs")?
    {
        let r = r
            .map_err(|e| anyhow::anyhow!(e))
            .context("walk session ref")?;
        let blob_id = r.id().detach();
        let Some(session) = repo
            .find_object(blob_id)
            .ok()
            .and_then(|o| serde_json::from_slice::<Session>(&o.detach().data).ok())
        else {
            eprintln!(
                "dkod reindex: skipping unparseable session ref {}",
                r.name().as_bstr()
            );
            continue;
        };
        if index_body_oid(&repo, root, &session.id) == Some(blob_id) {
            continue;
        }
        push_session_inserts(&mut batch, &session)?;
        sessions += 1;
    }

    // Commit links: ref-name suffix is the sha; pointer blob names the session.
    for r in repo
        .references()
        .context("list refs")?
        .prefixed("refs/dkod/commits/")
        .context("filter commit refs")?
    {
        let r = r
            .map_err(|e| anyhow::anyhow!(e))
            .context("walk commit ref")?;
        let name = r.name().as_bstr().to_string();
        let Some(sha) = name.strip_prefix("refs/dkod/commits/") else {
            continue;
        };
        if sha.len() < 3 {
            continue;
        }
        let Some(id) = legacy_pointer_session_id(&repo, r.id().detach(), &mut blob_id_cache) else {
            continue;
        };
        let path = index::commit_pointer_path(sha);
        if root
            .and_then(|root| index::blob_oid_at(&repo, root, &path))
            .is_some()
        {
            continue;
        }
        batch.insert(path, pointer_blob(&id));
        commit_links += 1;
    }

    // Patch-id links: same shape against `refs/dkod/patchid/*`.
    for r in repo
        .references()
        .context("list refs")?
        .prefixed("refs/dkod/patchid/")
        .context("filter patchid refs")?
    {
        let r = r
            .map_err(|e| anyhow::anyhow!(e))
            .context("walk patchid ref")?;
        let name = r.name().as_bstr().to_string();
        let Some(pid) = name.strip_prefix("refs/dkod/patchid/") else {
            continue;
        };
        if pid.len() < 3 {
            continue;
        }
        let Some(id) = legacy_pointer_session_id(&repo, r.id().detach(), &mut blob_id_cache) else {
            continue;
        };
        let path = index::patchid_pointer_path(pid);
        if root
            .and_then(|root| index::blob_oid_at(&repo, root, &path))
            .is_some()
        {
            continue;
        }
        batch.insert(path, pointer_blob(&id));
        patchid_links += 1;
    }

    // Fold any spilled outbox sessions into the same batch.
    let consumed = fold_outbox(repo_path, &mut batch);

    if dry_run {
        return Ok(ReindexReport {
            sessions,
            commit_links,
            patchid_links,
            dry_run_only: true,
        });
    }

    batch.message = format!("dkod: reindex {sessions} legacy session(s)");
    index::apply_batch(repo_path, &batch).context("reindex apply batch")?;
    for p in &consumed {
        let _ = std::fs::remove_file(p);
    }

    // Stamp `format = "v2"` into `.dkod/config.toml` (best-effort).
    if let Err(e) = stamp_storage_format_v2(repo_path) {
        eprintln!("dkod reindex: could not stamp storage format ({e:#})");
    }

    Ok(ReindexReport {
        sessions,
        commit_links,
        patchid_links,
        dry_run_only: false,
    })
}

/// Set `[storage] format = "v2"` in `<repo>/.dkod/config.toml`, preserving any
/// existing config. Best-effort signal that `dkod reindex` has run (§10).
fn stamp_storage_format_v2(repo_path: &Path) -> Result<()> {
    let dir = repo_path.join(".dkod");
    let path = dir.join("config.toml");
    let mut cfg: crate::config::Config = std::fs::read_to_string(&path)
        .ok()
        .and_then(|body| toml::from_str(&body).ok())
        .unwrap_or_default();
    cfg.storage.format = Some("v2".into());
    std::fs::create_dir_all(&dir).context("create .dkod dir")?;
    let body = toml::to_string_pretty(&cfg).context("serialize config")?;
    std::fs::write(&path, body).context("write .dkod/config.toml")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Agent, Message, Session};
    use tempfile::TempDir;

    fn fixture_session() -> Session {
        Session {
            id: Session::new_id(),
            agent: Agent::Codex,
            created_at: 1735689600,
            duration_ms: 100,
            prompt_summary: "fix bug".into(),
            messages: vec![Message::user("fix bug")],
            commits: vec![],
            files_touched: vec![],
            redaction_count: 0,
        }
    }

    #[test]
    fn write_then_read_session() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();

        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();
        let back = read_session(tmp.path(), &s.id).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn write_creates_session_ref() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();

        let repo = gix::open(tmp.path()).unwrap();
        let r = repo
            .find_reference(&crate::refs::session_ref(&s.id))
            .unwrap();
        // The ref points at a blob; the blob's id is a 40-char SHA-1 hex.
        assert_eq!(r.id().to_hex().to_string().len(), 40);
    }

    #[test]
    fn link_session_to_commit_writes_ref_pointing_at_session_blob() {
        use gix::ObjectId;

        let tmp = TempDir::new().unwrap();
        let mut repo = gix::init(tmp.path()).unwrap();
        // CI runners have no global git config, so the test repo inherits no
        // committer identity. `commit_as` writes a HEAD reflog and gix needs a
        // committer to sign the entry — apply the same fallback the production
        // code uses so this test runs in any environment.
        super::ensure_committer(&mut repo).unwrap();

        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();

        // Create a real commit so we have a real sha. Empty tree commit.
        let empty_tree_id: ObjectId = repo.empty_tree().id().into();
        let sig = gix::actor::SignatureRef {
            name: "test".into(),
            email: "t@example.com".into(),
            time: gix::date::Time::now_utc(),
        };
        let commit_id = repo
            .commit_as(
                sig,
                sig,
                "HEAD",
                "init",
                empty_tree_id,
                Vec::<ObjectId>::new(),
            )
            .unwrap()
            .detach();

        link_session_to_commit(tmp.path(), &s.id, &commit_id.to_string()).unwrap();

        // The new commit-link ref must point at the SAME blob the session ref points at.
        let session_ref = repo
            .find_reference(&crate::refs::session_ref(&s.id))
            .unwrap();
        let commit_ref = repo
            .find_reference(&crate::refs::commit_ref(&commit_id.to_string()))
            .unwrap();
        assert_eq!(session_ref.id(), commit_ref.id());
    }

    #[test]
    fn list_sessions_returns_all_written() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();

        let mut ids: Vec<String> = (0..3)
            .map(|_| {
                let mut s = fixture_session();
                s.id = Session::new_id();
                // ensure ids are distinct even on fast machines (uuid::now_v7 has ms resolution)
                std::thread::sleep(std::time::Duration::from_millis(2));
                let id = s.id.clone();
                write_session(tmp.path(), &s).unwrap();
                id
            })
            .collect();
        ids.sort();

        let mut listed = list_sessions(tmp.path()).unwrap();
        listed.sort();
        assert_eq!(ids, listed);
    }

    #[test]
    fn new_commits_since_returns_commits_after_start() {
        use gix::ObjectId;
        let tmp = TempDir::new().unwrap();
        let mut repo = gix::init(tmp.path()).unwrap();
        super::ensure_committer(&mut repo).unwrap();

        let sig = gix::actor::SignatureRef {
            name: "t".into(),
            email: "t@e.com".into(),
            time: gix::date::Time::now_utc(),
        };
        let tree: gix::ObjectId = repo.empty_tree().id().into();

        let a = repo
            .commit_as(sig, sig, "HEAD", "a", tree, Vec::<ObjectId>::new())
            .unwrap()
            .detach();
        let b = repo
            .commit_as(sig, sig, "HEAD", "b", tree, vec![a])
            .unwrap()
            .detach();
        let c = repo
            .commit_as(sig, sig, "HEAD", "c", tree, vec![b])
            .unwrap()
            .detach();

        let got = super::new_commits_since(tmp.path(), Some(&a.to_string())).unwrap();
        let set: std::collections::BTreeSet<String> = got.into_iter().collect();
        assert_eq!(set, [b.to_string(), c.to_string()].into_iter().collect());
    }

    #[test]
    fn new_commits_since_none_start_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let mut repo = gix::init(tmp.path()).unwrap();
        super::ensure_committer(&mut repo).unwrap();
        let sig = gix::actor::SignatureRef {
            name: "t".into(),
            email: "t@e.com".into(),
            time: gix::date::Time::now_utc(),
        };
        let tree: gix::ObjectId = repo.empty_tree().id().into();
        repo.commit_as(sig, sig, "HEAD", "a", tree, Vec::<gix::ObjectId>::new())
            .unwrap();
        assert!(super::new_commits_since(tmp.path(), None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn new_commits_since_no_new_commits_returns_empty() {
        use gix::ObjectId;
        let tmp = TempDir::new().unwrap();
        let mut repo = gix::init(tmp.path()).unwrap();
        super::ensure_committer(&mut repo).unwrap();
        let sig = gix::actor::SignatureRef {
            name: "t".into(),
            email: "t@e.com".into(),
            time: gix::date::Time::now_utc(),
        };
        let tree: gix::ObjectId = repo.empty_tree().id().into();
        let a = repo
            .commit_as(sig, sig, "HEAD", "a", tree, Vec::<ObjectId>::new())
            .unwrap()
            .detach();
        assert!(super::new_commits_since(tmp.path(), Some(&a.to_string()))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn write_session_with_commit_links_populates_commits_and_links() {
        use gix::ObjectId;
        let tmp = TempDir::new().unwrap();
        let mut repo = gix::init(tmp.path()).unwrap();
        super::ensure_committer(&mut repo).unwrap();
        let sig = gix::actor::SignatureRef {
            name: "t".into(),
            email: "t@e.com".into(),
            time: gix::date::Time::now_utc(),
        };
        let tree: gix::ObjectId = repo.empty_tree().id().into();

        // start HEAD = commit A
        let a = repo
            .commit_as(sig, sig, "HEAD", "a", tree, Vec::<ObjectId>::new())
            .unwrap()
            .detach();
        // then the session "produces" commit B
        let b = repo
            .commit_as(sig, sig, "HEAD", "b", tree, vec![a])
            .unwrap()
            .detach();

        let mut s = fixture_session();
        let linked =
            super::write_session_with_commit_links(tmp.path(), &mut s, Some(&a.to_string()))
                .unwrap();
        assert_eq!(linked, vec![b.to_string()]);
        assert_eq!(s.commits, vec![b.to_string()]);

        // session blob carries the commit; reading back confirms persistence
        let back = super::read_session(tmp.path(), &s.id).unwrap();
        assert_eq!(back.commits, vec![b.to_string()]);

        // commit-link ref exists and points at the SAME blob as the session ref
        let repo = gix::open(tmp.path()).unwrap();
        let session_ref = repo
            .find_reference(&crate::refs::session_ref(&s.id))
            .unwrap();
        let commit_ref = repo
            .find_reference(&crate::refs::commit_ref(&b.to_string()))
            .unwrap();
        assert_eq!(session_ref.id(), commit_ref.id());
    }

    #[test]
    fn write_session_with_commit_links_none_start_links_nothing() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let mut s = fixture_session();
        let linked = super::write_session_with_commit_links(tmp.path(), &mut s, None).unwrap();
        assert!(linked.is_empty());
        assert!(s.commits.is_empty());
        // session is still written even when nothing is linked
        assert_eq!(super::read_session(tmp.path(), &s.id).unwrap().id, s.id);
    }

    #[test]
    fn head_sha_returns_none_for_non_repo() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(head_sha(tmp.path()), None);
    }

    #[test]
    fn head_sha_returns_none_for_unborn_repo() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        assert_eq!(head_sha(tmp.path()), None);
    }

    #[test]
    fn head_sha_returns_sha_after_commit() {
        use gix::ObjectId;

        let tmp = TempDir::new().unwrap();
        let mut repo = gix::init(tmp.path()).unwrap();
        super::ensure_committer(&mut repo).unwrap();
        let sig = gix::actor::SignatureRef {
            name: "test".into(),
            email: "t@example.com".into(),
            time: gix::date::Time::now_utc(),
        };
        let tree: gix::ObjectId = repo.empty_tree().id().into();
        let commit_id = repo
            .commit_as(sig, sig, "HEAD", "init", tree, Vec::<ObjectId>::new())
            .unwrap()
            .detach();

        let sha = head_sha(tmp.path()).expect("head_sha after commit");
        assert_eq!(sha, commit_id.to_string());
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn new_commits_since_non_ancestor_start_returns_empty() {
        use gix::ObjectId;
        let tmp = TempDir::new().unwrap();
        let mut repo = gix::init(tmp.path()).unwrap();
        super::ensure_committer(&mut repo).unwrap();
        let sig = gix::actor::SignatureRef {
            name: "t".into(),
            email: "t@e.com".into(),
            time: gix::date::Time::now_utc(),
        };
        let tree: gix::ObjectId = repo.empty_tree().id().into();
        // Real commit on HEAD so head_id resolves.
        repo.commit_as(sig, sig, "HEAD", "a", tree, Vec::<ObjectId>::new())
            .unwrap();
        // A syntactically valid 40-hex SHA that is not in this repo, so it is
        // not an ancestor of HEAD. Must attribute nothing, not all of history.
        let stale = "0123456789abcdef0123456789abcdef01234567";
        assert!(super::new_commits_since(tmp.path(), Some(stale))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn relink_commit_repoints_new_to_same_blob() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();

        let old = "0000000000000000000000000000000000000001";
        let new = "0000000000000000000000000000000000000002";
        link_session_to_commit(tmp.path(), &s.id, old).unwrap();

        let did = relink_commit(tmp.path(), old, new).unwrap();
        assert!(did, "relink should report it acted");

        let repo = gix::open(tmp.path()).unwrap();
        let old_ref = repo.find_reference(&crate::refs::commit_ref(old)).unwrap();
        let new_ref = repo.find_reference(&crate::refs::commit_ref(new)).unwrap();
        let sess_ref = repo
            .find_reference(&crate::refs::session_ref(&s.id))
            .unwrap();
        assert_eq!(new_ref.id(), old_ref.id());
        assert_eq!(new_ref.id(), sess_ref.id());
    }

    #[test]
    fn relink_commit_absent_old_returns_false() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let new = "0000000000000000000000000000000000000002";
        let did =
            relink_commit(tmp.path(), "0000000000000000000000000000000000000001", new).unwrap();
        assert!(!did, "absent old ref means nothing to relink");
        let repo = gix::open(tmp.path()).unwrap();
        assert!(repo.find_reference(&crate::refs::commit_ref(new)).is_err());
    }

    #[test]
    fn relink_commit_keeps_old_ref() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();
        let old = "0000000000000000000000000000000000000001";
        let new = "0000000000000000000000000000000000000002";
        link_session_to_commit(tmp.path(), &s.id, old).unwrap();
        relink_commit(tmp.path(), old, new).unwrap();
        let repo = gix::open(tmp.path()).unwrap();
        assert!(repo.find_reference(&crate::refs::commit_ref(old)).is_ok());
    }

    #[test]
    fn relink_commit_last_writer_wins() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let a = fixture_session();
        let mut b = fixture_session();
        b.id = Session::new_id();
        write_session(tmp.path(), &a).unwrap();
        write_session(tmp.path(), &b).unwrap();

        let old1 = "0000000000000000000000000000000000000011";
        let old2 = "0000000000000000000000000000000000000022";
        let new = "0000000000000000000000000000000000000099";
        link_session_to_commit(tmp.path(), &a.id, old1).unwrap();
        link_session_to_commit(tmp.path(), &b.id, old2).unwrap();

        relink_commit(tmp.path(), old1, new).unwrap();
        relink_commit(tmp.path(), old2, new).unwrap();

        let repo = gix::open(tmp.path()).unwrap();
        let new_ref = repo.find_reference(&crate::refs::commit_ref(new)).unwrap();
        let b_ref = repo
            .find_reference(&crate::refs::session_ref(&b.id))
            .unwrap();
        assert_eq!(new_ref.id(), b_ref.id(), "last relink (sessionB) must win");
    }

    #[test]
    fn link_session_to_patchid_writes_ref_pointing_at_session_blob() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();

        let pid = "1111111111111111111111111111111111111111";
        link_session_to_patchid(tmp.path(), &s.id, pid).unwrap();

        let repo = gix::open(tmp.path()).unwrap();
        let pid_ref = repo.find_reference(&crate::refs::patchid_ref(pid)).unwrap();
        let sess_ref = repo
            .find_reference(&crate::refs::session_ref(&s.id))
            .unwrap();
        assert_eq!(
            pid_ref.id(),
            sess_ref.id(),
            "patchid ref must point at the session blob"
        );
    }

    #[test]
    fn link_session_to_patchid_last_writer_wins() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let a = fixture_session();
        let mut b = fixture_session();
        b.id = Session::new_id();
        write_session(tmp.path(), &a).unwrap();
        write_session(tmp.path(), &b).unwrap();

        let pid = "2222222222222222222222222222222222222222";
        link_session_to_patchid(tmp.path(), &a.id, pid).unwrap();
        link_session_to_patchid(tmp.path(), &b.id, pid).unwrap();

        let repo = gix::open(tmp.path()).unwrap();
        let pid_ref = repo.find_reference(&crate::refs::patchid_ref(pid)).unwrap();
        let b_ref = repo
            .find_reference(&crate::refs::session_ref(&b.id))
            .unwrap();
        assert_eq!(pid_ref.id(), b_ref.id(), "last writer (sessionB) must win");
    }

    fn index_root(repo_path: &std::path::Path) -> (gix::Repository, gix::ObjectId) {
        let repo = gix::open(repo_path).unwrap();
        let tip = crate::index::index_tip(&repo).expect("index ref must exist");
        let root = crate::index::commit_tree(&repo, tip).unwrap();
        (repo, root)
    }

    #[test]
    fn write_session_also_writes_index_meta_and_body() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();

        let (repo, root) = index_root(tmp.path());
        let body =
            crate::index::read_tree_blob(&repo, root, &crate::index::body_path(&s.id).unwrap())
                .expect("body.json present");
        let back: Session = serde_json::from_slice(&body).unwrap();
        assert_eq!(back, s, "body.json is the exact Session JSON");
        let meta =
            crate::index::read_tree_blob(&repo, root, &crate::index::meta_path(&s.id).unwrap())
                .expect("meta.json present");
        let meta: crate::SessionMeta = serde_json::from_slice(&meta).unwrap();
        assert_eq!(meta.id, s.id);
        assert_eq!(meta.body_bytes, body.len() as u64);
        // legacy ref still written (dual-write default ON)
        assert!(repo
            .find_reference(&crate::refs::session_ref(&s.id))
            .is_ok());
    }

    #[test]
    fn write_session_with_legacy_disabled_writes_index_only() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        std::fs::create_dir_all(tmp.path().join(".dkod")).unwrap();
        std::fs::write(
            tmp.path().join(".dkod/config.toml"),
            "[storage]\nwrite_legacy_refs = false\n",
        )
        .unwrap();
        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();
        let repo = gix::open(tmp.path()).unwrap();
        assert!(
            repo.find_reference(&crate::refs::session_ref(&s.id))
                .is_err(),
            "no legacy ref when dual-write is off"
        );
        assert_eq!(
            read_session(tmp.path(), &s.id).unwrap(),
            s,
            "index read path serves it"
        );
    }

    #[test]
    fn write_session_full_batches_session_links_and_patchids_into_one_commit() {
        use gix::ObjectId;
        let tmp = TempDir::new().unwrap();
        let mut repo = gix::init(tmp.path()).unwrap();
        super::ensure_committer(&mut repo).unwrap();
        let sig = gix::actor::SignatureRef {
            name: "t".into(),
            email: "t@e.com".into(),
            time: gix::date::Time::now_utc(),
        };
        let tree: gix::ObjectId = repo.empty_tree().id().into();
        let a = repo
            .commit_as(sig, sig, "HEAD", "a", tree, Vec::<ObjectId>::new())
            .unwrap()
            .detach();

        let mut s = fixture_session();
        let commits = vec![a.to_string()];
        let pid = "1111111111111111111111111111111111111111".to_string();
        let linked = write_session_full(
            tmp.path(),
            &mut s,
            &commits,
            &[(a.to_string(), pid.clone())],
        )
        .unwrap();
        assert_eq!(linked, vec![a.to_string()]);
        assert_eq!(s.commits, vec![a.to_string()]);

        let (repo, root) = index_root(tmp.path());
        // pointer blobs contain "<session-id>\n"
        let want = format!("{}\n", s.id).into_bytes();
        assert_eq!(
            crate::index::read_tree_blob(
                &repo,
                root,
                &crate::index::commit_pointer_path(&a.to_string())
            )
            .unwrap(),
            want
        );
        assert_eq!(
            crate::index::read_tree_blob(&repo, root, &crate::index::patchid_pointer_path(&pid))
                .unwrap(),
            want
        );
        // exactly ONE index commit was written for the whole finalize (§5.1):
        let tip = crate::index::index_tip(&repo).unwrap();
        let c = repo.find_object(tip).unwrap().try_into_commit().unwrap();
        assert_eq!(
            c.parent_ids().count(),
            0,
            "session+links land in a single root commit"
        );
        // legacy link refs still written (dual-write ON)
        assert!(repo
            .find_reference(&crate::refs::commit_ref(&a.to_string()))
            .is_ok());
        assert!(repo.find_reference(&crate::refs::patchid_ref(&pid)).is_ok());
    }

    #[test]
    fn outbox_sessions_fold_into_next_write() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        // simulate a spilled session (apply_batch exhaustion, §5.3)
        let spilled = fixture_session();
        spill_to_outbox(tmp.path(), &spilled).unwrap();
        let outbox = tmp
            .path()
            .join(".git/dkod/outbox")
            .join(format!("{}.json", spilled.id));
        assert!(outbox.exists());

        let next = {
            let mut s = fixture_session();
            s.id = Session::new_id();
            s
        };
        write_session(tmp.path(), &next).unwrap();
        assert_eq!(
            read_session(tmp.path(), &spilled.id).unwrap(),
            spilled,
            "folded into the index"
        );
        assert!(!outbox.exists(), "outbox entry consumed");
    }

    #[test]
    fn relink_commits_batches_pairs_into_one_index_commit() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();
        let old1 = "0000000000000000000000000000000000000001";
        let old2 = "0000000000000000000000000000000000000002";
        link_session_to_commit(tmp.path(), &s.id, old1).unwrap();
        link_session_to_commit(tmp.path(), &s.id, old2).unwrap();
        let repo = gix::open(tmp.path()).unwrap();
        let tip_before = crate::index::index_tip(&repo).unwrap();

        let new1 = "000000000000000000000000000000000000000a";
        let new2 = "000000000000000000000000000000000000000b";
        let n = relink_commits(
            tmp.path(),
            &[
                (old1.to_string(), new1.to_string()),
                (old2.to_string(), new2.to_string()),
            ],
        )
        .unwrap();
        assert_eq!(n, 2);
        let repo = gix::open(tmp.path()).unwrap();
        let tip_after = crate::index::index_tip(&repo).unwrap();
        let c = repo
            .find_object(tip_after)
            .unwrap()
            .try_into_commit()
            .unwrap();
        let parents: Vec<_> = c.parent_ids().map(|p| p.detach()).collect();
        assert_eq!(parents, vec![tip_before], "all pairs in ONE new commit");
        // old paths kept, new paths added (undo-friendly, §5.2)
        let root = crate::index::commit_tree(&repo, tip_after).unwrap();
        for sha in [old1, old2, new1, new2] {
            assert!(
                crate::index::read_tree_blob(&repo, root, &crate::index::commit_pointer_path(sha))
                    .is_some(),
                "pointer for {sha} missing"
            );
        }
        // legacy new refs written too (dual-write ON)
        assert!(repo.find_reference(&crate::refs::commit_ref(new1)).is_ok());
    }

    #[test]
    fn read_session_falls_back_to_legacy_ref_when_index_lacks_it() {
        // plumbing-style legacy-only repo: blob + session ref, NO index commit —
        // exactly what benchmarks/drift/run.sh produces.
        let tmp = TempDir::new().unwrap();
        let repo = gix::init(tmp.path()).unwrap();
        let s = fixture_session();
        let bytes = serde_json::to_vec(&s).unwrap();
        let blob = repo.write_blob(&bytes).unwrap().detach();
        use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
        use gix::refs::Target;
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "seed".into(),
                },
                expected: PreviousValue::Any,
                new: Target::Object(blob),
            },
            name: crate::refs::session_ref(&s.id).try_into().unwrap(),
            deref: false,
        })
        .unwrap();

        assert!(
            crate::index::index_tip(&repo).is_none(),
            "no index ref in this fixture"
        );
        assert_eq!(read_session(tmp.path(), &s.id).unwrap(), s);
        assert_eq!(list_sessions(tmp.path()).unwrap(), vec![s.id.clone()]);
    }

    #[test]
    fn read_session_finds_non_v7_id_via_date_scan() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        std::fs::create_dir_all(tmp.path().join(".dkod")).unwrap();
        std::fs::write(
            tmp.path().join(".dkod/config.toml"),
            "[storage]\nwrite_legacy_refs = false\n", // index-only: forces the scan path
        )
        .unwrap();
        let mut s = fixture_session();
        s.id = "foreign-import-001".into();
        s.created_at = 1735689600;
        write_session(tmp.path(), &s).unwrap();
        assert_eq!(read_session(tmp.path(), &s.id).unwrap(), s);
        assert_eq!(list_sessions(tmp.path()).unwrap(), vec![s.id.clone()]);
    }

    #[test]
    fn list_sessions_unions_index_and_legacy_dedup_by_id() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let s = fixture_session(); // dual-write ON → in BOTH index and legacy refs
        write_session(tmp.path(), &s).unwrap();
        assert_eq!(
            list_sessions(tmp.path()).unwrap(),
            vec![s.id.clone()],
            "no duplicate"
        );
    }

    #[test]
    fn lookup_commit_session_resolves_via_index_pointer() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let mut s = fixture_session();
        let sha = "00000000000000000000000000000000000000aa".to_string();
        write_session_full(tmp.path(), &mut s, std::slice::from_ref(&sha), &[]).unwrap();
        let meta = lookup_commit_session(tmp.path(), &sha).expect("resolved");
        assert_eq!(meta.id, s.id);
        assert_eq!(meta.prompt_summary, s.prompt_summary);
    }

    #[test]
    fn lookup_commit_session_falls_back_to_legacy_ref() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let s = fixture_session();
        // legacy-only seeding (write_session_legacy path): use the public fns,
        // then delete the index ref to simulate a legacy-only repo.
        write_session(tmp.path(), &s).unwrap();
        link_session_to_commit(
            tmp.path(),
            &s.id,
            "00000000000000000000000000000000000000bb",
        )
        .unwrap();
        let repo = gix::open(tmp.path()).unwrap();
        repo.find_reference(crate::index::INDEX_REF)
            .unwrap()
            .delete()
            .unwrap();
        let meta = lookup_commit_session(tmp.path(), "00000000000000000000000000000000000000bb")
            .expect("legacy fallback");
        assert_eq!(meta.id, s.id);
    }

    #[test]
    fn lookup_patchid_session_resolves_index_then_legacy() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let mut s = fixture_session();
        let sha = "00000000000000000000000000000000000000cc".to_string();
        let pid = "2222222222222222222222222222222222222222".to_string();
        write_session_full(
            tmp.path(),
            &mut s,
            std::slice::from_ref(&sha),
            &[(sha.clone(), pid.clone())],
        )
        .unwrap();
        assert_eq!(lookup_patchid_session(tmp.path(), &pid).unwrap().id, s.id);
        let repo = gix::open(tmp.path()).unwrap();
        repo.find_reference(crate::index::INDEX_REF)
            .unwrap()
            .delete()
            .unwrap();
        assert_eq!(
            lookup_patchid_session(tmp.path(), &pid).unwrap().id,
            s.id,
            "legacy fallback"
        );
    }

    #[test]
    fn read_session_meta_prefers_index_and_derives_from_legacy() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();
        let m = read_session_meta(tmp.path(), &s.id).unwrap();
        assert_eq!(m.id, s.id);
        assert!(m.body_bytes > 0);
        let repo = gix::open(tmp.path()).unwrap();
        repo.find_reference(crate::index::INDEX_REF)
            .unwrap()
            .delete()
            .unwrap();
        let m2 = read_session_meta(tmp.path(), &s.id).unwrap();
        assert_eq!(m2.id, s.id, "derived from the legacy blob");
    }

    #[test]
    fn reindex_legacy_refs_folds_sessions_and_links_idempotently() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        // Pure-legacy repo: disable index writes is not possible via public API,
        // so seed via the legacy halves: write_session + links, then delete the
        // index ref so only legacy refs remain.
        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();
        let sha = "00000000000000000000000000000000000000dd";
        let pid = "3333333333333333333333333333333333333333";
        link_session_to_commit(tmp.path(), &s.id, sha).unwrap();
        link_session_to_patchid(tmp.path(), &s.id, pid).unwrap();
        let repo = gix::open(tmp.path()).unwrap();
        repo.find_reference(crate::index::INDEX_REF)
            .unwrap()
            .delete()
            .unwrap();

        let report = reindex_legacy_refs(tmp.path(), false).unwrap();
        assert_eq!(report.sessions, 1);
        assert_eq!(report.commit_links, 1);
        assert_eq!(report.patchid_links, 1);
        assert!(!report.dry_run_only);

        // session readable from the index alone
        let repo = gix::open(tmp.path()).unwrap();
        let tip = crate::index::index_tip(&repo).expect("index rebuilt");
        let root = crate::index::commit_tree(&repo, tip).unwrap();
        // body bytes verbatim → same content as the legacy blob (§10.2)
        let body =
            crate::index::read_tree_blob(&repo, root, &crate::index::body_path(&s.id).unwrap())
                .unwrap();
        assert_eq!(serde_json::from_slice::<Session>(&body).unwrap(), s);

        // idempotent: second run writes nothing
        let again = reindex_legacy_refs(tmp.path(), false).unwrap();
        assert_eq!(again.sessions, 0, "already indexed → no new inserts");
        assert_eq!(
            crate::index::index_tip(&gix::open(tmp.path()).unwrap()),
            Some(tip),
            "no empty commit"
        );
    }

    #[test]
    fn reindex_dry_run_reports_without_writing() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();
        let repo = gix::open(tmp.path()).unwrap();
        repo.find_reference(crate::index::INDEX_REF)
            .unwrap()
            .delete()
            .unwrap();

        let report = reindex_legacy_refs(tmp.path(), true).unwrap();
        assert_eq!(report.sessions, 1);
        assert!(report.dry_run_only);
        assert!(
            crate::index::index_tip(&gix::open(tmp.path()).unwrap()).is_none(),
            "dry run wrote nothing"
        );
    }

    #[test]
    fn reindex_is_idempotent_for_non_v7_foreign_ids() {
        // A foreign (non-v7) id lands under sessions/<date>/<id> via created_at;
        // its body.json path is NOT computable from the id alone, so the
        // idempotency check must scan date dirs — a second run must fold 0.
        let tmp = TempDir::new().unwrap();
        let repo = gix::init(tmp.path()).unwrap();
        let mut s = fixture_session();
        s.id = "foreign-import-xyz".into();
        s.created_at = 1735689600;
        let blob = repo
            .write_blob(serde_json::to_vec(&s).unwrap())
            .unwrap()
            .detach();
        use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
        use gix::refs::Target;
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "seed".into(),
                },
                expected: PreviousValue::Any,
                new: Target::Object(blob),
            },
            name: crate::refs::session_ref(&s.id).try_into().unwrap(),
            deref: false,
        })
        .unwrap();

        let first = reindex_legacy_refs(tmp.path(), false).unwrap();
        assert_eq!(first.sessions, 1);
        let again = reindex_legacy_refs(tmp.path(), false).unwrap();
        assert_eq!(again.sessions, 0, "non-v7 id must be detected as present");
    }
}
