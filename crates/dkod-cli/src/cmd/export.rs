//! `dkod export agent-trace` — emit stored sessions as Agent Trace records
//! (the open attribution format at <https://github.com/cursor/agent-trace>).
//!
//! All mapping logic lives in `dkod_core::trace`; this is a thin wrapper that
//! resolves sessions and decides between stdout (`--out -`, single session
//! only) and one-JSON-file-per-session under an output directory.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

/// Export `session_id` (or, when `None`, every session in the repo) as
/// Agent Trace JSON. `out` is an output directory (created if needed,
/// default `agent-traces`), or `-` for stdout — which requires a single
/// explicit session id so the stream stays one valid JSON document.
pub fn run(cwd: &Path, session_id: Option<&str>, out: &str) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    if out == "-" {
        let id = session_id.ok_or_else(|| {
            anyhow!("--out - writes a single record to stdout; pass a session id")
        })?;
        let s = dkod_core::store::read_session(cwd, id)
            .with_context(|| format!("read session {id}"))?;
        println!("{}", dkod_core::trace::trace_json(&s)?);
        return Ok(());
    }

    let ids = match session_id {
        Some(id) => vec![id.to_string()],
        None => dkod_core::store::list_sessions(cwd).context("list sessions")?,
    };
    if ids.is_empty() {
        println!("no sessions to export");
        return Ok(());
    }

    let dir = cwd.join(out);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create output dir {}", dir.display()))?;

    let mut written = 0usize;
    for id in &ids {
        let s = dkod_core::store::read_session(cwd, id)
            .with_context(|| format!("read session {id}"))?;
        let path = dir.join(format!("{}.json", s.id));
        let mut body = dkod_core::trace::trace_json(&s)?;
        body.push('\n');
        std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
        written += 1;
    }
    println!(
        "exported {written} Agent Trace record(s) to {}",
        dir.display()
    );
    Ok(())
}
