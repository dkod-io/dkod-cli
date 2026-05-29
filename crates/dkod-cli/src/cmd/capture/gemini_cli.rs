use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

pub fn run(cwd: &Path, args: Vec<String>) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    let cfg = super::super::load_config(cwd)?;

    // Record HEAD before the agent runs so we can link the commits it produces.
    let head_at_start = dkod_core::store::head_sha(cwd);

    let gemini_bin: PathBuf = std::env::var_os("DKOD_GEMINI_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("gemini"));

    let mut session = dkod_core::capture::gemini_cli::capture_gemini_cli(
        dkod_core::capture::gemini_cli::CaptureOptions {
            args,
            gemini_bin,
            cwd: cwd.to_path_buf(),
        },
    )
    .context("capture gemini-cli session")?;

    let linked = super::finalize_session(cwd, &mut session, head_at_start.as_deref(), &cfg)?;
    eprintln!(
        "dkod: captured session {} ({} commit link(s))",
        session.id,
        linked.len()
    );
    Ok(())
}
