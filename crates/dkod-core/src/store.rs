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
}
