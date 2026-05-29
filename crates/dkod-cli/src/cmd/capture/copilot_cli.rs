use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

pub fn run(cwd: &Path, args: Vec<String>) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    let cfg = super::super::load_config(cwd)?;

    // Record HEAD before the agent runs so we can link the commits it produces.
    let head_at_start = dkod_core::store::head_sha(cwd);

    let copilot_bin: PathBuf = std::env::var_os("DKOD_COPILOT_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("copilot"));
    let copilot_home: PathBuf = std::env::var_os("COPILOT_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".copilot")))
        .ok_or_else(|| anyhow!("could not resolve COPILOT_HOME or $HOME/.copilot"))?;

    let mut session = dkod_core::capture::copilot_cli::capture_copilot_cli(
        dkod_core::capture::copilot_cli::CaptureOptions {
            args,
            copilot_bin,
            copilot_home,
            cwd: cwd.to_path_buf(),
        },
    )
    .context("capture copilot-cli session")?;

    let linked = super::finalize_session(cwd, &mut session, head_at_start.as_deref(), &cfg)?;
    eprintln!(
        "dkod: captured session {} ({} commit link(s))",
        session.id,
        linked.len()
    );
    Ok(())
}
