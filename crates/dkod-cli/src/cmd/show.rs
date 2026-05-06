use anyhow::{anyhow, Context, Result};
use std::path::Path;

pub fn run(cwd: &Path, id: &str) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    let s =
        dkod_core::store::read_session(cwd, id).with_context(|| format!("read session {id}"))?;

    let agent = match s.agent {
        dkod_core::Agent::ClaudeCode => "claude_code",
        dkod_core::Agent::Codex => "codex",
        dkod_core::Agent::CopilotCli => "copilot_cli",
        dkod_core::Agent::Cursor => "cursor",
        dkod_core::Agent::GeminiCli => "gemini_cli",
    };
    println!("session {}", s.id);
    println!("agent   {}", agent);
    println!("created {}  duration_ms={}", s.created_at, s.duration_ms);
    println!("summary {}", s.prompt_summary);
    if !s.commits.is_empty() {
        println!("commits {}", s.commits.join(", "));
    }
    if !s.files_touched.is_empty() {
        println!("files   {}", s.files_touched.join(", "));
    }
    println!();
    for m in &s.messages {
        match m {
            dkod_core::Message::User { content } => println!("> user\n{content}\n"),
            dkod_core::Message::Assistant { content } => println!("< assistant\n{content}\n"),
            dkod_core::Message::Reasoning { content } => println!("r reasoning\n{content}\n"),
            dkod_core::Message::Tool {
                name,
                input,
                output,
            } => {
                println!("[ tool: {name} ]");
                println!(
                    "  input:  {}",
                    serde_json::to_string(input).unwrap_or_else(|_| "<unserialisable>".into())
                );
                println!("  output: {output}\n");
            }
        }
    }
    Ok(())
}
