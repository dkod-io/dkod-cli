use crate::config::RedactConfig;
use regex::Regex;
use std::sync::OnceLock;

pub fn redact(input: &str, cfg: &RedactConfig) -> String {
    if !cfg.enabled {
        return input.to_string();
    }
    let mut out = input.to_string();
    for p in &cfg.patterns {
        out = match p.as_str() {
            "builtin:aws" => aws_re().replace_all(&out, "[REDACTED:aws]").to_string(),
            "builtin:github_token" => github_re()
                .replace_all(&out, "[REDACTED:github_token]")
                .to_string(),
            "builtin:openai_key" => openai_re()
                .replace_all(&out, "[REDACTED:openai_key]")
                .to_string(),
            "builtin:stripe" => stripe_re()
                .replace_all(&out, "[REDACTED:stripe]")
                .to_string(),
            "builtin:env_assignment" => env_re()
                .replace_all(&out, "${lhs}[REDACTED:env_assignment]")
                .to_string(),
            _ => out,
        };
    }
    for custom in &cfg.custom {
        match Regex::new(custom) {
            Ok(re) => out = re.replace_all(&out, "[REDACTED:custom]").to_string(),
            Err(e) => eprintln!("dkod: invalid custom redact pattern {custom:?}: {e}"),
        }
    }
    out
}

fn aws_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(?:AKIA|ASIA|AROA|ABIA|ACCA)[0-9A-Z]{16}\b").unwrap())
}

fn github_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Matches classic PATs (ghp_, gho_, ghu_, ghs_) and fine-grained PATs (github_pat_).
    RE.get_or_init(|| {
        Regex::new(r"\b(?:gh[pous]_[A-Za-z0-9]{36,255}|github_pat_[A-Za-z0-9_]{22,255})\b").unwrap()
    })
}

fn openai_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Test fixture has 56 chars after `sk-proj-`. Real keys are 40+. Use {40,} as lower bound.
    RE.get_or_init(|| Regex::new(r"sk-(?:proj-)?[A-Za-z0-9_\-]{40,}").unwrap())
}

fn stripe_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"sk_(?:live|test)_[A-Za-z0-9]{24,}").unwrap())
}

fn env_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?P<lhs>\b[A-Z][A-Z0-9_]*=)(?P<rhs>\S+)").unwrap())
}

/// Apply redaction to every text-bearing field of a `Session` in place.
/// Walks `prompt_summary` and each `Message` variant.
/// Tool inputs are JSON values; we redact every JSON string we encounter recursively.
pub fn redact_session(s: &mut crate::Session, cfg: &RedactConfig) {
    if !cfg.enabled {
        return;
    }
    s.prompt_summary = redact(&s.prompt_summary, cfg);
    for m in &mut s.messages {
        match m {
            crate::Message::User { content }
            | crate::Message::Assistant { content }
            | crate::Message::Reasoning { content } => {
                *content = redact(content, cfg);
            }
            crate::Message::Tool { input, output, .. } => {
                redact_json(input, cfg);
                *output = redact(output, cfg);
            }
        }
    }
}

fn redact_json(value: &mut serde_json::Value, cfg: &RedactConfig) {
    use serde_json::Value;
    match value {
        Value::String(s) => *s = redact(s, cfg),
        Value::Array(arr) => {
            for v in arr {
                redact_json(v, cfg);
            }
        }
        Value::Object(map) => {
            for (_k, v) in map.iter_mut() {
                redact_json(v, cfg);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(input: &str) -> String {
        let cfg = crate::config::RedactConfig::default();
        redact(input, &cfg)
    }

    #[test]
    fn redacts_aws_access_key() {
        assert_eq!(r("AKIAIOSFODNN7EXAMPLE"), "[REDACTED:aws]");
        assert!(r("token: AKIAIOSFODNN7EXAMPLE rest").contains("[REDACTED:aws]"));
    }

    #[test]
    fn redacts_github_token() {
        assert!(r("ghp_1234567890abcdefABCDEF1234567890abcdef").contains("[REDACTED:github_token]"));
        assert!(r(
            "github_pat_11ABCDEFG_1234567890abcdef1234567890ABCDEF1234567890abcdef1234567890ABCDEF"
        )
        .contains("[REDACTED:github_token]"));
    }

    #[test]
    fn redacts_openai_key() {
        assert!(
            r("sk-proj-abcdefABCDEF0123456789_-abcdefABCDEF0123456789_-abcdefAB")
                .contains("[REDACTED:openai_key]")
        );
    }

    #[test]
    fn redacts_stripe_key() {
        assert!(r("sk_live_abcdefABCDEF0123456789ABCD").contains("[REDACTED:stripe]"));
    }

    #[test]
    fn redacts_env_assignment() {
        assert_eq!(
            r("API_KEY=supersecret"),
            "API_KEY=[REDACTED:env_assignment]"
        );
        assert!(r("export DB_PASS=hunter2").contains("[REDACTED:env_assignment]"));
    }

    #[test]
    fn redaction_is_idempotent() {
        let cfg = crate::config::RedactConfig::default();
        let once = redact("AKIAIOSFODNN7EXAMPLE", &cfg);
        let twice = redact(&once, &cfg);
        assert_eq!(once, twice);
    }

    #[test]
    fn redacts_session_messages() {
        use crate::{Agent, Message, Session};
        let mut s = Session {
            id: "x".into(),
            agent: Agent::Codex,
            created_at: 0,
            duration_ms: 0,
            prompt_summary: "AKIAIOSFODNN7EXAMPLE".into(),
            messages: vec![
                Message::user("API_KEY=supersecret"),
                Message::assistant("see file"),
                Message::tool(
                    "read_file",
                    serde_json::json!({"path": "src/lib.rs"}),
                    "GITHUB_TOKEN=ghp_1234567890abcdef1234567890abcdef1234",
                ),
            ],
            commits: vec![],
            files_touched: vec![],
        };
        redact_session(&mut s, &crate::config::RedactConfig::default());
        assert_eq!(s.prompt_summary, "[REDACTED:aws]");

        if let Message::User { content } = &s.messages[0] {
            assert!(content.contains("[REDACTED:env_assignment]"));
        } else {
            panic!("expected User message");
        }

        if let Message::Tool { output, .. } = &s.messages[2] {
            assert!(output.contains("[REDACTED:env_assignment]"));
        } else {
            panic!("expected Tool message");
        }
    }

    #[test]
    fn redacts_reasoning_content() {
        use crate::{Agent, Message, Session};
        let mut s = Session {
            id: "x".into(),
            agent: Agent::Codex,
            created_at: 0,
            duration_ms: 0,
            prompt_summary: "ok".into(),
            messages: vec![Message::reasoning(
                "the user pasted GITHUB_TOKEN=ghp_1234567890abcdef1234567890abcdef1234",
            )],
            commits: vec![],
            files_touched: vec![],
        };
        redact_session(&mut s, &crate::config::RedactConfig::default());
        if let Message::Reasoning { content } = &s.messages[0] {
            assert!(
                content.contains("[REDACTED:env_assignment]"),
                "reasoning not redacted: {content}"
            );
        } else {
            panic!("expected Reasoning message");
        }
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn redact_session_is_a_no_op_when_disabled() {
        use crate::{Agent, Message, Session};
        let mut cfg = crate::config::RedactConfig::default();
        cfg.enabled = false;

        let mut s = Session {
            id: "x".into(),
            agent: Agent::Codex,
            created_at: 0,
            duration_ms: 0,
            prompt_summary: "AKIAIOSFODNN7EXAMPLE".into(),
            messages: vec![Message::user("API_KEY=supersecret")],
            commits: vec![],
            files_touched: vec![],
        };
        redact_session(&mut s, &cfg);
        assert_eq!(s.prompt_summary, "AKIAIOSFODNN7EXAMPLE");
        if let Message::User { content } = &s.messages[0] {
            assert_eq!(content, "API_KEY=supersecret");
        } else {
            panic!("expected User message");
        }
    }
}
