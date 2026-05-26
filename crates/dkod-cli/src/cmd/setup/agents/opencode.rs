//! OpenCode installer — stub.
//!
//! OpenCode has a `hooks` block in `opencode.json`. Full implementation
//! tracked as a follow-up.

use crate::cmd::setup::agents::{AgentInstaller, DetectedAgent, InstallContext, InstallOutcome};
use crate::cmd::setup::state::AgentState;
use anyhow::Result;
use std::path::Path;

pub struct OpenCode;

impl AgentInstaller for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn detect(&self, home: &Path) -> Option<DetectedAgent> {
        for candidate in [".config/opencode/opencode.json", ".opencode"] {
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
