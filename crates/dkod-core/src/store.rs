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

/// Serialize `session` as JSON, write it as a Git blob, and create the
/// `refs/dkod/sessions/<id>` reference pointing directly at that blob.
pub fn write_session(repo_path: &Path, session: &Session) -> Result<()> {
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
    session.commits = commits.clone();
    // The session write is the only fatal step — if it fails, the session is
    // not persisted and the caller must treat the flush as failed.
    write_session(repo_path, session)?;
    // Linking is best-effort and per-commit: a single failure neither aborts the
    // remaining links nor fails the call. Returns the shas successfully linked.
    let mut linked = Vec::new();
    for sha in &commits {
        if link_session_to_commit(repo_path, &session.id, sha).is_ok() {
            linked.push(sha.clone());
        }
    }
    Ok(linked)
}

/// Resolve `refs/dkod/sessions/<id>`, read the blob it points at, and
/// deserialize it back into a `Session`.
pub fn read_session(repo_path: &Path, id: &str) -> Result<Session> {
    let repo = gix::open(repo_path).context("open repo")?;
    let r = repo
        .find_reference(&refs::session_ref(id))
        .context("find session ref")?;
    let object = repo.find_object(r.id()).context("find object")?.detach();
    let session: Session = serde_json::from_slice(&object.data).context("deserialize session")?;
    Ok(session)
}

/// Write `refs/dkod/commits/<commit_sha>` pointing at the same blob the session ref points at.
/// Idempotent — overwrites any existing link ref for the same commit (UUID v7 makes session
/// id collisions implausible; commit shas are content-addressed, so overwrite-on-retry is safe).
pub fn link_session_to_commit(repo_path: &Path, session_id: &str, commit_sha: &str) -> Result<()> {
    use gix::refs::{
        transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
        Target,
    };

    let mut repo = gix::open(repo_path).context("open repo")?;
    ensure_committer(&mut repo)?;
    let session_ref = repo
        .find_reference(&refs::session_ref(session_id))
        .context("find session ref")?;
    let blob_id = session_ref.id().detach();

    let ref_name = refs::commit_ref(commit_sha);
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("dkod: link session {} to commit {}", session_id, commit_sha)
                    .into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(blob_id),
        },
        name: ref_name.try_into().context("invalid commit ref name")?,
        deref: false,
    })
    .context("edit commit ref")?;
    Ok(())
}

/// Enumerate all sessions stored under `refs/dkod/sessions/*` in this repo.
/// Returns the bare session ids (the part after the namespace prefix).
pub fn list_sessions(repo_path: &Path) -> Result<Vec<String>> {
    let repo = gix::open(repo_path).context("open repo")?;
    let mut ids = Vec::new();
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
            ids.push(id);
        }
    }
    Ok(ids)
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
}
