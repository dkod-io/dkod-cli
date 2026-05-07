use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

pub fn run(cwd: &Path, args: Vec<String>) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    let cfg = super::super::load_config(cwd)?;

    let factory_bin: PathBuf = std::env::var_os("DKOD_FACTORY_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("droid"));

    let mut session = dkod_core::capture::factory_ai::capture_factory_ai(
        dkod_core::capture::factory_ai::CaptureOptions {
            args,
            factory_bin,
            cwd: cwd.to_path_buf(),
        },
    )
    .context("capture factory-ai session")?;

    dkod_core::redact::redact_session(&mut session, &cfg.redact);
    dkod_core::store::write_session(cwd, &session).context("write session")?;
    eprintln!("dkod: captured session {}", session.id);
    Ok(())
}
