use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

pub fn run(cwd: &Path, args: Vec<String>) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    let cfg = super::super::load_config(cwd)?;

    let codex_bin: PathBuf = std::env::var_os("DKOD_CODEX_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("codex"));
    let codex_home: PathBuf = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
        .ok_or_else(|| anyhow!("could not resolve CODEX_HOME or $HOME/.codex"))?;

    let mut session =
        dkod_core::capture::codex::capture_codex(dkod_core::capture::codex::CaptureOptions {
            args,
            codex_bin,
            codex_home,
            cwd: cwd.to_path_buf(),
        })
        .context("capture codex session")?;

    dkod_core::redact::redact_session(&mut session, &cfg.redact);
    dkod_core::store::write_session(cwd, &session).context("write session")?;
    eprintln!("dkod: captured session {}", session.id);
    Ok(())
}
