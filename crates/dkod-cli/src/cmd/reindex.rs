//! `dkod reindex` — fold legacy per-session refs into the rollup index
//! (storage-v2 §10). Idempotent; safe to re-run; `--delete-legacy` arrives in
//! Phase 4.

use anyhow::{anyhow, Result};
use std::path::Path;

pub fn run(cwd: &Path, dry_run: bool) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;
    let r = dkod_core::store::reindex_legacy_refs(cwd, dry_run)?;
    let verb = if r.dry_run_only {
        "would fold"
    } else {
        "folded"
    };
    println!(
        "dkod reindex: {verb} {} session(s), {} commit link(s), {} patch-id link(s) into refs/dkod/index",
        r.sessions, r.commit_links, r.patchid_links
    );
    if !r.dry_run_only {
        println!("dkod reindex: push it with `git push origin refs/dkod/index` (dkod push arrives in a later release)");
    }
    Ok(())
}
