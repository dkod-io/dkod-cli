use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

pub fn run(cwd: &Path, args: Vec<String>) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    let cfg = super::super::load_config(cwd)?;

    // Record HEAD before the agent runs so we can link the commits it produces.
    let head_at_start = dkod_core::store::head_sha(cwd);

    let factory_bin: PathBuf = std::env::var_os("DKOD_FACTORY_BIN")
        .filter(|v| !v.is_empty())
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

    let linked = super::finalize_session(cwd, &mut session, head_at_start.as_deref(), &cfg)?;
    eprintln!(
        "dkod: captured session {} ({} commit link(s))",
        session.id,
        linked.len()
    );
    Ok(())
}
