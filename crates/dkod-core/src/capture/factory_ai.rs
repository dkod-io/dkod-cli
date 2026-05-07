//! Factory AI (Droid) CLI capture adapter.
//!
//! - [`parse_events`] reads a Droid NDJSON transcript and maps its records
//!   onto our [`crate::Session`] / [`crate::Message`] model.
//! - [`capture_factory_ai`] spawns `droid exec --output-format stream-json`,
//!   parses the NDJSON stream, and builds a Session.

use crate::capture::ansi::strip_ansi;
use crate::capture::timestamp::parse_rfc3339_to_millis;
use crate::capture::worktree_diff;
use crate::{Agent, Message, Session};
use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct CaptureOptions {
    pub args: Vec<String>,
    pub factory_bin: PathBuf,
    pub cwd: PathBuf,
}

pub fn parse_events(events_path: &Path) -> Result<Session> {
    let file = std::fs::File::open(events_path)
        .with_context(|| format!("open events file {}", events_path.display()))?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("dkod: factory-ai: read error: {e}");
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(v) => events.push(v),
            Err(e) => eprintln!("dkod: factory-ai: skipping malformed JSON: {e}"),
        }
    }
    parse_event_records(&events)
}

pub fn capture_factory_ai(opts: CaptureOptions) -> Result<Session> {
    let mut cmd = Command::new(&opts.factory_bin);
    cmd.arg("exec").arg("--output-format").arg("stream-json");
    cmd.args(&opts.args);
    cmd.current_dir(&opts.cwd);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    let spawn_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let spawn_instant = Instant::now();

    let before_snap = match worktree_diff::snapshot(&opts.cwd) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("dkod: worktree-diff: pre-spawn snapshot failed ({e}); files_touched will rely on events only");
            None
        }
    };

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "spawn {} exec --output-format stream-json (is droid installed?)",
            opts.factory_bin.display()
        )
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("droid stdout was not captured"))?;
    let reader = BufReader::new(stdout);

    let mut events: Vec<serde_json::Value> = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("dkod: factory-ai stdout read error: {e}");
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(v) => events.push(v),
            Err(e) => eprintln!("dkod: factory-ai stdout: skipping non-JSON line: {e}"),
        }
    }

    if events.is_empty() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(anyhow!("droid produced no output events"));
    }

    let status = child.wait().context("wait for droid child")?;
    let duration_ms = spawn_instant
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    if !status.success() {
        return Err(anyhow!(
            "droid exited with status {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "<signal>".into())
        ));
    }

    let mut session = parse_event_records(&events)?;
    session.created_at = spawn_unix;
    session.duration_ms = duration_ms;

    if let Some(before) = before_snap {
        if let Ok(after) = worktree_diff::snapshot(&opts.cwd) {
            let diff_paths = worktree_diff::symmetric_diff(&before, &after);
            let mut all: std::collections::BTreeSet<String> =
                session.files_touched.drain(..).collect();
            all.extend(diff_paths);
            session.files_touched = all.into_iter().collect();
        }
    }

    Ok(session)
}

fn parse_event_records(events: &[serde_json::Value]) -> Result<Session> {
    let mut session = Session {
        id: Session::new_id(),
        agent: Agent::FactoryAi,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: String::new(),
        messages: Vec::new(),
        commits: Vec::new(),
        files_touched: Vec::new(),
    };

    let mut call_to_msg: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut files_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut first_user_for_summary: Option<String> = None;
    let mut first_msg_millis: Option<i64> = None;
    let mut last_msg_millis: Option<i64> = None;

    for record in events {
        let event_type = record.get("type").and_then(|v| v.as_str()).unwrap_or("");

        let before_len = session.messages.len();

        match event_type {
            "system" => {}
            "user" => {
                let text = extract_text(record);
                if !text.trim().is_empty() {
                    if first_user_for_summary.is_none() {
                        first_user_for_summary = Some(text.clone());
                    }
                    session.messages.push(Message::user(text));
                }
            }
            "assistant" => {
                let text = extract_text(record);
                if !text.trim().is_empty() {
                    session.messages.push(Message::assistant(text));
                }
            }
            "thinking" | "reasoning" => {
                let text = extract_text(record);
                if !text.trim().is_empty() {
                    session.messages.push(Message::reasoning(text));
                }
            }
            "tool_use" => {
                let tool_name = record
                    .get("name")
                    .or_else(|| record.get("tool_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let tool_input = record
                    .get("input")
                    .or_else(|| record.get("parameters"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let tool_id = record
                    .get("id")
                    .or_else(|| record.get("tool_use_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                extract_files_from_tool(&tool_input, &mut files_seen, &mut session.files_touched);

                let idx = session.messages.len();
                session
                    .messages
                    .push(Message::tool(tool_name, tool_input, ""));
                if !tool_id.is_empty() {
                    call_to_msg.insert(tool_id.to_string(), idx);
                }
            }
            "tool_result" => {
                let tool_id = record
                    .get("tool_use_id")
                    .or_else(|| record.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let output = strip_ansi(
                    record
                        .get("content")
                        .or_else(|| record.get("output"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                );
                if let Some(&idx) = call_to_msg.get(tool_id) {
                    if let Some(Message::Tool { output: o, .. }) = session.messages.get_mut(idx) {
                        *o = output;
                    }
                }
            }
            "result" => {}
            _ => {}
        }

        let touched = session.messages.len() > before_len || event_type == "tool_result";
        if touched {
            if let Some(ts) = record.get("timestamp").and_then(|v| v.as_str()) {
                if let Some(m) = parse_rfc3339_to_millis(ts) {
                    if first_msg_millis.is_none() {
                        first_msg_millis = Some(m);
                    }
                    last_msg_millis = Some(m);
                }
            }
        }
    }

    if let Some(first) = first_user_for_summary {
        session.prompt_summary = summarize_prompt(&first);
    }
    if let Some(first_ms) = first_msg_millis {
        session.created_at = first_ms.div_euclid(1000);
        if let Some(last_ms) = last_msg_millis {
            let delta = last_ms.saturating_sub(first_ms);
            session.duration_ms = u64::try_from(delta).unwrap_or(0);
        }
    }

    Ok(session)
}

fn extract_text(record: &serde_json::Value) -> String {
    if let Some(msg) = record.get("message") {
        if let Some(arr) = msg.get("content").and_then(|v| v.as_array()) {
            let mut out = String::new();
            for part in arr {
                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(t);
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
        if let Some(s) = msg.get("content").and_then(|v| v.as_str()) {
            return s.to_string();
        }
    }
    if let Some(s) = record.get("content").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(s) = record.get("text").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    String::new()
}

fn extract_files_from_tool(
    tool_input: &serde_json::Value,
    files_seen: &mut std::collections::HashSet<String>,
    files_touched: &mut Vec<String>,
) {
    let path = tool_input
        .get("file_path")
        .or_else(|| tool_input.get("path"))
        .or_else(|| tool_input.get("filePath"))
        .and_then(|v| v.as_str());

    if let Some(p) = path {
        if files_seen.insert(p.to_string()) {
            files_touched.push(p.to_string());
        }
    }
}

fn summarize_prompt(s: &str) -> String {
    let mut one_line: String = s
        .split(['\n', '\r'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if one_line.chars().count() > 120 {
        one_line = one_line.chars().take(120).collect();
    }
    one_line
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/factory_ai/synthetic-events.jsonl")
    }

    #[test]
    fn parse_events_synthetic_fixture() {
        let session = parse_events(&fixture_path()).expect("parse events");
        assert!(matches!(session.agent, crate::Agent::FactoryAi));
        assert!(!session.messages.is_empty());
        assert!(!session.prompt_summary.is_empty());

        let has = |pred: fn(&crate::Message) -> bool| session.messages.iter().any(pred);
        assert!(
            has(|m| matches!(m, crate::Message::User { .. })),
            "no user msg"
        );
        assert!(
            has(|m| matches!(m, crate::Message::Assistant { .. })),
            "no assistant msg"
        );
        assert!(
            has(|m| matches!(m, crate::Message::Tool { .. })),
            "no tool msg"
        );
    }

    #[test]
    fn parse_events_populates_timestamps() {
        let session = parse_events(&fixture_path()).expect("parse events");
        assert_ne!(session.created_at, 0, "created_at not populated");
        assert!(session.duration_ms > 0, "duration_ms not populated");
    }

    #[test]
    fn parse_events_extracts_files_touched() {
        let session = parse_events(&fixture_path()).expect("parse events");
        assert!(!session.files_touched.is_empty(), "no files_touched");
        assert!(
            session.files_touched.iter().any(|p| p == "hello.txt"),
            "expected hello.txt in files_touched, got {:?}",
            session.files_touched
        );
    }

    #[test]
    fn parse_events_wires_tool_output() {
        let session = parse_events(&fixture_path()).expect("parse events");
        let tool_output_ok = session.messages.iter().any(|m| match m {
            crate::Message::Tool { output, .. } => !output.is_empty(),
            _ => false,
        });
        assert!(tool_output_ok, "tool output not attached to tool message");
    }

    #[test]
    fn parse_events_extracts_reasoning() {
        let session = parse_events(&fixture_path()).expect("parse events");
        let has_reasoning = session
            .messages
            .iter()
            .any(|m| matches!(m, crate::Message::Reasoning { .. }));
        assert!(has_reasoning, "expected reasoning message");
    }
}
