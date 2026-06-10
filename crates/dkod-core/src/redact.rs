use crate::config::RedactConfig;
use regex::Regex;
use std::sync::OnceLock;

pub fn redact(input: &str, cfg: &RedactConfig) -> String {
    redact_counting(input, cfg).0
}

/// Like [`redact`], but also returns how many replacements were made
/// (across builtins, the entropy rule, and custom patterns). Feeds the
/// per-session audit count (`Session::redaction_count`).
pub fn redact_counting(input: &str, cfg: &RedactConfig) -> (String, u64) {
    if !cfg.enabled {
        return (input.to_string(), 0);
    }
    let mut out = input.to_string();
    let mut count: u64 = 0;
    for p in &cfg.patterns {
        match p.as_str() {
            "builtin:aws" => apply(&mut out, aws_re(), "[REDACTED:aws]", &mut count),
            "builtin:github_token" => {
                apply(&mut out, github_re(), "[REDACTED:github_token]", &mut count)
            }
            "builtin:openai_key" => {
                apply(&mut out, openai_re(), "[REDACTED:openai_key]", &mut count)
            }
            "builtin:stripe" => apply(&mut out, stripe_re(), "[REDACTED:stripe]", &mut count),
            "builtin:env_assignment" => apply(
                &mut out,
                env_re(),
                "${lhs}[REDACTED:env_assignment]",
                &mut count,
            ),
            "builtin:entropy" => out = redact_entropy(&out, &mut count),
            _ => {}
        }
    }
    for custom in &cfg.custom {
        match Regex::new(custom) {
            Ok(re) => apply(&mut out, &re, "[REDACTED:custom]", &mut count),
            Err(e) => eprintln!("dkod: invalid custom redact pattern {custom:?}: {e}"),
        }
    }
    (out, count)
}

/// Replace every match of `re` in `out` with `rep`, adding the number of
/// matches to `count`.
fn apply(out: &mut String, re: &Regex, rep: &str, count: &mut u64) {
    let n = re.find_iter(out).count() as u64;
    if n > 0 {
        *count += n;
        *out = re.replace_all(out, rep).to_string();
    }
}

// --- entropy-based generic credential detection -------------------------
//
// Catches credentials the shape-specific builtins miss (random API keys,
// signing secrets, bearer tokens in unusual formats) by flagging long
// character runs whose per-char Shannon entropy looks machine-generated.

/// Minimum candidate length. Shorter runs don't carry enough signal to
/// separate secrets from ordinary identifiers, and real secrets are
/// almost always >= 24 chars.
const ENTROPY_MIN_LEN: usize = 24;

/// Per-char Shannon entropy threshold in bits. Hex tops out at 4.0 bits
/// (log2 16), so git SHAs and hex digests always stay below; random
/// base64 averages ~4.6-5.6 bits at these lengths; English text sits
/// around 3-4. 4.2 splits the two populations cleanly.
const ENTROPY_THRESHOLD_BITS: f64 = 4.2;

fn entropy_candidate_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Base64/base64url/hex-style alphabet, including `+/=` padding chars.
    RE.get_or_init(|| Regex::new(&format!(r"[A-Za-z0-9+/=_\-]{{{ENTROPY_MIN_LEN},}}")).unwrap())
}

fn redact_entropy(input: &str, count: &mut u64) -> String {
    entropy_candidate_re()
        .replace_all(input, |caps: &regex::Captures| {
            let tok = &caps[0];
            if entropy_skip(tok) || shannon_entropy_per_char(tok) < ENTROPY_THRESHOLD_BITS {
                tok.to_string()
            } else {
                *count += 1;
                "[REDACTED:entropy]".to_string()
            }
        })
        .to_string()
}

/// Clear false-positive classes the entropy rule must never touch.
fn entropy_skip(tok: &str) -> bool {
    // Git object ids (SHA-1 / SHA-256) legitimately appear in transcripts.
    // The 4.2-bit threshold already excludes hex (max 4.0 bits), but keep
    // an explicit guard so threshold tuning can never redact a SHA.
    let all_hex = tok.bytes().all(|b| b.is_ascii_hexdigit());
    if all_hex && (tok.len() == 40 || tok.len() == 64) {
        return true;
    }
    // Long numbers (ids, timestamps, order numbers) are not secrets.
    if tok.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    // Two or more `/` chars: overwhelmingly file paths, not base64.
    if tok.bytes().filter(|&b| b == b'/').count() >= 2 {
        return true;
    }
    false
}

/// Per-char Shannon entropy in bits. Candidates are ASCII by construction,
/// so byte-level frequencies are exact.
fn shannon_entropy_per_char(s: &str) -> f64 {
    let len = s.len();
    if len == 0 {
        return 0.0;
    }
    let mut counts = [0usize; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len as f64;
            -p * p.log2()
        })
        .sum()
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
/// Every replacement made increments `Session::redaction_count` (the audit count).
pub fn redact_session(s: &mut crate::Session, cfg: &RedactConfig) {
    if !cfg.enabled {
        return;
    }
    let mut count: u64 = 0;
    redact_field(&mut s.prompt_summary, cfg, &mut count);
    for m in &mut s.messages {
        match m {
            crate::Message::User { content }
            | crate::Message::Assistant { content }
            | crate::Message::Reasoning { content } => redact_field(content, cfg, &mut count),
            crate::Message::Tool { input, output, .. } => {
                redact_json(input, cfg, &mut count);
                redact_field(output, cfg, &mut count);
            }
        }
    }
    s.redaction_count += count;
}

fn redact_field(text: &mut String, cfg: &RedactConfig, count: &mut u64) {
    let (redacted, n) = redact_counting(text, cfg);
    *text = redacted;
    *count += n;
}

fn redact_json(value: &mut serde_json::Value, cfg: &RedactConfig, count: &mut u64) {
    use serde_json::Value;
    match value {
        Value::String(s) => {
            let (redacted, n) = redact_counting(s, cfg);
            *s = redacted;
            *count += n;
        }
        Value::Array(arr) => {
            for v in arr {
                redact_json(v, cfg, count);
            }
        }
        Value::Object(map) => {
            for (_k, v) in map.iter_mut() {
                redact_json(v, cfg, count);
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

    // The token-shaped string literals below are TEST FIXTURES, not
    // real secrets — every value is either an upstream-documented
    // example placeholder (`AKIAIOSFODNN7EXAMPLE` is AWS's own canon)
    // or a constructed-pattern that satisfies our regex's character
    // class without being valid at any provider. The
    // `gitleaks:allow` pragmas tell secret scanners (gitleaks,
    // CodeRabbit's secret detector, GitHub Advanced Security if it
    // ever lights up on this repo) to skip these specific lines so
    // the redactor's coverage tests can keep their realistic shapes
    // without polluting alert dashboards. The pragmas MUST be on
    // the same line as the literal — most scanners only honour
    // line-local directives — so the longer fixtures live in `let`
    // bindings rather than inlined into the assertion expression.

    #[test]
    fn redacts_aws_access_key() {
        let aws = "AKIAIOSFODNN7EXAMPLE"; // gitleaks:allow
        assert_eq!(r(aws), "[REDACTED:aws]");
        assert!(r(&format!("token: {aws} rest")).contains("[REDACTED:aws]"));
    }

    // `#[rustfmt::skip]` on the next two tests keeps rustfmt from
    // breaking the `let` lines so the `// gitleaks:allow` pragma can
    // sit on the same line as the literal — most scanners only honour
    // line-local directives, and cargo fmt's default 100-char limit
    // would otherwise force a multi-line layout that defeats them.
    #[test]
    #[rustfmt::skip]
    fn redacts_github_token() {
        let ghp = "ghp_1234567890abcdefABCDEF1234567890abcdef"; // gitleaks:allow
        assert!(r(ghp).contains("[REDACTED:github_token]"));
        let pat = "github_pat_11ABCDEFG_1234567890abcdef1234567890ABCDEF1234567890abcdef1234567890ABCDEF"; // gitleaks:allow
        assert!(r(pat).contains("[REDACTED:github_token]"));
    }

    #[test]
    #[rustfmt::skip]
    fn redacts_openai_key() {
        let key = "sk-proj-abcdefABCDEF0123456789_-abcdefABCDEF0123456789_-abcdefAB"; // gitleaks:allow
        assert!(r(key).contains("[REDACTED:openai_key]"));
    }

    #[test]
    fn redacts_stripe_key() {
        let key = "sk_live_abcdefABCDEF0123456789ABCD"; // gitleaks:allow
        assert!(r(key).contains("[REDACTED:stripe]"));
    }

    #[test]
    fn redacts_env_assignment() {
        assert_eq!(
            r("API_KEY=supersecret"),
            "API_KEY=[REDACTED:env_assignment]"
        );
        assert!(r("export DB_PASS=hunter2").contains("[REDACTED:env_assignment]"));
    }

    // --- entropy rule -------------------------------------------------

    #[test]
    #[rustfmt::skip]
    fn entropy_redacts_random_base64_secret_keeping_surrounding_text() {
        use crate::{Agent, Message, Session};
        let secret = "kJ8mN2pQ7rX4vB9zL5wY3tG6hD0fS1aC"; // gitleaks:allow
        let mut s = Session {
            id: "x".into(),
            agent: Agent::Codex,
            created_at: 0,
            duration_ms: 0,
            prompt_summary: "ok".into(),
            messages: vec![Message::user(format!("use token {secret} please"))],
            commits: vec![],
            files_touched: vec![],
            redaction_count: 0,
        };
        redact_session(&mut s, &crate::config::RedactConfig::default());
        if let Message::User { content } = &s.messages[0] {
            assert!(content.contains("[REDACTED:entropy]"), "not redacted: {content}");
            assert!(content.starts_with("use token "));
            assert!(content.ends_with(" please"));
            assert!(!content.contains(secret));
        } else {
            panic!("expected User message");
        }
        assert_eq!(s.redaction_count, 1);
    }

    #[test]
    fn entropy_keeps_git_shas() {
        // git object ids legitimately appear in transcripts; all-hex strings
        // of exactly 40 (SHA-1) or 64 (SHA-256) chars are never entropy hits.
        let sha1 = "3c28886a1b2c3d4e5f60718293a4b5c6d7e8f901";
        assert_eq!(r(sha1), sha1);
        let sha256 = "3c28886a1b2c3d4e5f60718293a4b5c6d7e8f9013c28886a1b2c3d4e5f60718a";
        assert_eq!(r(sha256), sha256);
    }

    #[test]
    fn entropy_keeps_low_entropy_english_runs() {
        let input = "this perfectly-normal-english-sentence-fragment stays intact";
        assert_eq!(r(input), input);
    }

    #[test]
    fn entropy_keeps_path_like_tokens() {
        let input = "see src/kJ8mN2pQ7rX4/vB9zL5wY3tG6hD0 for details";
        assert_eq!(r(input), input);
    }

    #[test]
    fn entropy_keeps_all_digit_runs() {
        let input = "order id 123456789012345678901234567890 confirmed";
        assert_eq!(r(input), input);
    }

    // --- audit count ---------------------------------------------------

    #[test]
    #[rustfmt::skip]
    fn redaction_count_accumulates_across_rules() {
        use crate::{Agent, Message, Session};
        let aws = "AKIAIOSFODNN7EXAMPLE"; // gitleaks:allow
        let secret = "kJ8mN2pQ7rX4vB9zL5wY3tG6hD0fS1aC"; // gitleaks:allow
        let mut s = Session {
            id: "x".into(),
            agent: Agent::Codex,
            created_at: 0,
            duration_ms: 0,
            prompt_summary: "ok".into(),
            messages: vec![Message::user(format!("key {aws} and token {secret}"))],
            commits: vec![],
            files_touched: vec![],
            redaction_count: 0,
        };
        redact_session(&mut s, &crate::config::RedactConfig::default());
        assert_eq!(s.redaction_count, 2, "messages: {:?}", s.messages);
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
            redaction_count: 0,
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
            redaction_count: 0,
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
            redaction_count: 0,
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
