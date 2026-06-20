pub mod claude_code;
pub mod codex;
pub mod copilot_cli;
pub mod cursor;
pub mod factory_ai;
pub mod gemini_cli;
pub mod hook;
pub mod opencode;

use anyhow::{Context, Result};

/// Redact, persist, and commit-link a freshly-captured session as ONE index
/// commit (storage-v2 §5.2/§16). Commits are discovered first so their
/// patch-id provenance lands in the same index commit as the session and its
/// commit links — no second commit per patch-id. The session write is the
/// only fatal step; commit discovery and patch-id computation are best-effort.
/// `head_at_start` is the repo HEAD recorded *before* the agent ran. Returns
/// the shas successfully linked (empty when there was no new commit / no
/// recorded start HEAD).
pub fn finalize_session(
    cwd: &std::path::Path,
    session: &mut dkod_core::Session,
    head_at_start: Option<&str>,
    cfg: &dkod_core::config::Config,
) -> Result<Vec<String>> {
    dkod_core::redact::redact_session(session, &cfg.redact);
    // Discover commits FIRST so patch-ids can land in the same index commit
    // as the session (storage-v2 §5.2/§16). Discovery is best-effort.
    let commits = dkod_core::store::new_commits_since(cwd, head_at_start).unwrap_or_default();
    let patch_ids: Vec<(String, String)> = commits
        .iter()
        .filter_map(|sha| {
            crate::cmd::patchid::compute_patch_id(cwd, sha).map(|pid| (sha.clone(), pid))
        })
        .collect();
    let linked = dkod_core::store::write_session_full(cwd, session, &commits, &patch_ids)
        .context("write session")?;
    Ok(linked)
}
