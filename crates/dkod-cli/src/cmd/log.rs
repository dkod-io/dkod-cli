use anyhow::{anyhow, Context, Result};
use std::path::Path;

pub fn run(cwd: &Path) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    let mut sessions: Vec<dkod_core::Session> = dkod_core::store::list_sessions(cwd)
        .context("list sessions")?
        .into_iter()
        .filter_map(|id| dkod_core::store::read_session(cwd, &id).ok())
        .collect();

    // Newest first by created_at; tie-broken by id (lexicographic = also time-ordered with v7).
    sessions.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });

    for s in sessions {
        let agent = match s.agent {
            dkod_core::Agent::ClaudeCode => "claude_code",
            dkod_core::Agent::Codex => "codex",
            dkod_core::Agent::CopilotCli => "copilot_cli",
        };
        println!("{}  {}  {}", s.id, agent, s.prompt_summary);
    }
    Ok(())
}
