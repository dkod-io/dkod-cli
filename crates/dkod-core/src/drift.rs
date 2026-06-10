//! Intent-vs-output drift detection: flag sessions where the agent did
//! materially more / other than the prompt asked. Pure and local — no I/O,
//! no git, no network. The CLI layer (`cmd::drift`) feeds it diff stats and
//! renders the verdict.

/// Match `path` (forward-slash, no leading `/`) against a simple glob
/// `pattern`. Supported syntax (dependency-free, sufficient for the default
/// sensitive-path set): path and pattern are split on `/`; a `**` segment
/// matches zero or more path segments; within a segment `*` matches any run
/// of non-`/` characters; all other characters match literally.
pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let txt: Vec<&str> = path.split('/').collect();
    seg_match(&pat, &txt)
}

fn seg_match(pat: &[&str], txt: &[&str]) -> bool {
    match pat.split_first() {
        None => txt.is_empty(),
        Some((&"**", rest)) => {
            // `**` matches zero or more path segments; the remaining pattern
            // must then match the remaining path (anchored to the end).
            for i in 0..=txt.len() {
                if seg_match(rest, &txt[i..]) {
                    return true;
                }
            }
            false
        }
        Some((&seg, rest)) => {
            if txt.is_empty() {
                return false;
            }
            if segment_match(seg, txt[0]) {
                seg_match(rest, &txt[1..])
            } else {
                false
            }
        }
    }
}

/// Match a single path segment (no `/`) against a single pattern segment where
/// `*` matches any run of characters.
fn segment_match(pat: &str, txt: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == txt;
    }
    let mut pos = 0usize;
    if !txt[pos..].starts_with(parts[0]) {
        return false;
    }
    pos += parts[0].len();
    for part in &parts[1..parts.len() - 1] {
        if part.is_empty() {
            continue;
        }
        match txt[pos..].find(part) {
            Some(found) => pos += found + part.len(),
            None => return false,
        }
    }
    let last = parts[parts.len() - 1];
    txt.len() >= pos + last.len() && txt[pos..].ends_with(last)
}

/// Diff magnitude for a session's commits, supplied by the CLI layer.
#[derive(Debug, Clone)]
pub struct DiffStats {
    pub files: usize,
    pub insertions: usize,
    pub deletions: usize,
}

/// A single reason a session was flagged. `rule` is a stable machine tag
/// (`"sensitive_path"`, `"magnitude"`, `"unmentioned"`); `detail` is for humans.
#[derive(Debug, Clone)]
pub struct DriftReason {
    pub rule: &'static str,
    pub detail: String,
}

/// The drift verdict for one session. Empty `reasons` == clean.
#[derive(Debug, Clone, Default)]
pub struct DriftVerdict {
    pub reasons: Vec<DriftReason>,
}

impl DriftVerdict {
    pub fn is_clean(&self) -> bool {
        self.reasons.is_empty()
    }
}

const LIST_CAP: usize = 5;
const SENSITIVE_CAP: usize = 10;

fn basename_lower(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_lowercase()
}

/// Analyze a session for intent-vs-output drift. Pure: no I/O. `diff_stats` is
/// `None` when the session's commits are unavailable (line-magnitude signal is
/// then skipped; file-count still applies).
pub fn analyze(
    session: &crate::Session,
    diff_stats: Option<&DiffStats>,
    cfg: &crate::config::DriftConfig,
) -> DriftVerdict {
    let mut verdict = DriftVerdict::default();
    if !cfg.enabled {
        return verdict;
    }

    let mut intent = session
        .messages
        .iter()
        .filter_map(|m| match m {
            crate::Message::User { content } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if intent.trim().is_empty() {
        intent = session.prompt_summary.clone();
    }
    let intent_lower = intent.to_lowercase();

    // Rule 1: sensitive-path tripwire.
    let mut sensitive_emitted = 0usize;
    for file in &session.files_touched {
        if sensitive_emitted >= SENSITIVE_CAP {
            break;
        }
        for glob in &cfg.sensitive_paths {
            if glob_match(glob, file) {
                verdict.reasons.push(DriftReason {
                    rule: "sensitive_path",
                    detail: format!("touched sensitive path: {file} (matched {glob})"),
                });
                sensitive_emitted += 1;
                break;
            }
        }
    }

    // Rule 2: small-ask / large-change magnitude.
    let small_ask = intent.chars().count() <= cfg.small_ask_max_chars
        || cfg
            .small_ask_keywords
            .iter()
            .any(|k| intent_lower.contains(&k.to_lowercase()));
    let large_by_files = session.files_touched.len() >= cfg.large_change_files;
    let large_by_lines = diff_stats
        .map(|d| d.insertions + d.deletions >= cfg.large_change_lines)
        .unwrap_or(false);
    if small_ask && (large_by_files || large_by_lines) {
        let lines_clause = match diff_stats {
            Some(d) => format!(" / {} lines", d.insertions + d.deletions),
            None => String::new(),
        };
        verdict.reasons.push(DriftReason {
            rule: "magnitude",
            detail: format!(
                "small-sounding request but {} files{} changed",
                session.files_touched.len(),
                lines_clause
            ),
        });
    }

    // Rule 3: unmentioned-file drift (only when the prompt named paths).
    let touched_basenames: std::collections::HashSet<String> = session
        .files_touched
        .iter()
        .map(|f| basename_lower(f))
        .collect();
    let mut mentioned: Vec<String> = Vec::new();
    for tok in intent
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '`' | '"' | '\''))
    {
        let t = tok.trim_matches(|c: char| matches!(c, '.' | ':'));
        if t.is_empty() {
            continue;
        }
        let path_shaped = t.contains('/') && t.rsplit('/').next().is_some_and(|b| b.contains('.'));
        let is_basename = touched_basenames.contains(&basename_lower(t));
        if path_shaped || is_basename {
            let tl = t.to_lowercase();
            if !mentioned.iter().any(|m| m.eq_ignore_ascii_case(&tl)) {
                mentioned.push(tl);
            }
        }
    }
    if !mentioned.is_empty() {
        let mentioned_basenames: std::collections::HashSet<String> =
            mentioned.iter().map(|m| basename_lower(m)).collect();
        let unmentioned: Vec<&String> = session
            .files_touched
            .iter()
            .filter(|f| !mentioned_basenames.contains(&basename_lower(f)))
            .collect();
        if !unmentioned.is_empty() {
            let shown: Vec<String> = unmentioned
                .iter()
                .take(LIST_CAP)
                .map(|s| s.to_string())
                .collect();
            let mentioned_shown: Vec<String> = mentioned.iter().take(LIST_CAP).cloned().collect();
            verdict.reasons.push(DriftReason {
                rule: "unmentioned",
                detail: format!(
                    "prompt referenced {}; also changed unrelated {}",
                    mentioned_shown.join(", "),
                    shown.join(", ")
                ),
            });
        }
    }

    verdict
}

#[cfg(test)]
mod glob_tests {
    use super::glob_match;
    #[test]
    fn double_star_suffix() {
        assert!(glob_match("**/*.pem", "a/b/c.pem"));
        assert!(glob_match("**/*.pem", "c.pem"));
        assert!(!glob_match("**/*.pem", "c.pemx"));
    }
    #[test]
    fn dir_prefix_double_star() {
        assert!(glob_match(
            ".github/workflows/**",
            ".github/workflows/ci.yml"
        ));
        assert!(glob_match(
            ".github/workflows/**",
            ".github/workflows/sub/x.yml"
        ));
        assert!(!glob_match(".github/workflows/**", "src/x.yml"));
    }
    #[test]
    fn env_and_auth_prefixes() {
        assert!(glob_match("**/.env*", ".env"));
        assert!(glob_match("**/.env*", "cfg/.env.local"));
        // `**/auth*` matches a basename starting with "auth" at any depth,
        // NOT a file inside an `auth/` directory (whose basename differs).
        assert!(glob_match("**/auth*", "src/auth.rs"));
        assert!(glob_match("**/auth*", "auth.rs"));
        assert!(!glob_match("**/auth*", "auth/mod.rs"));
        assert!(!glob_match("**/auth*", "src/oauth.rs"));
    }
    #[test]
    fn exact_basename_any_depth() {
        assert!(glob_match("**/Cargo.lock", "Cargo.lock"));
        assert!(glob_match("**/Cargo.lock", "crates/x/Cargo.lock"));
        assert!(!glob_match("**/Cargo.lock", "Cargo.toml"));
    }
    #[test]
    fn star_does_not_cross_slash() {
        assert!(glob_match("src/*.rs", "src/a.rs"));
        assert!(!glob_match("src/*.rs", "src/a/b.rs"));
    }
    #[test]
    fn middle_double_star() {
        assert!(glob_match("**/migrations/**", "db/migrations/001_init.sql"));
        assert!(glob_match("**/migrations/**", "migrations/x.sql"));
        assert!(!glob_match("**/migrations/**", "db/models/user.rs"));
    }
}

#[cfg(test)]
mod analyze_tests {
    use super::*;
    use crate::config::DriftConfig;
    use crate::{Agent, Message, Session};

    fn session(prompt: &str, files: &[&str]) -> Session {
        Session {
            id: "0192f8e2-7b3a-7000-8a3e-000000000001".into(),
            agent: Agent::ClaudeCode,
            created_at: 0,
            duration_ms: 0,
            prompt_summary: prompt.into(),
            messages: vec![Message::user(prompt)],
            commits: vec![],
            files_touched: files.iter().map(|s| s.to_string()).collect(),
            redaction_count: 0,
        }
    }

    #[test]
    fn sensitive_path_trips() {
        let s = session("fix the readme", &[".github/workflows/ci.yml"]);
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(!v.is_clean());
        assert!(v
            .reasons
            .iter()
            .any(|r| r.rule == "sensitive_path" && r.detail.contains(".github/workflows/ci.yml")));
    }
    #[test]
    fn benign_file_is_clean() {
        let s = session(
            "update the parser thoroughly across the module",
            &["src/lib.rs"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            v.is_clean(),
            "got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
    }
    #[test]
    fn small_ask_large_change_by_filecount_flags() {
        let s = session("fix typo", &["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"]);
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(v
            .reasons
            .iter()
            .any(|r| r.rule == "magnitude" && r.detail.contains("5 files")));
    }
    #[test]
    fn small_ask_small_change_is_clean() {
        let s = session("fix typo", &["a.rs"]);
        assert!(analyze(&s, None, &DriftConfig::default()).is_clean());
    }
    #[test]
    fn large_change_with_verbose_ask_is_clean_on_magnitude() {
        let long = "please carefully refactor the entire parser subsystem and update \
                    every call site and the associated tests across the whole repository \
                    being thorough and complete in the work";
        let s = session(long, &["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"]);
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(!v.reasons.iter().any(|r| r.rule == "magnitude"));
    }
    #[test]
    fn magnitude_uses_lines_when_diffstats_present() {
        let s = session("tiny tweak", &["a.rs"]);
        let stats = DiffStats {
            files: 1,
            insertions: 200,
            deletions: 0,
        };
        let v = analyze(&s, Some(&stats), &DriftConfig::default());
        assert!(v
            .reasons
            .iter()
            .any(|r| r.rule == "magnitude" && r.detail.contains("lines")));
    }
    #[test]
    fn unmentioned_file_flags_when_prompt_named_paths() {
        let s = session("please fix the bug in auth.rs", &["auth.rs", "billing.rs"]);
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(v
            .reasons
            .iter()
            .any(|r| r.rule == "unmentioned" && r.detail.contains("billing.rs")));
    }
    #[test]
    fn unmentioned_does_not_fire_on_vague_prompt() {
        let s = session(
            "improve the overall reliability of the system in a general sense across everything",
            &["a.rs", "b.rs", "c.rs"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(!v.reasons.iter().any(|r| r.rule == "unmentioned"));
    }
    #[test]
    fn disabled_config_is_always_clean() {
        let cfg = DriftConfig {
            enabled: false,
            ..DriftConfig::default()
        };
        let s = session(
            "fix typo",
            &[".github/workflows/ci.yml", "a.rs", "b.rs", "c.rs", "d.rs"],
        );
        assert!(analyze(&s, None, &cfg).is_clean());
    }
    #[test]
    fn intent_falls_back_to_prompt_summary_when_no_user_messages() {
        // No User messages → intent comes from prompt_summary. "fix typo" is a
        // small-ask keyword, 5 files is a large change → magnitude fires.
        let s = Session {
            id: "0192f8e2-7b3a-7000-8a3e-000000000002".into(),
            agent: Agent::ClaudeCode,
            created_at: 0,
            duration_ms: 0,
            prompt_summary: "fix typo".into(),
            messages: vec![], // no user content
            commits: vec![],
            files_touched: vec!["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"]
                .into_iter()
                .map(String::from)
                .collect(),
            redaction_count: 0,
        };
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            v.reasons.iter().any(|r| r.rule == "magnitude"),
            "prompt_summary fallback should drive the small-ask signal"
        );
    }
    #[test]
    fn empty_files_touched_is_clean() {
        let s = session("fix typo", &[]);
        assert!(analyze(&s, None, &DriftConfig::default()).is_clean());
    }
}
