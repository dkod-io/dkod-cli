pub mod capture;
pub mod init;
pub mod log;
pub mod show;

use anyhow::{Context, Result};
use std::path::Path;

/// Load `.dkod/config.toml` from `cwd`. Returns `Config::default()` when the
/// file does not exist. Validates that every custom redact regex compiles.
pub(crate) fn load_config(cwd: &Path) -> Result<dkod_core::config::Config> {
    let path = cwd.join(".dkod/config.toml");
    let cfg: dkod_core::config::Config = if path.exists() {
        let body = std::fs::read_to_string(&path).context("read .dkod/config.toml")?;
        toml::from_str(&body).context("parse .dkod/config.toml")?
    } else {
        dkod_core::config::Config::default()
    };

    let mut bad: Vec<String> = Vec::new();
    for pattern in &cfg.redact.custom {
        if let Err(e) = regex::Regex::new(pattern) {
            bad.push(format!("  invalid custom redact pattern {pattern:?}: {e}"));
        }
    }
    if !bad.is_empty() {
        return Err(anyhow::anyhow!(
            ".dkod/config.toml has invalid custom redact patterns:\n{}",
            bad.join("\n")
        ));
    }

    Ok(cfg)
}
