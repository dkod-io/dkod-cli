//! Claude Code capture adapter.
//!
//! Two layers, kept independent so the parser and tracker are unit-testable
//! without spawning the real `claude` binary or running the async server:
//!
//! - [`parse_transcript`] reads a Claude Code session JSONL transcript at
//!   `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl` and maps its
//!   records onto our [`crate::Session`] / [`crate::Message`] model.
//! - [`SessionTracker`] is a pure (sync) state machine: it consumes
//!   [`WireEvent`]s arriving from the on-disk hook script and tells the
//!   caller when a session has finished (cleanly via `SessionEnd`, or via
//!   the orphan watchdog after a grace period of silence).
//! - [`run_server`] is the async wrapper that listens on a UNIX socket for
//!   NDJSON wire events and drives the tracker.
//!
//! See `docs/research/claude-code-capture-protocol.md` for the upstream
//! schema this adapter targets.

use crate::{Agent, Message, Session};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Wire-protocol envelope. The hook script speaks NDJSON to the server, one
/// of these per line. Variants match the seven `kind`s in the design doc.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireEvent {
    SessionStart {
        v: u32,
        session_id: String,
        ts: String,
        cwd: String,
        transcript_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
    PromptSubmitted {
        v: u32,
        session_id: String,
        ts: String,
        cwd: String,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_mode: Option<String>,
    },
    ToolStart {
        v: u32,
        session_id: String,
        ts: String,
        cwd: String,
        tool_name: String,
        tool_input: serde_json::Value,
        tool_use_id: String,
    },
    ToolEnd {
        v: u32,
        session_id: String,
        ts: String,
        cwd: String,
        tool_name: String,
        tool_use_id: String,
        /// `"success"` | `"failure"`.
        status: String,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    PreCompact {
        v: u32,
        session_id: String,
        ts: String,
        cwd: String,
        /// `"manual"` | `"auto"`.
        trigger: String,
    },
    TurnStop {
        v: u32,
        session_id: String,
        ts: String,
        cwd: String,
    },
    SessionEnd {
        v: u32,
        session_id: String,
        ts: String,
        cwd: String,
        reason: String,
        transcript_path: String,
    },
}

impl WireEvent {
    /// Returns the `session_id` field that every variant carries.
    pub fn session_id(&self) -> &str {
        match self {
            WireEvent::SessionStart { session_id, .. }
            | WireEvent::PromptSubmitted { session_id, .. }
            | WireEvent::ToolStart { session_id, .. }
            | WireEvent::ToolEnd { session_id, .. }
            | WireEvent::PreCompact { session_id, .. }
            | WireEvent::TurnStop { session_id, .. }
            | WireEvent::SessionEnd { session_id, .. } => session_id,
        }
    }

    /// Returns the `cwd` field that every variant carries.
    pub fn cwd(&self) -> &str {
        match self {
            WireEvent::SessionStart { cwd, .. }
            | WireEvent::PromptSubmitted { cwd, .. }
            | WireEvent::ToolStart { cwd, .. }
            | WireEvent::ToolEnd { cwd, .. }
            | WireEvent::PreCompact { cwd, .. }
            | WireEvent::TurnStop { cwd, .. }
            | WireEvent::SessionEnd { cwd, .. } => cwd,
        }
    }
}

/// In-flight session state. Held in [`SessionTracker`] until the session
/// ends (cleanly or via the orphan watchdog).
#[derive(Debug, Clone)]
struct InFlight {
    transcript_path: PathBuf,
    cwd: PathBuf,
    last_event_at: Instant,
}

/// Outcome of an event applied to a [`SessionTracker`]: a session has
/// finished and the caller should now read the JSONL, build the [`Session`],
/// and write the blob.
#[derive(Debug, Clone)]
pub struct FinishedSession {
    pub session_id: String,
    pub transcript_path: PathBuf,
    pub cwd: PathBuf,
    pub end_reason: EndReason,
}

/// Why a session was reported as finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// `SessionEnd` arrived. The string is the reason the hook reported.
    Clean(String),
    /// Watchdog: no events for at least `grace`. Caller flushes whatever
    /// JSONL is currently on disk.
    Orphan,
}

/// Pure state machine that tracks in-flight Claude Code sessions keyed by
/// `session_id`. Multiple concurrent sessions in the same cwd each get
/// their own entry. The tracker performs no I/O — the async server in
/// [`run_server`] drives it from the wire protocol.
#[derive(Debug, Default)]
pub struct SessionTracker {
    sessions: BTreeMap<String, InFlight>,
}

impl SessionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of sessions currently tracked. Mostly useful for tests.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Returns `true` if the tracker has no in-flight sessions.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Apply a wire event to the tracker. Returns `Some(FinishedSession)`
    /// if this event finishes a session (via `SessionEnd`); otherwise
    /// `None`.
    ///
    /// Unknown / late events for an unseen session_id register a
    /// tentative entry — the watchdog will sweep it if no further events
    /// arrive (analogous to the doc's "recover late `session_start`").
    pub fn apply(&mut self, event: WireEvent) -> Option<FinishedSession> {
        let now = Instant::now();
        match event {
            WireEvent::SessionStart {
                session_id,
                cwd,
                transcript_path,
                ..
            } => {
                self.sessions.insert(
                    session_id,
                    InFlight {
                        transcript_path: PathBuf::from(transcript_path),
                        cwd: PathBuf::from(cwd),
                        last_event_at: now,
                    },
                );
                None
            }
            WireEvent::SessionEnd {
                session_id,
                reason,
                transcript_path,
                cwd,
                ..
            } => {
                let removed = self.sessions.remove(&session_id);
                let (final_path, final_cwd) = match removed {
                    Some(inflight) => {
                        // Prefer the path the SessionEnd hook reports, since
                        // it's the one Claude Code is closing right now;
                        // fall back to the SessionStart path if SessionEnd
                        // somehow has an empty value.
                        let path = if transcript_path.is_empty() {
                            inflight.transcript_path
                        } else {
                            PathBuf::from(transcript_path)
                        };
                        (path, inflight.cwd)
                    }
                    None => (PathBuf::from(transcript_path), PathBuf::from(cwd)),
                };
                Some(FinishedSession {
                    session_id,
                    transcript_path: final_path,
                    cwd: final_cwd,
                    end_reason: EndReason::Clean(reason),
                })
            }
            other => {
                // Any other kind: bump last_event_at on the matching
                // session, or register a tentative entry if we haven't seen
                // session_start yet (so the watchdog can still sweep it).
                let session_id = other.session_id().to_string();
                let cwd = PathBuf::from(other.cwd());
                self.sessions
                    .entry(session_id)
                    .and_modify(|s| s.last_event_at = now)
                    .or_insert(InFlight {
                        transcript_path: PathBuf::new(),
                        cwd,
                        last_event_at: now,
                    });
                None
            }
        }
    }

    /// Sweep for orphans whose `last_event_at` is older than `now - grace`.
    /// Returns one [`FinishedSession`] per swept entry, with
    /// [`EndReason::Orphan`].
    pub fn sweep_orphans(&mut self, grace: Duration) -> Vec<FinishedSession> {
        self.sweep_orphans_at(Instant::now(), grace)
    }

    /// Test-friendly variant of `sweep_orphans` that takes an explicit
    /// "now" instant.
    pub fn sweep_orphans_at(&mut self, now: Instant, grace: Duration) -> Vec<FinishedSession> {
        let mut finished = Vec::new();
        let mut to_remove = Vec::new();
        for (id, state) in &self.sessions {
            if now.saturating_duration_since(state.last_event_at) >= grace {
                to_remove.push(id.clone());
            }
        }
        for id in to_remove {
            if let Some(state) = self.sessions.remove(&id) {
                finished.push(FinishedSession {
                    session_id: id,
                    transcript_path: state.transcript_path,
                    cwd: state.cwd,
                    end_reason: EndReason::Orphan,
                });
            }
        }
        finished
    }
}

/// Pure parser: read a Claude Code session JSONL transcript and map it to
/// a [`Session`].
///
/// Does not redact and does not write anywhere. Caller composes those
/// steps. `thinking` content blocks become [`Message::Reasoning`]; the
/// caller (e.g. the redaction pass) decides what to do with them.
pub fn parse_transcript(path: &Path) -> Result<Session> {
    let file =
        std::fs::File::open(path).with_context(|| format!("open transcript {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut session = Session {
        id: Session::new_id(),
        agent: Agent::ClaudeCode,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: String::new(),
        messages: Vec::new(),
        commits: Vec::new(),
        files_touched: Vec::new(),
    };

    // tool_use_id -> index into session.messages, for matching tool_result
    // back to its tool message.
    let mut tool_to_msg: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut files_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut first_user_for_summary: Option<String> = None;

    for (lineno, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!(
                    "dkod: claude-code: read error at line {}: {}",
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
                    "dkod: claude-code: skipping malformed JSON at line {}: {}",
                    lineno + 1,
                    e
                );
                continue;
            }
        };
        let record_type = record.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match record_type {
            "user" => {
                handle_user(
                    &record,
                    &mut session,
                    &mut tool_to_msg,
                    &mut first_user_for_summary,
                );
            }
            "assistant" => {
                handle_assistant(&record, &mut session, &mut tool_to_msg, &mut files_seen);
            }
            // V1 ignores everything else (system, attachment, file-history-snapshot,
            // custom-title, agent-name, permission-mode, last-prompt, pr-link, etc.).
            _ => {}
        }
    }

    if let Some(first) = first_user_for_summary {
        session.prompt_summary = summarize_prompt(&first);
    }

    Ok(session)
}

fn handle_user(
    record: &serde_json::Value,
    session: &mut Session,
    tool_to_msg: &mut std::collections::HashMap<String, usize>,
    first_user_for_summary: &mut Option<String>,
) {
    let message = match record.get("message") {
        Some(m) => m,
        None => return,
    };
    let content = match message.get("content") {
        Some(c) => c,
        None => return,
    };

    // `content` can be a plain string OR a list of blocks.
    if let Some(s) = content.as_str() {
        if first_user_for_summary.is_none() {
            *first_user_for_summary = Some(s.to_string());
        }
        session.messages.push(Message::user(s));
        return;
    }
    if let Some(blocks) = content.as_array() {
        for block in blocks {
            let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match btype {
                "text" => {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        if first_user_for_summary.is_none() {
                            *first_user_for_summary = Some(t.to_string());
                        }
                        session.messages.push(Message::user(t));
                    }
                }
                "tool_result" => {
                    let id = block
                        .get("tool_use_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let output_text = tool_result_to_string(block.get("content"));
                    if let Some(&idx) = tool_to_msg.get(id) {
                        if let Some(Message::Tool { output, .. }) = session.messages.get_mut(idx) {
                            *output = output_text;
                        }
                    }
                }
                // image / others — ignored in V1.
                _ => {}
            }
        }
    }
}

fn handle_assistant(
    record: &serde_json::Value,
    session: &mut Session,
    tool_to_msg: &mut std::collections::HashMap<String, usize>,
    files_seen: &mut std::collections::HashSet<String>,
) {
    let message = match record.get("message") {
        Some(m) => m,
        None => return,
    };
    let blocks = match message.get("content").and_then(|v| v.as_array()) {
        Some(b) => b,
        None => return,
    };
    for block in blocks {
        let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match btype {
            "text" => {
                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                    session.messages.push(Message::assistant(t));
                }
            }
            "thinking" => {
                if let Some(t) = block.get("thinking").and_then(|v| v.as_str()) {
                    session.messages.push(Message::reasoning(t));
                }
            }
            "tool_use" => {
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = block
                    .get("input")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);

                // Edit/Write/MultiEdit/NotebookEdit -> capture file path.
                if let Some(path) = extract_edit_path(&name, &input) {
                    if files_seen.insert(path.clone()) {
                        session.files_touched.push(path);
                    }
                }

                let idx = session.messages.len();
                session.messages.push(Message::tool(name, input, ""));
                if !id.is_empty() {
                    tool_to_msg.insert(id, idx);
                }
            }
            _ => {}
        }
    }
}

fn extract_edit_path(name: &str, input: &serde_json::Value) -> Option<String> {
    match name {
        "Edit" | "Write" | "MultiEdit" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        "NotebookEdit" => input
            .get("notebook_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

fn tool_result_to_string(content: Option<&serde_json::Value>) -> String {
    let content = match content {
        Some(c) => c,
        None => return String::new(),
    };
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
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
    String::new()
}

/// Take the first user message and turn it into a 1-line, ≤120-char summary.
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

/// Run the async UNIX-socket NDJSON server.
///
/// Listens at `socket_path`, reads NDJSON wire events from each connection,
/// drives a [`SessionTracker`], and invokes `on_finished` for every
/// finished session (clean or orphan). Returns an error when the listener
/// fails; graceful shutdown / lifecycle / hook installation are Task 19's
/// job.
///
/// `orphan_grace` is how long since the last event before a session is
/// considered orphaned. A reasonable default is 60 seconds.
///
/// **Caveat:** the tracker registers a tentative entry for any wire event
/// whose `session_id` it hasn't seen via `SessionStart` yet (so a delayed
/// `SessionStart` doesn't lose the session). If such a tentative entry is
/// later swept by the orphan watchdog, the resulting [`FinishedSession`]
/// carries an empty `transcript_path`. Callers must treat an empty
/// transcript path as "no transcript was ever announced" and either skip
/// the flush or fall back to scanning `~/.claude/projects/<encoded-cwd>/`
/// for the matching `<session_id>.jsonl`.
pub async fn run_server<F>(socket_path: &Path, orphan_grace: Duration, on_finished: F) -> Result<()>
where
    F: Fn(FinishedSession) + Send + Sync + 'static,
{
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, BufReader as TokioBufReader};
    use tokio::net::UnixListener;

    // Best-effort: remove a stale socket file before binding.
    if socket_path.exists() {
        let _ = std::fs::remove_file(socket_path);
    }
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("bind unix socket {}", socket_path.display()))?;

    // Ensure the socket is mode 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600));
    }

    // Tracker is shared across the per-connection readers and the watchdog
    // tick. We use a std::sync::Mutex (not tokio::sync) — every critical
    // section is short and synchronous, and we don't await while holding
    // the lock.
    let tracker = Arc::new(Mutex::new(SessionTracker::new()));
    let on_finished = Arc::new(on_finished);

    // Watchdog task: every second, sweep orphans and fire callbacks for
    // any sessions that timed out.
    let watchdog = {
        let tracker = tracker.clone();
        let on_finished = on_finished.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let orphans = {
                    let mut t = match tracker.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    t.sweep_orphans(orphan_grace)
                };
                for finished in orphans {
                    on_finished(finished);
                }
            }
        })
    };

    // Accept loop: spawn a per-connection NDJSON reader. Each reader parses
    // lines, applies them to the shared tracker, and fires the callback
    // when the tracker reports a finished session.
    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("dkod: claude-code: accept error: {e}");
                watchdog.abort();
                return Err(anyhow::anyhow!("unix socket accept failed: {e}"));
            }
        };
        let tracker = tracker.clone();
        let on_finished = on_finished.clone();
        tokio::spawn(async move {
            let reader = TokioBufReader::new(stream);
            let mut lines = reader.lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<WireEvent>(&line) {
                            Ok(event) => {
                                let finished = {
                                    let mut t = match tracker.lock() {
                                        Ok(g) => g,
                                        Err(p) => p.into_inner(),
                                    };
                                    t.apply(event)
                                };
                                if let Some(fs) = finished {
                                    on_finished(fs);
                                }
                            }
                            Err(e) => {
                                eprintln!("dkod: claude-code: bad NDJSON line: {e}");
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("dkod: claude-code: read error: {e}");
                        break;
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_session_start() -> WireEvent {
        WireEvent::SessionStart {
            v: 1,
            session_id: "sid-1".into(),
            ts: "2026-05-03T12:00:00.000Z".into(),
            cwd: "/tmp/demo".into(),
            transcript_path: "/tmp/demo/transcript.jsonl".into(),
            model: Some("claude-opus-4-7".into()),
            agent_type: None,
            source: Some("startup".into()),
        }
    }

    fn sample_session_end(session_id: &str, reason: &str) -> WireEvent {
        WireEvent::SessionEnd {
            v: 1,
            session_id: session_id.into(),
            ts: "2026-05-03T12:01:00.000Z".into(),
            cwd: "/tmp/demo".into(),
            reason: reason.into(),
            transcript_path: "/tmp/demo/transcript.jsonl".into(),
        }
    }

    // ---- Wire-event round-trip tests ----

    #[test]
    fn wire_session_start_round_trip() {
        let e = sample_session_start();
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"kind\":\"session_start\""));
        let back: WireEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, WireEvent::SessionStart { .. }));
        assert_eq!(back.session_id(), "sid-1");
    }

    #[test]
    fn wire_prompt_submitted_round_trip() {
        let e = WireEvent::PromptSubmitted {
            v: 1,
            session_id: "sid".into(),
            ts: "t".into(),
            cwd: "/x".into(),
            prompt: "hi".into(),
            permission_mode: Some("default".into()),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"kind\":\"prompt_submitted\""));
        let back: WireEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, WireEvent::PromptSubmitted { .. }));
    }

    #[test]
    fn wire_tool_start_end_round_trip() {
        let start = WireEvent::ToolStart {
            v: 1,
            session_id: "sid".into(),
            ts: "t".into(),
            cwd: "/x".into(),
            tool_name: "Edit".into(),
            tool_input: serde_json::json!({"file_path": "src/lib.rs"}),
            tool_use_id: "toolu_1".into(),
        };
        let json = serde_json::to_string(&start).unwrap();
        assert!(json.contains("\"kind\":\"tool_start\""));
        let back: WireEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, WireEvent::ToolStart { .. }));

        let end = WireEvent::ToolEnd {
            v: 1,
            session_id: "sid".into(),
            ts: "t".into(),
            cwd: "/x".into(),
            tool_name: "Edit".into(),
            tool_use_id: "toolu_1".into(),
            status: "success".into(),
            duration_ms: 42,
            error: None,
        };
        let json = serde_json::to_string(&end).unwrap();
        assert!(json.contains("\"kind\":\"tool_end\""));
        let back: WireEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, WireEvent::ToolEnd { .. }));
    }

    #[test]
    fn wire_pre_compact_round_trip() {
        let e = WireEvent::PreCompact {
            v: 1,
            session_id: "sid".into(),
            ts: "t".into(),
            cwd: "/x".into(),
            trigger: "auto".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"kind\":\"pre_compact\""));
        let back: WireEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, WireEvent::PreCompact { .. }));
    }

    #[test]
    fn wire_turn_stop_round_trip() {
        let e = WireEvent::TurnStop {
            v: 1,
            session_id: "sid".into(),
            ts: "t".into(),
            cwd: "/x".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"kind\":\"turn_stop\""));
        let back: WireEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, WireEvent::TurnStop { .. }));
    }

    #[test]
    fn wire_session_end_round_trip() {
        let e = sample_session_end("sid-1", "prompt_input_exit");
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"kind\":\"session_end\""));
        let back: WireEvent = serde_json::from_str(&json).unwrap();
        match back {
            WireEvent::SessionEnd { reason, .. } => assert_eq!(reason, "prompt_input_exit"),
            _ => panic!("wrong variant"),
        }
    }

    // ---- SessionTracker tests ----

    #[test]
    fn tracker_clean_lifecycle() {
        let mut t = SessionTracker::new();
        assert!(t.is_empty());
        assert!(t.apply(sample_session_start()).is_none());
        assert_eq!(t.len(), 1);
        assert!(t
            .apply(WireEvent::PromptSubmitted {
                v: 1,
                session_id: "sid-1".into(),
                ts: "t".into(),
                cwd: "/tmp/demo".into(),
                prompt: "hi".into(),
                permission_mode: None,
            })
            .is_none());
        assert!(t
            .apply(WireEvent::ToolStart {
                v: 1,
                session_id: "sid-1".into(),
                ts: "t".into(),
                cwd: "/tmp/demo".into(),
                tool_name: "Edit".into(),
                tool_input: serde_json::json!({}),
                tool_use_id: "tu1".into(),
            })
            .is_none());
        assert!(t
            .apply(WireEvent::ToolEnd {
                v: 1,
                session_id: "sid-1".into(),
                ts: "t".into(),
                cwd: "/tmp/demo".into(),
                tool_name: "Edit".into(),
                tool_use_id: "tu1".into(),
                status: "success".into(),
                duration_ms: 1,
                error: None,
            })
            .is_none());
        let finished = t
            .apply(sample_session_end("sid-1", "logout"))
            .expect("session_end should finish a session");
        assert_eq!(finished.session_id, "sid-1");
        assert_eq!(finished.end_reason, EndReason::Clean("logout".into()));
        assert_eq!(
            finished.transcript_path,
            PathBuf::from("/tmp/demo/transcript.jsonl")
        );
        assert!(t.is_empty());
    }

    #[test]
    fn tracker_handles_concurrent_sessions() {
        let mut t = SessionTracker::new();
        t.apply(sample_session_start());
        t.apply(WireEvent::SessionStart {
            v: 1,
            session_id: "sid-2".into(),
            ts: "t".into(),
            cwd: "/tmp/demo".into(),
            transcript_path: "/tmp/demo/transcript-2.jsonl".into(),
            model: None,
            agent_type: None,
            source: None,
        });
        assert_eq!(t.len(), 2);
        let f = t.apply(sample_session_end("sid-1", "clear")).unwrap();
        assert_eq!(f.session_id, "sid-1");
        assert_eq!(t.len(), 1);
        // sid-2 still alive
        assert!(t.sessions.contains_key("sid-2"));
    }

    #[test]
    fn tracker_sweep_orphans_removes_stale_only() {
        let mut t = SessionTracker::new();
        t.apply(sample_session_start());
        // Hand-rewind sid-1's last_event_at to 120s ago.
        let stale_at = Instant::now()
            .checked_sub(Duration::from_secs(120))
            .expect("Instant::checked_sub failed");
        if let Some(s) = t.sessions.get_mut("sid-1") {
            s.last_event_at = stale_at;
        }
        // Add a fresh session (sid-2). It should NOT be swept.
        t.apply(WireEvent::SessionStart {
            v: 1,
            session_id: "sid-2".into(),
            ts: "t".into(),
            cwd: "/tmp/demo".into(),
            transcript_path: "/tmp/demo/2.jsonl".into(),
            model: None,
            agent_type: None,
            source: None,
        });
        let orphans = t.sweep_orphans(Duration::from_secs(60));
        assert_eq!(orphans.len(), 1, "expected exactly one orphan");
        assert_eq!(orphans[0].session_id, "sid-1");
        assert_eq!(orphans[0].end_reason, EndReason::Orphan);
        assert!(t.sessions.contains_key("sid-2"));
    }

    #[test]
    fn tracker_sweep_does_nothing_when_recent() {
        let mut t = SessionTracker::new();
        t.apply(sample_session_start());
        let orphans = t.sweep_orphans(Duration::from_secs(60));
        assert!(orphans.is_empty());
        assert_eq!(t.len(), 1);
    }

    // ---- JSONL parser fixture test ----

    #[test]
    fn parse_synthetic_transcript() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/claude-code/synthetic-transcript.jsonl");
        let session = parse_transcript(&fixture).expect("parse transcript");

        assert!(matches!(session.agent, Agent::ClaudeCode));
        assert!(!session.messages.is_empty());
        assert!(!session.prompt_summary.is_empty());

        let has = |pred: fn(&Message) -> bool| session.messages.iter().any(pred);
        assert!(has(|m| matches!(m, Message::User { .. })), "no user msg");
        assert!(
            has(|m| matches!(m, Message::Assistant { .. })),
            "no assistant msg"
        );
        assert!(
            has(|m| matches!(m, Message::Reasoning { .. })),
            "no reasoning msg"
        );
        assert!(has(|m| matches!(m, Message::Tool { .. })), "no tool msg");

        assert!(
            session.files_touched.iter().any(|p| p == "src/lib.rs"),
            "expected src/lib.rs in files_touched, got {:?}",
            session.files_touched
        );

        // The tool_result must have been wired back into the matching tool
        // message.
        let read_tool_output_ok = session.messages.iter().any(|m| match m {
            Message::Tool { name, output, .. } => name == "Read" && output.contains("pub fn hi"),
            _ => false,
        });
        assert!(
            read_tool_output_ok,
            "Read tool_result not attached to tool message"
        );
        let edit_tool_output_ok = session.messages.iter().any(|m| match m {
            Message::Tool { name, output, .. } => name == "Edit" && output.contains("applied edit"),
            _ => false,
        });
        assert!(
            edit_tool_output_ok,
            "Edit tool_result not attached to tool message"
        );
    }

    // ---- Async server smoke test ----

    #[tokio::test]
    async fn server_accepts_a_session_lifecycle() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        };
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("server.sock");

        let finished_count = Arc::new(AtomicUsize::new(0));
        let last_reason: Arc<Mutex<Option<EndReason>>> = Arc::new(Mutex::new(None));
        let finished_count_cb = finished_count.clone();
        let last_reason_cb = last_reason.clone();

        let socket_path_for_server = socket_path.clone();
        let server = tokio::spawn(async move {
            let _ = run_server(
                &socket_path_for_server,
                Duration::from_secs(60),
                move |fs| {
                    finished_count_cb.fetch_add(1, Ordering::SeqCst);
                    *last_reason_cb.lock().unwrap() = Some(fs.end_reason);
                },
            )
            .await;
        });

        // Wait for the socket to appear (yield rather than sleep).
        for _ in 0..50 {
            if socket_path.exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(socket_path.exists(), "server didn't bind in time");

        // Connect a client and send SessionStart + SessionEnd as NDJSON.
        let mut client = UnixStream::connect(&socket_path).await.unwrap();
        let start = sample_session_start();
        let end = sample_session_end("sid-1", "logout");
        let mut buf = serde_json::to_string(&start).unwrap();
        buf.push('\n');
        client.write_all(buf.as_bytes()).await.unwrap();
        client.flush().await.unwrap();
        tokio::task::yield_now().await;
        let mut buf = serde_json::to_string(&end).unwrap();
        buf.push('\n');
        client.write_all(buf.as_bytes()).await.unwrap();
        client.flush().await.unwrap();
        client.shutdown().await.unwrap();
        drop(client);

        // Wait for the callback to fire. Yield a bunch of times; each yield
        // hands control back to the runtime so the driver can drain the
        // channel. If the callback hasn't fired after a generous bound,
        // fall back to a short sleep before failing.
        for _ in 0..200 {
            if finished_count.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        if finished_count.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(
            finished_count.load(Ordering::SeqCst),
            1,
            "expected on_finished to fire exactly once"
        );
        let reason = last_reason.lock().unwrap().clone().unwrap();
        assert_eq!(reason, EndReason::Clean("logout".into()));

        server.abort();
    }
}
