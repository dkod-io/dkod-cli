pub mod claude_code;
pub mod codex;
pub mod copilot_cli;
pub mod cursor;
pub mod factory_ai;
pub mod gemini_cli;
pub mod hook;
pub mod opencode;

use anyhow::{Context, Result};

/// Redact, persist, and commit-link a freshly-captured session. The session
/// write is the only fatal step; commit-linking is best-effort (see
/// `dkod_core::store::write_session_with_commit_links`). `head_at_start` is the
/// repo HEAD recorded *before* the agent ran. Returns the shas successfully
/// linked (empty when there was no new commit / no recorded start HEAD).
pub fn finalize_session(
    cwd: &std::path::Path,
    session: &mut dkod_core::Session,
    head_at_start: Option<&str>,
    cfg: &dkod_core::config::Config,
) -> Result<Vec<String>> {
    dkod_core::redact::redact_session(session, &cfg.redact);
    let linked = dkod_core::store::write_session_with_commit_links(cwd, session, head_at_start)
        .context("write session")?;
    Ok(linked)
}
