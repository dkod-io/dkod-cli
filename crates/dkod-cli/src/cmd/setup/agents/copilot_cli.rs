//! GitHub Copilot CLI installer — stub.
//!
//! Wave 0 spike confirmed HOOKS-SUPPORTED via `~/.copilot/hooks/*.json`.
//! Full implementation is tracked as a Wave 2 follow-up PR; this stub
//! returns `NotYetImplemented` so the orchestrator can complete without
//! pretending the agent is wired up.

use crate::cmd::setup::agents::{AgentInstaller, DetectedAgent, InstallContext, InstallOutcome};
use crate::cmd::setup::state::AgentState;
use anyhow::Result;
use std::path::Path;

pub struct CopilotCli;

impl AgentInstaller for CopilotCli {
    fn name(&self) -> &'static str {
        "copilot-cli"
    }

    fn detect(&self, home: &Path) -> Option<DetectedAgent> {
        for candidate in [
            ".copilot/hooks",
            ".copilot/settings.json",
            ".config/copilot",
        ] {
            let p = home.join(candidate);
            if p.exists() {
                return Some(DetectedAgent {
                    name: self.name(),
                    config_path: p,
                    version: None,
                });
            }
        }
        None
    }

    fn install(&self, ctx: &mut InstallContext, _state: &mut AgentState) -> Result<InstallOutcome> {
        Ok(if self.detect(ctx.home).is_some() {
            InstallOutcome::NotYetImplemented
        } else {
            InstallOutcome::AgentNotPresent
        })
    }
}
