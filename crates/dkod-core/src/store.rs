use crate::{refs, Session};
use anyhow::{Context, Result};
use std::path::Path;

/// Serialize `session` as JSON, write it as a Git blob, and create the
/// `refs/dkod/sessions/<id>` reference pointing directly at that blob.
pub fn write_session(repo_path: &Path, session: &Session) -> Result<()> {
    use gix::refs::{
        transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
        Target,
    };

    let repo = gix::open(repo_path).context("open repo")?;
    let bytes = serde_json::to_vec(session).context("serialize session")?;
    let blob_id = repo.write_blob(&bytes).context("write blob")?.detach();
    let ref_name = refs::session_ref(&session.id);

    // gix 0.66: edit_reference + Target::Object pins a ref directly at a blob.
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "dkod: write session".into(),
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
}
