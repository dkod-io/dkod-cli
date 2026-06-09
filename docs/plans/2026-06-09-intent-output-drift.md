# Intent-vs-Output Drift (`dkod drift`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `dkod drift` — a local, rule-based command that flags captured sessions where the agent did materially more/other than the prompt asked, computed on-read over stored sessions, producing structured human-readable reasons.

**Architecture:** A pure `dkod_core::drift` module (`analyze` + a dependency-free glob matcher + types) is the testable heart. A new `[drift]` section on `dkod_core::config::Config` holds defaults. The CLI `cmd::drift` resolves sessions (like `cmd::log`), derives `DiffStats` via a `git` shell-out, calls `analyze`, and renders. Zero network, zero new crates, exit code always 0 (report, not gate).

**Tech Stack:** Rust (edition 2021), `serde`/`toml` (config), `git` CLI via `std::process::Command` (diff stats), `gix` (repo guard), `anyhow`; `assert_cmd` + `tempfile` for tests.

---

## Hard rules (apply to EVERY task; paste verbatim into any subagent prompt)

1. **Git identity:** every commit uses
   `git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit ...`.
   Never `--author` alone. Never `Co-Authored-By:` lines or any model/agent attribution.
2. **CodeRabbit:** before every commit containing code, run
   `coderabbit --type uncommitted --plain`, resolve findings, then commit.
   After each code commit, run `coderabbit --type committed --plain`.
   Before opening the PR, run `coderabbit --type all --base main --plain`.
   Docs-only commits: note "CodeRabbit does not meaningfully review docs" in the message.
3. **Cargo gates — every commit must pass all four:**
   `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`,
   `cargo fmt --all -- --check`.
4. **TDD:** write the failing test first, run it to confirm it fails for the
   expected reason, implement the minimum, run it green, then commit.
5. **Pushing:** `git push` to `dkod-io/dkod-cli` requires
   `gh auth switch --user haim-ari` first — the active account `haimari`
   lacks push rights and will 403. Re-run the switch before every push and
   before any `gh pr ...` write.
6. **YAGNI:** implement only the design. No LLM, no severity levels, no
   CI-gate mode, no stored verdicts, no new crates.

---

## Background facts (verified — do not re-discover)

- `crates/dkod-core/src/session.rs`: `pub struct Session { pub id: String, pub agent: Agent, pub created_at: i64, pub duration_ms: u64, pub prompt_summary: String, pub messages: Vec<Message>, pub commits: Vec<String>, pub files_touched: Vec<String> }`. `pub enum Message { User { content: String }, Assistant { content: String }, Reasoning { content: String }, Tool { name, input, output } }`. `pub fn agent_label(&Agent) -> &'static str`.
- `crates/dkod-core/src/lib.rs`: `pub mod capture; pub mod config; pub mod redact; pub mod refs; pub mod session; pub mod store; pub use session::*;`.
- `crates/dkod-core/src/config.rs`: `Config { redact: RedactConfig }`, `#[derive(Debug, Clone, Default, Serialize, Deserialize)] #[serde(default)]` on `Config`; `RedactConfig` has `#[serde(default)]` + a manual `impl Default`. Existing tests: `parses_minimal_config`, `defaults_redaction_to_on_with_full_builtin_set`.
- `crates/dkod-cli/src/cmd/mod.rs`: `pub mod blame; pub mod capture; pub mod init; pub mod log; pub mod patchid; pub mod relink; pub mod setup; pub mod show;` plus `pub(crate) fn load_config(cwd: &Path) -> Result<dkod_core::config::Config>`.
- `crates/dkod-cli/src/cmd/log.rs` read pattern: `gix::open(cwd).map_err(|_| anyhow!("not a git repo (run \`git init\` first)"))?;` then `store::list_sessions(cwd)? .into_iter().filter_map(|id| store::read_session(cwd, &id).ok()).collect()`, sort `b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id))`, print `"{} {} {}"` with `agent_label`.
- `crates/dkod-cli/src/main.rs`: clap `Cmd` enum; `Blame { path: String }` is the visible-read-subcommand model; dispatch `Cmd::Blame { path } => cmd::blame::run(&std::env::current_dir()?, &path)`. `maybe_warn_drift()` runs for all commands except `Setup`/`CaptureHook`/`Relink`. **`maybe_warn_drift` is an UNRELATED existing fn about agent-hook drift — do NOT conflate it with this feature, do NOT rename it, and DO let it run for `Drift` (don't add `Drift` to its exclusion `matches!`).**
- Git shell-out precedent: `blame.rs`/`patchid.rs`/`init.rs` use `std::process::Command::new("git").arg("-C").arg(cwd)...`. `dkod_core` is a dev-dependency of `dkod-cli`. Test convention (`tests/e2e_blame.rs`, `e2e_relink.rs`): `assert_cmd::Command::cargo_bin("dkod")`, a `git(repo, args)` helper setting `GIT_AUTHOR_*`/`GIT_COMMITTER_*`, the `Session` literal.
- **Verified: no `glob`/`globset` crate is in the tree** — the matcher is internal and dependency-free.

---

## Phase 1 — `DriftConfig` in `dkod-core`

### Task 1.1: add the `[drift]` config section

**Files:**
- Modify: `crates/dkod-core/src/config.rs`

- [ ] **Step 1: Write the failing tests.** Append to the `#[cfg(test)] mod tests` in `config.rs`:

```rust
#[test]
fn defaults_drift_enabled_with_sensitive_paths_and_thresholds() {
    let c: Config = toml::from_str("").unwrap();
    assert!(c.drift.enabled);
    assert!(c.drift.sensitive_paths.iter().any(|p| p == ".github/workflows/**"));
    assert!(c.drift.sensitive_paths.iter().any(|p| p == "**/auth*"));
    assert_eq!(c.drift.small_ask_max_chars, 140);
    assert_eq!(c.drift.large_change_files, 5);
    assert_eq!(c.drift.large_change_lines, 150);
}

#[test]
fn drift_section_overrides_parse() {
    let toml = r#"
        [drift]
        enabled = false
        large_change_files = 9
        small_ask_keywords = ["typo", "nit"]
    "#;
    let c: Config = toml::from_str(toml).unwrap();
    assert!(!c.drift.enabled);
    assert_eq!(c.drift.large_change_files, 9);
    assert_eq!(c.drift.small_ask_keywords, vec!["typo", "nit"]);
    // Unspecified fields fall back to defaults.
    assert_eq!(c.drift.small_ask_max_chars, 140);
}
```

- [ ] **Step 2: Run to verify failure.**
  Run: `cargo test -p dkod-core drift`
  Expected: FAIL — no `drift` field on `Config`.

- [ ] **Step 3: Implement.** In `config.rs`, add the field to `Config`:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub redact: RedactConfig,
    pub drift: DriftConfig,
}
```

and add the new struct (next to `RedactConfig`):

```rust
/// Configuration for `dkod drift` (intent-vs-output drift detection). All
/// fields default so the feature works with zero config; `enabled = false`
/// makes `dkod drift` report every session clean.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DriftConfig {
    pub enabled: bool,
    /// Globs (forward-slash, `**`/`*`) for paths whose modification is always
    /// worth flagging, regardless of the prompt.
    pub sensitive_paths: Vec<String>,
    /// A prompt at or below this many chars counts as "small-sounding".
    pub small_ask_max_chars: usize,
    /// Substrings that mark a prompt as small-sounding regardless of length.
    pub small_ask_keywords: Vec<String>,
    /// At or above this many touched files counts as a "large change".
    pub large_change_files: usize,
    /// At or above this many changed lines (when diff stats are available)
    /// counts as a "large change".
    pub large_change_lines: usize,
}

impl Default for DriftConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sensitive_paths: vec![
                ".github/workflows/**",
                "**/Dockerfile",
                "**/*.pem",
                "**/*.key",
                "**/.env*",
                "**/secrets*",
                "**/Cargo.lock",
                "**/package-lock.json",
                "**/yarn.lock",
                "**/poetry.lock",
                "**/go.sum",
                "**/migrations/**",
                "**/auth*",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            small_ask_max_chars: 140,
            small_ask_keywords: vec![
                "typo", "rename", "comment", "bump", "tweak", "format", "lint",
                "whitespace", "one-liner", "small fix",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            large_change_files: 5,
            large_change_lines: 150,
        }
    }
}
```

- [ ] **Step 4: Run green.**
  Run: `cargo test -p dkod-core drift` and `cargo test -p dkod-core config`
  Expected: new tests + existing config tests PASS.

- [ ] **Step 5: Gates + CodeRabbit + commit.**
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-core/src/config.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(config): add [drift] config section with defaults"
  coderabbit --type committed --plain
  ```

---

## Phase 2 — dependency-free glob matcher

### Task 2.1: `glob_match` in the new `dkod_core::drift` module

**Files:**
- Create: `crates/dkod-core/src/drift.rs`
- Modify: `crates/dkod-core/src/lib.rs` (add `pub mod drift;`)

- [ ] **Step 1: Register the module.** In `lib.rs`, add `pub mod drift;` (alphabetical: after `pub mod config;`, before `pub mod redact;`).

- [ ] **Step 2: Create `drift.rs` with `glob_match` + failing tests.**

```rust
//! Intent-vs-output drift detection: flag sessions where the agent did
//! materially more / other than the prompt asked. Pure and local — no I/O,
//! no git, no network. The CLI layer (`cmd::drift`) feeds it diff stats and
//! renders the verdict.

/// Match `path` (forward-slash, no leading `/`) against a simple glob
/// `pattern`. Supported syntax (sufficient for the default sensitive-path
/// set, dependency-free): the path and pattern are split on `/`; a `**`
/// segment matches zero or more path segments; within a segment `*` matches
/// any run of non-`/` characters; all other characters match literally.
pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let txt: Vec<&str> = path.split('/').collect();
    seg_match(&pat, &txt)
}

fn seg_match(pat: &[&str], txt: &[&str]) -> bool {
    match pat.split_first() {
        None => txt.is_empty(),
        Some((&"**", rest)) => {
            // `**` consumes zero or more leading segments.
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

/// Match a single path segment against a single pattern segment, where `*`
/// matches any run of characters (the caller has already split on `/`, so a
/// segment never contains `/`).
fn segment_match(pat: &str, txt: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == txt; // no wildcard
    }
    let mut pos = 0usize;
    // First part must be a prefix.
    if !txt[pos..].starts_with(parts[0]) {
        return false;
    }
    pos += parts[0].len();
    // Middle parts must appear in order.
    for part in &parts[1..parts.len() - 1] {
        if part.is_empty() {
            continue;
        }
        match txt[pos..].find(part) {
            Some(found) => pos += found + part.len(),
            None => return false,
        }
    }
    // Last part must be a suffix at/after pos.
    let last = parts[parts.len() - 1];
    txt.len() >= pos + last.len() && txt[pos..].ends_with(last)
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
        assert!(glob_match(".github/workflows/**", ".github/workflows/ci.yml"));
        assert!(glob_match(".github/workflows/**", ".github/workflows/sub/x.yml"));
        assert!(!glob_match(".github/workflows/**", "src/x.yml"));
    }

    #[test]
    fn env_and_auth_prefixes() {
        assert!(glob_match("**/.env*", ".env"));
        assert!(glob_match("**/.env*", "cfg/.env.local"));
        assert!(glob_match("**/auth*", "src/auth.rs"));
        assert!(glob_match("**/auth*", "auth/mod.rs"));
        assert!(!glob_match("**/auth*", "src/oauth.rs")); // segment must START with "auth"
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
}
```

- [ ] **Step 3: Run to verify failure (write tests first).**
  Before writing the impl, the tests don't compile / fail. After pasting the
  impl above (Step 2 includes it), run:
  Run: `cargo test -p dkod-core glob`
  Expected: 5 PASS. (If TDD-strict, first paste only the tests + `pub(crate) fn glob_match(_: &str, _: &str) -> bool { false }`, watch them fail, then fill in the body.)

- [ ] **Step 4: Gates + CodeRabbit + commit.**
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-core/src/lib.rs crates/dkod-core/src/drift.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(drift): dependency-free glob matcher for sensitive paths"
  coderabbit --type committed --plain
  ```

---

## Phase 3 — `drift::analyze` + types

### Task 3.1: the three heuristics

**Files:**
- Modify: `crates/dkod-core/src/drift.rs`

- [ ] **Step 1: Write the failing tests.** Append a second test module to `drift.rs`:

```rust
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
        }
    }

    #[test]
    fn sensitive_path_trips() {
        let s = session("fix the readme", &[".github/workflows/ci.yml"]);
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(!v.is_clean());
        assert!(v.reasons.iter().any(|r| r.rule == "sensitive_path"
            && r.detail.contains(".github/workflows/ci.yml")));
    }

    #[test]
    fn benign_file_is_clean() {
        let s = session("update the parser thoroughly across the module", &["src/lib.rs"]);
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(v.is_clean(), "got: {:?}", v.reasons.iter().map(|r| &r.detail).collect::<Vec<_>>());
    }

    #[test]
    fn small_ask_large_change_by_filecount_flags() {
        let s = session("fix typo", &["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"]); // 5 >= default 5
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(v.reasons.iter().any(|r| r.rule == "magnitude" && r.detail.contains("5 files")));
    }

    #[test]
    fn small_ask_small_change_is_clean() {
        let s = session("fix typo", &["a.rs"]);
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(v.is_clean());
    }

    #[test]
    fn large_change_with_verbose_ask_is_clean_on_magnitude() {
        let long = "please carefully refactor the entire parser subsystem and update \
                    every call site and the associated tests across the whole repository \
                    being thorough and complete in the work";
        let s = session(long, &["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"]);
        let v = analyze(&s, None, &DriftConfig::default());
        // Magnitude rule must NOT fire (ask is not small-sounding).
        assert!(!v.reasons.iter().any(|r| r.rule == "magnitude"));
    }

    #[test]
    fn magnitude_uses_lines_when_diffstats_present() {
        let s = session("tiny tweak", &["a.rs"]); // only 1 file
        let stats = DiffStats { files: 1, insertions: 200, deletions: 0 }; // 200 >= 150
        let v = analyze(&s, Some(&stats), &DriftConfig::default());
        assert!(v.reasons.iter().any(|r| r.rule == "magnitude" && r.detail.contains("lines")));
    }

    #[test]
    fn unmentioned_file_flags_when_prompt_named_paths() {
        let s = session("please fix the bug in auth.rs", &["auth.rs", "billing.rs"]);
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(v.reasons.iter().any(|r| r.rule == "unmentioned" && r.detail.contains("billing.rs")));
    }

    #[test]
    fn unmentioned_does_not_fire_on_vague_prompt() {
        // No path-shaped or basename token in the prompt → rule must not fire,
        // even with several files. Use a verbose prompt so magnitude stays quiet too.
        let s = session(
            "improve the overall reliability of the system in a general sense across everything",
            &["a.rs", "b.rs", "c.rs"],
        );
        let v = analyze(&s, None, &DriftConfig::default());
        assert!(!v.reasons.iter().any(|r| r.rule == "unmentioned"));
    }

    #[test]
    fn disabled_config_is_always_clean() {
        let mut cfg = DriftConfig::default();
        cfg.enabled = false;
        let s = session("fix typo", &[".github/workflows/ci.yml", "a.rs", "b.rs", "c.rs", "d.rs"]);
        assert!(analyze(&s, None, &cfg).is_clean());
    }
}
```

- [ ] **Step 2: Run to verify failure.**
  Run: `cargo test -p dkod-core analyze`
  Expected: FAIL — `analyze`/`DiffStats`/`DriftVerdict` not found.

- [ ] **Step 3: Implement.** Add to `drift.rs` (above the test modules):

```rust
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

/// Cap on how many entries we list inside a single reason, to keep output sane.
const LIST_CAP: usize = 5;
/// Cap on how many sensitive-path reasons we emit per session.
const SENSITIVE_CAP: usize = 10;

/// The lowercase final path component.
fn basename_lower(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_lowercase()
}

/// Analyze a session for intent-vs-output drift. Pure: no I/O. `diff_stats`
/// is `None` when the session's commits are unavailable (the line-magnitude
/// signal is then skipped; file-count still applies).
pub fn analyze(
    session: &crate::Session,
    diff_stats: Option<&DiffStats>,
    cfg: &crate::config::DriftConfig,
) -> DriftVerdict {
    let mut verdict = DriftVerdict::default();
    if !cfg.enabled {
        return verdict;
    }

    // Intent: all user messages joined; fall back to prompt_summary.
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
    // "Mentioned" = path-shaped tokens (contain '/' AND a dotted extension)
    // plus tokens whose lowercased value equals a touched file's basename.
    let touched_basenames: std::collections::HashSet<String> =
        session.files_touched.iter().map(|f| basename_lower(f)).collect();
    let mut mentioned: Vec<String> = Vec::new();
    for tok in intent.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '`' | '"' | '\'')) {
        let t = tok.trim_matches(|c: char| matches!(c, '.' | ':'));
        if t.is_empty() {
            continue;
        }
        let path_shaped = t.contains('/') && t.rsplit('/').next().is_some_and(|b| b.contains('.'));
        let tl = t.to_lowercase();
        let is_basename = touched_basenames.contains(&basename_lower(t));
        if path_shaped || is_basename {
            // Normalize what we remember to basenames for comparison.
            if !mentioned.iter().any(|m| m.eq_ignore_ascii_case(t)) {
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
            let shown: Vec<String> = unmentioned.iter().take(LIST_CAP).map(|s| s.to_string()).collect();
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
```

> **Executor note:** `is_some_and` is stable in the pinned toolchain; if clippy
> or the compiler objects, use `.map_or(false, |b| b.contains('.'))`. Keep the
> behavior identical. Do not add a regex/glob crate.

- [ ] **Step 4: Run green.**
  Run: `cargo test -p dkod-core analyze` then `cargo test -p dkod-core drift`
  Expected: all analyze + glob tests PASS.

- [ ] **Step 5: Gates + CodeRabbit + commit.**
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-core/src/drift.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(drift): analyze() with the three drift heuristics"
  coderabbit --type committed --plain
  ```

---

## Phase 4 — `cmd::drift` + diff stats + CLI wiring

### Task 4.1: the command, the numstat helper, and `main.rs`

**Files:**
- Create: `crates/dkod-cli/src/cmd/drift.rs`
- Modify: `crates/dkod-cli/src/cmd/mod.rs` (add `pub mod drift;`)
- Modify: `crates/dkod-cli/src/main.rs` (add the `Drift` subcommand + dispatch)

- [ ] **Step 1: Register the module.** In `crates/dkod-cli/src/cmd/mod.rs`, add `pub mod drift;` (alphabetical: after `pub mod capture;`, before `pub mod init;`).

- [ ] **Step 2: Create `drift.rs` with `parse_numstat` + failing unit test.**

```rust
//! `dkod drift` — report sessions where the agent did materially more / other
//! than the prompt asked. Pure analysis lives in `dkod_core::drift`; this
//! layer resolves sessions, derives diff stats from git, and renders.

use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Command;

/// Parse `git show --numstat --format=` output (lines of `<ins>\t<del>\t<path>`,
/// where `<ins>`/`<del>` are `-` for binary files) into (files, insertions,
/// deletions). Unknown/blank lines are skipped.
fn parse_numstat(out: &str) -> (usize, usize, usize) {
    let mut files = 0usize;
    let mut ins = 0usize;
    let mut del = 0usize;
    for line in out.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let (a, b, path) = match (cols.next(), cols.next(), cols.next()) {
            (Some(a), Some(b), Some(p)) => (a, b, p),
            _ => continue,
        };
        if path.is_empty() {
            continue;
        }
        files += 1;
        ins += a.parse::<usize>().unwrap_or(0); // "-" (binary) → 0
        del += b.parse::<usize>().unwrap_or(0);
    }
    (files, ins, del)
}

/// Sum per-commit numstat over a session's commits. `None` only when there are
/// no commits or none of them resolve (best-effort otherwise).
fn diff_stats(cwd: &Path, commits: &[String]) -> Option<dkod_core::drift::DiffStats> {
    if commits.is_empty() {
        return None;
    }
    let mut files = 0usize;
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    let mut any = false;
    for sha in commits {
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["show", "--numstat", "--format=", sha])
            .output();
        let out = match out {
            Ok(o) if o.status.success() => o,
            _ => continue,
        };
        any = true;
        let (f, i, d) = parse_numstat(&String::from_utf8_lossy(&out.stdout));
        files += f;
        insertions += i;
        deletions += d;
    }
    if any {
        Some(dkod_core::drift::DiffStats { files, insertions, deletions })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::parse_numstat;

    #[test]
    fn parses_numstat_lines() {
        let out = "3\t1\tsrc/a.rs\n0\t9\tsrc/b.rs\n-\t-\tlogo.png\n\n";
        let (files, ins, del) = parse_numstat(out);
        assert_eq!(files, 3);
        assert_eq!(ins, 3); // 3 + 0 + 0(binary)
        assert_eq!(del, 10); // 1 + 9 + 0
    }

    #[test]
    fn ignores_malformed_lines() {
        let out = "garbage line with no tabs\n2\t2\tok.rs\n";
        let (files, ins, del) = parse_numstat(out);
        assert_eq!((files, ins, del), (1, 2, 2));
    }
}
```

- [ ] **Step 3: Run to verify the parser test fails then passes.**
  Run: `cargo test -p dkod-cli parse_numstat`
  Expected: compiles and the 2 tests PASS (the parser body is included above).
  (If TDD-strict: paste tests + a stub `parse_numstat` returning `(0,0,0)`, see them fail, then fill in.)

- [ ] **Step 4: Add `run` + rendering** to `drift.rs` (below `diff_stats`):

```rust
/// Render one session's verdict in single-session detail mode.
fn print_detail(s: &dkod_core::Session, verdict: &dkod_core::drift::DriftVerdict, stats: Option<&dkod_core::drift::DiffStats>) {
    println!("{}  {}  {}", s.id, dkod_core::agent_label(&s.agent), s.prompt_summary);
    if let Some(d) = stats {
        println!("    files: {}  +{} -{}", d.files, d.insertions, d.deletions);
    }
    if verdict.is_clean() {
        println!("    clean");
    } else {
        for r in &verdict.reasons {
            println!("    - {}", r.detail);
        }
    }
}

/// Render one flagged session in list mode (header + reasons).
fn print_listing(s: &dkod_core::Session, verdict: &dkod_core::drift::DriftVerdict) {
    println!("{}  {}  {}", s.id, dkod_core::agent_label(&s.agent), s.prompt_summary);
    for r in &verdict.reasons {
        println!("    - {}", r.detail);
    }
}

/// `dkod drift [session-id] [--all]`. With a session id, prints that session's
/// detail; otherwise lists sessions (flagged only, unless `all`). Always Ok.
pub fn run(cwd: &Path, session_id: Option<&str>, all: bool) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;
    let cfg = super::load_config(cwd)?;

    if let Some(id) = session_id {
        let s = dkod_core::store::read_session(cwd, id)
            .map_err(|e| anyhow!("read session {id}: {e:#}"))?;
        let stats = diff_stats(cwd, &s.commits);
        let verdict = dkod_core::drift::analyze(&s, stats.as_ref(), &cfg.drift);
        print_detail(&s, &verdict, stats.as_ref());
        return Ok(());
    }

    let mut sessions: Vec<dkod_core::Session> = dkod_core::store::list_sessions(cwd)
        .map_err(|e| anyhow!("list sessions: {e:#}"))?
        .into_iter()
        .filter_map(|id| dkod_core::store::read_session(cwd, &id).ok())
        .collect();
    sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));

    for s in &sessions {
        let stats = diff_stats(cwd, &s.commits);
        let verdict = dkod_core::drift::analyze(s, stats.as_ref(), &cfg.drift);
        if verdict.is_clean() && !all {
            continue;
        }
        if all && verdict.is_clean() {
            println!("{}  {}  {}  [clean]", s.id, dkod_core::agent_label(&s.agent), s.prompt_summary);
        } else {
            print_listing(s, &verdict);
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Wire the subcommand in `main.rs`.** Add to the `Cmd` enum (near `Blame`):

```rust
    /// Report sessions where the agent did materially more or other than the
    /// prompt asked (intent-vs-output drift).
    Drift {
        /// Session id to analyze; omit to scan all sessions.
        session_id: Option<String>,
        /// Include clean sessions in the listing.
        #[arg(long)]
        all: bool,
    },
```

and the dispatch arm (next to `Cmd::Blame`):

```rust
        Cmd::Drift { session_id, all } => {
            cmd::drift::run(&std::env::current_dir()?, session_id.as_deref(), all)
        }
```

**Do NOT** add `Drift` to the `maybe_warn_drift()` exclusion `matches!` — `Drift` is a normal read command and should run the (unrelated) agent-hook drift warning like `log`/`show`/`blame`.

- [ ] **Step 6: Build + smoke + gates.**
  Run: `cargo build` then `./target/debug/dkod drift --help` (confirm it appears in `--help`, unlike the hidden commands).
  Run: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check`
  Expected: all green; `parse_numstat` tests pass; `drift` shows in help.

- [ ] **Step 7: CodeRabbit + commit.**
  ```bash
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-cli/src/cmd/drift.rs crates/dkod-cli/src/cmd/mod.rs crates/dkod-cli/src/main.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(drift): dkod drift command (numstat + analyze + render)"
  coderabbit --type committed --plain
  ```

---

## Phase 5 — end-to-end

### Task 5.1: `e2e_drift.rs`

**Files:**
- Test: `crates/dkod-cli/tests/e2e_drift.rs` (new)

- [ ] **Step 1: Write the test.**

```rust
//! End-to-end: `dkod drift` flags a session that touched a sensitive path from
//! a small-sounding prompt, hides a clean session by default, and shows it
//! under --all.

use assert_cmd::Command as AssertCommand;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@e.com")
        .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@e.com")
        .output()
        .expect("git");
    assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
}

fn head(repo: &Path) -> String {
    String::from_utf8(Command::new("git").args(["rev-parse","HEAD"]).current_dir(repo).output().unwrap().stdout)
        .unwrap().trim().to_string()
}

fn seed(repo: &Path, prompt: &str, files: &[&str], commits: &[String]) -> String {
    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: prompt.into(),
        messages: vec![dkod_core::Message::user(prompt)],
        commits: commits.to_vec(),
        files_touched: files.iter().map(|s| s.to_string()).collect(),
    };
    dkod_core::store::write_session(repo, &s).unwrap();
    s.id
}

#[test]
fn drift_flags_sensitive_path_and_hides_clean() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    // A real commit touching a sensitive path (gives the drifting session a commit too).
    std::fs::create_dir_all(repo.path().join(".github/workflows")).unwrap();
    std::fs::write(repo.path().join(".github/workflows/ci.yml"), "on: push\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "ci"]);
    let ci_sha = head(repo.path());

    // Drifting: "fix typo" prompt, touched the workflow file.
    let drift_id = seed(repo.path(), "fix the typo in the readme", &[".github/workflows/ci.yml"], &[ci_sha]);
    // Clean: verbose prompt, benign files (not sensitive, ask not small).
    let clean_id = seed(
        repo.path(),
        "refactor the parser module and update its tests across the codebase",
        &["src/parser.rs", "src/parser_tests.rs"],
        &[],
    );

    // Default: drifting shown with the sensitive-path reason; clean hidden.
    let out = AssertCommand::cargo_bin("dkod").unwrap()
        .current_dir(repo.path()).arg("drift").assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains(&drift_id), "drifting session should be listed:\n{stdout}");
    assert!(stdout.contains("sensitive path") && stdout.contains("ci.yml"),
        "expected the sensitive-path reason:\n{stdout}");
    assert!(!stdout.contains(&clean_id), "clean session must be hidden by default:\n{stdout}");

    // Single clean session → "clean".
    let out = AssertCommand::cargo_bin("dkod").unwrap()
        .current_dir(repo.path()).args(["drift", &clean_id]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("clean"), "single clean session should report clean:\n{stdout}");

    // --all shows both.
    let out = AssertCommand::cargo_bin("dkod").unwrap()
        .current_dir(repo.path()).args(["drift", "--all"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains(&drift_id) && stdout.contains(&clean_id),
        "--all should show both sessions:\n{stdout}");
}
```

- [ ] **Step 2: Run.**
  Run: `cargo test -p dkod-cli --test e2e_drift`
  Expected: PASS. If the clean session trips an unexpected rule, read the failure
  and adjust the *fixture* (prompt/files) so it is genuinely clean — do NOT
  weaken the drifting-session assertions. (The clean prompt must be long enough
  to not be "small-sounding" and must name no path-shaped tokens that would make
  the magnitude/unmentioned rules misfire; `src/parser.rs` etc. are not sensitive
  and only 2 files < 5.)

- [ ] **Step 3: Gates + CodeRabbit + commit.**
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-cli/tests/e2e_drift.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "test(drift): e2e dkod drift flags sensitive path, hides clean"
  coderabbit --type committed --plain
  ```

---

## Phase 6 — docs + PR

### Task 6.1: Update the positioning roadmap

**Files:**
- Modify: `docs/plans/2026-05-27-macroscope-competitive-positioning.md`

- [ ] **Step 1: Read roadmap item #3** ("Intent-vs-output drift") in that doc.
- [ ] **Step 2: Edit** to mark v1 shipped: a local, rule-based `dkod drift`
  command (three heuristics — sensitive-path tripwire, small-ask/large-change
  magnitude, unmentioned-file drift) producing structured reasons, computed
  on-read, zero-network. Note the deferred **opt-in LLM semantic layer** (user's
  own key, off by default) for subtle same-magnitude drift. Reference
  `docs/plans/2026-06-09-intent-output-drift-design.md`.
- [ ] **Step 3: Commit (docs-only).**
  ```bash
  git add docs/plans/2026-05-27-macroscope-competitive-positioning.md
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "docs: intent-vs-output drift v1 shipped (dkod drift)"
  ```
  (CodeRabbit does not meaningfully review docs — say so in your report; skip the CodeRabbit passes for this docs-only commit.)

### Task 6.2: Open the PR and drive to merge

- [ ] **Step 1: Final verification.**
  `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` → green.
- [ ] **Step 2: Full-branch CodeRabbit.** `coderabbit --type all --base main --plain`; resolve every actionable finding (new commits, re-run until clean).
- [ ] **Step 3: Push.**
  ```bash
  gh auth switch --user haim-ari
  git push -u origin feat/intent-output-drift
  ```
- [ ] **Step 4: Open the PR.** Title (≤70 chars): `feat: dkod drift (intent-vs-output drift)`. Body: the three local heuristics, structured reasons, on-read/zero-network, why semantic LLM is deferred (privacy + deps), and the documented limitations. Reference the design doc.
- [ ] **Step 5: Drive to green and merge.** Trigger `@coderabbitai review`, wait for CI + CodeRabbit, fix findings (re-`gh auth switch` before each push), repeat until CI green + CodeRabbit approved, then squash-merge `--delete-branch`. After merge, sync local `main` (`git checkout main && git fetch origin && git reset --hard origin/main`).

---

## Self-review checklist (controller, before dispatch)

- Spec coverage: `[drift]` config (P1) ✓; glob matcher (P2) ✓; `analyze` + 3 heuristics + types (P3) ✓; `cmd::drift` + numstat + render + wiring (P4) ✓; e2e (P5) ✓; docs+PR (P6) ✓. On-read ✓ (no capture change). Structured reasons ✓. Exit 0 ✓. enabled=false short-circuit ✓. Intent = user msgs + prompt_summary fallback ✓.
- Type/name consistency: `DriftConfig`/`DiffStats`/`DriftReason`/`DriftVerdict`/`analyze`/`glob_match`/`parse_numstat`/`diff_stats`/`cmd::drift::run(cwd, Option<&str>, bool)`/`Cmd::Drift { session_id: Option<String>, all: bool }` — consistent across phases.
- Placeholder scan: every code step shows complete code.
- Risk: `maybe_warn_drift` name-collision flagged (do not conflate / rename); `is_some_and` fallback noted.

## Known limitations (carry in the PR description)

1. Heuristic, not semantic — misses subtle same-magnitude drift (deferred opt-in LLM layer).
2. Thresholds tuned to under-flag (precision over recall).
3. Line-magnitude needs the commits present; file-level checks don't.
4. Mentioned-path extraction is shallow (path-shaped tokens / basenames); only weakens the conservative unmentioned-file rule.
