use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub agent: Agent,
    pub created_at: i64,
    pub duration_ms: u64,
    pub prompt_summary: String,
    pub messages: Vec<Message>,
    pub commits: Vec<String>,
    pub files_touched: Vec<String>,
    /// Audit count: total number of replacements the capture-time redactor
    /// made in this session. Defaults to 0 so sessions stored before this
    /// field existed keep deserializing.
    #[serde(default)]
    pub redaction_count: u64,
}

impl Session {
    pub fn new_id() -> String {
        uuid::Uuid::now_v7().to_string()
    }
}

/// Marker spliced into truncated tool outputs by the capture-time size budget
/// (storage-v2 design §7). `SessionMeta::derive` detects it; the budget
/// module (Phase 2) writes it. Single source of truth for both sides.
pub const TRUNCATION_MARKER_PREFIX: &str = "[dkod: truncated ";

/// Small derived header written alongside the full session body in the
/// rollup index (`sessions/<date>/<id>/meta.json`, design §4.3). NOT part of
/// the `Session` schema — derived on write, so `Session` (and its 39 literal
/// construction sites) never changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub agent: Agent,
    pub created_at: i64,
    pub duration_ms: u64,
    pub prompt_summary: String,
    pub commits: Vec<String>,
    pub files_touched: Vec<String>,
    #[serde(default)]
    pub redaction_count: u64,
    /// Serialized (uncompressed) byte length of `body.json`.
    pub body_bytes: u64,
    /// True when any tool output carries the truncation marker.
    #[serde(default)]
    pub truncated: bool,
}

impl SessionMeta {
    pub fn derive(s: &Session, body_bytes: u64) -> Self {
        let truncated = s.messages.iter().any(|m| {
            matches!(m, Message::Tool { output, .. } if output.contains(TRUNCATION_MARKER_PREFIX))
        });
        Self {
            id: s.id.clone(),
            agent: s.agent.clone(),
            created_at: s.created_at,
            duration_ms: s.duration_ms,
            prompt_summary: s.prompt_summary.clone(),
            commits: s.commits.clone(),
            files_touched: s.files_touched.clone(),
            redaction_count: s.redaction_count,
            body_bytes,
            truncated,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    ClaudeCode,
    Codex,
    CopilotCli,
    Cursor,
    FactoryAi,
    GeminiCli,
    OpenCode,
}

/// Stable snake_case label for an agent. Single source of truth for the
/// CLI's human-readable agent column (log / show / blame).
pub fn agent_label(a: &Agent) -> &'static str {
    match a {
        Agent::ClaudeCode => "claude_code",
        Agent::Codex => "codex",
        Agent::CopilotCli => "copilot_cli",
        Agent::Cursor => "cursor",
        Agent::FactoryAi => "factory_ai",
        Agent::GeminiCli => "gemini_cli",
        Agent::OpenCode => "open_code",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    User {
        content: String,
    },
    Assistant {
        content: String,
    },
    Reasoning {
        content: String,
    },
    Tool {
        name: String,
        input: serde_json::Value,
        output: String,
    },
}

impl Message {
    pub fn user(s: impl Into<String>) -> Self {
        Self::User { content: s.into() }
    }
    pub fn assistant(s: impl Into<String>) -> Self {
        Self::Assistant { content: s.into() }
    }
    pub fn reasoning(s: impl Into<String>) -> Self {
        Self::Reasoning { content: s.into() }
    }
    pub fn tool(
        name: impl Into<String>,
        input: serde_json::Value,
        output: impl Into<String>,
    ) -> Self {
        Self::Tool {
            name: name.into(),
            input,
            output: output.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_label_is_stable_snake_case() {
        assert_eq!(agent_label(&Agent::ClaudeCode), "claude_code");
        assert_eq!(agent_label(&Agent::OpenCode), "open_code");
        assert_eq!(agent_label(&Agent::FactoryAi), "factory_ai");
    }

    #[test]
    fn factory_ai_agent_serializes_as_snake_case() {
        let json = serde_json::to_string(&Agent::FactoryAi).unwrap();
        assert_eq!(json, "\"factory_ai\"");
        let back: Agent = serde_json::from_str("\"factory_ai\"").unwrap();
        assert_eq!(back, Agent::FactoryAi);
    }

    #[test]
    fn round_trips_through_json() {
        let s = Session {
            id: "0192f8e2-7b3a-7000-8a3e-000000000001".into(),
            agent: Agent::ClaudeCode,
            created_at: 1735689600,
            duration_ms: 12_345,
            prompt_summary: "fix the auth bug".into(),
            messages: vec![
                Message::user("fix the auth bug"),
                Message::assistant("done"),
            ],
            commits: vec!["deadbeef".into()],
            files_touched: vec!["src/auth.rs".into()],
            redaction_count: 3,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn session_json_without_redaction_count_deserializes_to_zero() {
        // Sessions stored before the audit-count field existed must keep
        // deserializing; the count defaults to 0.
        let json = r#"{
            "id": "0192f8e2-7b3a-7000-8a3e-000000000001",
            "agent": "claude_code",
            "created_at": 1735689600,
            "duration_ms": 1,
            "prompt_summary": "fix the auth bug",
            "messages": [],
            "commits": [],
            "files_touched": []
        }"#;
        let s: Session = serde_json::from_str(json).unwrap();
        assert_eq!(s.redaction_count, 0);
    }

    #[test]
    fn tool_helper_round_trips() {
        let m = Message::tool("read_file", serde_json::json!({"path": "src/lib.rs"}), "ok");
        let json = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        // sanity-check the tag-based wire format
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["role"], "tool");
        assert_eq!(v["name"], "read_file");
    }

    #[test]
    fn reasoning_helper_round_trips() {
        let m = Message::reasoning("think step by step");
        let json = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        // sanity-check the wire format
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["role"], "reasoning");
        assert_eq!(v["content"], "think step by step");
    }

    #[test]
    fn new_session_id_is_unique_and_time_ordered() {
        let a = Session::new_id();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = Session::new_id();
        assert_ne!(a, b);
        // UUID v7 is time-ordered as a string when sorted lexicographically
        // for ids generated at least 1 ms apart.
        assert!(a < b);
    }

    #[test]
    fn session_meta_derives_all_header_fields() {
        let s = Session {
            id: "0192f8e2-7b3a-7000-8a3e-000000000001".into(),
            agent: Agent::ClaudeCode,
            created_at: 1735689600,
            duration_ms: 12_345,
            prompt_summary: "fix the auth bug".into(),
            messages: vec![Message::user("fix the auth bug")],
            commits: vec!["deadbeef".into()],
            files_touched: vec!["src/auth.rs".into()],
            redaction_count: 3,
        };
        let m = SessionMeta::derive(&s, 215_040);
        assert_eq!(m.id, s.id);
        assert_eq!(m.agent, s.agent);
        assert_eq!(m.created_at, 1735689600);
        assert_eq!(m.duration_ms, 12_345);
        assert_eq!(m.prompt_summary, "fix the auth bug");
        assert_eq!(m.commits, vec!["deadbeef".to_string()]);
        assert_eq!(m.files_touched, vec!["src/auth.rs".to_string()]);
        assert_eq!(m.redaction_count, 3);
        assert_eq!(m.body_bytes, 215_040);
        assert!(!m.truncated);
    }

    #[test]
    fn session_meta_truncated_is_derived_from_the_marker() {
        let mut s = Session {
            id: "x".into(),
            agent: Agent::Codex,
            created_at: 0,
            duration_ms: 0,
            prompt_summary: "ok".into(),
            messages: vec![Message::tool(
                "bash",
                serde_json::json!({}),
                format!(
                    "head…{}412 KiB of tool output]…tail",
                    TRUNCATION_MARKER_PREFIX
                ),
            )],
            commits: vec![],
            files_touched: vec![],
            redaction_count: 0,
        };
        assert!(SessionMeta::derive(&s, 1).truncated);
        s.messages = vec![Message::user("no marker here")];
        assert!(!SessionMeta::derive(&s, 1).truncated);
    }

    #[test]
    fn session_meta_round_trips_through_json() {
        let m = SessionMeta {
            id: "a".into(),
            agent: Agent::Codex,
            created_at: 1,
            duration_ms: 2,
            prompt_summary: "p".into(),
            commits: vec![],
            files_touched: vec![],
            redaction_count: 0,
            body_bytes: 9,
            truncated: true,
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: SessionMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
}
