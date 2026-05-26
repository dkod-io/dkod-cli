//! Codex (OpenAI CLI) installer — the explicit-consent shim case.
//!
//! Codex doesn't expose a hook API (Wave 0 spike). To capture sessions
//! automatically we install a PATH shim at `~/.dkod/bin/codex` that
//! `exec`s the real binary under `dkod capture codex --`. Shimming is
//! invasive — it intercepts a binary the user didn't ask us to wrap and
//! prepends to their shell rc — so we ask for explicit consent first.

use crate::cmd::setup::agents::{AgentInstaller, DetectedAgent, InstallContext, InstallOutcome};
use crate::cmd::setup::state::{fingerprint, AgentState, Consent, Scope};
use anyhow::{Context, Result};
use std::path::Path;

pub struct Codex;

const MANAGED_BLOCK_START: &str = "# >>> dkod managed block — do not edit";
const MANAGED_BLOCK_END: &str = "# <<< dkod managed block";
const MANAGED_BLOCK_BODY: &str = r#"export PATH="$HOME/.dkod/bin:$PATH""#;

impl AgentInstaller for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn detect(&self, home: &Path) -> Option<DetectedAgent> {
        // Codex stashes its state under `~/.codex/`. Either a config.toml
        // or the directory itself is enough to call the agent present.
        let cfg = home.join(".codex/config.toml");
        if cfg.exists() {
            return Some(DetectedAgent {
                name: self.name(),
                config_path: cfg,
                version: None,
            });
        }
        let dir = home.join(".codex");
        if dir.exists() {
            return Some(DetectedAgent {
                name: self.name(),
                config_path: dir,
                version: None,
            });
        }
        None
    }

    fn install(&self, ctx: &mut InstallContext, state: &mut AgentState) -> Result<InstallOutcome> {
        if self.detect(ctx.home).is_none() {
            return Ok(InstallOutcome::AgentNotPresent);
        }

        // Respect a previously-recorded permanent decline.
        if matches!(state.consent, Some(Consent::Never)) {
            return Ok(InstallOutcome::DeclinedByUser);
        }

        if ctx.non_interactive {
            state.consent = Some(Consent::SkippedNoninteractive);
            return Ok(InstallOutcome::SkippedNoninteractive);
        }

        let answer = ctx.prompter.ask(
            "Codex doesn't support hooks. Install a PATH shim at \
             ~/.dkod/bin/codex and add it to your shell?",
            &["y", "n", "never"],
        )?;
        match answer.as_str() {
            "y" => {
                install_shim(ctx.home, state)?;
                state.consent = Some(Consent::Yes);
                state.installed = true;
                state.scope = Some(Scope::User);
                Ok(InstallOutcome::Installed)
            }
            "never" => {
                state.consent = Some(Consent::Never);
                Ok(InstallOutcome::DeclinedByUser)
            }
            _ => {
                state.consent = Some(Consent::No);
                Ok(InstallOutcome::DeclinedByUser)
            }
        }
    }
}

/// Write `~/.dkod/bin/codex` + append the managed PATH block to whichever
/// shell rc files exist in `home`. Idempotent: re-installing detects the
/// block by sentinel and leaves it alone.
fn install_shim(home: &Path, state: &mut AgentState) -> Result<()> {
    let bin_dir = home.join(".dkod/bin");
    std::fs::create_dir_all(&bin_dir).with_context(|| format!("create {}", bin_dir.display()))?;
    let shim_path = bin_dir.join("codex");
    let shim_body = "#!/usr/bin/env bash\nexec dkod capture codex -- \"$@\"\n";
    std::fs::write(&shim_path, shim_body)
        .with_context(|| format!("write shim {}", shim_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&shim_path)
            .with_context(|| format!("stat {}", shim_path.display()))?
            .permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&shim_path, perm)
            .with_context(|| format!("chmod {}", shim_path.display()))?;
    }

    for rc in ["~/.zshrc", "~/.bashrc"]
        .iter()
        .map(|p| home.join(p.strip_prefix("~/").unwrap()))
    {
        if !rc.exists() {
            continue;
        }
        ensure_managed_block(&rc)?;
    }

    state.hook_path = Some(shim_path);
    state.fingerprint = Some(fingerprint(shim_body.as_bytes()));
    Ok(())
}

/// Append the dkod managed PATH block to `rc` if it isn't already there.
/// Read + scan + append-if-missing — never mutates content outside the
/// block. Refuses on a read-only file with a clear error.
fn ensure_managed_block(rc: &Path) -> Result<()> {
    let body = std::fs::read_to_string(rc).with_context(|| format!("read {}", rc.display()))?;
    if body.contains(MANAGED_BLOCK_START) {
        return Ok(());
    }

    let snippet = format!("\n{MANAGED_BLOCK_START}\n{MANAGED_BLOCK_BODY}\n{MANAGED_BLOCK_END}\n");
    let new_body = body + &snippet;

    // Use a write that fails loudly on permission errors rather than
    // silently swallowing — surgeon-style: if we can't touch this file,
    // tell the user exactly which file and why.
    std::fs::write(rc, new_body)
        .with_context(|| format!("append managed block to {}", rc.display()))?;
    Ok(())
}
