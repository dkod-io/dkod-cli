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
}

impl Session {
    pub fn new_id() -> String {
        uuid::Uuid::now_v7().to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    ClaudeCode,
    Codex,
    CopilotCli,
    GeminiCli,
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
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
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
}
