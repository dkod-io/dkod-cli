//! Codex CLI capture adapter.
//!
//! Two layers, kept independent so the parser is unit-testable without
//! spawning the real `codex` binary:
//!
//! - [`parse_rollout`] reads a Codex rollout JSONL file and maps its
//!   records onto our [`crate::Session`] / [`crate::Message`] model.
//! - [`capture_codex`] is the production wrapper: it spawns
//!   `codex exec --json`, watches stdout for the `thread.started`
//!   event to learn the rollout file path, then calls `parse_rollout`.
//!
//! See `docs/research/codex-transcript-format.md` for the upstream
//! schema this adapter targets.

use crate::{Agent, Message, Session};
use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Options for [`capture_codex`].
#[derive(Debug, Clone)]
pub struct CaptureOptions {
    /// User args forwarded after `codex exec --json --skip-git-repo-check -C <cwd>`.
    pub args: Vec<String>,
    /// Resolved Codex binary path (`$DKOD_CODEX_BIN` or `codex`).
    pub codex_bin: PathBuf,
    /// `$CODEX_HOME` (default `$HOME/.codex`); used to locate the rollout.
    pub codex_home: PathBuf,
    /// Working directory passed to Codex via `-C`.
    pub cwd: PathBuf,
}

/// The minimum Codex CLI version we've explicitly tested against.
/// Versions below this still work in practice but emit a stderr warning.
const TESTED_CLI_VERSION_FLOOR: &str = "0.34.0";

/// Pure parser: read a Codex rollout JSONL file and map it to a [`Session`].
///
/// Does not redact and does not write anywhere. Caller composes those steps.
pub fn parse_rollout(rollout_path: &Path) -> Result<Session> {
    let file = std::fs::File::open(rollout_path)
        .with_context(|| format!("open rollout {}", rollout_path.display()))?;
    let reader = BufReader::new(file);

    let mut session = Session {
        id: Session::new_id(),
        agent: Agent::Codex,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: String::new(),
        messages: Vec::new(),
        commits: Vec::new(),
        files_touched: Vec::new(),
    };

    // call_id -> index into session.messages, for matching function_call_output back to its tool message.
    let mut call_to_msg: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut files_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Last assistant content already pushed; lets us dedupe an `agent_message`
    // event that mirrors a previous `response_item.message` (role=assistant).
    let mut last_assistant: Option<String> = None;
    let mut first_user_for_summary: Option<String> = None;

    for (lineno, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("dkod: rollout read error at line {}: {}", lineno + 1, e);
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
                    "dkod: rollout: skipping malformed JSON at line {}: {}",
                    lineno + 1,
                    e
                );
                continue;
            }
        };
        let record_type = record.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let payload = record
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        match record_type {
            "session_meta" => {
                if let Some(ver) = payload.get("cli_version").and_then(|v| v.as_str()) {
                    if version_below_floor(ver, TESTED_CLI_VERSION_FLOOR) {
                        eprintln!(
                            "dkod: warning: codex cli_version={} is below the tested floor {}; capture may misparse",
                            ver, TESTED_CLI_VERSION_FLOOR
                        );
                    }
                } else {
                    eprintln!(
                        "dkod: warning: codex cli_version is missing; tested floor is {}; capture may misparse",
                        TESTED_CLI_VERSION_FLOOR
                    );
                }
            }
            "turn_context" => {
                // No-op in V1: we don't yet have a metadata field on Session.
            }
            "event_msg" => {
                let inner_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match inner_type {
                    "user_message" => {
                        if let Some(msg) = payload.get("message").and_then(|v| v.as_str()) {
                            if first_user_for_summary.is_none() {
                                first_user_for_summary = Some(msg.to_string());
                            }
                            session.messages.push(Message::user(msg));
                        }
                    }
                    "agent_message" => {
                        if let Some(msg) = payload.get("message").and_then(|v| v.as_str()) {
                            // Skip if it duplicates the most recently emitted assistant content.
                            let dup = last_assistant.as_deref() == Some(msg);
                            if !dup {
                                session.messages.push(Message::assistant(msg));
                                last_assistant = Some(msg.to_string());
                            }
                        }
                    }
                    // token_count / task_started / task_complete / error / etc. — V1 ignores.
                    _ => {}
                }
            }
            "response_item" => {
                let inner_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match inner_type {
                    "message" => {
                        let role = payload.get("role").and_then(|v| v.as_str()).unwrap_or("");
                        let content = extract_message_text(&payload);
                        match role {
                            "user" => {
                                if first_user_for_summary.is_none() {
                                    first_user_for_summary = Some(content.clone());
                                }
                                session.messages.push(Message::user(content));
                            }
                            "assistant" => {
                                session.messages.push(Message::assistant(content.clone()));
                                last_assistant = Some(content);
                            }
                            "developer" => {
                                // System / instruction text; not user-visible content.
                            }
                            other => {
                                eprintln!(
                                    "dkod: rollout: ignoring response_item.message with unknown role {:?}",
                                    other
                                );
                            }
                        }
                    }
                    "reasoning" => {
                        let text = extract_reasoning_text(&payload);
                        if !text.is_empty() {
                            session.messages.push(Message::reasoning(text));
                        }
                    }
                    "function_call" => {
                        let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        if name != "shell" {
                            // V1 only models the shell tool. Other tools (mcp, web_search) are skipped.
                            eprintln!("dkod: rollout: skipping function_call name={:?}", name);
                            continue;
                        }
                        let args_raw = payload
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let args_json: serde_json::Value =
                            serde_json::from_str(args_raw).unwrap_or(serde_json::Value::Null);

                        // Detect apply_patch and extract files touched from the patch body.
                        if let Some(cmd_arr) = args_json.get("command").and_then(|v| v.as_array()) {
                            let head = cmd_arr.first().and_then(|v| v.as_str()).unwrap_or("");
                            if head == "apply_patch" {
                                if let Some(body) = cmd_arr.get(1).and_then(|v| v.as_str()) {
                                    for path in extract_apply_patch_paths(body) {
                                        if files_seen.insert(path.clone()) {
                                            session.files_touched.push(path);
                                        }
                                    }
                                }
                            }
                        }

                        let idx = session.messages.len();
                        session.messages.push(Message::tool("shell", args_json, ""));
                        if let Some(call_id) = payload.get("call_id").and_then(|v| v.as_str()) {
                            call_to_msg.insert(call_id.to_string(), idx);
                        }
                    }
                    "function_call_output" => {
                        let call_id = payload
                            .get("call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let output = payload
                            .get("output")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Some(&idx) = call_to_msg.get(call_id) {
                            if let Some(Message::Tool { output: o, .. }) =
                                session.messages.get_mut(idx)
                            {
                                *o = output;
                            }
                        }
                    }
                    other => {
                        eprintln!("dkod: rollout: ignoring response_item type {:?}", other);
                    }
                }
            }
            other => {
                eprintln!("dkod: rollout: ignoring record type {:?}", other);
            }
        }
    }

    if let Some(first) = first_user_for_summary {
        session.prompt_summary = summarize_prompt(&first);
    }

    Ok(session)
}

/// Production wrapper: spawn `codex exec --json` with the given args, find
/// the rollout file via `thread_id`, then call [`parse_rollout`].
///
/// On the returned session, sets `created_at` to the unix-seconds time of
/// the spawn and `duration_ms` to the wall-clock duration of the child.
pub fn capture_codex(opts: CaptureOptions) -> Result<Session> {
    let mut cmd = Command::new(&opts.codex_bin);
    cmd.arg("exec")
        .arg("--json")
        .arg("--skip-git-repo-check")
        .arg("-C")
        .arg(&opts.cwd)
        .args(&opts.args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    let spawn_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let spawn_instant = Instant::now();

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "spawn {} exec --json (is codex installed?)",
            opts.codex_bin.display()
        )
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("codex stdout was not captured"))?;
    let reader = BufReader::new(stdout);

    let mut thread_id: Option<String> = None;
    let mut warned_unknown = false;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("dkod: codex stdout read error: {e}");
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let evt: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("dkod: codex stdout: skipping non-JSON line: {e}");
                continue;
            }
        };
        match evt.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "thread.started" => {
                if let Some(tid) = evt.get("thread_id").and_then(|v| v.as_str()) {
                    if thread_id.is_none() {
                        thread_id = Some(tid.to_string());
                    }
                }
            }
            "turn.started" | "turn.completed" | "turn.failed" | "item.started" | "item.updated"
            | "item.completed" | "error" => {
                // V1 doesn't act on these, but they're known.
            }
            other => {
                if !warned_unknown {
                    eprintln!(
                        "dkod: codex stdout: ignoring unknown event type {:?}",
                        other
                    );
                    warned_unknown = true;
                }
            }
        }
    }

    let status = child.wait().context("wait for codex child")?;
    let duration_ms = spawn_instant
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    if !status.success() {
        return Err(anyhow!(
            "codex exec exited with status {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "<signal>".into())
        ));
    }

    let thread_id = thread_id.ok_or_else(|| {
        anyhow!("codex never emitted a thread.started event; cannot locate rollout")
    })?;

    let rollout = locate_rollout(&opts.codex_home, &thread_id)?;
    let mut session = parse_rollout(&rollout)?;
    session.created_at = spawn_unix;
    session.duration_ms = duration_ms;
    Ok(session)
}

fn locate_rollout(codex_home: &Path, thread_id: &str) -> Result<PathBuf> {
    // Glob `<codex_home>/sessions/*/*/*/rollout-*-<thread_id>.jsonl`.
    // Retry up to 1s in 50ms ticks because the writer may fsync slightly after exit.
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let matches = scan_rollouts(codex_home, thread_id);
        match matches.len() {
            1 => return Ok(matches.into_iter().next().unwrap()),
            n if n > 1 => {
                return Err(anyhow!(
                    "found {} rollout files matching thread_id {} under {}: {:?}",
                    n,
                    thread_id,
                    codex_home.display(),
                    matches
                ));
            }
            _ => {
                if Instant::now() >= deadline {
                    return Err(anyhow!(
                        "no rollout file matching thread_id {} found under {} after 1s",
                        thread_id,
                        codex_home.display()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn scan_rollouts(codex_home: &Path, thread_id: &str) -> Vec<PathBuf> {
    let sessions = codex_home.join("sessions");
    let mut out = Vec::new();
    let needle = format!("-{thread_id}.jsonl");
    let years = match std::fs::read_dir(&sessions) {
        Ok(it) => it,
        Err(_) => return out,
    };
    for y in years.flatten() {
        let months = match std::fs::read_dir(y.path()) {
            Ok(it) => it,
            Err(_) => continue,
        };
        for m in months.flatten() {
            let days = match std::fs::read_dir(m.path()) {
                Ok(it) => it,
                Err(_) => continue,
            };
            for d in days.flatten() {
                let files = match std::fs::read_dir(d.path()) {
                    Ok(it) => it,
                    Err(_) => continue,
                };
                for f in files.flatten() {
                    let name = f.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("rollout-") && name.ends_with(&needle) {
                        out.push(f.path());
                    }
                }
            }
        }
    }
    out
}

/// Pull the assistant/user text content out of a `response_item.message` payload.
/// Codex carries content as `[{"type": "input_text"|"output_text", "text": "..."}, ...]`.
fn extract_message_text(payload: &serde_json::Value) -> String {
    let mut out = String::new();
    if let Some(arr) = payload.get("content").and_then(|v| v.as_array()) {
        for part in arr {
            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
    } else if let Some(s) = payload.get("content").and_then(|v| v.as_str()) {
        out.push_str(s);
    }
    out
}

fn extract_reasoning_text(payload: &serde_json::Value) -> String {
    if let Some(s) = payload.get("text").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    let mut out = String::new();
    if let Some(arr) = payload.get("content").and_then(|v| v.as_array()) {
        for part in arr {
            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
    }
    out
}

/// Walk an `apply_patch` body and return the unique paths it adds, updates, or deletes.
fn extract_apply_patch_paths(body: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in body.lines() {
        // Strip a single leading `+` or `-` that some patch bodies use as prefixes for header lines.
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

/// Take the first user message and turn it into a 1-line, ≤120-char summary.
fn summarize_prompt(s: &str) -> String {
    let mut one_line: String = s
        .split(['\n', '\r'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    // Truncate at 120 *chars* (not bytes) to be Unicode-safe.
    if one_line.chars().count() > 120 {
        one_line = one_line.chars().take(120).collect();
    }
    one_line
}

/// Returns true when `version` is parseable as semver-ish `X.Y.Z` and is
/// strictly less than `floor`. Unparseable versions are treated as not-below
/// (we already warn separately when `cli_version` is missing).
fn version_below_floor(version: &str, floor: &str) -> bool {
    let v = parse_three_part(version);
    let f = parse_three_part(floor);
    match (v, f) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

fn parse_three_part(s: &str) -> Option<(u32, u32, u32)> {
    let core = s.split(['-', '+']).next().unwrap_or(s);
    let mut parts = core.split('.');
    let a: u32 = parts.next()?.parse().ok()?;
    let b: u32 = parts.next()?.parse().ok()?;
    let c: u32 = parts.next().unwrap_or("0").parse().ok()?;
    Some((a, b, c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_rollout_synthetic_fixture() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/codex/synthetic-rollout.jsonl");
        let session = parse_rollout(&fixture).expect("parse rollout");

        assert!(matches!(session.agent, crate::Agent::Codex));
        assert!(!session.messages.is_empty());

        // First user message becomes prompt_summary (truncated).
        assert!(!session.prompt_summary.is_empty());

        // We expect at least one each of: user, assistant, reasoning, tool.
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
            has(|m| matches!(m, crate::Message::Reasoning { .. })),
            "no reasoning msg"
        );
        assert!(
            has(|m| matches!(m, crate::Message::Tool { .. })),
            "no tool msg"
        );

        // The apply_patch in the fixture should produce a files_touched entry.
        assert!(!session.files_touched.is_empty(), "no files_touched");
        assert!(
            session.files_touched.iter().any(|p| p == "hello.txt"),
            "expected hello.txt in files_touched, got {:?}",
            session.files_touched
        );

        // The function_call_output text must be wired back into the tool message.
        let tool_output_ok = session.messages.iter().any(|m| match m {
            crate::Message::Tool { output, .. } => output.contains("Updated the following files"),
            _ => false,
        });
        assert!(
            tool_output_ok,
            "function_call_output not attached to tool message"
        );
    }

    #[test]
    fn dedupes_agent_message_against_response_item_assistant() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/codex/synthetic-rollout.jsonl");
        let session = parse_rollout(&fixture).expect("parse rollout");
        // Both the assistant `response_item.message` and the trailing
        // `event_msg.agent_message` carry the same text. We should keep one.
        let count = session
            .messages
            .iter()
            .filter(|m| matches!(m, crate::Message::Assistant { content } if content.contains("created hello.txt")))
            .count();
        assert_eq!(
            count, 1,
            "expected dedupe of agent_message vs assistant response_item; got {count}"
        );
    }

    #[test]
    fn extract_apply_patch_paths_handles_add_update_delete() {
        let body = "*** Begin Patch\n\
                    *** Add File: a.txt\n\
                    +hi\n\
                    *** Update File: b.rs\n\
                    @@\n\
                    *** Delete File: c.md\n\
                    *** End Patch";
        let paths = extract_apply_patch_paths(body);
        assert_eq!(paths, vec!["a.txt", "b.rs", "c.md"]);
    }

    #[test]
    fn summarize_prompt_truncates_to_120_chars_and_strips_newlines() {
        let long = "x".repeat(200);
        assert_eq!(summarize_prompt(&long).chars().count(), 120);
        assert_eq!(summarize_prompt("first line\nsecond line"), "first line");
    }

    #[test]
    fn version_below_floor_works() {
        assert!(version_below_floor("0.33.0", "0.34.0"));
        assert!(!version_below_floor("0.34.0", "0.34.0"));
        assert!(!version_below_floor("0.35.1", "0.34.0"));
        assert!(!version_below_floor("not-a-version", "0.34.0"));
    }
}
