//! GitHub Copilot CLI capture adapter.
//!
//! Two layers, kept independent so the parser is unit-testable without
//! spawning the real `copilot` binary:
//!
//! - [`parse_events`] reads a Copilot CLI events JSONL file and maps its
//!   records onto our [`crate::Session`] / [`crate::Message`] model.
//! - [`capture_copilot_cli`] is the production wrapper: it spawns
//!   `copilot -p --output-format json`, watches stdout for structured
//!   events, then reads the on-disk events.jsonl for the full transcript.

use crate::capture::ansi::strip_ansi;
use crate::capture::timestamp::parse_rfc3339_to_millis;
use crate::capture::worktree_diff;
use crate::{Agent, Message, Session};
use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Options for [`capture_copilot_cli`].
#[derive(Debug, Clone)]
pub struct CaptureOptions {
    /// User args forwarded after `copilot -p --output-format json`.
    pub args: Vec<String>,
    /// Resolved Copilot binary path (`$DKOD_COPILOT_BIN` or `copilot`).
    pub copilot_bin: PathBuf,
    /// `$COPILOT_HOME` (default `$HOME/.copilot`); used to locate session files.
    pub copilot_home: PathBuf,
    /// Working directory passed to Copilot.
    pub cwd: PathBuf,
}

/// Pure parser: read a Copilot CLI events JSONL file and map it to a [`Session`].
///
/// Tolerant of unknown event types — logs and skips them.
pub fn parse_events(events_path: &Path) -> Result<Session> {
    let file = std::fs::File::open(events_path)
        .with_context(|| format!("open events file {}", events_path.display()))?;
    let reader = BufReader::new(file);

    let mut session = Session {
        id: Session::new_id(),
        agent: Agent::CopilotCli,
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

    for (lineno, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!(
                    "dkod: copilot-cli: events read error at line {}: {}",
                    lineno + 1,
                    e
                );
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let record: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "dkod: copilot-cli: skipping malformed JSON at line {}: {}",
                    lineno + 1,
                    e
                );
                continue;
            }
        };

        let event_type = record
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let data = record
            .get("data")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let before_len = session.messages.len();

        match event_type {
            "session.start" => {
                // Session metadata — no messages to emit.
            }
            "user.message" | "user_message" => {
                let text = extract_text(&data);
                if !text.is_empty() {
                    if first_user_for_summary.is_none() {
                        first_user_for_summary = Some(text.clone());
                    }
                    session.messages.push(Message::user(text));
                }
            }
            "assistant.message" | "agent_message" => {
                let text = extract_text(&data);
                if !text.is_empty() {
                    session.messages.push(Message::assistant(text));
                }
            }
            "reasoning" => {
                let text = extract_text(&data);
                if !text.trim().is_empty() {
                    session.messages.push(Message::reasoning(text));
                }
            }
            "tool.execution_start" | "tool_use" => {
                let tool_name = data
                    .get("tool_name")
                    .or_else(|| data.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let tool_input = data
                    .get("parameters")
                    .or_else(|| data.get("arguments"))
                    .or_else(|| data.get("input"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let tool_id = data
                    .get("tool_id")
                    .or_else(|| data.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                extract_files_from_tool(&tool_name, &tool_input, &mut files_seen, &mut session.files_touched);

                let idx = session.messages.len();
                session.messages.push(Message::tool(tool_name, tool_input, ""));
                if !tool_id.is_empty() {
                    call_to_msg.insert(tool_id.to_string(), idx);
                }
            }
            "tool.execution_complete" | "tool_result" => {
                let tool_id = data
                    .get("tool_id")
                    .or_else(|| data.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let output = strip_ansi(
                    data.get("output")
                        .or_else(|| data.get("result"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                );
                if let Some(&idx) = call_to_msg.get(tool_id) {
                    if let Some(Message::Tool { output: o, .. }) = session.messages.get_mut(idx) {
                        *o = output;
                    }
                }
            }
            "file.change" | "file_change" => {
                if let Some(changes) = data.get("changes").and_then(|v| v.as_array()) {
                    for change in changes {
                        if let Some(path) = change.get("path").and_then(|v| v.as_str()) {
                            if files_seen.insert(path.to_string()) {
                                session.files_touched.push(path.to_string());
                            }
                        }
                    }
                } else if let Some(path) = data.get("path").and_then(|v| v.as_str()) {
                    if files_seen.insert(path.to_string()) {
                        session.files_touched.push(path.to_string());
                    }
                }
            }
            "session.shutdown" | "session.end" => {
                // Terminal event — no messages to emit.
            }
            "error" => {
                let msg = data
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !msg.is_empty() {
                    eprintln!("dkod: copilot-cli: agent error: {msg}");
                }
            }
            _ => {
                // Unknown event type — tolerate gracefully.
            }
        }

        if session.messages.len() > before_len {
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

/// Production wrapper: spawn `copilot -p --output-format json` with the given
/// args, capture the session ID from stdout, then read events.jsonl.
pub fn capture_copilot_cli(opts: CaptureOptions) -> Result<Session> {
    let mut cmd = Command::new(&opts.copilot_bin);
    cmd.arg("-p").arg("--output-format").arg("json").arg("--no-ask-user");
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
            eprintln!(
                "dkod: worktree-diff: pre-spawn snapshot failed ({e}); files_touched will rely on events only"
            );
            None
        }
    };

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "spawn {} -p --output-format json (is copilot installed?)",
            opts.copilot_bin.display()
        )
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("copilot stdout was not captured"))?;
    let reader = BufReader::new(stdout);

    let mut session_id: Option<String> = None;
    let mut stdout_events: Vec<serde_json::Value> = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("dkod: copilot-cli stdout read error: {e}");
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let evt: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("dkod: copilot-cli stdout: skipping non-JSON line: {e}");
                continue;
            }
        };

        if session_id.is_none() {
            if let Some(sid) = evt
                .get("data")
                .and_then(|d| d.get("session_id"))
                .or_else(|| evt.get("session_id"))
                .and_then(|v| v.as_str())
            {
                session_id = Some(sid.to_string());
            }
        }
        stdout_events.push(evt);
    }

    let status = child.wait().context("wait for copilot child")?;
    let duration_ms = spawn_instant
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    if !status.success() {
        return Err(anyhow!(
            "copilot exited with status {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "<signal>".into())
        ));
    }

    let mut session = if let Some(ref sid) = session_id {
        match locate_events_file(&opts.copilot_home, sid) {
            Ok(events_path) => parse_events(&events_path)?,
            Err(_) => parse_stdout_events(&stdout_events)?,
        }
    } else {
        parse_stdout_events(&stdout_events)?
    };

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

/// Parse an in-memory vector of stdout JSONL events into a Session.
/// Fallback when the on-disk events.jsonl is unavailable.
fn parse_stdout_events(events: &[serde_json::Value]) -> Result<Session> {
    let mut session = Session {
        id: Session::new_id(),
        agent: Agent::CopilotCli,
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

    for record in events {
        let event_type = record
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let data = record
            .get("data")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        match event_type {
            "user.message" | "user_message" => {
                let text = extract_text(&data);
                if !text.is_empty() {
                    if first_user_for_summary.is_none() {
                        first_user_for_summary = Some(text.clone());
                    }
                    session.messages.push(Message::user(text));
                }
            }
            "assistant.message" | "agent_message" => {
                let text = extract_text(&data);
                if !text.is_empty() {
                    session.messages.push(Message::assistant(text));
                }
            }
            "reasoning" => {
                let text = extract_text(&data);
                if !text.trim().is_empty() {
                    session.messages.push(Message::reasoning(text));
                }
            }
            "tool.execution_start" | "tool_use" => {
                let tool_name = data
                    .get("tool_name")
                    .or_else(|| data.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let tool_input = data
                    .get("parameters")
                    .or_else(|| data.get("arguments"))
                    .or_else(|| data.get("input"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let tool_id = data
                    .get("tool_id")
                    .or_else(|| data.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                extract_files_from_tool(tool_name, &tool_input, &mut files_seen, &mut session.files_touched);

                let idx = session.messages.len();
                session.messages.push(Message::tool(tool_name, tool_input, ""));
                if !tool_id.is_empty() {
                    call_to_msg.insert(tool_id.to_string(), idx);
                }
            }
            "tool.execution_complete" | "tool_result" => {
                let tool_id = data
                    .get("tool_id")
                    .or_else(|| data.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let output = strip_ansi(
                    data.get("output")
                        .or_else(|| data.get("result"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                );
                if let Some(&idx) = call_to_msg.get(tool_id) {
                    if let Some(Message::Tool { output: o, .. }) = session.messages.get_mut(idx) {
                        *o = output;
                    }
                }
            }
            "file.change" | "file_change" => {
                if let Some(changes) = data.get("changes").and_then(|v| v.as_array()) {
                    for change in changes {
                        if let Some(path) = change.get("path").and_then(|v| v.as_str()) {
                            if files_seen.insert(path.to_string()) {
                                session.files_touched.push(path.to_string());
                            }
                        }
                    }
                } else if let Some(path) = data.get("path").and_then(|v| v.as_str()) {
                    if files_seen.insert(path.to_string()) {
                        session.files_touched.push(path.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(first) = first_user_for_summary {
        session.prompt_summary = summarize_prompt(&first);
    }

    Ok(session)
}

/// Locate `~/.copilot/session-state/{session_id}/events.jsonl`.
fn locate_events_file(copilot_home: &Path, session_id: &str) -> Result<PathBuf> {
    let path = copilot_home
        .join("session-state")
        .join(session_id)
        .join("events.jsonl");

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if path.exists() {
            return Ok(path);
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "events.jsonl not found at {} after 1s",
                path.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn extract_text(data: &serde_json::Value) -> String {
    if let Some(s) = data.get("text").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(s) = data.get("message").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(s) = data.get("content").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(arr) = data.get("content").and_then(|v| v.as_array()) {
        let mut out = String::new();
        for part in arr {
            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
        return out;
    }
    if let Some(s) = data.as_str() {
        return s.to_string();
    }
    String::new()
}

fn extract_files_from_tool(
    tool_name: &str,
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

    if tool_name == "shell" || tool_name == "run_command" {
        if let Some(cmd_arr) = tool_input.get("command").and_then(|v| v.as_array()) {
            let head = cmd_arr.first().and_then(|v| v.as_str()).unwrap_or("");
            if head == "apply_patch" {
                if let Some(body) = cmd_arr.get(1).and_then(|v| v.as_str()) {
                    for p in extract_apply_patch_paths(body) {
                        if files_seen.insert(p.clone()) {
                            files_touched.push(p);
                        }
                    }
                }
            }
        }
    }
}

fn extract_apply_patch_paths(body: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in body.lines() {
        let line = raw.trim_start_matches(['+', '-']).trim_start();
        for marker in ["*** Add File:", "*** Update File:", "*** Delete File:"] {
            if let Some(rest) = line.strip_prefix(marker) {
                let path = rest.trim().to_string();
                if !path.is_empty() && seen.insert(path.clone()) {
                    paths.push(path);
                }
                break;
            }
        }
    }
    paths
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
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/copilot_cli/synthetic-events.jsonl")
    }

    #[test]
    fn parse_events_synthetic_fixture() {
        let session = parse_events(&fixture_path()).expect("parse events");

        assert!(matches!(session.agent, crate::Agent::CopilotCli));
        assert!(!session.messages.is_empty());
        assert!(!session.prompt_summary.is_empty());

        let has = |pred: fn(&crate::Message) -> bool| session.messages.iter().any(pred);
        assert!(has(|m| matches!(m, crate::Message::User { .. })), "no user msg");
        assert!(has(|m| matches!(m, crate::Message::Assistant { .. })), "no assistant msg");
        assert!(has(|m| matches!(m, crate::Message::Tool { .. })), "no tool msg");
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
    fn summarize_prompt_truncates() {
        let long = "x".repeat(200);
        assert_eq!(summarize_prompt(&long).chars().count(), 120);
        assert_eq!(summarize_prompt("first line\nsecond"), "first line");
    }
}
