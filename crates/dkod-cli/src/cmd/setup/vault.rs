//! Vault repo manager for the seamless capture wizard.
//!
//! The vault is a personal git repo (default: `~/.dkod/vault`) where orphan
//! sessions land — sessions whose hook fired outside any git repo or in a
//! git repo the user explicitly opted out of auto-init. Nothing is ever
//! silently dropped: if it doesn't fit in a real project repo, it goes here.
//!
//! Writes themselves go through the existing
//! [`dkod_core::store::write_session`] path with the vault as the
//! destination — this module owns only the bootstrap (init / detect)
//! responsibility.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

/// Returns `true` iff `path` is an openable gitoxide repository.
pub fn is_initialized(path: &Path) -> bool {
    gix::open(path).is_ok()
}

/// Ensure a vault git repo exists at `path`.
///
/// * Path already a git repo → no-op.
/// * Path missing → create parent dirs, `gix::init` a fresh repo.
/// * Path present and empty → `gix::init` into it.
/// * Path present, non-empty, not a git repo → error. Refusing to clobber
///   directories whose contents we don't recognize is the whole point.
pub fn ensure(path: &Path) -> Result<()> {
    if is_initialized(path) {
        return Ok(());
    }

    if path.exists() {
        let mut entries = std::fs::read_dir(path)
            .with_context(|| format!("read vault dir {}", path.display()))?;
        if entries.next().is_some() {
            return Err(anyhow!(
                "vault path {} exists but is not a git repo and is not empty; \
                 refusing to initialize",
                path.display()
            ));
        }
    } else if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create vault parent dir {}", parent.display()))?;
        }
    }

    gix::init(path).with_context(|| format!("gix init vault at {}", path.display()))?;
    Ok(())
}
