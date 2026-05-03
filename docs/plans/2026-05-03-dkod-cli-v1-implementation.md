# dkod-cli V1 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Ship `dkod-core` (shared library crate) and `dkod-cli` (binary) — a Rust CLI that captures Claude Code and Codex sessions into custom git refs (`refs/dkod/sessions/<id>`), with redaction-on-by-default and the four commands `init` / `capture` / `log` / `show`.

**Architecture:** Single Cargo workspace with two crates. `dkod-core` owns the session data model, gitoxide-based ref read/write, redaction engine, and config parsing — re-used later by `dkod-app` (Tauri) and `dkod-indexer` (server). `dkod-cli` is a thin clap-based binary that wires `dkod-core` to user commands and adapter wrappers.

**Tech Stack:**
- Rust stable, Cargo workspace
- `gix` (gitoxide) for all git operations
- `clap` 4.x for CLI parsing
- `serde` + `serde_json` for session blobs
- `toml` for config parsing
- `regex` + `aho-corasick` for redaction
- `uuid` (v7) for session ids
- `tokio` for async, only where needed (Claude Code socket)
- Test deps: `tempfile`, `assert_cmd`, `predicates`, `insta` (snapshot)

**Reference design doc:** `docs/plans/2026-05-03-dkod-pivot-design.md`.

**Where this work happens:** brand-new repo at `/Users/haimari/vsCode/haim-ari/github/dkod-cli`. This plan starts by creating that repo. Move this file into the new repo's `docs/plans/` after Task 1.

---

## Phase 1 — Bootstrap

### Task 1: Create the dkod-cli repo

**Files:**
- Create: `/Users/haimari/vsCode/haim-ari/github/dkod-cli/` (new directory)
- Create: `Cargo.toml` (workspace root)
- Create: `crates/dkod-core/Cargo.toml`
- Create: `crates/dkod-core/src/lib.rs`
- Create: `crates/dkod-cli/Cargo.toml`
- Create: `crates/dkod-cli/src/main.rs`
- Create: `LICENSE` (MIT)
- Create: `README.md` (one-paragraph, links to design doc)
- Create: `.gitignore`
- Create: `rust-toolchain.toml` (pin stable)
- Create: `.github/workflows/ci.yml` (cargo fmt + clippy + test)

**Step 1: Init the repo**

```bash
mkdir -p /Users/haimari/vsCode/haim-ari/github/dkod-cli/crates/{dkod-core,dkod-cli}/src
cd /Users/haimari/vsCode/haim-ari/github/dkod-cli
git init
```

**Step 2: Write workspace `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["crates/dkod-core", "crates/dkod-cli"]

[workspace.package]
edition = "2021"
license = "MIT"
authors = ["dkod"]
repository = "https://github.com/dkod-io/dkod-cli"
rust-version = "1.75"

[workspace.dependencies]
gix = { version = "0.66", default-features = false, features = ["max-performance-safe"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
regex = "1"
uuid = { version = "1", features = ["v7", "serde"] }
clap = { version = "4", features = ["derive"] }
anyhow = "1"
thiserror = "1"
tempfile = "3"
assert_cmd = "2"
predicates = "3"
insta = { version = "1", features = ["yaml"] }
```

**Step 3: Write `crates/dkod-core/Cargo.toml` and stub `lib.rs`**

```toml
[package]
name = "dkod-core"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
gix.workspace = true
serde.workspace = true
serde_json.workspace = true
toml.workspace = true
regex.workspace = true
uuid.workspace = true
anyhow.workspace = true
thiserror.workspace = true

[dev-dependencies]
tempfile.workspace = true
insta.workspace = true
```

```rust
// crates/dkod-core/src/lib.rs
//! Shared crate for dkod CLI, app, and indexer.
```

**Step 4: Write `crates/dkod-cli/Cargo.toml` and stub `main.rs`**

```toml
[package]
name = "dkod-cli"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[[bin]]
name = "dkod"
path = "src/main.rs"

[dependencies]
dkod-core = { path = "../dkod-core" }
clap.workspace = true
anyhow.workspace = true
serde.workspace = true
serde_json.workspace = true

[dev-dependencies]
tempfile.workspace = true
assert_cmd.workspace = true
predicates.workspace = true
```

```rust
// crates/dkod-cli/src/main.rs
fn main() {
    println!("dkod");
}
```

**Step 5: Verify the workspace compiles**

Run: `cargo build`
Expected: clean build, no warnings.

**Step 6: Add LICENSE, README, .gitignore, rust-toolchain.toml, CI**

LICENSE = standard MIT text.
README = one paragraph: "dkod captures every AI agent session into your git repo as a custom ref. See `docs/plans/2026-05-03-dkod-pivot-design.md`."
.gitignore = `target/` + standard Rust ignores.
rust-toolchain.toml:
```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```
.github/workflows/ci.yml: cargo fmt --check, clippy -- -D warnings, test.

**Step 7: Move this plan + the design doc into the new repo**

```bash
mkdir -p /Users/haimari/vsCode/haim-ari/github/dkod-cli/docs/plans
cp /Users/haimari/vsCode/haim-ari/github/dkod-app/docs/plans/2026-05-03-dkod-pivot-design.md /Users/haimari/vsCode/haim-ari/github/dkod-cli/docs/plans/
mv /Users/haimari/vsCode/haim-ari/github/dkod-app/docs/plans/2026-05-03-dkod-cli-v1-implementation.md /Users/haimari/vsCode/haim-ari/github/dkod-cli/docs/plans/
```

**Step 8: Commit**

```bash
git -C /Users/haimari/vsCode/haim-ari/github/dkod-cli add -A
git -C /Users/haimari/vsCode/haim-ari/github/dkod-cli -c user.name=haim-ari -c user.email=haimari1@gmail.com commit -m "chore: bootstrap dkod-cli workspace"
```

---

## Phase 2 — Session data model

### Task 2: Define the `Session` and `Message` types

**Files:**
- Create: `crates/dkod-core/src/session.rs`
- Modify: `crates/dkod-core/src/lib.rs`
- Test: `crates/dkod-core/src/session.rs` (inline `#[cfg(test)] mod tests`)

**Step 1: Write the failing test**

```rust
// crates/dkod-core/src/session.rs
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
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p dkod-core round_trips_through_json`
Expected: FAIL — `Session`, `Agent`, `Message` not defined.

**Step 3: Write the minimal implementation**

```rust
// crates/dkod-core/src/session.rs
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    ClaudeCode,
    Codex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    User { content: String },
    Assistant { content: String },
    Tool { name: String, input: serde_json::Value, output: String },
}

impl Message {
    pub fn user(s: impl Into<String>) -> Self { Self::User { content: s.into() } }
    pub fn assistant(s: impl Into<String>) -> Self { Self::Assistant { content: s.into() } }
}
```

```rust
// crates/dkod-core/src/lib.rs
pub mod session;
pub use session::*;
```

**Step 4: Run to verify it passes**

Run: `cargo test -p dkod-core round_trips_through_json`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/dkod-core/src/{lib,session}.rs
git commit -m "feat(core): Session and Message types"
```

---

### Task 3: Session id generation (UUID v7)

**Files:**
- Modify: `crates/dkod-core/src/session.rs`

**Step 1: Write the failing test**

```rust
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
```

**Step 2: Run, verify it fails (no `Session::new_id`)**

**Step 3: Implement**

```rust
impl Session {
    pub fn new_id() -> String {
        uuid::Uuid::now_v7().to_string()
    }
}
```

**Step 4: Run, verify it passes**

**Step 5: Commit**

```bash
git commit -am "feat(core): Session::new_id using UUID v7"
```

---

## Phase 3 — Ref layout

### Task 4: Compute and parse ref paths

**Files:**
- Create: `crates/dkod-core/src/refs.rs`
- Modify: `crates/dkod-core/src/lib.rs`

**Step 1: Write the failing tests**

```rust
// crates/dkod-core/src/refs.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ref_path_is_correct() {
        let id = "0192f8e2-7b3a-7000-8a3e-000000000001";
        assert_eq!(session_ref(id), "refs/dkod/sessions/0192f8e2-7b3a-7000-8a3e-000000000001");
    }

    #[test]
    fn commit_ref_path_is_correct() {
        let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        assert_eq!(commit_ref(sha), "refs/dkod/commits/deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    }

    #[test]
    fn parses_session_ref() {
        let r = "refs/dkod/sessions/abc-123";
        assert_eq!(parse_session_ref(r), Some("abc-123".to_string()));
    }

    #[test]
    fn rejects_non_session_ref() {
        assert_eq!(parse_session_ref("refs/heads/main"), None);
    }
}
```

**Step 2: Run, verify they fail**

**Step 3: Implement**

```rust
pub fn session_ref(id: &str) -> String {
    format!("refs/dkod/sessions/{id}")
}

pub fn commit_ref(sha: &str) -> String {
    format!("refs/dkod/commits/{sha}")
}

pub fn parse_session_ref(r: &str) -> Option<String> {
    r.strip_prefix("refs/dkod/sessions/").map(|s| s.to_string())
}
```

```rust
// in lib.rs
pub mod refs;
```

**Step 4: Run, verify they pass**

**Step 5: Commit**

```bash
git commit -am "feat(core): ref layout helpers"
```

---

## Phase 4 — Storage (gitoxide)

### Task 5: Write a session blob into a repo

**Files:**
- Create: `crates/dkod-core/src/store.rs`
- Modify: `crates/dkod-core/src/lib.rs`

**Step 1: Write the failing test**

```rust
// crates/dkod-core/src/store.rs
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture_session() -> Session {
        Session {
            id: Session::new_id(),
            agent: Agent::Codex,
            created_at: 1735689600,
            duration_ms: 100,
            prompt_summary: "fix bug".into(),
            messages: vec![Message::user("fix bug")],
            commits: vec![],
            files_touched: vec![],
        }
    }

    #[test]
    fn write_then_read_session() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();

        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();
        let back = read_session(tmp.path(), &s.id).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn write_creates_session_ref() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        let s = fixture_session();
        write_session(tmp.path(), &s).unwrap();

        let repo = gix::open(tmp.path()).unwrap();
        let r = repo.find_reference(&refs::session_ref(&s.id)).unwrap();
        assert!(r.id().to_string().len() == 40); // valid sha
    }
}
```

**Step 2: Run, verify it fails**

**Step 3: Implement**

```rust
// crates/dkod-core/src/store.rs
use crate::{refs, Session};
use anyhow::{Context, Result};
use std::path::Path;

pub fn write_session(repo_path: &Path, session: &Session) -> Result<()> {
    let repo = gix::open(repo_path).context("open repo")?;
    let bytes = serde_json::to_vec(session)?;
    let blob_id = repo.write_blob(&bytes)?.detach();
    let ref_name = refs::session_ref(&session.id);

    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange { mode: RefLog::AndReference, force_create_reflog: false, message: "dkod: write session".into() },
            expected: PreviousValue::Any,
            new: gix::refs::Target::Object(blob_id),
        },
        name: ref_name.try_into()?,
        deref: false,
    })?;
    Ok(())
}

pub fn read_session(repo_path: &Path, id: &str) -> Result<Session> {
    let repo = gix::open(repo_path).context("open repo")?;
    let r = repo.find_reference(&refs::session_ref(id))?;
    let object = repo.find_object(r.id())?.detach();
    let s: Session = serde_json::from_slice(&object.data)?;
    Ok(s)
}
```

```rust
// in lib.rs
pub mod store;
```

> **Note for the implementer:** the exact gitoxide API surface for writing a ref pointing at a blob may need adjustment — `gix::refs::Target::Object` may need a different variant. If the API doesn't accept it cleanly, fall back to writing the ref via `git update-ref` shelled out, but only as a last resort. Read gitoxide's `examples/` first.

**Step 4: Run, verify both tests pass**

Run: `cargo test -p dkod-core store::`

**Step 5: Commit**

```bash
git commit -am "feat(core): write/read session blob via gitoxide"
```

---

### Task 6: Link a session to a commit

**Files:**
- Modify: `crates/dkod-core/src/store.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn link_session_to_commit_writes_ref() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let s = fixture_session();
    write_session(tmp.path(), &s).unwrap();

    // Make an empty commit so we have a real sha
    let repo = gix::open(tmp.path()).unwrap();
    let tree = repo.empty_tree();
    let sig = gix::actor::Signature {
        name: "test".into(), email: "t@t".into(),
        time: gix::date::Time::now_utc(),
    };
    let commit_id = repo.commit("HEAD", "init", tree.id(), Vec::<gix::ObjectId>::new()).unwrap();

    link_session_to_commit(tmp.path(), &s.id, &commit_id.to_string()).unwrap();

    let r = repo.find_reference(&refs::commit_ref(&commit_id.to_string())).unwrap();
    // The commit-link ref points at the session blob
    assert_eq!(r.id().to_string(), repo.find_reference(&refs::session_ref(&s.id)).unwrap().id().to_string());
}
```

**Step 2: Run, verify it fails**

**Step 3: Implement** — analogous to `write_session` but writes `refs/dkod/commits/<sha>` pointing at the same blob.

**Step 4: Run, verify pass**

**Step 5: Commit**

```bash
git commit -am "feat(core): link session to commit"
```

---

### Task 7: List all sessions in a repo

**Files:**
- Modify: `crates/dkod-core/src/store.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn list_sessions_returns_all_written() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let mut ids: Vec<String> = (0..3).map(|_| {
        let s = fixture_session();
        let id = s.id.clone();
        write_session(tmp.path(), &s).unwrap();
        id
    }).collect();
    ids.sort();

    let mut listed = list_sessions(tmp.path()).unwrap();
    listed.sort();
    assert_eq!(ids, listed);
}
```

**Step 2-4: Implement and run.**

```rust
pub fn list_sessions(repo_path: &Path) -> Result<Vec<String>> {
    let repo = gix::open(repo_path)?;
    let mut ids = Vec::new();
    for r in repo.references()?.prefixed("refs/dkod/sessions/")? {
        let r = r?;
        if let Some(id) = refs::parse_session_ref(r.name().as_bstr().to_string().as_str()) {
            ids.push(id);
        }
    }
    Ok(ids)
}
```

**Step 5: Commit**

```bash
git commit -am "feat(core): list_sessions"
```

---

## Phase 5 — Config

### Task 8: Parse `.dkod/config.toml`

**Files:**
- Create: `crates/dkod-core/src/config.rs`
- Modify: `crates/dkod-core/src/lib.rs`

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let toml = r#"
            [redact]
            enabled = true
            patterns = ["builtin:aws"]
        "#;
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.redact.enabled, true);
        assert_eq!(c.redact.patterns, vec!["builtin:aws"]);
    }

    #[test]
    fn defaults_redaction_to_on_with_full_builtin_set() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.redact.enabled);
        assert!(c.redact.patterns.contains(&"builtin:aws".to_string()));
        assert!(c.redact.patterns.contains(&"builtin:github_token".to_string()));
        assert!(c.redact.patterns.contains(&"builtin:openai_key".to_string()));
        assert!(c.redact.patterns.contains(&"builtin:stripe".to_string()));
        assert!(c.redact.patterns.contains(&"builtin:env_assignment".to_string()));
    }
}
```

**Step 2: Run, verify fail**

**Step 3: Implement**

```rust
// crates/dkod-core/src/config.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub redact: RedactConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RedactConfig {
    pub enabled: bool,
    pub patterns: Vec<String>,
    pub custom: Vec<String>,
}

impl Default for RedactConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            patterns: vec![
                "builtin:aws".into(),
                "builtin:github_token".into(),
                "builtin:openai_key".into(),
                "builtin:stripe".into(),
                "builtin:env_assignment".into(),
            ],
            custom: vec![],
        }
    }
}
```

```rust
// in lib.rs
pub mod config;
```

**Step 4: Run, verify pass.**

**Step 5: Commit**

```bash
git commit -am "feat(core): config parsing with default-on redaction"
```

---

## Phase 6 — Redaction

### Task 9: Builtin redaction patterns — one test per pattern

**Files:**
- Create: `crates/dkod-core/src/redact.rs`
- Modify: `crates/dkod-core/src/lib.rs`

**Step 1: Write five failing tests**

```rust
// crates/dkod-core/src/redact.rs
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
        assert!(r("github_pat_11ABCDEFG_1234567890abcdef1234567890ABCDEF1234567890abcdef1234567890ABCDEF").contains("[REDACTED:github_token]"));
    }

    #[test]
    fn redacts_openai_key() {
        assert!(r("sk-proj-abcdefABCDEF0123456789_-abcdefABCDEF0123456789_-abcdefAB").contains("[REDACTED:openai_key]"));
    }

    #[test]
    fn redacts_stripe_key() {
        assert!(r("sk_live_abcdefABCDEF0123456789").contains("[REDACTED:stripe]"));
    }

    #[test]
    fn redacts_env_assignment() {
        assert_eq!(r("API_KEY=supersecret"), "API_KEY=[REDACTED:env_assignment]");
        assert!(r("export DB_PASS=hunter2").contains("[REDACTED:env_assignment]"));
    }
}
```

**Step 2: Run, verify all five fail**

**Step 3: Implement** (use `regex::RegexSet` for cheap multi-pattern matching, then a per-match replacement loop)

```rust
use crate::config::RedactConfig;
use regex::Regex;

pub fn redact(input: &str, cfg: &RedactConfig) -> String {
    if !cfg.enabled { return input.to_string(); }
    let mut out = input.to_string();
    for p in &cfg.patterns {
        out = match p.as_str() {
            "builtin:aws" => redact_with(&out, r"AKIA[0-9A-Z]{16}", "[REDACTED:aws]"),
            "builtin:github_token" => redact_with(&out, r"gh[pous]_[A-Za-z0-9_]{36,255}", "[REDACTED:github_token]"),
            "builtin:openai_key" => redact_with(&out, r"sk-(?:proj-)?[A-Za-z0-9_\-]{40,}", "[REDACTED:openai_key]"),
            "builtin:stripe" => redact_with(&out, r"sk_(?:live|test)_[A-Za-z0-9]{24,}", "[REDACTED:stripe]"),
            "builtin:env_assignment" => redact_with(&out, r"(?P<lhs>\b[A-Z][A-Z0-9_]*=)(?P<rhs>\S+)", "${lhs}[REDACTED:env_assignment]"),
            _ => out,
        };
    }
    for custom in &cfg.custom {
        out = redact_with(&out, custom, "[REDACTED:custom]");
    }
    out
}

fn redact_with(input: &str, pattern: &str, replacement: &str) -> String {
    Regex::new(pattern).unwrap().replace_all(input, replacement).to_string()
}
```

> **Note:** the env-assignment regex is intentionally narrow (uppercase LHS, no spaces in value). It will produce false positives, but the design says "annoying false positive beats irreversible leak."

**Step 4: Run, verify all five pass**

**Step 5: Commit**

```bash
git commit -am "feat(core): redaction engine with five builtin patterns"
```

---

### Task 10: Apply redaction to a `Session` struct

**Files:**
- Modify: `crates/dkod-core/src/redact.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn redacts_session_messages() {
    use crate::{Session, Agent, Message};
    let mut s = Session {
        id: "x".into(), agent: Agent::Codex, created_at: 0, duration_ms: 0,
        prompt_summary: "AKIAIOSFODNN7EXAMPLE".into(),
        messages: vec![Message::user("API_KEY=supersecret")],
        commits: vec![], files_touched: vec![],
    };
    redact_session(&mut s, &crate::config::RedactConfig::default());
    assert_eq!(s.prompt_summary, "[REDACTED:aws]");
    if let Message::User { content } = &s.messages[0] {
        assert!(content.contains("[REDACTED:env_assignment]"));
    } else { panic!() }
}
```

**Step 2: Run, verify fail**

**Step 3: Implement**

```rust
pub fn redact_session(s: &mut crate::Session, cfg: &RedactConfig) {
    s.prompt_summary = redact(&s.prompt_summary, cfg);
    for m in &mut s.messages {
        match m {
            crate::Message::User { content } | crate::Message::Assistant { content } => {
                *content = redact(content, cfg);
            }
            crate::Message::Tool { output, .. } => {
                *output = redact(output, cfg);
            }
        }
    }
}
```

**Step 4: Run, verify pass**

**Step 5: Commit**

```bash
git commit -am "feat(core): redact_session"
```

---

## Phase 7 — CLI scaffold

### Task 11: clap scaffold for `dkod` with four subcommands

**Files:**
- Modify: `crates/dkod-cli/src/main.rs`
- Create: `crates/dkod-cli/tests/cli.rs`

**Step 1: Write the failing test**

```rust
// crates/dkod-cli/tests/cli.rs
use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn shows_help() {
    let mut cmd = Command::cargo_bin("dkod").unwrap();
    cmd.arg("--help").assert().success()
        .stdout(contains("init"))
        .stdout(contains("capture"))
        .stdout(contains("log"))
        .stdout(contains("show"));
}
```

**Step 2: Run, verify fail**

Run: `cargo test -p dkod-cli`
Expected: FAIL — no help output / binary not yet wired.

**Step 3: Implement**

```rust
// crates/dkod-cli/src/main.rs
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "dkod", about = "Capture AI agent sessions into git refs")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Initialize dkod in the current repo
    Init,
    /// Capture a session by wrapping an agent invocation
    Capture {
        agent: String,
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// List sessions in this repo
    Log,
    /// Show a session by id
    Show { id: String },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init => todo!("implemented in Task 12"),
        Cmd::Capture { .. } => todo!("implemented in Task 14+"),
        Cmd::Log => todo!("implemented in Task 13"),
        Cmd::Show { .. } => todo!("implemented in Task 13"),
    }
}
```

**Step 4: Run, verify pass**

**Step 5: Commit**

```bash
git commit -am "feat(cli): clap scaffold for init/capture/log/show"
```

---

### Task 12: `dkod init`

**Files:**
- Create: `crates/dkod-cli/src/cmd/mod.rs`, `init.rs`
- Modify: `crates/dkod-cli/src/main.rs`
- Add to test: `crates/dkod-cli/tests/cli.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn init_writes_config_in_a_repo() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::process::Command::new("git").arg("init").arg(tmp.path()).output().unwrap();

    Command::cargo_bin("dkod").unwrap()
        .current_dir(&tmp).arg("init")
        .assert().success();

    let cfg = tmp.path().join(".dkod/config.toml");
    assert!(cfg.exists());
    let body = std::fs::read_to_string(&cfg).unwrap();
    assert!(body.contains("[redact]"));
    assert!(body.contains("enabled = true"));
}

#[test]
fn init_is_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::process::Command::new("git").arg("init").arg(tmp.path()).output().unwrap();
    Command::cargo_bin("dkod").unwrap().current_dir(&tmp).arg("init").assert().success();
    Command::cargo_bin("dkod").unwrap().current_dir(&tmp).arg("init").assert().success();
}

#[test]
fn init_outside_a_repo_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("dkod").unwrap().current_dir(&tmp).arg("init")
        .assert().failure().stderr(predicates::str::contains("not a git repo"));
}
```

**Step 2: Run, verify fail**

**Step 3: Implement** — `init` checks `gix::open(".")` works, creates `.dkod/`, writes `Config::default()` serialized to TOML if not present.

```rust
// crates/dkod-cli/src/cmd/init.rs
use anyhow::{anyhow, Result};
use std::path::Path;

pub fn run(cwd: &Path) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo"))?;
    let dir = cwd.join(".dkod");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("config.toml");
    if !path.exists() {
        let cfg = dkod_core::config::Config::default();
        std::fs::write(&path, toml::to_string_pretty(&cfg)?)?;
    }
    Ok(())
}
```

```rust
// in main.rs
mod cmd;
// match arm:
Cmd::Init => cmd::init::run(&std::env::current_dir()?),
```

**Step 4: Run, verify all three pass**

**Step 5: Commit**

```bash
git commit -am "feat(cli): dkod init writes .dkod/config.toml"
```

---

### Task 13: `dkod log` and `dkod show`

**Files:**
- Create: `crates/dkod-cli/src/cmd/log.rs`, `show.rs`
- Modify: `main.rs`, `cli.rs` test file

**Step 1: Write the failing tests** (`log` lists session ids one per line; `show` prints transcript)

```rust
#[test]
fn log_lists_sessions_written_directly() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::process::Command::new("git").arg("init").arg(tmp.path()).output().unwrap();

    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::Codex,
        created_at: 0, duration_ms: 0,
        prompt_summary: "hello".into(),
        messages: vec![dkod_core::Message::user("hi")],
        commits: vec![], files_touched: vec![],
    };
    dkod_core::store::write_session(tmp.path(), &s).unwrap();

    Command::cargo_bin("dkod").unwrap()
        .current_dir(&tmp).arg("log")
        .assert().success()
        .stdout(predicates::str::contains(&s.id));
}

#[test]
fn show_prints_session_transcript() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::process::Command::new("git").arg("init").arg(tmp.path()).output().unwrap();
    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::Codex,
        created_at: 0, duration_ms: 0,
        prompt_summary: "fix bug".into(),
        messages: vec![dkod_core::Message::user("fix bug"), dkod_core::Message::assistant("done")],
        commits: vec![], files_touched: vec![],
    };
    dkod_core::store::write_session(tmp.path(), &s).unwrap();

    Command::cargo_bin("dkod").unwrap()
        .current_dir(&tmp).args(["show", &s.id])
        .assert().success()
        .stdout(predicates::str::contains("fix bug"))
        .stdout(predicates::str::contains("done"));
}
```

**Step 2: Run, verify fail**

**Step 3: Implement** — both commands shell into `dkod_core::store`. `log` outputs id + agent + summary one per line, sorted newest-first by `created_at`. `show` pretty-prints transcript.

**Step 4: Run, verify pass**

**Step 5: Commit**

```bash
git commit -am "feat(cli): dkod log and dkod show"
```

---

## Phase 8 — Capture: Codex (simpler adapter first)

### Task 14: Research spike — what does Codex CLI emit?

**Files:**
- Create: `docs/research/codex-transcript-format.md`

This is a research task before code. Read the OpenAI Codex CLI source / docs to determine:

- What format does Codex emit on stdout? Plain text? JSON? Streaming?
- Is there a `--json` or transcript-export flag?
- Where does it write any local history (`~/.codex/...`)?
- Does it expose a hook / plugin API?

Decide between three capture strategies in the doc:
- (a) Wrapper exec + parse stdout — works if output is structured.
- (b) Wrapper exec + parse history file — works if Codex writes one.
- (c) Hybrid: wrap exec for diff / timing, read history file for transcript content.

Document the chosen strategy and any constraints. **Do not write code yet — commit only the research doc.**

```bash
git add docs/research/codex-transcript-format.md
git commit -m "docs: research Codex CLI transcript format"
```

---

### Task 15: Codex adapter — `dkod capture codex -- <args>`

**Files:**
- Create: `crates/dkod-core/src/capture/mod.rs`, `codex.rs`
- Modify: `crates/dkod-core/src/lib.rs`
- Create: `crates/dkod-cli/src/cmd/capture.rs`
- Modify: `main.rs`

**Step 1: Write the failing test**

Use a fake `codex` binary written as a small bash script in tests. The wrapper should exec it, capture stdout, build a `Session`, redact, and `write_session` into the repo.

```rust
#[test]
fn capture_codex_writes_a_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::process::Command::new("git").arg("init").arg(tmp.path()).output().unwrap();
    Command::cargo_bin("dkod").unwrap().current_dir(&tmp).arg("init").assert().success();

    // Fake codex binary: writes a known transcript to stdout
    let fake = tmp.path().join("fake-codex.sh");
    std::fs::write(&fake, "#!/bin/sh\necho '>>> USER\nfix the bug\n>>> ASSISTANT\ndone'\n").unwrap();
    std::process::Command::new("chmod").args(["+x", fake.to_str().unwrap()]).status().unwrap();

    Command::cargo_bin("dkod").unwrap()
        .current_dir(&tmp)
        .env("DKOD_CODEX_BIN", &fake)
        .args(["capture", "codex", "--", "anything"])
        .assert().success();

    // log should now show one session
    Command::cargo_bin("dkod").unwrap()
        .current_dir(&tmp).arg("log")
        .assert().success()
        .stdout(predicates::str::contains("fix the bug"));
}
```

**Step 2: Run, verify fail**

**Step 3: Implement** — `capture::codex::run(args, cfg) -> Result<Session>`:
- Spawn the real Codex binary (or `DKOD_CODEX_BIN` for tests).
- Tee stdout while parsing it according to the strategy chosen in Task 14.
- Build `Session` (id from `Session::new_id`, timestamps, agent = Codex).
- `redact_session` using config.
- `write_session` into the repo.

**Step 4: Run, verify pass**

**Step 5: Commit**

```bash
git commit -am "feat(capture): codex adapter"
```

---

### Task 16: Capture diff produced during a session

**Files:**
- Modify: `crates/dkod-core/src/capture/mod.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn capture_records_files_touched() {
    // ... set up repo with an initial commit, then have fake codex
    //     modify a file, then run capture, then assert the resulting
    //     Session.files_touched contains that file path.
}
```

**Step 2-5:** Implement using `gix` to diff `HEAD` against the working tree before/after the agent runs. Write touched paths into `Session.files_touched`. Commit.

```bash
git commit -am "feat(capture): record files_touched via gix diff"
```

---

## Phase 9 — Capture: Claude Code

### Task 17: Define the Claude Code capture wire protocol

**Files:**
- Create: `docs/protocols/claude-code-capture.md`

Decide and document:

- A small NDJSON protocol: each line is one event from Claude Code (user message, assistant chunk, tool call, tool result, completion).
- Transport: a UNIX domain socket at `$XDG_RUNTIME_DIR/dkod/<repo-hash>.sock` (Linux/macOS), with a Windows-named-pipe fallback documented but unimplemented in V1.
- The hook script's job: tee Claude Code's stream-json output to the socket while preserving Claude Code's own behavior.

Commit doc only. No code yet.

```bash
git commit -am "docs: define Claude Code capture wire protocol"
```

---

### Task 18: Socket server in `dkod-core`

**Files:**
- Create: `crates/dkod-core/src/capture/claude_code.rs`

**Step 1: Write the failing test** — start the server, write fake NDJSON lines into the socket, assert a `Session` comes out matching them.

**Step 2-5:** Implement using `tokio::net::UnixListener`. Stream lines, parse JSON, accumulate into a `Session`. Close on EOF or on a `{"event":"end"}` marker. Commit.

```bash
git commit -am "feat(capture): Claude Code NDJSON socket server"
```

---

### Task 19: Claude Code hook script + `dkod capture claude-code`

**Files:**
- Create: `crates/dkod-cli/scripts/claude-code-hook.sh` (template, embedded into binary)
- Modify: `crates/dkod-cli/src/cmd/capture.rs`

**Step 1: Write the failing E2E test** — fake Claude Code by piping a known stream-json file through the hook script into the socket; assert the resulting session.

**Step 2-5:** The hook script `tee`s Claude Code's stream-json into `nc -U <socket>` (or equivalent). `dkod capture claude-code` starts the socket server, waits for client-disconnect, writes the session.

```bash
git commit -am "feat(capture): Claude Code adapter end-to-end"
```

---

## Phase 10 — Distribution

### Task 20: Smoke test — full lifecycle through real git push/fetch

**Files:**
- Create: `crates/dkod-cli/tests/smoke.rs`

End-to-end: init → capture (fake codex) → make a commit → `git push` to a bare repo → `git fetch` from a fresh clone → `dkod log` in the clone returns the same session. This is the test that proves session refs ride on normal git push.

Run, verify pass, commit.

---

### Task 21: `cargo install` works against local checkout

```bash
cargo install --path crates/dkod-cli
which dkod
dkod --help
```

Document any release-profile tweaks in `crates/dkod-cli/Cargo.toml` (e.g. `lto = "thin"`, `strip = true`, `codegen-units = 1`).

```bash
git commit -am "chore: tune release profile for dkod-cli"
```

---

### Task 22: `install.sh` script

**Files:**
- Create: `install.sh` (repo root)
- Create: `.github/workflows/release.yml` (build matrix: macOS x64/arm64, Linux x64/arm64; upload binaries to GitHub Releases)

`install.sh` detects OS+arch, downloads the latest release tarball from the Releases endpoint, verifies SHA256, places the binary at `~/.local/bin/dkod` (or `/usr/local/bin` if writable), and prints next-step instructions.

Test the script locally against a draft release. Commit.

---

### Task 23: Homebrew tap

Create a small `Formula/dkod.rb` in a separate `homebrew-tap` repo (or postpone to V1.1). Fetch from the GitHub Release tarball. Commit on the tap repo.

---

## Done

V1 of `dkod-core` + `dkod-cli` ships with:
- Capture for Claude Code and Codex.
- `init` / `capture` / `log` / `show`.
- Sessions stored under `refs/dkod/sessions/<id>` and rideable on `git push`.
- Redaction default-on for AWS / GitHub / OpenAI / Stripe / env-assignment.
- Distribution via `cargo install`, `install.sh`, GitHub Releases.

Next plan after this lands: `dkod-app` (Tauri viewer).
