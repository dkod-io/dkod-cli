use anyhow::{anyhow, Context, Result};
use std::path::Path;

pub fn run(cwd: &Path) -> Result<()> {
    // 1. Ensure we're inside a git repo.
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    // 2. If a config already exists, validate it (via load_config); otherwise
    //    write a default and validate that.
    let dir = cwd.join(".dkod");
    std::fs::create_dir_all(&dir).context("create .dkod/")?;
    let path = dir.join("config.toml");

    if !path.exists() {
        let cfg = dkod_core::config::Config::default();
        let body = toml::to_string_pretty(&cfg).context("serialize default config")?;
        std::fs::write(&path, body).context("write .dkod/config.toml")?;
    }

    // load_config performs the custom-regex validation step.
    let _cfg = super::load_config(cwd)?;
    Ok(())
}
