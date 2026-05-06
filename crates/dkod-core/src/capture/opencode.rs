use crate::capture::ansi::strip_ansi;
use crate::capture::timestamp::parse_rfc3339_to_millis;
use crate::capture::worktree_diff;
use crate::{Agent, Message, Session};
use anyhow::{anyhow, Context, Result};
use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct CaptureOptions {
    pub args: Vec<String>,
    pub opencode_bin: PathBuf,
    pub cwd: PathBuf,
}

pub fn parse_output(json: &serde_json::Value) -> Result<Session> {
    let mut session = Session {
        id: Session::new_id(),
        agent: Agent::OpenCode,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: String::new(),
        messages: Vec::new(),
        commits: Vec::new(),
        files_touched: Vec::new(),
    };

    let msgs = json
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("missing or non-array \"messages\" field"))?;

    let mut tool_to_msg: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut files_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut first_user_for_summary: Option<String> = None;
    let mut first_msg_millis: Option<i64> = None;
    let mut last_msg_millis: Option<i64> = None;

    for msg in msgs {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let parts = msg.get("parts").and_then(|v| v.as_array());
        let ts = msg.get("created_at").and_then(|v| v.as_str());

        let before_len = session.messages.len();

        if let Some(parts) = parts {
            for part in parts {
                let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match (role, part_type) {
                    ("user", "text") => {
                        let text = part.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        if !text.is_empty() {
                            if first_user_for_summary.is_none() {
                                first_user_for_summary = Some(text.to_string());
                            }
                            session.messages.push(Message::user(text));
                        }
                    }
                    ("assistant", "text") => {
                        let text = part.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        if !text.is_empty() {
                            session.messages.push(Message::assistant(text));
                        }
                    }
                    ("assistant", "thinking") => {
                        let text = part.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        if !text.trim().is_empty() {
                            session.messages.push(Message::reasoning(text));
                        }
                    }
                    ("assistant", "tool_use") => {
                        let tu = part.get("tool_use").unwrap_or(part);
                        let name = tu.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                        let input = tu
                            .get("input")
                            .or_else(|| tu.get("parameters"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let tool_id = tu.get("id").and_then(|v| v.as_str()).unwrap_or("");

                        extract_files_from_tool(
                            name,
                            &input,
                            &mut files_seen,
                            &mut session.files_touched,
                        );

                        let idx = session.messages.len();
                        session.messages.push(Message::tool(name, input, ""));
                        if !tool_id.is_empty() {
                            tool_to_msg.insert(tool_id.to_string(), idx);
                        }
                    }
                    ("tool", "tool_result") => {
                        let tr = part.get("tool_result").unwrap_or(part);
                        let tool_id = tr.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let output = strip_ansi(
                            tr.get("output")
                                .or_else(|| tr.get("result"))
                                .and_then(|v| v.as_str())
                                .unwrap_or(""),
                        );
                        if let Some(&idx) = tool_to_msg.get(tool_id) {
                            if let Some(Message::Tool { output: o, .. }) =
                                session.messages.get_mut(idx)
                            {
                                *o = output;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if session.messages.len() > before_len {
            if let Some(ts_str) = ts {
                if let Some(m) = parse_rfc3339_to_millis(ts_str) {
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

pub fn capture_opencode(opts: CaptureOptions) -> Result<Session> {
    let mut cmd = Command::new(&opts.opencode_bin);
    cmd.arg("-p").arg("-f").arg("json").arg("-q");
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
                "dkod: worktree-diff: pre-spawn snapshot failed ({e}); files_touched will rely on output only"
            );
            None
        }
    };

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "spawn {} -p -f json -q (is opencode installed?)",
            opts.opencode_bin.display()
        )
    })?;

    let mut stdout_buf = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout
            .read_to_string(&mut stdout_buf)
            .context("read opencode stdout")?;
    }

    let status = child.wait().context("wait for opencode child")?;
    let duration_ms = spawn_instant
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    if !status.success() {
        return Err(anyhow!(
            "opencode exited with status {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "<signal>".into())
        ));
    }

    let json: serde_json::Value =
        serde_json::from_str(stdout_buf.trim()).context("parse opencode JSON output")?;

    let mut session = parse_output(&json)?;
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

fn extract_files_from_tool(
    _tool_name: &str,
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

    fn fixture() -> serde_json::Value {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/opencode/synthetic-output.json");
        let body = std::fs::read_to_string(&path).expect("read fixture");
        serde_json::from_str(&body).expect("parse fixture")
    }

    #[test]
    fn parse_output_synthetic_fixture() {
        let session = parse_output(&fixture()).expect("parse output");
        assert!(matches!(session.agent, crate::Agent::OpenCode));
        assert!(!session.messages.is_empty());
        assert!(!session.prompt_summary.is_empty());

        let has = |pred: fn(&crate::Message) -> bool| session.messages.iter().any(pred);
        assert!(has(|m| matches!(m, crate::Message::User { .. })));
        assert!(has(|m| matches!(m, crate::Message::Assistant { .. })));
        assert!(has(|m| matches!(m, crate::Message::Tool { .. })));
    }

    #[test]
    fn parse_output_populates_timestamps() {
        let session = parse_output(&fixture()).expect("parse output");
        assert_ne!(session.created_at, 0);
        assert!(session.duration_ms > 0);
    }

    #[test]
    fn parse_output_extracts_files_touched() {
        let session = parse_output(&fixture()).expect("parse output");
        assert!(!session.files_touched.is_empty());
        assert!(
            session.files_touched.iter().any(|p| p == "main.go"),
            "expected main.go in files_touched, got {:?}",
            session.files_touched
        );
    }

    #[test]
    fn parse_output_wires_tool_output() {
        let session = parse_output(&fixture()).expect("parse output");
        let tool_output_ok = session.messages.iter().any(|m| match m {
            crate::Message::Tool { output, .. } => !output.is_empty(),
            _ => false,
        });
        assert!(tool_output_ok, "tool output not attached to tool message");
    }

    #[test]
    fn parse_output_extracts_reasoning() {
        let session = parse_output(&fixture()).expect("parse output");
        let has_reasoning = session
            .messages
            .iter()
            .any(|m| matches!(m, crate::Message::Reasoning { .. }));
        assert!(has_reasoning, "expected reasoning message");
    }
}
