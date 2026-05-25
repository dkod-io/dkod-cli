//! Cursor CLI installer — stub.
//!
//! Wave 0 spike: partial hook support (`.cursor/hooks.json`, only 3
//! `after*` events fire reliably in `cursor-agent`). Full implementation
//! tracked as a follow-up.

use crate::cmd::setup::agents::{AgentInstaller, DetectedAgent, InstallContext, InstallOutcome};
use crate::cmd::setup::state::AgentState;
use anyhow::Result;
use std::path::Path;

pub struct Cursor;

impl AgentInstaller for Cursor {
    fn name(&self) -> &'static str {
        "cursor"
    }

    fn detect(&self, home: &Path) -> Option<DetectedAgent> {
        for candidate in [".cursor/hooks.json", ".cursor/settings.json", ".cursor"] {
            let p = home.join(candidate);
            if p.exists() {
                return Some(DetectedAgent {
                    name: "cursor",
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
