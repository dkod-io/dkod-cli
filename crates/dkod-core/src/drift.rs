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

/// Area words that authorize a sensitive glob when they appear in the intent:
/// "update the CI workflow" authorizes `.github/workflows/**` without naming
/// the file. Keyed by the exact default glob string; custom globs simply have
/// no association (and are never area-suppressed).
const AREA_KEYWORDS: &[(&str, &[&str])] = &[
    (
        ".github/workflows/**",
        &["workflow", "workflows", "ci", "pipeline"],
    ),
    ("**/Dockerfile", &["dockerfile"]),
    ("**/auth*", &["auth", "authentication", "authorization"]),
    ("**/migrations/**", &["migration", "migrations"]),
];

fn basename_lower(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_lowercase()
}

/// Lowercase alphanumeric word tokens of the intent ("run cargo add serde"
/// → {run, cargo, add, serde}; "src/auth.rs" → {src, auth, rs}).
fn word_tokens(intent_lower: &str) -> std::collections::HashSet<String> {
    intent_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// True when `keyword` appears in the intent: single-word keywords must match
/// a whole word token (so "dep" never matches "deploy"); multi-word keywords
/// match as substrings.
fn keyword_present(
    keyword: &str,
    intent_lower: &str,
    tokens: &std::collections::HashSet<String>,
) -> bool {
    let k = keyword.to_lowercase();
    if k.split_whitespace().nth(1).is_some() {
        intent_lower.contains(&k)
    } else {
        tokens.contains(&k)
    }
}

/// Path-shaped or basename tokens the intent explicitly mentions (lowercased,
/// deduplicated). Shared by the sensitive-path authorization check and the
/// unmentioned-file rule.
fn mentioned_paths(
    intent: &str,
    touched_basenames: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut mentioned: Vec<String> = Vec::new();
    for tok in intent
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '`' | '"' | '\''))
    {
        // Trim trailing punctuation (sentence-ending `.` / `:`) but keep
        // leading dots so dotfiles like `.env` stay recognizable.
        let t = tok.trim_end_matches(['.', ':']);
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
    mentioned
}

/// Basename without its last extension ("test_cache.py" → "test_cache").
fn stem(basename: &str) -> &str {
    basename
        .rsplit_once('.')
        .map(|(s, _)| s)
        .filter(|s| !s.is_empty())
        .unwrap_or(basename)
}

/// For a test-file stem, the name of the unit under test:
/// "test_cache" → "cache", "retry.test" → "retry", "http_client_spec" →
/// "http_client". `None` when the stem is not test-shaped.
fn test_subject(stem: &str) -> Option<&str> {
    for prefix in ["test_", "test-"] {
        if let Some(rest) = stem.strip_prefix(prefix) {
            return Some(rest).filter(|r| !r.is_empty());
        }
    }
    for suffix in ["_test", "-test", ".test", "_spec", "-spec", ".spec"] {
        if let Some(rest) = stem.strip_suffix(suffix) {
            return Some(rest).filter(|r| !r.is_empty());
        }
    }
    None
}

/// True when one of `a`/`b` is a test file for the other ("test_cache.py" ↔
/// "cache.py"). Inputs are lowercased basenames.
fn test_pair(a: &str, b: &str) -> bool {
    test_subject(stem(a)) == Some(stem(b)) || test_subject(stem(b)) == Some(stem(a))
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
    let tokens = word_tokens(&intent_lower);
    let touched_basenames: std::collections::HashSet<String> = session
        .files_touched
        .iter()
        .map(|f| basename_lower(f))
        .collect();
    let mentioned = mentioned_paths(&intent, &touched_basenames);
    let mentioned_basenames: std::collections::HashSet<String> =
        mentioned.iter().map(|m| basename_lower(m)).collect();

    // A dependency ask makes lockfile churn the mechanical consequence of the
    // prompt: lockfiles are exempt from the tripwire, manifest+lockfile pairs
    // from the unmentioned rule. Non-lockfile sensitive paths stay armed.
    let dep_ask = cfg
        .dep_ask_keywords
        .iter()
        .any(|k| keyword_present(k, &intent_lower, &tokens));
    let is_lockfile = |file: &str| cfg.lockfile_paths.iter().any(|g| glob_match(g, file));
    let is_manifest = |file: &str| cfg.manifest_paths.iter().any(|g| glob_match(g, file));

    // Rule 1: sensitive-path tripwire (authorization-aware).
    let mut sensitive_emitted = 0usize;
    for file in &session.files_touched {
        if sensitive_emitted >= SENSITIVE_CAP {
            break;
        }
        // The prompt naming the file (basename or path token) authorizes it.
        let explicitly_mentioned = mentioned_basenames.contains(&basename_lower(file))
            || mentioned.iter().any(|m| m == &file.to_lowercase());
        for glob in &cfg.sensitive_paths {
            if !glob_match(glob, file) {
                continue;
            }
            if dep_ask && is_lockfile(file) {
                continue; // suppressed: lockfile churn under a dependency ask
            }
            if explicitly_mentioned {
                continue; // suppressed: the prompt named this very file
            }
            // The prompt naming the area ("update the CI workflow") authorizes
            // exactly the associated glob — other sensitive globs still fire.
            let area_authorized = AREA_KEYWORDS
                .iter()
                .any(|(g, words)| g == glob && words.iter().any(|w| tokens.contains(*w)));
            if area_authorized {
                continue;
            }
            verdict.reasons.push(DriftReason {
                rule: "sensitive_path",
                detail: format!("touched sensitive path: {file} (matched {glob})"),
            });
            sensitive_emitted += 1;
            break;
        }
    }

    // Rule 2: small-ask / large-change magnitude. Broad-scope phrases
    // ("rename X everywhere") make a prompt not-small regardless of length.
    let broad_scope = cfg
        .broad_scope_phrases
        .iter()
        .any(|p| intent_lower.contains(&p.to_lowercase()));
    let small_ask = !broad_scope
        && (intent.chars().count() <= cfg.small_ask_max_chars
            || cfg
                .small_ask_keywords
                .iter()
                .any(|k| intent_lower.contains(&k.to_lowercase())));
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
    // Exemptions: under a dependency ask, manifest+lockfile churn is the
    // mechanical companion of the ask; a named test file vouches for its
    // source under test (and vice versa).
    if !mentioned.is_empty() {
        let unmentioned: Vec<&String> = session
            .files_touched
            .iter()
            .filter(|f| !mentioned_basenames.contains(&basename_lower(f)))
            .filter(|f| !(dep_ask && (is_lockfile(f) || is_manifest(f))))
            .filter(|f| {
                let fb = basename_lower(f);
                !mentioned.iter().any(|m| test_pair(&basename_lower(m), &fb))
            })
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

    // --- dependency-aware lockfile suppression (issue #28) ---

    #[test]
    fn dep_ask_with_lockfile_churn_is_clean() {
        let s = session(
            "add the axios dependency and use it for the weather fetch in src/weather.js",
            &["package.json", "package-lock.json", "src/weather.js"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            v.is_clean(),
            "got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
    }

    #[test]
    fn cargo_add_lockfile_churn_is_clean() {
        let s = session(
            "run cargo add serde and derive Serialize on the config structs in src/config.rs",
            &["Cargo.toml", "Cargo.lock", "src/config.rs"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            v.is_clean(),
            "got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
    }

    #[test]
    fn dep_ask_suppresses_only_lockfiles_not_other_sensitive_paths() {
        let s = session(
            "update the dependencies",
            &["Cargo.lock", ".github/workflows/ci.yml"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            v.reasons.iter().any(
                |r| r.rule == "sensitive_path" && r.detail.contains(".github/workflows/ci.yml")
            ),
            "workflow must still flag; got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
        assert!(
            !v.reasons.iter().any(|r| r.detail.contains("Cargo.lock")),
            "lockfile must be suppressed under a dep ask; got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
    }

    #[test]
    fn lockfile_churn_without_dep_ask_still_flags() {
        let s = session(
            "fix the flaky retry test",
            &["tests/retry.test.js", "package-lock.json"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(v
            .reasons
            .iter()
            .any(|r| r.rule == "sensitive_path" && r.detail.contains("package-lock.json")));
    }

    // --- authorization-aware sensitive-path tripwire (issue #28) ---

    #[test]
    fn authorized_ci_workflow_ask_is_clean() {
        let s = session(
            "update the ci workflow to also run clippy on pull requests",
            &[".github/workflows/ci.yml"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            v.is_clean(),
            "got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unauthorized_workflow_touch_still_flags() {
        let s = session("fix typo", &[".github/workflows/ci.yml"]);
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(v
            .reasons
            .iter()
            .any(|r| r.rule == "sensitive_path" && r.detail.contains("ci.yml")));
    }

    #[test]
    fn explicit_path_mention_suppresses_sensitive_reason() {
        let s = session(
            "pin the ubuntu runner to 22.04 in .github/workflows/deploy.yml like the other repos",
            &[".github/workflows/deploy.yml"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            v.is_clean(),
            "got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
    }

    #[test]
    fn authorized_mention_suppresses_only_matching_glob() {
        // Workflow explicitly named; the Dockerfile is not — only the
        // Dockerfile reason should survive.
        let s = session(
            "update the ci workflow to cache cargo builds",
            &[".github/workflows/ci.yml", "Dockerfile"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            v.reasons
                .iter()
                .any(|r| r.rule == "sensitive_path" && r.detail.contains("Dockerfile")),
            "Dockerfile must still flag; got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
        assert!(!v.reasons.iter().any(|r| r.detail.contains("ci.yml")));
    }

    #[test]
    fn dockerfile_area_word_authorizes_dockerfile() {
        let s = session(
            "use node 20 in the Dockerfile and switch to npm ci",
            &["Dockerfile"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            v.is_clean(),
            "got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
    }

    #[test]
    fn migration_area_word_authorizes_migrations() {
        let s = session(
            "write a migration adding the deleted_at column to users and update the model",
            &[
                "db/migrations/0051_add_deleted_at.sql",
                "app/models/user.py",
            ],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            v.is_clean(),
            "got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
    }

    #[test]
    fn auth_area_word_authorizes_auth_paths() {
        let s = session(
            "implement the password reset flow in src/auth.rs with token expiry",
            &["src/auth.rs"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            !v.reasons.iter().any(|r| r.rule == "sensitive_path"),
            "got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
    }

    // --- scope-aware small-ask (issue #28 stretch) ---

    #[test]
    fn broad_scope_rename_everywhere_does_not_fire_magnitude() {
        let s = session(
            "rename the User.fullname field to display_name everywhere it appears",
            &[
                "a.py", "b.py", "c.py", "d.py", "e.py", "f.py", "g.py", "h.py", "i.py",
            ],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            !v.reasons.iter().any(|r| r.rule == "magnitude"),
            "got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
    }

    #[test]
    fn vague_short_prompt_with_many_files_still_fires_magnitude() {
        let s = session(
            "improve stuff",
            &[
                "a.py", "b.py", "c.py", "d.py", "e.py", "f.py", "g.py", "h.py", "i.py",
            ],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(v.reasons.iter().any(|r| r.rule == "magnitude"));
    }

    // --- test-file / source-under-test pairing in the unmentioned rule ---

    #[test]
    fn named_test_file_pairs_with_its_source_under_test() {
        let s = session(
            "make tests/test_cache.py pass",
            &["tests/test_cache.py", "src/cache.py"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            !v.reasons.iter().any(|r| r.rule == "unmentioned"),
            "got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unmentioned_still_fires_on_unrelated_companion() {
        let s = session(
            "make tests/test_cache.py pass",
            &["tests/test_cache.py", "src/billing.py"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(v
            .reasons
            .iter()
            .any(|r| r.rule == "unmentioned" && r.detail.contains("src/billing.py")));
    }

    #[test]
    fn dotfile_mention_keeps_leading_dot_and_authorizes() {
        // ".env" must survive token trimming (only trailing punctuation is
        // stripped), so naming it authorizes the **/.env* tripwire.
        let s = session(
            "add the STRIPE_KEY placeholder to .env.example",
            &[".env.example"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(
            v.is_clean(),
            "got: {:?}",
            v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>()
        );
    }
}
