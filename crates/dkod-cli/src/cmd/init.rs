use anyhow::{anyhow, Context, Result};
use std::path::Path;

pub fn run(cwd: &Path) -> Result<()> {
    // 1. Ensure we're inside a git repo.
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    // 2. If a config already exists, validate it; otherwise write a default.
    let dir = cwd.join(".dkod");
    std::fs::create_dir_all(&dir).context("create .dkod/")?;
    let path = dir.join("config.toml");

    let cfg: dkod_core::config::Config = if path.exists() {
        let body = std::fs::read_to_string(&path).context("read .dkod/config.toml")?;
        toml::from_str(&body).context("parse .dkod/config.toml")?
    } else {
        let cfg = dkod_core::config::Config::default();
        let body = toml::to_string_pretty(&cfg).context("serialize default config")?;
        std::fs::write(&path, body).context("write .dkod/config.toml")?;
        cfg
    };

    // 3. Validate every custom regex compiles. Refuse to proceed if any fail.
    let mut bad: Vec<String> = Vec::new();
    for pattern in &cfg.redact.custom {
        if let Err(e) = regex::Regex::new(pattern) {
            bad.push(format!("  invalid custom redact pattern {pattern:?}: {e}"));
        }
    }
    if !bad.is_empty() {
        let msg = format!(
            ".dkod/config.toml has invalid custom redact patterns:\n{}",
            bad.join("\n")
        );
        return Err(anyhow!(msg));
    }

    Ok(())
}
