# Storage v2 Implementation Plan — Rollup Index, Size Budgets, GC, Lazy Propagation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the approved Storage v2 design
(`docs/plans/2026-06-10-storage-v2-design.md`): one `refs/dkod/index` commit-chain
ref replacing per-session blob refs (O(1) refs, real fetch negotiation), a
capture-time size budget (~256 KiB), `dkod gc --keep`, lazy metadata-only
propagation, and `dkod reindex` migration — with a **permanent** legacy
read-fallback and dual-write **default ON through Phase 3** so every existing
repo, old CLI, and existing test keeps working unmodified.

**Architecture:** A new `dkod_core::index` module owns the index format: tree
paths (`sessions/<date>/<id>/{meta.json,body.json}`, `commits/<aa>/<38>`,
`patchid/<aa>/<38>`), a gitoxide tree codec (read–modify–write of one tree per
batch), `IndexBatch` + `apply_batch` (lock file + ref CAS + bounded retries +
outbox spill). `dkod_core::store` keeps every public signature stable and gains
the index-first/legacy-second fallback chain. `dkod_core::budget` (Phase 2) is
the redact-then-truncate pipeline. The CLI layer adds `dkod reindex`,
`dkod push`, `dkod fetch`, `dkod gc`, rewires `dkod init`'s refspecs, and keeps
all git-network operations (push/fetch/bundle/ls-remote) as `git` shell-outs per
the existing convention (`blame.rs`/`patchid.rs`/`init.rs`).

**Tech Stack:** Rust 2021 (rust-version 1.75, pinned `stable`), gix 0.66
(`default-features = false`, `max-performance-safe` + `dirwalk` — **no
`revision` feature**: all commit walks are manual, like
`store::new_commits_since`), serde/serde_json/toml, uuid (v7), `flate2`
(added Phase 2 for compressed-size measurement; already in the dependency graph
via gix's zlib backend), `git` CLI via `std::process::Command` for network ops,
`assert_cmd` + `tempfile` for e2e tests.

**Design doc:** `docs/plans/2026-06-10-storage-v2-design.md` — section numbers
cited as §N below. Do not relitigate its decisions.

---

## Hard rules (apply to EVERY task; paste verbatim into any subagent prompt)

1. **Git identity:** every commit uses
   `git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "..."`.
   Never `--author` alone (sets author but not committer). Never
   `Co-Authored-By:` lines or any model/agent attribution. This applies to every
   commit, amend, and fix loop.
2. **CodeRabbit at every commit boundary** (use the Claude Code plugin
   `/coderabbit:review`, NOT the raw `coderabbit`/`cr` CLI; never pass
   `--agent`/`--plain`/`--interactive`):
   - before each commit: `/coderabbit:review uncommitted` — resolve findings first;
   - after each commit: `/coderabbit:review committed`;
   - before opening each phase PR: `/coderabbit:review --base main`;
   - after the PR opens: wait for the server-side review, fix every actionable
     finding, push, repeat until clean. Do not merge with open findings.
   - Docs-only commits: CodeRabbit does not meaningfully review docs/config —
     say so in your report; never claim "reviewed clean" for them.
3. **Cargo gates — every commit must pass all four:**
   `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check`.
4. **TDD:** write the failing test first, run it to confirm it fails for the
   expected reason, implement the minimum, run it green, then commit.
5. **Pushing:** `git push` to `dkod-io/dkod-cli` requires
   `gh auth switch --user haim-ari` first — the default active account
   `haimari` lacks push rights and will 403. Re-run the switch before every
   push and before any `gh pr ...` write. Never push without it.
6. **YAGNI:** implement only the design. No cross-session batching, no
   multi-remote reconciliation beyond origin, no Session JSON schema change,
   no transcript encryption, no semantic compaction, no new crates beyond
   `flate2`, no indexer changes (one follow-up issue only, Task 2.6).
7. **Compatibility floor (this plan's own hard invariant):** the legacy
   read-fallback (§6.2) is permanent; dual-write defaults ON until Task 4.5;
   the `Session` struct gains **zero** fields in this plan (`SessionMeta` is a
   separate struct) — see "Schema blast radius" below before touching
   `session.rs`.

**Gate & commit ritual** (referenced by name from every task — run exactly this):

```bash
cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
# /coderabbit:review uncommitted   ← resolve findings
git add <files listed in the task>
git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "<message listed in the task>"
# /coderabbit:review committed
```

---

## Background facts (verified against the tree on 2026-06-10 — do not re-discover)

- `crates/dkod-core/src/session.rs`: `pub struct Session { pub id: String, pub agent: Agent, pub created_at: i64, pub duration_ms: u64, pub prompt_summary: String, pub messages: Vec<Message>, pub commits: Vec<String>, pub files_touched: Vec<String>, #[serde(default)] pub redaction_count: u64 }`. `Session::new_id()` = `uuid::Uuid::now_v7().to_string()`. `pub enum Message { User{content}, Assistant{content}, Reasoning{content}, Tool{name, input: serde_json::Value, output: String} }`. `pub fn agent_label(&Agent) -> &'static str`.
- `crates/dkod-core/src/store.rs` public API (signatures MUST stay stable, §5.2): `write_session(repo_path: &Path, session: &Session) -> Result<()>`; `write_session_with_commit_links(repo_path, session: &mut Session, head_at_start: Option<&str>) -> Result<Vec<String>>`; `read_session(repo_path, id) -> Result<Session>`; `link_session_to_commit(repo_path, session_id, commit_sha) -> Result<()>`; `link_session_to_patchid(repo_path, session_id, patch_id) -> Result<()>`; `relink_commit(repo_path, old_sha, new_sha) -> Result<bool>`; `list_sessions(repo_path) -> Result<Vec<String>>`; `new_commits_since(repo_path, start: Option<&str>) -> Result<Vec<String>>`; `head_sha(path) -> Option<String>`. Internal: `pub(crate) fn ensure_committer(&mut gix::Repository) -> Result<()>` (fallback identity `dkod <noreply@dkod.io>`); `fn write_link_ref(repo, ref_name, blob_id, message)` uses `PreviousValue::Any` (last-writer-wins).
- `crates/dkod-core/src/refs.rs`: `session_ref(id)`, `commit_ref(sha)`, `patchid_ref(pid)`, `parse_session_ref(r)`.
- `crates/dkod-core/src/redact.rs`: `redact_session(&mut Session, &RedactConfig)` (in-place, idempotent, counts into `redaction_count`). `crates/dkod-core/src/config.rs`: `Config { redact: RedactConfig, drift: DriftConfig }`, `#[serde(default)]` everywhere, manual `Default` impls.
- `crates/dkod-cli/src/cmd/capture/mod.rs::finalize_session(cwd, &mut Session, head_at_start: Option<&str>, cfg: &Config) -> Result<Vec<String>>`: redact → `write_session_with_commit_links` → per-linked-sha `patchid::compute_patch_id` + `link_session_to_patchid` (best-effort).
- `crates/dkod-cli/src/cmd/capture/claude_code.rs:561` (`handle_finished_session`): calls `redact_session` + `write_session_with_commit_links` directly (NOT `finalize_session`, and writes **no** patch-id links today). This plan keeps that call site byte-identical — its behavior is preserved by the stable signature.
- `crates/dkod-cli/src/cmd/blame.rs`: `session_for_commit(cwd, sha)` = `find_reference(commit_ref(sha))` → blob → `Session`, fallback `compute_patch_id` → `patchid_ref`. `session_from_ref_name(repo, ref_name) -> Option<(String, String, String)>` (agent label, 8-char short id, prompt_summary). Per-sha results cached in a `HashMap` per run.
- `crates/dkod-cli/src/cmd/relink.rs`: parses `old new` 40-hex pairs from stdin, loops `store::relink_commit` per pair, always exits 0.
- `crates/dkod-cli/src/cmd/init.rs`: `const DKOD_FETCH_REFSPEC = "+refs/dkod/*:refs/dkod/*"`; `ensure_dkod_refspec` adds it to **every** remote; `remote_already_has_dkod_refspec` does exact-line matching via `git config --get-all remote.<r>.fetch`; `discover_remote_sessions` ls-remotes `refs/dkod/sessions/*` on origin and fetches the namespace; `cmd::init::run(cwd: &Path)` takes no flags today and `Cmd::Init` is a bare variant in `main.rs`.
- `crates/dkod-cli/src/cmd/patchid.rs`: `pub(crate) fn compute_patch_id(cwd, sha) -> Option<String>` (git shell-out; CLI layer on purpose).
- `crates/dkod-cli/src/main.rs`: clap `Cmd` enum; dispatch passes `&std::env::current_dir()?`; `maybe_warn_drift()` runs for all commands except `Setup`/`CaptureHook`/`Relink` — new read commands must NOT be added to that exclusion list; new hidden/hook-path commands must be.
- **e2e test conventions** (`crates/dkod-cli/tests/e2e_blame.rs`, `e2e_relink.rs`, `finalize_session.rs`, `e2e_drift.rs`): `assert_cmd::Command::cargo_bin("dkod")`, `tempfile::TempDir`, a local `fn git(repo: &Path, args: &[&str])` helper that sets `GIT_AUTHOR_NAME/EMAIL` + `GIT_COMMITTER_NAME/EMAIL` to `t`/`t@e.com` and asserts success, a `fn head(repo) -> String` helper, sessions seeded via a `dkod_core::Session` struct literal + `store::write_session`. Follow these exactly in new e2e files.
- **Schema blast radius (why `Session` gains no fields):** struct-literal
  construction sites for `Session` —
  `grep -rn "Session {" crates --include="*.rs" | grep -v "pub struct" | wc -l`
  → **39 sites across 21 files** (all 6 core capture parsers, `redact.rs` ×6,
  `store.rs`, `drift.rs`, `cmd/drift.rs`, `cmd/import.rs`, and 9 test files);
  the `redaction_count: 0` literal line alone appears **29 times**
  (`grep -rn 'redaction_count: 0' crates --include='*.rs' | wc -l`). Adding a
  `Session` field — even with `#[serde(default)]` — forces editing all 39
  literal sites and broke cross-branch merges three times the week of
  2026-06-08. Therefore: `SessionMeta` is a **new, separate** struct
  (Task 1.5); `truncated` is **derived** from the truncation marker in tool
  outputs, never stored on `Session`. If any executor believes a `Session`
  field is unavoidable, STOP and escalate instead.
- **Drift benchmark coupling:** `benchmarks/drift/run.sh` seeds fixtures via
  raw plumbing — `git hash-object -w --stdin` + `git update-ref
  refs/dkod/sessions/<id> <blob>` — i.e. pure-legacy repos with **no index
  ref**. The permanent fallback chain (§6.2) must keep it working **in every
  phase**; Task 1.10 adds an e2e regression test that replicates exactly this
  plumbing-seeded pattern so the benchmark can never silently break. The
  benchmark itself is only touched in optional Task 5.3 (adds an opt-in
  `DKOD_BENCH_V2=1` mode); its default path stays legacy forever.
- **dkod-indexer (separate repo):** its reconciler `git ls-remote`s
  `refs/dkod/*` and ingests per-session refs. `refs/dkod/index` matches that
  same glob, and dual-write keeps emitting per-session refs through Phase 3 —
  so the index-ref addition is **backward compatible** for the deployed
  indexer; no indexer change ships in this plan. One follow-up issue is filed
  in Task 2.6 (out of scope otherwise, per §2 non-goals and §11).
- **Existing tests that assert v1 init-refspec behavior** (the ONLY existing
  tests this plan may modify, and only in Task 2.4, because the design
  explicitly replaces that behavior in §9.1): `crates/dkod-cli/tests/cli.rs` —
  `count_dkod_refspecs` helper (line ~126 matches `+refs/dkod/*:refs/dkod/*`),
  `init_writes_dkod_refspec_when_remote_exists`, `init_refspec_is_idempotent`,
  `init_writes_refspec_to_all_remotes`; and
  `crates/dkod-cli/tests/init_session_discovery.rs` — the duplicate-refspec
  assertion (lines ~240–242) and the clone-side assertions at lines ~114/129
  that check `refs/dkod/sessions/<sid>` materializes in the clone. Every other
  existing test passes **unmodified** in every phase — that is the shippable
  gate.
- gix 0.66 API facts used below: `repo.write_blob(&[u8])`,
  `repo.write_object(&impl WriteTo)` (works for `gix::objs::Tree` and
  `gix::objs::Commit`), `repo.find_object(oid)?.try_into_tree()?.decode()?`
  → `gix::objs::TreeRef`, `repo.edit_reference(RefEdit)` with
  `PreviousValue::{Any, MustNotExist, MustExistAndMatch(Target)}`,
  `repo.try_find_reference(name)`, `gix::objs::tree::{Entry, EntryKind}`
  (`Entry` implements `Ord` with git's tree-name ordering — sort before
  writing), `gix::actor::SignatureRef { name, email, time: gix::date::Time::now_utc() }`,
  `repo.committer() -> Option<Result<SignatureRef, _>>` (always `Some` after
  `ensure_committer`). `repo.path()` returns the `.git` dir.
- uuid crate facts: `uuid::Uuid::parse_str`, `get_version_num() -> usize`,
  `as_bytes() -> &[u8; 16]` (first 6 bytes of a v7 = big-endian unix
  milliseconds), and for tests `uuid::Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, secs, nanos))`
  (the `v7` feature is already enabled workspace-wide).

## Sequencing contract (why every phase ships green)

| Phase | What changes | Why existing tests still pass unmodified |
|---|---|---|
| 1 | index written *in addition to* legacy refs (dual-write ON by default); reads index-first with legacy fallback; `dkod reindex` | Every legacy ref the old tests assert on (`refs/dkod/sessions/*`, `commits/*`, `patchid/*`, blob-id equality) is still written; reads resolve identically through either branch of the fallback chain; plumbing-seeded repos (benchmark pattern) have no index ref → pure-v1 behavior (§6.2). |
| 2 | capture budget (only truncates >256 KiB sessions — all test fixtures are tiny → no-op); auto-push (silently skipped when no `origin`, the test default); init refspec switch | Only the 6 init-refspec/discovery test sites listed above change, in the same commit as the behavior they specify (Task 2.4). |
| 3 | lazy propagation is **additive** (`dkod-archive` remote, `dkod fetch`, offline messaging) | No default local behavior changes; `--subscribe` defaults preserve Phase-2 wiring when the probe fails. |
| 4 | gc/epoch/`--delete-legacy` are explicit opt-in commands; dual-write default flips OFF | Tests that need legacy refs set `write_legacy_refs = true` explicitly from Phase 1 onward… they do NOT — instead Task 4.5 lists the exact tests whose fixtures gain a `.dkod/config.toml` with `write_legacy_refs = true`, keeping the legacy-path assertions meaningful while the default flips. |
| 5 | optional polish | additive only. |

Branch naming: one branch + PR per phase — `feat/storage-v2-phase1` …
`feat/storage-v2-phase5`, each cut from up-to-date `main` after the previous
phase merges.

---

## Phase 1 — index core + dual-write + read fallback + reindex (§15.1)

### Task 1.1: `[storage]` config section + core loader

**Files:**
- Modify: `crates/dkod-core/src/config.rs`

- [ ] **Step 1: Write the failing tests.** Append to `#[cfg(test)] mod tests` in `config.rs`:

```rust
#[test]
fn defaults_storage_to_dual_write_on_with_no_format() {
    let c: Config = toml::from_str("").unwrap();
    assert!(c.storage.write_legacy_refs);
    assert_eq!(c.storage.format, None);
}

#[test]
fn storage_section_overrides_parse() {
    let toml = r#"
        [storage]
        format = "v2"
        write_legacy_refs = false
    "#;
    let c: Config = toml::from_str(toml).unwrap();
    assert_eq!(c.storage.format.as_deref(), Some("v2"));
    assert!(!c.storage.write_legacy_refs);
}

#[test]
fn load_storage_config_defaults_when_file_missing_or_bad() {
    let tmp = tempfile::TempDir::new().unwrap();
    let s = load_storage_config(tmp.path());
    assert!(s.write_legacy_refs); // missing file → default
    std::fs::create_dir_all(tmp.path().join(".dkod")).unwrap();
    std::fs::write(tmp.path().join(".dkod/config.toml"), "not [valid toml").unwrap();
    let s = load_storage_config(tmp.path());
    assert!(s.write_legacy_refs); // unparseable file → default, never an error
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-core storage` — FAIL: no `storage` field / no `load_storage_config`.

- [ ] **Step 3: Implement.** In `config.rs`: add `pub storage: StorageConfig` to `Config` (after `drift`), plus:

```rust
/// Storage-format coordination, committed in `.dkod/config.toml` (§12 of the
/// storage-v2 design). Old CLIs ignore unknown keys (serde default behavior).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// `Some("v2")` once `dkod reindex` (or a new init) has run; `None` means
    /// a v1-era repo. Informational — readers always use the fallback chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Dual-write toggle: when true, every index write also writes the legacy
    /// `refs/dkod/{sessions,commits,patchid}/*` refs so old CLIs keep reading.
    pub write_legacy_refs: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self { format: None, write_legacy_refs: true }
    }
}

/// Best-effort `[storage]` load for `dkod-core` writers (`store.rs` cannot
/// take a `Config` parameter without breaking public signatures). Missing or
/// unparseable `.dkod/config.toml` → defaults; never errors.
pub fn load_storage_config(repo_path: &std::path::Path) -> StorageConfig {
    let path = repo_path.join(".dkod/config.toml");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return StorageConfig::default();
    };
    toml::from_str::<Config>(&body)
        .map(|c| c.storage)
        .unwrap_or_default()
}
```

`dkod-core` needs `tempfile` only as a dev-dependency — it already has it.

- [ ] **Step 4: Run green.** `cargo test -p dkod-core storage` and `cargo test -p dkod-core config`.
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-core/src/config.rs`.
  Message: `feat(config): [storage] section (format, write_legacy_refs) + core loader`

### Task 1.2: index path helpers + UUIDv7 date derivation

**Files:**
- Create: `crates/dkod-core/src/index.rs`
- Modify: `crates/dkod-core/src/lib.rs` (add `pub mod index;` after `pub mod drift;`)

- [ ] **Step 1: Write the failing tests.** Create `index.rs` containing only this test module and stubs that return `None`/empty (so the tests fail for the right reason), then fill in Step 3's bodies:

```rust
#[cfg(test)]
mod path_tests {
    use super::*;

    /// UUIDv7 generated at exactly 2025-01-01T00:00:00Z (unix 1735689600).
    fn v7_at(secs: u64) -> String {
        uuid::Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, secs, 0)).to_string()
    }

    #[test]
    fn v7_id_maps_to_its_embedded_date() {
        let id = v7_at(1735689600);
        assert_eq!(uuid_v7_unix_ms(&id), Some(1735689600000));
        assert_eq!(session_dir(&id), Some(format!("sessions/2025-01-01/{id}")));
        assert_eq!(meta_path(&id), Some(format!("sessions/2025-01-01/{id}/meta.json")));
        assert_eq!(body_path(&id), Some(format!("sessions/2025-01-01/{id}/body.json")));
    }

    #[test]
    fn non_v7_id_yields_none_paths() {
        assert_eq!(uuid_v7_unix_ms("not-a-uuid"), None);
        // v4 uuid: version nibble is 4, not 7.
        assert_eq!(uuid_v7_unix_ms("123e4567-e89b-42d3-a456-426614174000"), None);
        assert_eq!(session_dir("not-a-uuid"), None);
    }

    #[test]
    fn session_dir_for_falls_back_to_created_at() {
        // non-v7 id + created_at 2025-01-01 → date from created_at
        let d = session_dir_for("legacy-id-1", 1735689600);
        assert_eq!(d, "sessions/2025-01-01/legacy-id-1");
        // v7 id wins over created_at
        let id = v7_at(1735689600);
        assert_eq!(session_dir_for(&id, 0), format!("sessions/2025-01-01/{id}"));
        // negative created_at clamps to epoch
        assert_eq!(session_dir_for("x", -5), "sessions/1970-01-01/x");
    }

    #[test]
    fn date_math_handles_leap_years_and_epoch() {
        assert_eq!(date_from_unix_ms(0), "1970-01-01");
        // 2024-02-29T12:00:00Z = 1709208000
        assert_eq!(date_from_unix_ms(1709208000 * 1000), "2024-02-29");
        // 2026-06-10T00:00:00Z = 1781049600
        assert_eq!(date_from_unix_ms(1781049600 * 1000), "2026-06-10");
    }

    #[test]
    fn pointer_paths_use_two_hex_fanout() {
        let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        assert_eq!(commit_pointer_path(sha), format!("commits/de/{}", &sha[2..]));
        assert_eq!(patchid_pointer_path(sha), format!("patchid/de/{}", &sha[2..]));
    }
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-core path_tests` — FAIL (stubs).

- [ ] **Step 3: Implement** (top of `index.rs`):

```rust
//! Storage v2 rollup index (`dkod-index/1`, design 2026-06-10): one
//! `refs/dkod/index` ref pointing at a commit chain whose tree holds every
//! session's metadata + body and the commit/patch-id lookup tables. This
//! module owns the tree layout, the tree codec, and the batched, CAS-guarded
//! write path. `store.rs` composes it with the legacy-ref dual-write and the
//! permanent legacy read fallback.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

/// The single v2 ref (§4.1). Points at a commit; parent = previous tip.
pub const INDEX_REF: &str = "refs/dkod/index";
/// Contents of the root `version` blob.
pub const INDEX_VERSION: &str = "1\n";

/// Unix milliseconds embedded in a UUIDv7's first 48 bits, or `None` for
/// anything that is not a v7 UUID (foreign imports, hand-rolled test ids).
pub(crate) fn uuid_v7_unix_ms(id: &str) -> Option<u64> {
    let u = uuid::Uuid::parse_str(id).ok()?;
    if u.get_version_num() != 7 {
        return None;
    }
    let b = u.as_bytes();
    Some(
        ((b[0] as u64) << 40)
            | ((b[1] as u64) << 32)
            | ((b[2] as u64) << 24)
            | ((b[3] as u64) << 16)
            | ((b[4] as u64) << 8)
            | (b[5] as u64),
    )
}

/// `YYYY-MM-DD` (UTC) for a unix-milliseconds timestamp. Civil-from-days
/// algorithm (Howard Hinnant) — dependency-free, valid for all dates ≥ 1970.
pub(crate) fn date_from_unix_ms(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

/// `sessions/<date>/<id>` for a v7 id (date computable from the id alone,
/// §4.2), `None` otherwise.
pub fn session_dir(id: &str) -> Option<String> {
    uuid_v7_unix_ms(id).map(|ms| format!("sessions/{}/{id}", date_from_unix_ms(ms)))
}

/// Write-side directory: v7 timestamp when available, else the session's
/// `created_at` (clamped to epoch) — §10's rule for non-v7 ids.
pub fn session_dir_for(id: &str, created_at: i64) -> String {
    match session_dir(id) {
        Some(d) => d,
        None => {
            let ms = created_at.max(0) as u64 * 1000;
            format!("sessions/{}/{id}", date_from_unix_ms(ms))
        }
    }
}

pub fn meta_path(id: &str) -> Option<String> {
    session_dir(id).map(|d| format!("{d}/meta.json"))
}

pub fn body_path(id: &str) -> Option<String> {
    session_dir(id).map(|d| format!("{d}/body.json"))
}

/// `commits/<aa>/<remaining-38>` (§4.2 hex fan-out).
pub fn commit_pointer_path(sha: &str) -> String {
    format!("commits/{}/{}", &sha[..2], &sha[2..])
}

/// `patchid/<aa>/<remaining-38>`.
pub fn patchid_pointer_path(pid: &str) -> String {
    format!("patchid/{}/{}", &pid[..2], &pid[2..])
}
```

(`anyhow`/`Path` imports are used from Task 1.3 onward; `#[allow(unused_imports)]` is NOT needed — Task 1.3 lands in the same phase, but to keep THIS commit green, only import what this task uses: drop the `anyhow`/`Path` lines here and add them in Task 1.3.)

- [ ] **Step 4: Run green.** `cargo test -p dkod-core path_tests` — 5 PASS.
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-core/src/index.rs`, `crates/dkod-core/src/lib.rs`.
  Message: `feat(index): tree-path helpers + uuidv7 date derivation for the rollup index`

### Task 1.3: gitoxide tree codec (read / upsert / write)

**Files:**
- Modify: `crates/dkod-core/src/index.rs`

- [ ] **Step 1: Write the failing tests.** Append to `index.rs`:

```rust
#[cfg(test)]
mod tree_tests {
    use super::*;
    use tempfile::TempDir;

    fn repo() -> (TempDir, gix::Repository) {
        let tmp = TempDir::new().unwrap();
        let mut r = gix::init(tmp.path()).unwrap();
        crate::store::ensure_committer(&mut r).unwrap();
        (tmp, r)
    }

    #[test]
    fn upsert_creates_nested_path_and_read_blob_round_trips() {
        let (_tmp, repo) = repo();
        let blob = repo.write_blob(b"hello").unwrap().detach();
        let root = upsert_path(&repo, None, "sessions/2025-01-01/abc/body.json", blob).unwrap();
        assert_eq!(
            read_tree_blob(&repo, root, "sessions/2025-01-01/abc/body.json").unwrap(),
            b"hello".to_vec()
        );
        assert!(read_tree_blob(&repo, root, "sessions/2025-01-01/abc/meta.json").is_none());
        assert!(read_tree_blob(&repo, root, "nope/nope").is_none());
    }

    #[test]
    fn upsert_preserves_siblings_and_overwrites_same_path() {
        let (_tmp, repo) = repo();
        let a = repo.write_blob(b"a").unwrap().detach();
        let b = repo.write_blob(b"b").unwrap().detach();
        let root = upsert_path(&repo, None, "commits/de/adbeef", a).unwrap();
        let root = upsert_path(&repo, Some(root), "commits/de/other", b).unwrap();
        let root = upsert_path(&repo, Some(root), "commits/de/adbeef", b).unwrap();
        assert_eq!(read_tree_blob(&repo, root, "commits/de/adbeef").unwrap(), b"b".to_vec());
        assert_eq!(read_tree_blob(&repo, root, "commits/de/other").unwrap(), b"b".to_vec());
    }

    #[test]
    fn blob_oid_at_returns_oid_for_existing_path() {
        let (_tmp, repo) = repo();
        let blob = repo.write_blob(b"x").unwrap().detach();
        let root = upsert_path(&repo, None, "version", blob).unwrap();
        assert_eq!(blob_oid_at(&repo, root, "version"), Some(blob));
        assert_eq!(blob_oid_at(&repo, root, "epoch"), None);
    }

    #[test]
    fn list_tree_dir_names_subdirectories() {
        let (_tmp, repo) = repo();
        let blob = repo.write_blob(b"x").unwrap().detach();
        let root = upsert_path(&repo, None, "sessions/2025-01-01/a/meta.json", blob).unwrap();
        let root = upsert_path(&repo, Some(root), "sessions/2025-01-02/b/meta.json", blob).unwrap();
        let dates = list_tree_dir(&repo, root, "sessions").unwrap();
        assert_eq!(dates, vec!["2025-01-01".to_string(), "2025-01-02".to_string()]);
        let ids = list_tree_dir(&repo, root, "sessions/2025-01-01").unwrap();
        assert_eq!(ids, vec!["a".to_string()]);
    }
}
```

Note the tests refer to **tree** oids directly — commit plumbing arrives in
Task 1.4. Make `ensure_committer` reachable: in `store.rs` it is already
`pub(crate)`; that is sufficient (same crate).

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-core tree_tests` — FAIL: functions missing.

- [ ] **Step 3: Implement.** Add to `index.rs` (with `use anyhow::{anyhow, Context, Result};` now imported):

```rust
/// Decode a tree object into owned entries. Empty Vec for `None`.
fn tree_entries(
    repo: &gix::Repository,
    tree: Option<gix::ObjectId>,
) -> Result<Vec<gix::objs::tree::Entry>> {
    let Some(oid) = tree else { return Ok(Vec::new()) };
    let obj = repo.find_object(oid).context("find tree object")?;
    let tree = obj.try_into_tree().map_err(|e| anyhow!("not a tree: {e}"))?;
    let decoded = tree.decode().context("decode tree")?;
    Ok(decoded.entries.iter().map(|e| e.into()).collect())
}

/// Write a tree from entries (sorted into git's canonical tree order via
/// `Entry: Ord`, which honors the directory-sorts-as-`name/` rule).
fn write_tree(
    repo: &gix::Repository,
    mut entries: Vec<gix::objs::tree::Entry>,
) -> Result<gix::ObjectId> {
    entries.sort();
    Ok(repo
        .write_object(&gix::objs::Tree { entries })
        .context("write tree")?
        .detach())
}

/// Read-modify-write a single `path` (slash-separated, all intermediate
/// components trees) to point at `blob`, returning the new ROOT tree oid.
/// `root = None` starts from an empty tree. A non-tree in an intermediate
/// position is an error (corrupt index — never silently overwritten).
pub(crate) fn upsert_path(
    repo: &gix::Repository,
    root: Option<gix::ObjectId>,
    path: &str,
    blob: gix::ObjectId,
) -> Result<gix::ObjectId> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err(anyhow!("empty index path"));
    }
    upsert_segments(repo, root, &segments, blob)
}

fn upsert_segments(
    repo: &gix::Repository,
    tree: Option<gix::ObjectId>,
    segments: &[&str],
    blob: gix::ObjectId,
) -> Result<gix::ObjectId> {
    use gix::objs::tree::{Entry, EntryKind};
    let mut entries = tree_entries(repo, tree)?;
    let name = segments[0];
    let existing = entries.iter().position(|e| e.filename == name);
    if segments.len() == 1 {
        let entry = Entry {
            mode: EntryKind::Blob.into(),
            filename: name.into(),
            oid: blob,
        };
        match existing {
            Some(i) => entries[i] = entry,
            None => entries.push(entry),
        }
    } else {
        let child = match existing {
            Some(i) => {
                if !entries[i].mode.is_tree() {
                    return Err(anyhow!("index path conflict at {name:?}: blob where tree expected"));
                }
                Some(entries[i].oid)
            }
            None => None,
        };
        let new_child = upsert_segments(repo, child, &segments[1..], blob)?;
        let entry = Entry {
            mode: EntryKind::Tree.into(),
            filename: name.into(),
            oid: new_child,
        };
        match existing {
            Some(i) => entries[i] = entry,
            None => entries.push(entry),
        }
    }
    write_tree(repo, entries)
}

/// Resolve `path` under the ROOT TREE `root` to its blob oid. `None` when any
/// component is absent or the leaf is not a blob.
pub(crate) fn blob_oid_at(
    repo: &gix::Repository,
    root: gix::ObjectId,
    path: &str,
) -> Option<gix::ObjectId> {
    let mut current = root;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    for (i, seg) in segments.iter().enumerate() {
        let entries = tree_entries(repo, Some(current)).ok()?;
        let entry = entries.iter().find(|e| e.filename == *seg)?;
        if i == segments.len() - 1 {
            return entry.mode.is_blob().then_some(entry.oid);
        }
        if !entry.mode.is_tree() {
            return None;
        }
        current = entry.oid;
    }
    None
}

/// Blob bytes at `path` under root tree `root`.
pub(crate) fn read_tree_blob(
    repo: &gix::Repository,
    root: gix::ObjectId,
    path: &str,
) -> Option<Vec<u8>> {
    let oid = blob_oid_at(repo, root, path)?;
    Some(repo.find_object(oid).ok()?.detach().data)
}

/// Names of the SUBTREE entries directly under `dir` ("" = root), sorted.
/// Used to walk `sessions/<date>/<id>` (§6.1) and gc candidates (§8).
pub(crate) fn list_tree_dir(
    repo: &gix::Repository,
    root: gix::ObjectId,
    dir: &str,
) -> Result<Vec<String>> {
    let mut current = root;
    for seg in dir.split('/').filter(|s| !s.is_empty()) {
        let entries = tree_entries(repo, Some(current))?;
        match entries.iter().find(|e| e.filename == seg && e.mode.is_tree()) {
            Some(e) => current = e.oid,
            None => return Ok(Vec::new()),
        }
    }
    let mut names: Vec<String> = tree_entries(repo, Some(current))?
        .into_iter()
        .filter(|e| e.mode.is_tree())
        .map(|e| e.filename.to_string())
        .collect();
    names.sort();
    Ok(names)
}
```

> **Executor note (gix 0.66 API drift):** if `EntryRef → Entry` lacks a `From`
> impl, build `Entry { mode: e.mode, filename: e.filename.to_owned(), oid: e.oid.into() }`
> manually inside `tree_entries`. If `entries.sort()` does not produce git's
> canonical order in this gix version (a `verify`/write error mentioning
> ordering), sort with the explicit comparator: compare `filename` bytes with a
> trailing `/` appended for tree entries. Keep behavior identical either way.

- [ ] **Step 4: Run green.** `cargo test -p dkod-core tree_tests` — 4 PASS.
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-core/src/index.rs`.
  Message: `feat(index): gitoxide tree codec (upsert/read/list) for the rollup index`

### Task 1.4: `IndexBatch`, `build_commit`, `apply_batch` (lock + CAS + retries)

**Files:**
- Modify: `crates/dkod-core/src/index.rs`

- [ ] **Step 1: Write the failing tests.** Append to `index.rs`:

```rust
#[cfg(test)]
mod batch_tests {
    use super::*;
    use tempfile::TempDir;

    fn repo() -> (TempDir, gix::Repository) {
        let tmp = TempDir::new().unwrap();
        let mut r = gix::init(tmp.path()).unwrap();
        crate::store::ensure_committer(&mut r).unwrap();
        (tmp, r)
    }

    fn batch(msg: &str, paths: &[(&str, &[u8])]) -> IndexBatch {
        let mut b = IndexBatch::new(msg);
        for (p, bytes) in paths {
            b.insert(p.to_string(), bytes.to_vec());
        }
        b
    }

    #[test]
    fn first_apply_creates_root_with_version_and_epoch() {
        let (tmp, repo) = repo();
        let tip = apply_batch(tmp.path(), &batch("dkod: test", &[("commits/aa/bb", b"x\n")]))
            .unwrap()
            .expect("a commit must be written");
        assert_eq!(index_tip(&repo), Some(tip));
        let root = commit_tree(&repo, tip).unwrap();
        assert_eq!(read_tree_blob(&repo, root, "version").unwrap(), b"1\n".to_vec());
        assert_eq!(read_tree_blob(&repo, root, "epoch").unwrap(), b"0\n".to_vec());
        assert_eq!(read_tree_blob(&repo, root, "commits/aa/bb").unwrap(), b"x\n".to_vec());
        // root commit has no parent
        let commit = repo.find_object(tip).unwrap().try_into_commit().unwrap();
        assert_eq!(commit.parent_ids().count(), 0);
    }

    #[test]
    fn second_apply_chains_onto_first() {
        let (tmp, repo) = repo();
        let t1 = apply_batch(tmp.path(), &batch("m1", &[("a", b"1")])).unwrap().unwrap();
        let t2 = apply_batch(tmp.path(), &batch("m2", &[("b", b"2")])).unwrap().unwrap();
        let commit = repo.find_object(t2).unwrap().try_into_commit().unwrap();
        let parents: Vec<_> = commit.parent_ids().map(|p| p.detach()).collect();
        assert_eq!(parents, vec![t1]);
        let root = commit_tree(&repo, t2).unwrap();
        assert!(read_tree_blob(&repo, root, "a").is_some(), "older path must persist");
        assert!(read_tree_blob(&repo, root, "b").is_some());
    }

    #[test]
    fn reapplying_identical_batch_is_a_noop() {
        let (tmp, _repo) = repo();
        let b = batch("m", &[("a", b"1")]);
        let t1 = apply_batch(tmp.path(), &b).unwrap().unwrap();
        assert_eq!(apply_batch(tmp.path(), &b).unwrap(), None, "no empty commit on no-op");
        let repo2 = gix::open(tmp.path()).unwrap();
        assert_eq!(index_tip(&repo2), Some(t1), "tip unchanged");
    }

    #[test]
    fn build_commit_without_ref_edit_leaves_index_ref_alone() {
        let (tmp, repo) = repo();
        let t1 = apply_batch(tmp.path(), &batch("m", &[("a", b"1")])).unwrap().unwrap();
        let blob = repo.write_blob(b"side").unwrap().detach();
        let side = build_commit(&repo, Some(t1), &[("b".to_string(), blob)], "side").unwrap();
        assert!(side.is_some());
        assert_eq!(index_tip(&repo), Some(t1), "ref must not move");
    }

    #[test]
    fn commit_inserts_cas_rejects_stale_expected() {
        let (tmp, repo) = repo();
        let t1 = apply_batch(tmp.path(), &batch("m1", &[("a", b"1")])).unwrap().unwrap();
        let t2 = apply_batch(tmp.path(), &batch("m2", &[("b", b"2")])).unwrap().unwrap();
        assert_ne!(t1, t2);
        let blob = repo.write_blob(b"3").unwrap().detach();
        // expected tip is stale (t1) while the ref is at t2 → must error
        let err = commit_inserts(&repo, Some(t2), Some(t1), &[("c".to_string(), blob)], "m3");
        assert!(err.is_err(), "stale CAS must be rejected");
    }

    #[test]
    fn concurrent_apply_batch_keeps_every_insert() {
        let (tmp, _repo) = repo();
        let path = tmp.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let p = path.clone();
                std::thread::spawn(move || {
                    let b = {
                        let mut b = IndexBatch::new(format!("t{i}"));
                        b.insert(format!("commits/aa/{i:038}"), vec![b'0' + i as u8]);
                        b
                    };
                    apply_batch(&p, &b).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let repo = gix::open(&path).unwrap();
        let tip = index_tip(&repo).unwrap();
        let root = commit_tree(&repo, tip).unwrap();
        for i in 0..8 {
            assert!(
                read_tree_blob(&repo, root, &format!("commits/aa/{i:038}")).is_some(),
                "insert {i} lost under contention"
            );
        }
    }
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-core batch_tests` — FAIL: types/functions missing.

- [ ] **Step 3: Implement.** Add to `index.rs`:

```rust
/// One logical index write: ordered `(tree_path, blob_bytes)` inserts applied
/// to the current tip's tree in a single read-modify-write, producing one
/// child commit (§5.1).
pub struct IndexBatch {
    pub message: String,
    inserts: Vec<(String, Vec<u8>)>,
}

impl IndexBatch {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), inserts: Vec::new() }
    }
    pub fn insert(&mut self, path: String, bytes: Vec<u8>) {
        self.inserts.push((path, bytes));
    }
    pub fn is_empty(&self) -> bool {
        self.inserts.is_empty()
    }
    pub fn len(&self) -> usize {
        self.inserts.len()
    }
}

/// Current `refs/dkod/index` tip commit, if the ref exists.
pub fn index_tip(repo: &gix::Repository) -> Option<gix::ObjectId> {
    let r = repo.try_find_reference(INDEX_REF).ok()??;
    Some(r.id().detach())
}

/// ROOT TREE oid of an index commit.
pub(crate) fn commit_tree(repo: &gix::Repository, tip: gix::ObjectId) -> Result<gix::ObjectId> {
    let commit = repo
        .find_object(tip)
        .context("find index commit")?
        .try_into_commit()
        .map_err(|e| anyhow!("index tip is not a commit: {e}"))?;
    Ok(commit.tree_id().context("index commit tree id")?.detach())
}

/// The repo's committer signature (guaranteed present after
/// `store::ensure_committer`) as an owned `gix::actor::Signature`.
fn signature(repo: &gix::Repository) -> Result<gix::actor::Signature> {
    let sig = repo
        .committer()
        .ok_or_else(|| anyhow!("no committer configured (ensure_committer not called)"))?
        .map_err(|e| anyhow!("committer time: {e}"))?;
    Ok(sig.to_owned())
}

/// Write the COMMIT OBJECT applying `inserts` (path → existing blob oid) on
/// top of `parent`'s tree, WITHOUT touching any ref. Seeds `version`/`epoch`
/// blobs when `parent` is `None` (new root). Returns `Ok(None)` when every
/// insert is already present with the same oid (no object written — the
/// idempotence rule of §10.3).
pub(crate) fn build_commit(
    repo: &gix::Repository,
    parent: Option<gix::ObjectId>,
    inserts: &[(String, gix::ObjectId)],
    message: &str,
) -> Result<Option<gix::ObjectId>> {
    let old_root = match parent {
        Some(p) => Some(commit_tree(repo, p)?),
        None => None,
    };
    let mut root = old_root;
    if parent.is_none() {
        let version = repo.write_blob(INDEX_VERSION.as_bytes()).context("write version blob")?.detach();
        let epoch = repo.write_blob(b"0\n").context("write epoch blob")?.detach();
        root = Some(upsert_path(repo, root, "version", version)?);
        root = Some(upsert_path(repo, root.take(), "epoch", epoch)?);
    }
    for (path, oid) in inserts {
        let already = root.and_then(|r| blob_oid_at(repo, r, path));
        if already == Some(*oid) {
            continue; // idempotent skip
        }
        root = Some(upsert_path(repo, root, path, *oid)?);
    }
    let Some(new_root) = root else { return Ok(None) };
    if old_root == Some(new_root) {
        return Ok(None); // nothing changed → no empty commit
    }
    let sig = signature(repo)?;
    let commit = gix::objs::Commit {
        tree: new_root,
        parents: parent.into_iter().collect(),
        author: sig.clone(),
        committer: sig,
        encoding: None,
        message: message.into(),
        extra_headers: Vec::new(),
    };
    Ok(Some(repo.write_object(&commit).context("write index commit")?.detach()))
}

/// `build_commit` + move `refs/dkod/index` from `expected_tip` to the new
/// commit with compare-and-swap semantics (§5.3 layer 2). `expected_tip =
/// None` requires the ref to not exist yet.
pub(crate) fn commit_inserts(
    repo: &gix::Repository,
    parent: Option<gix::ObjectId>,
    expected_tip: Option<gix::ObjectId>,
    inserts: &[(String, gix::ObjectId)],
    message: &str,
) -> Result<Option<gix::ObjectId>> {
    use gix::refs::{
        transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
        Target,
    };
    let Some(new_tip) = build_commit(repo, parent, inserts, message)? else {
        return Ok(None);
    };
    let expected = match expected_tip {
        Some(t) => PreviousValue::MustExistAndMatch(Target::Object(t)),
        None => PreviousValue::MustNotExist,
    };
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
            },
            expected,
            new: Target::Object(new_tip),
        },
        name: INDEX_REF.try_into().context("invalid index ref name")?,
        deref: false,
    })
    .context("CAS edit of refs/dkod/index")?;
    Ok(Some(new_tip))
}

/// Advisory lock file serializing local index writers (§5.3 layer 1).
/// `O_CREAT|O_EXCL`; waits up to ~10 s in 100 ms steps; a lock older than
/// 30 s is treated as stale and removed. Released on Drop.
struct IndexLock {
    path: std::path::PathBuf,
}

impl IndexLock {
    fn acquire(repo: &gix::Repository) -> Result<Self> {
        let dir = repo.path().join("dkod");
        std::fs::create_dir_all(&dir).context("create .git/dkod")?;
        let path = dir.join("index.lock");
        for _ in 0..100 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|m| std::time::SystemTime::now().duration_since(m).ok())
                        .is_some_and(|age| age > std::time::Duration::from_secs(30));
                    if stale {
                        let _ = std::fs::remove_file(&path); // stale takeover
                        continue;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => return Err(e).context("create index lock"),
            }
        }
        Err(anyhow!("timed out waiting for index lock at {}", path.display()))
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Apply one batch as one index commit: lock → write blobs → CAS loop
/// (5 bounded attempts with jitter, §5.3). Returns the new tip, or `Ok(None)`
/// when the batch was already fully present (idempotent no-op). Errors only
/// after retry exhaustion — callers on the capture path spill to the outbox
/// (`store::spill_to_outbox`) instead of failing the session.
pub fn apply_batch(repo_path: &Path, batch: &IndexBatch) -> Result<Option<gix::ObjectId>> {
    let mut repo = gix::open(repo_path).context("open repo")?;
    crate::store::ensure_committer(&mut repo)?;
    let _lock = IndexLock::acquire(&repo)?;
    let mut inserts = Vec::with_capacity(batch.inserts.len());
    for (path, bytes) in &batch.inserts {
        let oid = repo.write_blob(bytes.as_slice()).context("write index blob")?.detach();
        inserts.push((path.clone(), oid));
    }
    let mut last_err = None;
    for attempt in 0..5 {
        let tip = index_tip(&repo);
        match commit_inserts(&repo, tip, tip, &inserts, &batch.message) {
            Ok(res) => return Ok(res),
            Err(e) => {
                last_err = Some(e);
                // jitter: 10–50 ms scaled by attempt
                let ms = 10 + (attempt as u64 * 10) + (std::process::id() as u64 % 10);
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("index CAS retries exhausted")))
}
```

- [ ] **Step 4: Run green.** `cargo test -p dkod-core batch_tests` — 6 PASS. The concurrency test is the slowest (lock contention); it must still finish in seconds.
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-core/src/index.rs`.
  Message: `feat(index): IndexBatch + apply_batch with lock-file + ref-CAS retry (one commit per batch)`

### Task 1.5: `SessionMeta` + truncation-marker constant

**Files:**
- Modify: `crates/dkod-core/src/session.rs`

- [ ] **Step 1: Write the failing tests.** Append to `session.rs` tests:

```rust
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
            format!("head…{}412 KiB of tool output]…tail", TRUNCATION_MARKER_PREFIX),
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
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-core session_meta` — FAIL.

- [ ] **Step 3: Implement.** Add to `session.rs` (below `Session`):

```rust
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
```

- [ ] **Step 4: Run green.** `cargo test -p dkod-core session_meta` — 3 PASS; full `cargo test -p dkod-core` still green.
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-core/src/session.rs`.
  Message: `feat(session): SessionMeta derived header + truncation marker constant`

### Task 1.6: dual-write write path in `store.rs` (+ outbox spill/fold)

**Files:**
- Modify: `crates/dkod-core/src/store.rs`
- Modify: `crates/dkod-core/src/index.rs` (two small helpers)

This task changes the BODIES of `write_session`, `write_session_with_commit_links`,
`link_session_to_commit`, `link_session_to_patchid`, `relink_commit` — every
signature stays byte-identical. It adds new public functions
`write_session_full` and `relink_commits`. **Every existing test in
`store.rs`, `crates/dkod-cli/tests/`, and `cmd/capture/claude_code.rs` must
pass unmodified** — they assert legacy refs, which dual-write (default ON)
keeps writing.

- [ ] **Step 1: Write the failing tests.** Append to `mod tests` in `store.rs`:

```rust
fn index_root(repo_path: &std::path::Path) -> (gix::Repository, gix::ObjectId) {
    let repo = gix::open(repo_path).unwrap();
    let tip = crate::index::index_tip(&repo).expect("index ref must exist");
    let root = crate::index::commit_tree(&repo, tip).unwrap();
    (repo, root)
}

#[test]
fn write_session_also_writes_index_meta_and_body() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let s = fixture_session();
    write_session(tmp.path(), &s).unwrap();

    let (repo, root) = index_root(tmp.path());
    let body = crate::index::read_tree_blob(&repo, root, &crate::index::body_path(&s.id).unwrap())
        .expect("body.json present");
    let back: Session = serde_json::from_slice(&body).unwrap();
    assert_eq!(back, s, "body.json is the exact Session JSON");
    let meta = crate::index::read_tree_blob(&repo, root, &crate::index::meta_path(&s.id).unwrap())
        .expect("meta.json present");
    let meta: crate::SessionMeta = serde_json::from_slice(&meta).unwrap();
    assert_eq!(meta.id, s.id);
    assert_eq!(meta.body_bytes, body.len() as u64);
    // legacy ref still written (dual-write default ON)
    assert!(repo.find_reference(&crate::refs::session_ref(&s.id)).is_ok());
}

#[test]
fn write_session_with_legacy_disabled_writes_index_only() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    std::fs::create_dir_all(tmp.path().join(".dkod")).unwrap();
    std::fs::write(
        tmp.path().join(".dkod/config.toml"),
        "[storage]\nwrite_legacy_refs = false\n",
    )
    .unwrap();
    let s = fixture_session();
    write_session(tmp.path(), &s).unwrap();
    let repo = gix::open(tmp.path()).unwrap();
    assert!(
        repo.find_reference(&crate::refs::session_ref(&s.id)).is_err(),
        "no legacy ref when dual-write is off"
    );
    assert_eq!(read_session(tmp.path(), &s.id).unwrap(), s, "index read path serves it");
}

#[test]
fn write_session_full_batches_session_links_and_patchids_into_one_commit() {
    use gix::ObjectId;
    let tmp = TempDir::new().unwrap();
    let mut repo = gix::init(tmp.path()).unwrap();
    super::ensure_committer(&mut repo).unwrap();
    let sig = gix::actor::SignatureRef {
        name: "t".into(),
        email: "t@e.com".into(),
        time: gix::date::Time::now_utc(),
    };
    let tree: gix::ObjectId = repo.empty_tree().id().into();
    let a = repo.commit_as(sig, sig, "HEAD", "a", tree, Vec::<ObjectId>::new()).unwrap().detach();

    let mut s = fixture_session();
    let commits = vec![a.to_string()];
    let pid = "1111111111111111111111111111111111111111".to_string();
    let linked = write_session_full(
        tmp.path(),
        &mut s,
        &commits,
        &[(a.to_string(), pid.clone())],
    )
    .unwrap();
    assert_eq!(linked, vec![a.to_string()]);
    assert_eq!(s.commits, vec![a.to_string()]);

    let (repo, root) = index_root(tmp.path());
    // pointer blobs contain "<session-id>\n"
    let want = format!("{}\n", s.id).into_bytes();
    assert_eq!(
        crate::index::read_tree_blob(&repo, root, &crate::index::commit_pointer_path(&a.to_string())).unwrap(),
        want
    );
    assert_eq!(
        crate::index::read_tree_blob(&repo, root, &crate::index::patchid_pointer_path(&pid)).unwrap(),
        want
    );
    // exactly ONE index commit was written for the whole finalize (§5.1):
    let tip = crate::index::index_tip(&repo).unwrap();
    let c = repo.find_object(tip).unwrap().try_into_commit().unwrap();
    assert_eq!(c.parent_ids().count(), 0, "session+links land in a single root commit");
    // legacy link refs still written (dual-write ON)
    assert!(repo.find_reference(&crate::refs::commit_ref(&a.to_string())).is_ok());
    assert!(repo.find_reference(&crate::refs::patchid_ref(&pid)).is_ok());
}

#[test]
fn outbox_sessions_fold_into_next_write() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    // simulate a spilled session (apply_batch exhaustion, §5.3)
    let spilled = fixture_session();
    spill_to_outbox(tmp.path(), &spilled).unwrap();
    let outbox = tmp.path().join(".git/dkod/outbox").join(format!("{}.json", spilled.id));
    assert!(outbox.exists());

    let next = {
        let mut s = fixture_session();
        s.id = Session::new_id();
        s
    };
    write_session(tmp.path(), &next).unwrap();
    assert_eq!(read_session(tmp.path(), &spilled.id).unwrap(), spilled, "folded into the index");
    assert!(!outbox.exists(), "outbox entry consumed");
}

#[test]
fn relink_commits_batches_pairs_into_one_index_commit() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let s = fixture_session();
    write_session(tmp.path(), &s).unwrap();
    let old1 = "0000000000000000000000000000000000000001";
    let old2 = "0000000000000000000000000000000000000002";
    link_session_to_commit(tmp.path(), &s.id, old1).unwrap();
    link_session_to_commit(tmp.path(), &s.id, old2).unwrap();
    let repo = gix::open(tmp.path()).unwrap();
    let tip_before = crate::index::index_tip(&repo).unwrap();

    let new1 = "000000000000000000000000000000000000000a";
    let new2 = "000000000000000000000000000000000000000b";
    let n = relink_commits(
        tmp.path(),
        &[(old1.to_string(), new1.to_string()), (old2.to_string(), new2.to_string())],
    )
    .unwrap();
    assert_eq!(n, 2);
    let repo = gix::open(tmp.path()).unwrap();
    let tip_after = crate::index::index_tip(&repo).unwrap();
    let c = repo.find_object(tip_after).unwrap().try_into_commit().unwrap();
    let parents: Vec<_> = c.parent_ids().map(|p| p.detach()).collect();
    assert_eq!(parents, vec![tip_before], "all pairs in ONE new commit");
    // old paths kept, new paths added (undo-friendly, §5.2)
    let root = crate::index::commit_tree(&repo, tip_after).unwrap();
    for sha in [old1, old2, new1, new2] {
        assert!(
            crate::index::read_tree_blob(&repo, root, &crate::index::commit_pointer_path(sha)).is_some(),
            "pointer for {sha} missing"
        );
    }
    // legacy new refs written too (dual-write ON)
    assert!(repo.find_reference(&crate::refs::commit_ref(new1)).is_ok());
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-core -p dkod-cli` — new tests FAIL (missing `write_session_full`, `relink_commits`, `spill_to_outbox`, no index writes); ALL existing tests still PASS.

- [ ] **Step 3: Implement in `store.rs`.** Exact specification:

Add at the top: `use crate::index;` and `use crate::SessionMeta;`.

**New private helpers:**

```rust
/// (meta_path, meta_bytes) + (body_path, body_bytes) inserts for `session`,
/// pushed onto `batch`. Path date: v7 timestamp, else created_at (§10).
fn push_session_inserts(batch: &mut index::IndexBatch, session: &Session) -> Result<()> {
    let body = serde_json::to_vec(session).context("serialize session body")?;
    let meta = serde_json::to_vec(&SessionMeta::derive(session, body.len() as u64))
        .context("serialize session meta")?;
    let dir = index::session_dir_for(&session.id, session.created_at);
    batch.insert(format!("{dir}/meta.json"), meta);
    batch.insert(format!("{dir}/body.json"), body);
    Ok(())
}

/// Pointer blob contents: `<session-id>\n` (§4.4).
fn pointer_blob(session_id: &str) -> Vec<u8> {
    format!("{session_id}\n").into_bytes()
}

/// Spill a session that could not reach the index (CAS exhaustion) to
/// `.git/dkod/outbox/<id>.json` (§5.3). pub(crate) so tests can call it.
pub(crate) fn spill_to_outbox(repo_path: &Path, session: &Session) -> Result<()> {
    let repo = gix::open(repo_path).context("open repo")?;
    let dir = repo.path().join("dkod/outbox");
    std::fs::create_dir_all(&dir).context("create outbox dir")?;
    let bytes = serde_json::to_vec(session).context("serialize outbox session")?;
    std::fs::write(dir.join(format!("{}.json", session.id)), bytes).context("write outbox file")?;
    Ok(())
}

/// Fold every parseable `.git/dkod/outbox/*.json` into `batch` (meta + body +
/// commit pointers from `session.commits`; patch-ids are unknown for spilled
/// sessions and are skipped). Returns the file paths to delete after a
/// successful apply. Unparseable files are left in place and skipped.
fn fold_outbox(repo_path: &Path, batch: &mut index::IndexBatch) -> Vec<std::path::PathBuf> {
    let Ok(repo) = gix::open(repo_path) else { return Vec::new() };
    let dir = repo.path().join("dkod/outbox");
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut consumed = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(session) = serde_json::from_slice::<Session>(&bytes) else { continue };
        if push_session_inserts(batch, &session).is_err() {
            continue;
        }
        for sha in &session.commits {
            if sha.len() >= 3 {
                batch.insert(index::commit_pointer_path(sha), pointer_blob(&session.id));
            }
        }
        consumed.push(path);
    }
    consumed
}

/// Apply `batch` to the index; on retry exhaustion spill `session` to the
/// outbox and WARN instead of failing — a capture is never lost to
/// contention (§5.3). Deletes `consumed_outbox` files only on success.
fn apply_or_spill(
    repo_path: &Path,
    batch: &index::IndexBatch,
    session: &Session,
    consumed_outbox: &[std::path::PathBuf],
) {
    match index::apply_batch(repo_path, batch) {
        Ok(_) => {
            for p in consumed_outbox {
                let _ = std::fs::remove_file(p);
            }
        }
        Err(e) => {
            eprintln!("dkod: index write failed ({e:#}); session spilled to outbox");
            let _ = spill_to_outbox(repo_path, session);
        }
    }
}
```

**`write_session` (same signature):** keep the existing body verbatim as the
"legacy half", and restructure to:

1. `let storage = crate::config::load_storage_config(repo_path);`
2. Build `IndexBatch::new(format!("dkod: add session {} ({}, 0 commit(s))", session.id, crate::agent_label(&session.agent)))`; `push_session_inserts(&mut batch, session)?` (serialize failure stays fatal — same as today); `let consumed = fold_outbox(repo_path, &mut batch);` `apply_or_spill(repo_path, &batch, session, &consumed);`
3. `if storage.write_legacy_refs { /* existing blob+ref code, unchanged */ }`
4. `Ok(())`

**New `write_session_full`:**

```rust
/// The batched finalize entry point (§5.2/§16): ONE index commit containing
/// the session, all its commit pointers, and all its patch-id pointers.
/// `commits` is the discovered commit list (caller runs `new_commits_since`);
/// `patch_ids` is `(commit_sha, patch_id)` pairs (best-effort, may be empty).
/// Sets `session.commits = commits` BEFORE serializing so body, meta, and
/// pointers agree. Returns the linked shas. Legacy refs (session + commit
/// links + patchid links) are also written iff dual-write is on; legacy link
/// failures are skipped per-entry (today's best-effort policy), and only the
/// session write itself is fatal.
pub fn write_session_full(
    repo_path: &Path,
    session: &mut Session,
    commits: &[String],
    patch_ids: &[(String, String)],
) -> Result<Vec<String>>
```

Body specification:

1. `session.commits = commits.to_vec();`
2. `let storage = crate::config::load_storage_config(repo_path);`
3. Build one batch, message `format!("dkod: add session {} ({}, {} commit(s))", session.id, crate::agent_label(&session.agent), commits.len())`; `push_session_inserts` (fatal on serialize error); for each `sha` in `commits` with `sha.len() >= 3`: `batch.insert(index::commit_pointer_path(sha), pointer_blob(&session.id))`; for each `(sha, pid)` in `patch_ids` with `pid.len() >= 3` and `commits.contains(sha)`: `batch.insert(index::patchid_pointer_path(pid), pointer_blob(&session.id))`; fold outbox; `apply_or_spill`.
4. If `storage.write_legacy_refs`: call the legacy half of `write_session` logic — write the session blob + session ref (fatal on failure, preserving today's "session write is the only fatal step"); then per-commit `link_session_to_commit_legacy_only` and per-pair `link_session_to_patchid` legacy writes, each best-effort (`let _ =`). To avoid double index writes, factor the CURRENT bodies of `write_session` / `link_session_to_commit` / `link_session_to_patchid` into private `fn write_session_legacy(repo_path, session) -> Result<()>`, `fn link_commit_legacy(repo_path, session_id, sha) -> Result<()>`, `fn link_patchid_legacy(repo_path, session_id, pid) -> Result<()>` and call those from both the public single-shot functions and `write_session_full`.
5. `Ok(commits.to_vec())`.

**`write_session_with_commit_links` (same signature)** becomes:

```rust
pub fn write_session_with_commit_links(
    repo_path: &Path,
    session: &mut Session,
    head_at_start: Option<&str>,
) -> Result<Vec<String>> {
    let commits = new_commits_since(repo_path, head_at_start).unwrap_or_default();
    write_session_full(repo_path, session, &commits, &[])
}
```

**`link_session_to_commit` / `link_session_to_patchid` (same signatures):**

1. index half: single-pointer `IndexBatch` (messages `dkod: link session <id> to commit <sha>` / `… to patch-id <pid>`), `apply_batch` — on error, WARN only (link refs are best-effort today).
2. legacy half iff `write_legacy_refs`: the factored `link_*_legacy` body (which still errors when the legacy session ref is absent — preserving today's contract and tests).
3. When dual-write is OFF, step 2 is replaced by an existence check: `read_session(repo_path, &session_id)?;` so linking a never-written session still errors.

**`relink_commit` (same signature)** delegates: `Ok(relink_commits(repo_path, &[(old_sha.to_string(), new_sha.to_string())])? == 1)`.

**New `relink_commits`:**

```rust
/// Batched post-rewrite relink (§5.2): resolve every `(old, new)` pair —
/// index pointer at `commits/<old>` first, legacy `refs/dkod/commits/<old>`
/// second — and write all new pointers as ONE index commit
/// (`dkod: relink <n> commit(s) after history rewrite`). Old paths/refs are
/// kept (additive). Legacy new refs are written iff dual-write is on AND the
/// pair resolved via a legacy ref or the legacy session ref exists. Returns
/// how many pairs resolved. Pairs that resolve nowhere are skipped.
pub fn relink_commits(repo_path: &Path, pairs: &[(String, String)]) -> Result<usize>
```

Body: open repo; for each pair: try `index::index_tip` + `index::read_tree_blob(repo, root, &commit_pointer_path(old))` → session id (trim trailing newline); else `try_find_reference(commit_ref(old))` → blob → parse `Session` → id. Collect `(new_sha, session_id, legacy_blob: Option<ObjectId>)`. Build one batch of `commit_pointer_path(new) → pointer_blob(id)` inserts; apply via `apply_batch` (errors propagate — relink is not on the capture hot path, and `cmd::relink` already swallows per-call errors). Then iff dual-write: for each pair that had a legacy old ref, `write_link_ref(repo, commit_ref(new), legacy_blob, message)` best-effort. Return resolved count.

- [ ] **Step 4: Run green.** `cargo test -p dkod-core && cargo test -p dkod-cli` — every pre-existing test passes unmodified; the 6 new tests pass.
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-core/src/store.rs`, `crates/dkod-core/src/index.rs`.
  Message: `feat(store): dual-write index+legacy with batched finalize entry point and outbox spill`

### Task 1.7: index-first read path + blame/relink rewire

**Files:**
- Modify: `crates/dkod-core/src/store.rs`
- Modify: `crates/dkod-core/src/index.rs` (scan helper)
- Modify: `crates/dkod-cli/src/cmd/blame.rs`
- Modify: `crates/dkod-cli/src/cmd/relink.rs`

- [ ] **Step 1: Write the failing tests.** In `store.rs` tests:

```rust
#[test]
fn read_session_falls_back_to_legacy_ref_when_index_lacks_it() {
    // plumbing-style legacy-only repo: blob + session ref, NO index commit —
    // exactly what benchmarks/drift/run.sh produces.
    let tmp = TempDir::new().unwrap();
    let repo = gix::init(tmp.path()).unwrap();
    let s = fixture_session();
    let bytes = serde_json::to_vec(&s).unwrap();
    let blob = repo.write_blob(&bytes).unwrap().detach();
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    use gix::refs::Target;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "seed".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(blob),
        },
        name: crate::refs::session_ref(&s.id).try_into().unwrap(),
        deref: false,
    })
    .unwrap();

    assert!(crate::index::index_tip(&repo).is_none(), "no index ref in this fixture");
    assert_eq!(read_session(tmp.path(), &s.id).unwrap(), s);
    assert_eq!(list_sessions(tmp.path()).unwrap(), vec![s.id.clone()]);
}

#[test]
fn read_session_finds_non_v7_id_via_date_scan() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    std::fs::create_dir_all(tmp.path().join(".dkod")).unwrap();
    std::fs::write(
        tmp.path().join(".dkod/config.toml"),
        "[storage]\nwrite_legacy_refs = false\n", // index-only: forces the scan path
    )
    .unwrap();
    let mut s = fixture_session();
    s.id = "foreign-import-001".into();
    s.created_at = 1735689600;
    write_session(tmp.path(), &s).unwrap();
    assert_eq!(read_session(tmp.path(), &s.id).unwrap(), s);
    assert_eq!(list_sessions(tmp.path()).unwrap(), vec![s.id.clone()]);
}

#[test]
fn list_sessions_unions_index_and_legacy_dedup_by_id() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let s = fixture_session(); // dual-write ON → in BOTH index and legacy refs
    write_session(tmp.path(), &s).unwrap();
    assert_eq!(list_sessions(tmp.path()).unwrap(), vec![s.id.clone()], "no duplicate");
}

#[test]
fn lookup_commit_session_resolves_via_index_pointer() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let mut s = fixture_session();
    let sha = "00000000000000000000000000000000000000aa".to_string();
    write_session_full(tmp.path(), &mut s, &[sha.clone()], &[]).unwrap();
    let meta = lookup_commit_session(tmp.path(), &sha).expect("resolved");
    assert_eq!(meta.id, s.id);
    assert_eq!(meta.prompt_summary, s.prompt_summary);
}

#[test]
fn lookup_commit_session_falls_back_to_legacy_ref() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let s = fixture_session();
    // legacy-only seeding (write_session_legacy path): use the public fns,
    // then delete the index ref to simulate a legacy-only repo.
    write_session(tmp.path(), &s).unwrap();
    link_session_to_commit(tmp.path(), &s.id, "00000000000000000000000000000000000000bb").unwrap();
    let repo = gix::open(tmp.path()).unwrap();
    repo.find_reference(crate::index::INDEX_REF).unwrap().delete().unwrap();
    let meta = lookup_commit_session(tmp.path(), "00000000000000000000000000000000000000bb")
        .expect("legacy fallback");
    assert_eq!(meta.id, s.id);
}

#[test]
fn lookup_patchid_session_resolves_index_then_legacy() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let mut s = fixture_session();
    let sha = "00000000000000000000000000000000000000cc".to_string();
    let pid = "2222222222222222222222222222222222222222".to_string();
    write_session_full(tmp.path(), &mut s, &[sha.clone()], &[(sha, pid.clone())]).unwrap();
    assert_eq!(lookup_patchid_session(tmp.path(), &pid).unwrap().id, s.id);
    let repo = gix::open(tmp.path()).unwrap();
    repo.find_reference(crate::index::INDEX_REF).unwrap().delete().unwrap();
    assert_eq!(lookup_patchid_session(tmp.path(), &pid).unwrap().id, s.id, "legacy fallback");
}

#[test]
fn read_session_meta_prefers_index_and_derives_from_legacy() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let s = fixture_session();
    write_session(tmp.path(), &s).unwrap();
    let m = read_session_meta(tmp.path(), &s.id).unwrap();
    assert_eq!(m.id, s.id);
    assert!(m.body_bytes > 0);
    let repo = gix::open(tmp.path()).unwrap();
    repo.find_reference(crate::index::INDEX_REF).unwrap().delete().unwrap();
    let m2 = read_session_meta(tmp.path(), &s.id).unwrap();
    assert_eq!(m2.id, s.id, "derived from the legacy blob");
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-core` — new tests FAIL (missing fns / index-only read path).

- [ ] **Step 3: Implement.**

In `index.rs`, add the non-v7 scan helper (§6.2) and the id walk:

```rust
/// All session ids in the index (sorted date-dir walk → chronological for v7
/// ids). Empty when there is no index ref.
pub fn list_index_session_ids(repo: &gix::Repository) -> Vec<String> {
    let Some(tip) = index_tip(repo) else { return Vec::new() };
    let Ok(root) = commit_tree(repo, tip) else { return Vec::new() };
    let mut ids = Vec::new();
    for date in list_tree_dir(repo, root, "sessions").unwrap_or_default() {
        for id in list_tree_dir(repo, root, &format!("sessions/{date}")).unwrap_or_default() {
            ids.push(id);
        }
    }
    ids
}

/// Locate a session dir by scanning every date directory — the non-v7-id
/// fallback (§6.2). O(date-dirs); only hit for foreign/hand-rolled ids.
pub(crate) fn find_session_dir_by_scan(
    repo: &gix::Repository,
    root: gix::ObjectId,
    id: &str,
) -> Option<String> {
    for date in list_tree_dir(repo, root, "sessions").ok()? {
        let dir = format!("sessions/{date}/{id}");
        if blob_oid_at(repo, root, &format!("{dir}/body.json")).is_some() {
            return Some(dir);
        }
    }
    None
}
```

In `store.rs`:

**`read_session` (same signature):** open repo; if `index::index_tip` is
`Some(tip)` with `root = index::commit_tree(...)` Ok:
try `index::body_path(id)` → `read_tree_blob`; on miss (or non-v7 id) try
`find_session_dir_by_scan(repo, root, id)` → `read_tree_blob("{dir}/body.json")`.
Any hit → `serde_json::from_slice` (a parse failure here is a real error, not
a fallthrough). No hit → the EXISTING legacy body
(`find_reference(session_ref(id))` → blob → parse), unchanged.

**`list_sessions` (same signature):** `BTreeSet<String>` seeded with
`index::list_index_session_ids(&repo)`, then extend with the EXISTING legacy
ref iteration; return `Ok(set.into_iter().collect())` (sorted; the existing
test sorts both sides, so this is compatible).

**New read functions:**

```rust
/// Session header for `id`: `meta.json` at the index tip, else derived from
/// the legacy session blob (§6.2). Errors only when the session exists
/// nowhere.
pub fn read_session_meta(repo_path: &Path, id: &str) -> Result<SessionMeta>

/// Blame primary lookup (§6.1): `commits/<fanout(sha)>` pointer → meta;
/// legacy `refs/dkod/commits/<sha>` → derived meta. None = human commit.
pub fn lookup_commit_session(repo_path: &Path, sha: &str) -> Option<SessionMeta>

/// Blame patch-id fallback: `patchid/<fanout(pid)>` pointer → meta; legacy
/// `refs/dkod/patchid/<pid>` → derived meta.
pub fn lookup_patchid_session(repo_path: &Path, pid: &str) -> Option<SessionMeta>
```

Shared private helpers:

```rust
/// meta for a session id via index (meta.json direct, then scan) or legacy
/// blob (parse Session → SessionMeta::derive(s, blob_len)).
fn meta_for_id(repo: &gix::Repository, id: &str) -> Option<SessionMeta>

/// Resolve an index pointer path to the session id it names.
fn pointer_session_id(repo: &gix::Repository, path: &str) -> Option<String> {
    let tip = index::index_tip(repo)?;
    let root = index::commit_tree(repo, tip).ok()?;
    let bytes = index::read_tree_blob(repo, root, path)?;
    Some(String::from_utf8_lossy(&bytes).trim_end().to_string())
}
```

`lookup_commit_session`: `pointer_session_id(commit_pointer_path(sha))` →
`meta_for_id`; else legacy `find_reference(commit_ref(sha))` → blob → `Session`
→ `SessionMeta::derive`. Guard `sha.len() >= 3` before fanning out.
`lookup_patchid_session`: same shape with `patchid_pointer_path`/`patchid_ref`.

In `blame.rs`: DELETE `session_from_ref_name` and replace `session_for_commit` with:

```rust
/// Resolve a blamed commit SHA to its annotation tuple. Index pointer first,
/// legacy ref second (handled inside dkod_core::store); patch-id fallback
/// unchanged in spirit (§6.1). Metadata-only — never loads body.json, which
/// keeps blame fast under lazy propagation (Phase 3).
fn session_for_commit(cwd: &Path, sha: &str) -> Option<(String, String, String)> {
    let to_tuple = |m: dkod_core::SessionMeta| {
        let short = m.id.get(..8).unwrap_or(&m.id).to_string();
        (dkod_core::agent_label(&m.agent).to_string(), short, m.prompt_summary)
    };
    if let Some(m) = dkod_core::store::lookup_commit_session(cwd, sha) {
        return Some(to_tuple(m));
    }
    let pid = crate::cmd::patchid::compute_patch_id(cwd, sha)?;
    dkod_core::store::lookup_patchid_session(cwd, &pid).map(to_tuple)
}
```

(The per-sha `HashMap` cache in `run` already prevents repeated lookups per
line; do not change `run`.)

In `relink.rs`: replace the per-pair loop in `run` with the batched call,
keeping the always-exit-0 contract:

```rust
pub fn run(cwd: &Path) -> Result<()> {
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf); // best-effort
    let pairs = parse_pairs(&buf);
    if pairs.is_empty() {
        return Ok(());
    }
    match dkod_core::store::relink_commits(cwd, &pairs) {
        Ok(n) if n > 0 => {
            eprintln!("dkod: re-linked {n} session commit-ref(s) after history rewrite")
        }
        Ok(_) => {}
        Err(_) => {} // never break the user's rebase
    }
    Ok(())
}
```

- [ ] **Step 4: Run green.** `cargo test -p dkod-core && cargo test -p dkod-cli` — all pre-existing tests (including `e2e_blame`, `e2e_patchid_blame`, `e2e_relink`) pass unmodified; the 7 new tests pass.
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-core/src/store.rs`, `crates/dkod-core/src/index.rs`, `crates/dkod-cli/src/cmd/blame.rs`, `crates/dkod-cli/src/cmd/relink.rs`.
  Message: `feat(store): index-first read fallback chain; blame + relink ride the index`

### Task 1.8: `finalize_session` moves to the batched entry point

**Files:**
- Modify: `crates/dkod-cli/src/cmd/capture/mod.rs`
- Test: `crates/dkod-cli/tests/finalize_session.rs` (append only)

- [ ] **Step 1: Write the failing test.** Append to `tests/finalize_session.rs` (reuse its existing `git`/`head`/`fixture_session` helpers):

```rust
#[test]
fn finalize_writes_one_index_commit_covering_session_links_and_patchids() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "A"]);
    let a = head(repo.path());
    std::fs::write(repo.path().join("f.txt"), "a\nb\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "B"]);
    let b = head(repo.path());

    let cfg = dkod_core::config::Config::default();
    let mut session = fixture_session("one batched commit");
    finalize_session(repo.path(), &mut session, Some(&a), &cfg).unwrap();

    // exactly ONE commit on refs/dkod/index (root, no parent)
    let gx = gix::open(repo.path()).unwrap();
    let tip = gx
        .try_find_reference("refs/dkod/index")
        .unwrap()
        .expect("index ref")
        .id()
        .detach();
    let c = gx.find_object(tip).unwrap().try_into_commit().unwrap();
    assert_eq!(c.parent_ids().count(), 0, "session + commit link + patchid link batched");
    let msg = c.message_raw().unwrap().to_string();
    assert!(msg.contains("1 commit(s)"), "message: {msg}");

    // legacy patchid ref still exists (dual-write ON, finalize parity)
    let out = std::process::Command::new("git")
        .args(["for-each-ref", "refs/dkod/patchid/"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "legacy patchid link missing"
    );
    let _ = b;
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-cli --test finalize_session` — the new test FAILS (today finalize produces one index commit for session+commit links via `write_session_with_commit_links` plus a SECOND one per patchid link).

- [ ] **Step 3: Implement.** Replace `finalize_session`'s body in `cmd/capture/mod.rs`:

```rust
pub fn finalize_session(
    cwd: &std::path::Path,
    session: &mut dkod_core::Session,
    head_at_start: Option<&str>,
    cfg: &dkod_core::config::Config,
) -> Result<Vec<String>> {
    dkod_core::redact::redact_session(session, &cfg.redact);
    // Discover commits FIRST so patch-ids can land in the same index commit
    // as the session (storage-v2 §5.2/§16). Discovery is best-effort.
    let commits = dkod_core::store::new_commits_since(cwd, head_at_start).unwrap_or_default();
    let patch_ids: Vec<(String, String)> = commits
        .iter()
        .filter_map(|sha| {
            crate::cmd::patchid::compute_patch_id(cwd, sha).map(|pid| (sha.clone(), pid))
        })
        .collect();
    let linked = dkod_core::store::write_session_full(cwd, session, &commits, &patch_ids)
        .context("write session")?;
    Ok(linked)
}
```

`claude_code.rs::handle_finished_session` stays byte-identical (Background
facts): it keeps calling `write_session_with_commit_links`, which now writes
one index commit for session+commit links and no patch-id links — exactly its
v1 behavior surface.

- [ ] **Step 4: Run green.** `cargo test -p dkod-cli` — the two pre-existing finalize tests pass unmodified; `finalize_patchid.rs` passes unmodified (legacy patchid refs still written by the dual-write half).
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-cli/src/cmd/capture/mod.rs`, `crates/dkod-cli/tests/finalize_session.rs`.
  Message: `feat(capture): finalize batches session, commit links, and patch-ids into one index commit`

### Task 1.9: `dkod reindex`

**Files:**
- Create: `crates/dkod-cli/src/cmd/reindex.rs`
- Modify: `crates/dkod-cli/src/cmd/mod.rs` (add `pub mod reindex;` after `pub mod relink;`)
- Modify: `crates/dkod-cli/src/main.rs` (add the subcommand)
- Modify: `crates/dkod-core/src/store.rs` (the core fold function)
- Test: `crates/dkod-cli/tests/e2e_reindex.rs` (new)

- [ ] **Step 1: Write the failing core test.** In `store.rs` tests:

```rust
#[test]
fn reindex_legacy_refs_folds_sessions_and_links_idempotently() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    // Pure-legacy repo: disable index writes is not possible via public API,
    // so seed via the legacy halves: write_session + links, then delete the
    // index ref so only legacy refs remain.
    let s = fixture_session();
    write_session(tmp.path(), &s).unwrap();
    let sha = "00000000000000000000000000000000000000dd";
    let pid = "3333333333333333333333333333333333333333";
    link_session_to_commit(tmp.path(), &s.id, sha).unwrap();
    link_session_to_patchid(tmp.path(), &s.id, pid).unwrap();
    let repo = gix::open(tmp.path()).unwrap();
    repo.find_reference(crate::index::INDEX_REF).unwrap().delete().unwrap();

    let report = reindex_legacy_refs(tmp.path(), false).unwrap();
    assert_eq!(report.sessions, 1);
    assert_eq!(report.commit_links, 1);
    assert_eq!(report.patchid_links, 1);
    assert!(!report.dry_run_only);

    // session readable from the index alone
    let repo = gix::open(tmp.path()).unwrap();
    let tip = crate::index::index_tip(&repo).expect("index rebuilt");
    let root = crate::index::commit_tree(&repo, tip).unwrap();
    // body bytes verbatim → same content as the legacy blob (§10.2)
    let body = crate::index::read_tree_blob(&repo, root, &crate::index::body_path(&s.id).unwrap()).unwrap();
    assert_eq!(serde_json::from_slice::<Session>(&body).unwrap(), s);

    // idempotent: second run writes nothing
    let again = reindex_legacy_refs(tmp.path(), false).unwrap();
    assert_eq!(again.sessions, 0, "already indexed → no new inserts");
    assert_eq!(crate::index::index_tip(&gix::open(tmp.path()).unwrap()), Some(tip), "no empty commit");
}

#[test]
fn reindex_dry_run_reports_without_writing() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let s = fixture_session();
    write_session(tmp.path(), &s).unwrap();
    let repo = gix::open(tmp.path()).unwrap();
    repo.find_reference(crate::index::INDEX_REF).unwrap().delete().unwrap();

    let report = reindex_legacy_refs(tmp.path(), true).unwrap();
    assert_eq!(report.sessions, 1);
    assert!(report.dry_run_only);
    assert!(crate::index::index_tip(&gix::open(tmp.path()).unwrap()).is_none(), "dry run wrote nothing");
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-core reindex` — FAIL.

- [ ] **Step 3: Implement the core.** In `store.rs`:

```rust
/// Outcome of folding legacy refs into the index (§10).
#[derive(Debug)]
pub struct ReindexReport {
    pub sessions: usize,
    pub commit_links: usize,
    pub patchid_links: usize,
    pub dry_run_only: bool,
}

/// Fold every local legacy ref (`refs/dkod/{sessions,commits,patchid}/*`) and
/// outbox spill into the index as ONE batched commit
/// (`dkod: reindex <n> legacy session(s)`). Body bytes are copied verbatim
/// (same blob oid — no new storage, §10.2); link refs become pointer blobs.
/// Idempotent: already-present paths are skipped at the oid level, and a
/// fully-indexed repo produces no commit. Counts report what was MISSING
/// from the index before the run.
pub fn reindex_legacy_refs(repo_path: &Path, dry_run: bool) -> Result<ReindexReport>
```

Body specification:

1. Open repo; snapshot `tip = index::index_tip(&repo)` and `root` (if any).
2. Enumerate `refs/dkod/sessions/*` (existing `references().prefixed(...)`
   pattern): for each, blob bytes → parse `Session` (unparseable → warn +
   skip); skip if `root` already has `body.json` for the id **with the same
   blob oid** (`index::blob_oid_at` vs the legacy ref's target oid); otherwise
   push meta+body inserts (`push_session_inserts`) and count.
3. Enumerate `refs/dkod/commits/*`: ref name suffix = sha; target blob →
   `Session` id (cache parses by blob oid in a `HashMap<ObjectId, String>`);
   skip if the pointer path already resolves; else insert pointer blob, count.
   Same for `refs/dkod/patchid/*`.
4. Fold the outbox (`fold_outbox`) into the same batch.
5. `dry_run` → return the report with `dry_run_only: true` (no apply).
6. Apply (`index::apply_batch`, message
   `format!("dkod: reindex {} legacy session(s)", sessions)`); propagate
   errors (reindex is interactive, not capture-hot-path).
7. Write `format = "v2"` into `.dkod/config.toml`: load the file (or
   `Config::default()`), set `cfg.storage.format = Some("v2".into())`,
   `toml::to_string_pretty`, write back. Best-effort (warn on failure).

- [ ] **Step 4: CLI command.** `cmd/reindex.rs`:

```rust
//! `dkod reindex` — fold legacy per-session refs into the rollup index
//! (storage-v2 §10). Idempotent; safe to re-run; `--delete-legacy` arrives in
//! Phase 4.

use anyhow::{anyhow, Result};
use std::path::Path;

pub fn run(cwd: &Path, dry_run: bool) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;
    let r = dkod_core::store::reindex_legacy_refs(cwd, dry_run)?;
    let verb = if r.dry_run_only { "would fold" } else { "folded" };
    println!(
        "dkod reindex: {verb} {} session(s), {} commit link(s), {} patch-id link(s) into refs/dkod/index",
        r.sessions, r.commit_links, r.patchid_links
    );
    if !r.dry_run_only {
        println!("dkod reindex: push it with `git push origin refs/dkod/index` (dkod push arrives in a later release)");
    }
    Ok(())
}
```

`main.rs`: add to `Cmd` (visible, near `Log`):

```rust
    /// Fold legacy per-session refs (refs/dkod/sessions|commits|patchid/*)
    /// into the rollup index ref (refs/dkod/index). Idempotent.
    Reindex {
        /// Report what would be folded without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
```

dispatch: `Cmd::Reindex { dry_run } => cmd::reindex::run(&std::env::current_dir()?, dry_run),`.
Do NOT add `Reindex` to `maybe_warn_drift`'s exclusion list (it is a normal
interactive command).

- [ ] **Step 5: Write the e2e test.** `tests/e2e_reindex.rs` (with the standard `git` helper from Background facts):

```rust
//! End-to-end: `dkod reindex` folds plumbing-seeded legacy refs (the exact
//! pattern benchmarks/drift/run.sh uses) into the index, after which the
//! session reads back even if the legacy ref is deleted.

#[test]
fn reindex_folds_plumbing_seeded_legacy_session() {
    // 1. git init; seed a session via `git hash-object -w --stdin` +
    //    `git update-ref refs/dkod/sessions/<id> <blob>` (NO dkod writer).
    // 2. cargo_bin("dkod") reindex → assert stdout contains "folded 1 session(s)".
    // 3. `git update-ref -d refs/dkod/sessions/<id>` (delete the legacy ref).
    // 4. `dkod show <id>` succeeds and prints the prompt summary — proving the
    //    index alone serves it.
    // 5. run `dkod reindex` again → stdout contains "folded 0 session(s)".
}
```

Write the full body following the comments: build the session JSON with a
`dkod_core::Session` literal serialized via `serde_json::to_string`, pipe it
to `git hash-object -w --stdin` with `std::process::Command` + `Stdio::piped`,
and use `assert_cmd::Command::cargo_bin("dkod")` for steps 2/4/5.

- [ ] **Step 6: Run green.** `cargo test -p dkod-core reindex && cargo test -p dkod-cli --test e2e_reindex`, then the full suite.
- [ ] **Step 7: Gate & commit ritual.** Files: `crates/dkod-core/src/store.rs`, `crates/dkod-cli/src/cmd/reindex.rs`, `crates/dkod-cli/src/cmd/mod.rs`, `crates/dkod-cli/src/main.rs`, `crates/dkod-cli/tests/e2e_reindex.rs`.
  Message: `feat(reindex): dkod reindex folds legacy refs into the rollup index (idempotent)`

### Task 1.10: legacy-fallback regression e2e (benchmark guard) + phase close-out

**Files:**
- Test: `crates/dkod-cli/tests/e2e_legacy_fallback.rs` (new)

- [ ] **Step 1: Write the test** (this is the guard that keeps
  `benchmarks/drift/run.sh` working forever — it replicates run.sh's seeding
  byte-for-byte and exercises `log`, `show`, `drift`, and `blame` against a
  repo that has **no index ref at all**):

```rust
//! Permanent-compat guard (storage-v2 §6.2): a repo whose sessions were
//! seeded with raw git plumbing against legacy refs — exactly what
//! benchmarks/drift/run.sh does — must keep working with NO refs/dkod/index
//! present, in every storage-v2 phase, forever.

use assert_cmd::Command as AssertCommand;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.com")
        .output()
        .expect("git");
    assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
}

/// `git hash-object -w --stdin` — run.sh line for line.
fn hash_object(repo: &Path, bytes: &[u8]) -> String {
    let mut child = Command::new("git")
        .args(["hash-object", "-w", "--stdin"])
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn plumbing_seeded_legacy_repo_works_without_index_ref() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);

    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 1735689600,
        duration_ms: 0,
        prompt_summary: "benchmark-style legacy session".into(),
        messages: vec![dkod_core::Message::user("benchmark-style legacy session")],
        commits: vec![],
        files_touched: vec!["src/lib.rs".into()],
        redaction_count: 0,
    };
    let blob = hash_object(repo.path(), &serde_json::to_vec(&s).unwrap());
    git(repo.path(), &["update-ref", &format!("refs/dkod/sessions/{}", s.id), &blob]);

    // No index ref may exist.
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/dkod/index"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&refs.stdout).trim().is_empty());

    for args in [vec!["log"], vec!["show", s.id.as_str()], vec!["drift", s.id.as_str()]] {
        let out = AssertCommand::cargo_bin("dkod")
            .unwrap()
            .current_dir(repo.path())
            .args(&args)
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert!(
            stdout.contains(&s.id) || stdout.contains("benchmark-style"),
            "dkod {args:?} lost the legacy session:\n{stdout}"
        );
    }

    // And reading must NOT have created an index ref as a side effect.
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/dkod/index"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&refs.stdout).trim().is_empty(),
        "read path must never write the index"
    );
}
```

- [ ] **Step 2: Run.** `cargo test -p dkod-cli --test e2e_legacy_fallback` — PASS (it should already pass given Tasks 1.6–1.7; if not, the read path has a bug — fix the read path, never the test).
- [ ] **Step 3: Run the real benchmark once** as a manual sanity check:
  `benchmarks/drift/run.sh` — expected: same confusion matrix as on `main`
  (the binary changed; the legacy read path must be bit-compatible).
- [ ] **Step 4: Gate & commit ritual.** Files: `crates/dkod-cli/tests/e2e_legacy_fallback.rs`.
  Message: `test(store): permanent legacy-fallback guard (drift-benchmark plumbing pattern)`
- [ ] **Step 5: Phase close-out.** Full gates; `/coderabbit:review --base main`;
  `gh auth switch --user haim-ari`; push `feat/storage-v2-phase1`; PR titled
  `feat: storage v2 phase 1 — rollup index, dual-write, read fallback, reindex`;
  body: what landed, the dual-write-ON guarantee, the §6.2 permanent fallback,
  zero existing-test modifications. Drive CodeRabbit + CI to green; squash-merge;
  sync local main.

---

## Phase 2 — capture size budget + auto-push/reconcile (§15.2)

Branch `feat/storage-v2-phase2` off updated `main`.

### Task 2.1: `[capture]` config section + `flate2` dependency

**Files:**
- Modify: `Cargo.toml` (workspace: add `flate2 = "1"`)
- Modify: `crates/dkod-core/Cargo.toml` (add `flate2.workspace = true`)
- Modify: `crates/dkod-core/src/config.rs`

- [ ] **Step 1: Write the failing tests** (config.rs tests):

```rust
#[test]
fn defaults_capture_budget_256kib_full_fidelity_off() {
    let c: Config = toml::from_str("").unwrap();
    assert_eq!(c.capture.session_budget_kib, 256);
    assert!(!c.capture.full_fidelity);
}

#[test]
fn capture_section_overrides_parse() {
    let toml = r#"
        [capture]
        session_budget_kib = 0
        full_fidelity = true
    "#;
    let c: Config = toml::from_str(toml).unwrap();
    assert_eq!(c.capture.session_budget_kib, 0);
    assert!(c.capture.full_fidelity);
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-core capture_` — FAIL.
- [ ] **Step 3: Implement.** Add `pub capture: CaptureConfig` to `Config` and:

```rust
/// Capture-time size budget (storage-v2 §7). Enforced after redaction,
/// before write; `session_budget_kib = 0` or `full_fidelity = true` disables
/// truncation entirely.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    /// Target compressed (deflate) session size in KiB; 0 = unlimited.
    pub session_budget_kib: u64,
    /// Never truncate, regardless of size (the full-fidelity opt-out).
    pub full_fidelity: bool,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self { session_budget_kib: 256, full_fidelity: false }
    }
}
```

Workspace `Cargo.toml`: add `flate2 = "1"` under `[workspace.dependencies]`
(flate2 is already compiled into the tree via gix's zlib backend — no new
supply-chain surface). `crates/dkod-core/Cargo.toml`: `flate2.workspace = true`.

- [ ] **Step 4: Run green**, then **Gate & commit ritual.** Files: `Cargo.toml`, `Cargo.lock`, `crates/dkod-core/Cargo.toml`, `crates/dkod-core/src/config.rs`.
  Message: `feat(config): [capture] size-budget section + flate2 dependency`

### Task 2.2: the budget module (redact-then-truncate pipeline, §7)

**Files:**
- Create: `crates/dkod-core/src/budget.rs`
- Modify: `crates/dkod-core/src/lib.rs` (add `pub mod budget;` after `pub mod`… alphabetical: between `capture` and `config`)

- [ ] **Step 1: Write the failing tests.** Create `budget.rs` with this test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CaptureConfig;
    use crate::{Agent, Message, Session, TRUNCATION_MARKER_PREFIX};

    fn session_with_tool_output(output: String) -> Session {
        Session {
            id: "0192f8e2-7b3a-7000-8a3e-000000000001".into(),
            agent: Agent::Codex,
            created_at: 0,
            duration_ms: 0,
            prompt_summary: "ok".into(),
            messages: vec![
                Message::user("run the build"),
                Message::tool("bash", serde_json::json!({"cmd": "make"}), output),
            ],
            commits: vec![],
            files_touched: vec![],
            redaction_count: 0,
        }
    }

    /// Incompressible random-ish text (so the deflate size tracks length).
    fn noisy(len: usize) -> String {
        let mut x: u32 = 0x9E3779B9;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                char::from(b'!' + (x % 90) as u8)
            })
            .collect()
    }

    #[test]
    fn under_budget_session_is_untouched() {
        let mut s = session_with_tool_output("small output".into());
        let before = s.clone();
        let out = enforce_budget(&mut s, &CaptureConfig::default());
        assert!(!out.truncated);
        assert!(!out.over_budget);
        assert_eq!(s, before);
    }

    #[test]
    fn over_budget_tool_output_gets_head_tail_truncated_with_marker() {
        // 2 MiB of noise compresses to far more than 256 KiB.
        let mut s = session_with_tool_output(noisy(2 * 1024 * 1024));
        let out = enforce_budget(&mut s, &CaptureConfig::default());
        assert!(out.truncated);
        let Message::Tool { output, .. } = &s.messages[1] else { panic!("tool msg") };
        assert!(output.contains(TRUNCATION_MARKER_PREFIX), "marker spliced in");
        assert!(output.len() < 64 * 1024, "output capped (got {})", output.len());
        assert!(out.compressed_bytes <= 256 * 1024, "post-truncation size respects the budget");
    }

    #[test]
    fn user_and_assistant_text_is_never_truncated() {
        let big = noisy(2 * 1024 * 1024);
        let mut s = session_with_tool_output("tiny".into());
        s.messages.push(Message::user(big.clone()));
        s.messages.push(Message::assistant(big.clone()));
        s.messages.push(Message::reasoning(big.clone()));
        let out = enforce_budget(&mut s, &CaptureConfig::default());
        assert!(out.over_budget, "enormous non-tool text → write anyway (§7 step 4)");
        assert!(!out.truncated, "nothing to truncate: tool output was tiny");
        assert!(matches!(&s.messages[2], Message::User { content } if content == &big));
        assert!(matches!(&s.messages[3], Message::Assistant { content } if content == &big));
        assert!(matches!(&s.messages[4], Message::Reasoning { content } if content == &big));
    }

    #[test]
    fn full_fidelity_and_zero_budget_disable_truncation() {
        for cfg in [
            CaptureConfig { session_budget_kib: 0, full_fidelity: false },
            CaptureConfig { session_budget_kib: 256, full_fidelity: true },
        ] {
            let mut s = session_with_tool_output(noisy(2 * 1024 * 1024));
            let before = s.clone();
            let out = enforce_budget(&mut s, &cfg);
            assert!(!out.truncated);
            assert_eq!(s, before);
            let _ = out;
        }
    }

    #[test]
    fn truncate_output_keeps_head_and_tail_on_char_boundaries() {
        let text = format!("HEAD{}TAIL", "é".repeat(10_000)); // multibyte middle
        let t = truncate_output(&text, 8, 8).expect("over the cap");
        assert!(t.starts_with("HEAD"));
        assert!(t.ends_with("TAIL"));
        assert!(t.contains(TRUNCATION_MARKER_PREFIX));
        assert!(truncate_output("short", 8, 8).is_none(), "under cap → None");
    }

    #[test]
    fn second_pass_tightens_largest_outputs_first() {
        // Two big outputs: after pass 1 (8+8 KiB each) the session can still
        // be over a tiny budget; pass 2 (2+2 KiB) must kick in.
        let mut s = session_with_tool_output(noisy(512 * 1024));
        s.messages.push(Message::tool("bash", serde_json::json!({}), noisy(512 * 1024)));
        let cfg = CaptureConfig { session_budget_kib: 8, full_fidelity: false };
        let out = enforce_budget(&mut s, &cfg);
        assert!(out.truncated);
        for m in &s.messages {
            if let Message::Tool { output, .. } = m {
                assert!(output.len() <= 4096 + 256, "pass-2 cap applied: {}", output.len());
            }
        }
    }
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-core budget` — FAIL.

- [ ] **Step 3: Implement** (above the tests):

```rust
//! Capture-time size budget (storage-v2 design §7). Ordering is
//! load-bearing: `redact_session` runs FIRST (full contiguous text for the
//! pattern + entropy rules), truncation second — every persisted byte was
//! scanned in full context, and dropped middles never persist anywhere.

use crate::config::CaptureConfig;
use crate::{Message, Session, TRUNCATION_MARKER_PREFIX};
use std::io::Write;

/// What `enforce_budget` did. `truncated` → at least one tool output was cut
/// (the marker is in the body, and meta derives `truncated: true` from it).
/// `over_budget` → still over after all passes (enormous user/assistant
/// text): written anyway, caller prints the one-line warning (§7 step 4).
#[derive(Debug, Clone)]
pub struct BudgetOutcome {
    pub truncated: bool,
    pub over_budget: bool,
    pub compressed_bytes: usize,
}

/// Deflate-compressed size of the serialized session — the budget's measure.
pub(crate) fn compressed_len(session: &Session) -> usize {
    let bytes = serde_json::to_vec(session).unwrap_or_default();
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    let _ = enc.write_all(&bytes);
    enc.finish().map(|v| v.len()).unwrap_or(bytes.len())
}

/// Largest index ≤ `i` that is a char boundary of `s` (1.75-compatible
/// stand-in for the unstable `str::floor_char_boundary`).
fn floor_char_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Head+tail truncation with the explicit marker splice (§7 step 2):
/// `<head>…[dkod: truncated <n> KiB of tool output]…<tail>`. `None` when
/// `output` already fits in `head + tail` bytes. Byte counts refer to
/// post-redaction text — the only text that ever existed on disk.
pub(crate) fn truncate_output(output: &str, head: usize, tail: usize) -> Option<String> {
    if output.len() <= head + tail {
        return None;
    }
    let h = floor_char_boundary(output, head);
    let mut t = output.len() - tail;
    while !output.is_char_boundary(t) {
        t += 1;
    }
    let dropped_kib = (output.len() - h - (output.len() - t)) / 1024;
    Some(format!(
        "{}…{}{} KiB of tool output]…{}",
        &output[..h],
        TRUNCATION_MARKER_PREFIX,
        dropped_kib.max(1),
        &output[t..]
    ))
}

/// Apply the §7 pipeline in place. Caller MUST have redacted first.
pub fn enforce_budget(session: &mut Session, cfg: &CaptureConfig) -> BudgetOutcome {
    let mut outcome = BudgetOutcome {
        truncated: false,
        over_budget: false,
        compressed_bytes: 0,
    };
    if cfg.full_fidelity || cfg.session_budget_kib == 0 {
        outcome.compressed_bytes = compressed_len(session);
        return outcome;
    }
    let budget = (cfg.session_budget_kib as usize) * 1024;

    // Pass 1: cheap check.
    outcome.compressed_bytes = compressed_len(session);
    if outcome.compressed_bytes <= budget {
        return outcome;
    }

    // Pass 2: cap every tool output at 16 KiB (8 head + 8 tail).
    for m in session.messages.iter_mut() {
        if let Message::Tool { output, .. } = m {
            if let Some(t) = truncate_output(output, 8 * 1024, 8 * 1024) {
                *output = t;
                outcome.truncated = true;
            }
        }
    }
    outcome.compressed_bytes = compressed_len(session);
    if outcome.compressed_bytes <= budget {
        return outcome;
    }

    // Pass 3: tighten to 4 KiB (2 + 2), largest outputs first, re-checking
    // after each so we stop as early as possible.
    let mut order: Vec<(usize, usize)> = session
        .messages
        .iter()
        .enumerate()
        .filter_map(|(i, m)| match m {
            Message::Tool { output, .. } => Some((i, output.len())),
            _ => None,
        })
        .collect();
    order.sort_by(|a, b| b.1.cmp(&a.1));
    for (i, _) in order {
        if let Message::Tool { output, .. } = &mut session.messages[i] {
            if let Some(t) = truncate_output(output, 2 * 1024, 2 * 1024) {
                *output = t;
                outcome.truncated = true;
                outcome.compressed_bytes = compressed_len(session);
                if outcome.compressed_bytes <= budget {
                    return outcome;
                }
            }
        }
    }

    // Step 4: still over (enormous user/assistant/reasoning text) — write
    // anyway; prompts and assistant text are the forensic core (§7).
    outcome.compressed_bytes = compressed_len(session);
    outcome.over_budget = outcome.compressed_bytes > budget;
    outcome
}
```

- [ ] **Step 4: Run green.** `cargo test -p dkod-core budget` — 6 PASS.
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-core/src/budget.rs`, `crates/dkod-core/src/lib.rs`.
  Message: `feat(budget): redact-then-truncate session size budget (256 KiB default, head+tail markers)`

### Task 2.3: wire the budget into `finalize_session`

**Files:**
- Modify: `crates/dkod-cli/src/cmd/capture/mod.rs`
- Test: `crates/dkod-cli/tests/finalize_session.rs` (append only)

- [ ] **Step 1: Write the failing test** (append; reuse the file's helpers):

```rust
#[test]
fn finalize_truncates_oversized_tool_output_and_meta_records_it() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "A"]);

    let cfg = dkod_core::config::Config::default();
    let mut session = fixture_session("big tool output");
    // 2 MiB of pseudo-random output → far over the 256 KiB budget.
    let mut x: u32 = 7;
    let noise: String = (0..2 * 1024 * 1024)
        .map(|_| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            char::from(b'!' + (x % 90) as u8)
        })
        .collect();
    session
        .messages
        .push(dkod_core::Message::tool("bash", serde_json::json!({}), noise));

    finalize_session(repo.path(), &mut session, None, &cfg).unwrap();

    let back = dkod_core::store::read_session(repo.path(), &session.id).unwrap();
    let dkod_core::Message::Tool { output, .. } = &back.messages[1] else { panic!("tool") };
    assert!(output.contains(dkod_core::TRUNCATION_MARKER_PREFIX));
    let meta = dkod_core::store::read_session_meta(repo.path(), &session.id).unwrap();
    assert!(meta.truncated, "meta.json discloses the truncation (§7 step 4)");
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-cli --test finalize_session` — the new test FAILS (no truncation happens).
- [ ] **Step 3: Implement.** In `finalize_session`, between `redact_session`
  and the commit discovery, insert:

```rust
    // Size budget (storage-v2 §7): AFTER redaction (ordering is load-bearing
    // — redaction must see full contiguous text), BEFORE write.
    let outcome = dkod_core::budget::enforce_budget(session, &cfg.capture);
    if outcome.over_budget {
        eprintln!(
            "dkod: session {} is {} KiB compressed — over the {} KiB budget even after \
             tool-output truncation; writing anyway (prompts and assistant text are never cut)",
            session.id,
            outcome.compressed_bytes / 1024,
            cfg.capture.session_budget_kib
        );
    }
```

- [ ] **Step 4: Run green**, full suite (the pre-existing finalize tests use
  tiny fixtures → budget is a no-op for them, by construction).
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-cli/src/cmd/capture/mod.rs`, `crates/dkod-cli/tests/finalize_session.rs`.
  Message: `feat(capture): enforce the session size budget in finalize (redact → truncate → write)`

### Task 2.4: init wires the single non-forced index refspec (+ discovery switch)

**Files:**
- Modify: `crates/dkod-cli/src/cmd/init.rs`
- Modify: `crates/dkod-cli/src/main.rs` (`Init` gains `--remote`)
- Modify (the ONLY existing-test edits in this plan, listed in Background facts): `crates/dkod-cli/tests/cli.rs`, `crates/dkod-cli/tests/init_session_discovery.rs`

This task implements §5.5 + §9.1: subscribe to `refs/dkod/index` (non-forced,
no `+`) on **origin only**, migrate away the old broad `+refs/dkod/*` refspec
init previously wrote, leave foreign refspecs alone, and switch discovery to
the index ref with a legacy-count fallback.

- [ ] **Step 1: Update the behavior-spec tests** (same commit as the code —
  these tests SPECIFY v1 behavior the design replaces; everything else in the
  suite stays untouched):
  - `tests/cli.rs::count_dkod_refspecs`: match `refs/dkod/index:refs/dkod/index`
    instead of `+refs/dkod/*:refs/dkod/*`; add a sibling
    `count_legacy_refspecs` matching the old line.
  - `init_writes_dkod_refspec_when_remote_exists`: assert
    `count_dkod_refspecs(origin) == 1` AND `count_legacy_refspecs(origin) == 0`.
  - `init_refspec_is_idempotent`: three runs → still exactly 1 index line.
  - `init_writes_refspec_to_all_remotes` → RENAME to
    `init_wires_index_refspec_on_origin_only`: with `origin` + `upstream`
    configured, init wires origin only (`upstream` count = 0); a second run of
    `dkod init --remote upstream` wires upstream too (count = 1).
  - NEW `init_migrates_legacy_broad_refspec_in_place`: pre-seed
    `git config --add remote.origin.fetch '+refs/dkod/*:refs/dkod/*'` plus a
    foreign line `+refs/foo/*:refs/foo/*`; run init; assert the dkod broad
    line is gone, the index line present exactly once, and the foreign line
    untouched (§9.1 "leaving foreign refspecs alone").
  - `tests/init_session_discovery.rs`: the duplicate-refspec assertion
    (~line 240) now counts the index refspec line; the clone-side assertions
    (~lines 114/129) change from "refs/dkod/sessions/<sid> exists in the
    clone" to "`dkod show <sid>` succeeds in the clone" (capability, not
    mechanism — the index fetch carries the session without materializing a
    legacy ref).
- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-cli --test cli --test init_session_discovery` — the updated tests FAIL against the old init.
- [ ] **Step 3: Implement in `init.rs`.**

```rust
/// v1 refspec init used to write — now only ever REMOVED (migrate-in-place,
/// storage-v2 §9.1). Kept as a constant for exact-match removal.
const DKOD_LEGACY_FETCH_REFSPEC: &str = "+refs/dkod/*:refs/dkod/*";
/// v2 subscription: ONE ref, NON-forced (no `+`) so a vanilla `git fetch`
/// fast-forwards only and can never clobber a local-ahead index (§5.5).
const DKOD_INDEX_REFSPEC: &str = "refs/dkod/index:refs/dkod/index";
```

Replace `ensure_dkod_refspec(cwd)` with `ensure_dkod_refspec(cwd, remote: Option<&str>)`:

1. `let target = remote.unwrap_or("origin");` — if `git remote` doesn't list
   it: print `dkod init: remote {target} not configured; skipping refspec wiring`
   and return Ok (preserves the no-remote test).
2. For EVERY listed remote: if `remote_already_has_line(cwd, r, DKOD_LEGACY_FETCH_REFSPEC)`,
   run `git config --fixed-value --unset-all remote.<r>.fetch '+refs/dkod/*:refs/dkod/*'`
   (accept exit codes 0 and 5 — 5 means "nothing matched", a benign race).
3. For `target` only: if not `remote_already_has_line(cwd, target, DKOD_INDEX_REFSPEC)`,
   `git config --add remote.<target>.fetch refs/dkod/index:refs/dkod/index`.

Rename `remote_already_has_dkod_refspec` to
`remote_already_has_line(cwd, remote, line) -> Result<bool>` (same body,
parameterized on the matched line).

Replace `discover_remote_sessions(cwd)`:

1. `git ls-remote origin refs/dkod/index` — if it lists the ref:
   `git fetch --quiet origin refs/dkod/index:refs/dkod/index` (non-forced),
   then count sessions via
   `dkod_core::store::list_sessions(cwd).map(|v| v.len()).unwrap_or(0)` and
   print `dkod: fetched the session index from origin ({n} session(s) — dkod log to browse)`.
   A fetch rejection because the local index is ahead is fine: print nothing
   (§5.5 — local-ahead must never be clobbered).
2. Else fall back to the EXISTING `refs/dkod/sessions/*` count + the EXISTING
   legacy fetch (repos that only have legacy refs keep the old path, §9.1).
3. All of it stays best-effort/silent on network errors, exactly as today.

`main.rs`: `Init` becomes

```rust
    /// Initialize dkod in the current repo
    Init {
        /// Remote to subscribe to the session index (default: origin).
        #[arg(long)]
        remote: Option<String>,
    },
```

dispatch: `Cmd::Init { remote } => cmd::init::run(&std::env::current_dir()?, remote.as_deref()),`
and `cmd::init::run(cwd: &Path, remote: Option<&str>)` threads it through.
Update the breadcrumb text in `BREADCRUMB_CONTENT` from
`(refs/dkod/sessions/*)` to `(refs/dkod/index + refs/dkod/sessions/*)`.

- [ ] **Step 4: Run green.** Full suite.
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-cli/src/cmd/init.rs`, `crates/dkod-cli/src/main.rs`, `crates/dkod-cli/tests/cli.rs`, `crates/dkod-cli/tests/init_session_discovery.rs`.
  Message: `feat(init): subscribe one non-forced index refspec on origin; migrate the broad v1 refspec in place`

### Task 2.5: `dkod push` — best-effort push with fetch-replay-reconcile (§5.4)

**Files:**
- Modify: `crates/dkod-core/src/index.rs` (`local_only_inserts`, `reconcile_onto`)
- Create: `crates/dkod-cli/src/cmd/push.rs`
- Modify: `crates/dkod-cli/src/cmd/mod.rs` (`pub mod push;`), `crates/dkod-cli/src/main.rs`
- Test: `crates/dkod-cli/tests/e2e_push.rs` (new)

- [ ] **Step 1: Write the failing core tests** (in `index.rs`):

```rust
#[cfg(test)]
mod reconcile_tests {
    use super::*;
    use tempfile::TempDir;

    fn repo() -> (TempDir, gix::Repository) {
        let tmp = TempDir::new().unwrap();
        let mut r = gix::init(tmp.path()).unwrap();
        crate::store::ensure_committer(&mut r).unwrap();
        (tmp, r)
    }

    #[test]
    fn local_only_inserts_returns_paths_missing_from_theirs() {
        let (tmp, repo) = repo();
        let mut base = IndexBatch::new("base");
        base.insert("sessions/2025-01-01/x/meta.json".into(), b"x".to_vec());
        let base_tip = apply_batch(tmp.path(), &base).unwrap().unwrap();

        // "remote" advanced with session z (side commit, ref untouched)
        let z = repo.write_blob(b"z").unwrap().detach();
        let remote_tip = build_commit(
            &repo,
            Some(base_tip),
            &[("sessions/2025-01-01/z/meta.json".to_string(), z)],
            "remote",
        )
        .unwrap()
        .unwrap();

        // local advanced with session y
        let mut local = IndexBatch::new("local");
        local.insert("sessions/2025-01-02/y/meta.json".into(), b"y".to_vec());
        let local_tip = apply_batch(tmp.path(), &local).unwrap().unwrap();

        let only = local_only_inserts(&repo, local_tip, remote_tip).unwrap();
        let paths: Vec<&str> = only.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["sessions/2025-01-02/y/meta.json"], "x shared, z theirs, y ours");
    }

    #[test]
    fn local_only_inserts_excludes_version_and_epoch() {
        let (tmp, repo) = repo();
        let t1 = apply_batch(tmp.path(), &{
            let mut b = IndexBatch::new("a");
            b.insert("a".into(), b"1".to_vec());
            b
        })
        .unwrap()
        .unwrap();
        // unrelated remote root (fresh teammate index)
        let z = repo.write_blob(b"z").unwrap().detach();
        let remote = build_commit(&repo, None, &[("b".to_string(), z)], "other root")
            .unwrap()
            .unwrap();
        let only = local_only_inserts(&repo, t1, remote).unwrap();
        let paths: Vec<&str> = only.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["a"], "version/epoch never replayed");
    }

    #[test]
    fn reconcile_onto_replays_local_only_paths_as_one_commit() {
        let (tmp, repo) = repo();
        let base = apply_batch(tmp.path(), &{
            let mut b = IndexBatch::new("base");
            b.insert("sessions/2025-01-01/x/meta.json".into(), b"x".to_vec());
            b
        })
        .unwrap()
        .unwrap();
        let z = repo.write_blob(b"z").unwrap().detach();
        let remote = build_commit(
            &repo,
            Some(base),
            &[("sessions/2025-01-01/z/meta.json".to_string(), z)],
            "remote",
        )
        .unwrap()
        .unwrap();
        let _local = apply_batch(tmp.path(), &{
            let mut b = IndexBatch::new("local");
            b.insert("sessions/2025-01-02/y/meta.json".into(), b"y".to_vec());
            b
        })
        .unwrap()
        .unwrap();

        let new_tip = reconcile_onto(tmp.path(), remote).unwrap();
        let repo = gix::open(tmp.path()).unwrap();
        assert_eq!(index_tip(&repo), Some(new_tip), "local ref adopted the reconcile tip");
        let c = repo.find_object(new_tip).unwrap().try_into_commit().unwrap();
        let parents: Vec<_> = c.parent_ids().map(|p| p.detach()).collect();
        assert_eq!(parents, vec![remote], "reconcile commit sits on the remote tip");
        assert!(c.message_raw().unwrap().to_string().contains("reconcile"));
        let root = commit_tree(&repo, new_tip).unwrap();
        for p in [
            "sessions/2025-01-01/x/meta.json",
            "sessions/2025-01-01/z/meta.json",
            "sessions/2025-01-02/y/meta.json",
        ] {
            assert!(read_tree_blob(&repo, root, p).is_some(), "{p} lost in reconcile");
        }
    }

    #[test]
    fn reconcile_onto_with_nothing_local_just_adopts_remote() {
        let (tmp, repo) = repo();
        let base = apply_batch(tmp.path(), &{
            let mut b = IndexBatch::new("base");
            b.insert("a".into(), b"1".to_vec());
            b
        })
        .unwrap()
        .unwrap();
        let z = repo.write_blob(b"z").unwrap().detach();
        let remote = build_commit(&repo, Some(base), &[("b".to_string(), z)], "remote")
            .unwrap()
            .unwrap();
        let tip = reconcile_onto(tmp.path(), remote).unwrap();
        assert_eq!(tip, remote, "no reconcile commit when nothing is local-only");
        assert_eq!(index_tip(&gix::open(tmp.path()).unwrap()), Some(remote));
    }
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-core reconcile` — FAIL.

- [ ] **Step 3: Implement in `index.rs`.**

```rust
/// Recursive tree diff: every `(path, blob_oid)` present under `ours` whose
/// path is absent (or different) under `theirs`. Identical subtree oids are
/// skipped wholesale, so cost ∝ divergence, not archive size. Root-level
/// `version`/`epoch` blobs are excluded (epoch is gc's, §8.3). Replay safety:
/// distinct sessions touch distinct paths; the only same-path collision is a
/// pointer blob, which is last-writer-wins by policy (§5.4).
pub fn local_only_inserts(
    repo: &gix::Repository,
    ours: gix::ObjectId,
    theirs: gix::ObjectId,
) -> Result<Vec<(String, gix::ObjectId)>> {
    let ours_root = commit_tree(repo, ours)?;
    let theirs_root = commit_tree(repo, theirs)?;
    let mut out = Vec::new();
    diff_trees(repo, Some(ours_root), Some(theirs_root), "", &mut out)?;
    out.retain(|(p, _)| p != "version" && p != "epoch");
    Ok(out)
}

fn diff_trees(
    repo: &gix::Repository,
    ours: Option<gix::ObjectId>,
    theirs: Option<gix::ObjectId>,
    prefix: &str,
    out: &mut Vec<(String, gix::ObjectId)>,
) -> Result<()> {
    if ours == theirs {
        return Ok(());
    }
    let our_entries = tree_entries(repo, ours)?;
    let their_entries = tree_entries(repo, theirs)?;
    for e in our_entries {
        let name = e.filename.to_string();
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let theirs_match = their_entries.iter().find(|t| t.filename == e.filename);
        if e.mode.is_tree() {
            let their_child = theirs_match.filter(|t| t.mode.is_tree()).map(|t| t.oid);
            diff_trees(repo, Some(e.oid), their_child, &path, out)?;
        } else if theirs_match.map(|t| t.oid) != Some(e.oid) {
            out.push((path, e.oid));
        }
    }
    Ok(())
}

/// §5.4 replay: fold every local-only insert onto `remote_tip` as ONE
/// `dkod: reconcile <n> session(s)` commit and move the local index ref to
/// it (dkod-driven non-fast-forward is allowed, §5.5). Returns the new local
/// tip — `remote_tip` itself when nothing was local-only. Counts sessions as
/// distinct `sessions/<date>/<id>` prefixes among the replayed paths.
pub fn reconcile_onto(repo_path: &Path, remote_tip: gix::ObjectId) -> Result<gix::ObjectId> {
    let mut repo = gix::open(repo_path).context("open repo")?;
    crate::store::ensure_committer(&mut repo)?;
    let _lock = IndexLock::acquire(&repo)?;
    let local_tip = index_tip(&repo);
    let inserts = match local_tip {
        Some(l) if l != remote_tip => local_only_inserts(&repo, l, remote_tip)?,
        _ => Vec::new(),
    };
    let sessions: std::collections::BTreeSet<&str> = inserts
        .iter()
        .filter_map(|(p, _)| {
            let mut it = p.split('/');
            (it.next() == Some("sessions")).then(|| ())?;
            it.next()?; // date
            it.next() // id
        })
        .collect();
    let message = format!("dkod: reconcile {} session(s)", sessions.len());
    let new_tip = commit_inserts(&repo, Some(remote_tip), local_tip, &inserts, &message)?
        .unwrap_or(remote_tip);
    if new_tip == remote_tip && index_tip(&repo) != Some(remote_tip) {
        // nothing local-only, but the ref still points at the old local tip →
        // adopt the remote tip (same CAS discipline).
        commit_adopt(&repo, remote_tip, local_tip)?;
    }
    Ok(new_tip)
}

/// Point the local index ref at `tip` (CAS from `expected`). Used by adopt
/// paths (reconcile-with-nothing-local, gc epoch adoption in Phase 4).
pub(crate) fn commit_adopt(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    expected: Option<gix::ObjectId>,
) -> Result<()> {
    use gix::refs::{
        transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
        Target,
    };
    let expected = match expected {
        Some(t) => PreviousValue::MustExistAndMatch(Target::Object(t)),
        None => PreviousValue::MustNotExist,
    };
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "dkod: adopt remote index tip".into(),
            },
            expected,
            new: Target::Object(tip),
        },
        name: INDEX_REF.try_into().context("invalid index ref name")?,
        deref: false,
    })
    .context("adopt index tip")?;
    Ok(())
}
```

Note `commit_inserts` is called with `parent = Some(remote_tip)` but
`expected_tip = local_tip` — the CAS guards the LOCAL ref while the commit
chains onto the REMOTE tip. That asymmetry is the §5.4 replay.

- [ ] **Step 4: CLI `cmd/push.rs`.**

```rust
//! `dkod push` — push refs/dkod/index to origin with the §5.4
//! fetch-replay-retry loop (bounded at 3 rounds). Also exposes the quiet
//! best-effort entry point `finalize_session` calls when auto_push is on.

use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Command;

const FETCH_TMP_REF: &str = "refs/dkod/fetch-tmp";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PushOutcome {
    Pushed,
    UpToDate,
    NoRemote,
    NoIndex,
    StayedLocal(String),
}

/// Best-effort push: NEVER errors, never prints on the boring paths
/// (NoRemote/UpToDate/NoIndex). `finalize_session` calls this; offline or
/// rejected pushes stay local and ride the next attempt (§5.4).
pub(crate) fn push_index_quiet(cwd: &Path) -> PushOutcome {
    match try_push(cwd) {
        Ok(o) => {
            if let PushOutcome::StayedLocal(reason) = &o {
                eprintln!("dkod: index push deferred ({reason}) — run `dkod push` later");
            }
            o
        }
        Err(e) => {
            eprintln!("dkod: index push deferred ({e:#}) — run `dkod push` later");
            PushOutcome::StayedLocal(format!("{e:#}"))
        }
    }
}

/// `dkod push`: explicit flush; errors are surfaced.
pub fn run(cwd: &Path) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;
    match try_push(cwd)? {
        PushOutcome::Pushed => println!("dkod: pushed refs/dkod/index to origin"),
        PushOutcome::UpToDate => println!("dkod: index already up to date on origin"),
        PushOutcome::NoRemote => println!("dkod: no origin remote configured — nothing to push"),
        PushOutcome::NoIndex => println!("dkod: no local index yet — capture a session first"),
        PushOutcome::StayedLocal(reason) => {
            return Err(anyhow!("could not push the index after 3 reconcile rounds: {reason}"))
        }
    }
    Ok(())
}

fn git_out(cwd: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|e| anyhow!("spawn git {args:?}: {e}"))
}

fn try_push(cwd: &Path) -> Result<PushOutcome> {
    let repo = gix::open(cwd).map_err(|e| anyhow!("open repo: {e}"))?;
    if dkod_core::index::index_tip(&repo).is_none() {
        return Ok(PushOutcome::NoIndex);
    }
    if !git_out(cwd, &["remote", "get-url", "origin"])?.status.success() {
        return Ok(PushOutcome::NoRemote);
    }
    let mut last = String::new();
    for _round in 0..3 {
        let push = git_out(cwd, &["push", "--quiet", "origin", "refs/dkod/index:refs/dkod/index"])?;
        if push.status.success() {
            // "Everything up-to-date" also exits 0 — both count as success.
            return Ok(PushOutcome::Pushed);
        }
        last = String::from_utf8_lossy(&push.stderr).trim().to_string();
        // Rejected (teammate pushed first) or transient: fetch the remote tip
        // into a temp ref and replay (§5.4). A failing fetch = offline/auth →
        // stay local.
        let fetch = git_out(
            cwd,
            &["fetch", "--quiet", "origin", &format!("+refs/dkod/index:{FETCH_TMP_REF}")],
        )?;
        if !fetch.status.success() {
            return Ok(PushOutcome::StayedLocal(last));
        }
        let repo = gix::open(cwd).map_err(|e| anyhow!("re-open repo: {e}"))?;
        let remote_tip = repo
            .try_find_reference(FETCH_TMP_REF)
            .ok()
            .flatten()
            .map(|r| r.id().detach());
        let _ = git_out(cwd, &["update-ref", "-d", FETCH_TMP_REF]); // always clean up
        let Some(remote_tip) = remote_tip else {
            return Ok(PushOutcome::StayedLocal(last));
        };
        if Some(remote_tip) == dkod_core::index::index_tip(&repo) {
            return Ok(PushOutcome::UpToDate);
        }
        dkod_core::index::reconcile_onto(cwd, remote_tip)?;
    }
    Ok(PushOutcome::StayedLocal(last))
}
```

`mod.rs`: `pub mod push;` (between `patchid` and `reindex`). `main.rs`:

```rust
    /// Push the session index (refs/dkod/index) to origin, reconciling with
    /// teammates' pushes if needed.
    Push,
```

dispatch: `Cmd::Push => cmd::push::run(&std::env::current_dir()?),`. `Push` is
a normal command (no `maybe_warn_drift` exclusion).

- [ ] **Step 5: Write the e2e test.** `tests/e2e_push.rs`:

```rust
//! End-to-end §5.4: two writers race a push; the loser fetch-replays and both
//! sessions survive on the remote.

#[test]
fn push_reconciles_divergent_indexes() {
    // 1. bare remote: TempDir + `git init -q --bare remote.git`.
    // 2. repo A: git init; remote add origin <bare>; seed session SA via
    //    dkod_core::store::write_session; cargo_bin("dkod") push → success.
    // 3. repo B: git init; remote add origin <bare>; seed session SB;
    //    `dkod push` → first push is rejected (non-FF), reconcile kicks in,
    //    command still succeeds.
    // 4. repo C: git clone <bare>; `dkod init` (fetches the index);
    //    `dkod log` lists BOTH SA's and SB's ids.
}

#[test]
fn push_with_no_origin_reports_and_exits_zero() {
    // git init only; `dkod push` → success; stdout contains "no origin".
}
```

Write the full bodies using the standard helpers (Background facts) — bare
remote via `git(dir, &["init", "-q", "--bare", "remote.git"])`, file-path
remotes work offline.

- [ ] **Step 6: Run green.** Full suite.
- [ ] **Step 7: Gate & commit ritual.** Files: `crates/dkod-core/src/index.rs`, `crates/dkod-cli/src/cmd/push.rs`, `crates/dkod-cli/src/cmd/mod.rs`, `crates/dkod-cli/src/main.rs`, `crates/dkod-cli/tests/e2e_push.rs`.
  Message: `feat(push): dkod push with bounded fetch-replay-reconcile (one ref, §5.4 semantics)`

### Task 2.6: auto-push on finalize + `[storage] auto_push` + indexer follow-up

**Files:**
- Modify: `crates/dkod-core/src/config.rs` (add `auto_push`)
- Modify: `crates/dkod-cli/src/cmd/capture/mod.rs`
- Test: `crates/dkod-cli/tests/e2e_push.rs` (append)

- [ ] **Step 1: Failing config test:**

```rust
#[test]
fn storage_auto_push_defaults_on() {
    let c: Config = toml::from_str("").unwrap();
    assert!(c.storage.auto_push);
    let c: Config = toml::from_str("[storage]\nauto_push = false\n").unwrap();
    assert!(!c.storage.auto_push);
}
```

Add `pub auto_push: bool` to `StorageConfig` (+ `auto_push: true` in
`Default`).

- [ ] **Step 2: Failing e2e** (append to `e2e_push.rs`):

```rust
#[test]
fn finalize_auto_pushes_when_origin_exists() {
    // repo with bare origin; build a Session and call
    // dkod_cli::cmd::capture::finalize_session(...) directly (the crate
    // exposes it pub; finalize_session.rs already imports it);
    // then `git ls-remote <bare> refs/dkod/index` is non-empty.
}

#[test]
fn finalize_with_auto_push_off_stays_local() {
    // same, but .dkod/config.toml has [storage] auto_push = false →
    // ls-remote stays empty; the session is still written locally.
}
```

- [ ] **Step 3: Implement.** End of `finalize_session`, before `Ok(linked)`:

```rust
    // Best-effort propagation (§5.4): one non-forced ref push; offline /
    // rejected stays local and rides the next finalize or `dkod push`.
    if cfg.storage.auto_push {
        let _ = crate::cmd::push::push_index_quiet(cwd);
    }
```

(Existing finalize tests have no `origin` → `PushOutcome::NoRemote`, silent,
no network, no slowdown.)

- [ ] **Step 4: Run green.** Full suite.
- [ ] **Step 5: File the dkod-indexer follow-up** (the ONE cross-repo note, §11 — do not implement anything there):

```bash
gh auth switch --user haim-ari
gh issue create --repo dkod-io/dkod-indexer \
  --title "Storage v2: ingest via refs/dkod/index tip cursor (legacy per-session refs being phased out)" \
  --body "dkod-cli storage v2 adds refs/dkod/index (commit chain; tree = sessions/<date>/<id>/{meta,body}.json + commits/ + patchid/ pointer tables — see dkod-cli docs/plans/2026-06-10-storage-v2-design.md §4/§11). Backward compatible today: the index ref matches the existing refs/dkod/* ls-remote glob and dual-write keeps emitting per-session refs until v2 Phase 4. Follow-up: switch the reconciler to 'index tip sha = change cursor' (tip unchanged → skip; changed → fetch delta, walk new meta blobs) — strictly cheaper than 300k-ref diffing. Until then no action needed; note that repos that opt into 'dkod reindex --delete-legacy' will stop exposing per-session refs."
```

- [ ] **Step 6: Gate & commit ritual.** Files: `crates/dkod-core/src/config.rs`, `crates/dkod-cli/src/cmd/capture/mod.rs`, `crates/dkod-cli/tests/e2e_push.rs`.
  Message: `feat(capture): auto-push the index on finalize ([storage] auto_push, default on)`
- [ ] **Step 7: Phase close-out.** Full gates; `/coderabbit:review --base main`;
  `gh auth switch --user haim-ari`; push `feat/storage-v2-phase2`; PR
  `feat: storage v2 phase 2 — session size budget + auto-push/reconcile`;
  call out the 6 init-test updates as design-mandated behavior-spec changes
  (§9.1) and that the drift benchmark ran clean. Drive to green; squash-merge;
  sync main.

---

## Phase 3 — lazy propagation (§15.3, §9)

Branch `feat/storage-v2-phase3` off updated `main`. The meta/body split is
already in the format (Phase 1); this phase wires the filtered `dkod-archive`
remote, the host probe, subscription tiers, and body backfill + offline
messaging.

### Task 3.1: subscription tiers + `dkod-archive` remote + filter probe in init

**Files:**
- Modify: `crates/dkod-cli/src/cmd/init.rs`
- Modify: `crates/dkod-cli/src/main.rs` (`Init` gains `--subscribe`)
- Test: `crates/dkod-cli/tests/e2e_lazy_init.rs` (new)

- [ ] **Step 1: Write the failing e2e tests.** `tests/e2e_lazy_init.rs` (standard helpers; bare file remotes support `uploadpack.allowFilter` when configured):

```rust
//! Storage-v2 §9: init wires the lazy dkod-archive remote (blob:limit filter,
//! promisor) when the host supports filters, falls back to the full origin
//! subscription when it does not, and never wires the index refspec on BOTH.

#[test]
fn init_lazy_wires_dkod_archive_remote_with_filter_and_promisor() {
    // 1. bare remote with `git -C remote.git config uploadpack.allowFilter true`.
    // 2. work repo: git init; remote add origin <bare>; `dkod init`.
    // 3. assert `git config remote.dkod-archive.url` == origin url;
    //    `remote.dkod-archive.fetch` == "refs/dkod/index:refs/dkod/index";
    //    `remote.dkod-archive.partialclonefilter` == "blob:limit=100k";
    //    `remote.dkod-archive.promisor` == "true".
    // 4. assert origin has NO refs/dkod/index refspec line (never both, §9.1).
    // 5. re-run `dkod init` → all four keys still appear exactly once.
}

#[test]
fn init_lazy_falls_back_to_full_when_filters_unsupported() {
    // bare remote WITHOUT uploadpack.allowFilter (and with
    // `uploadpack.allowFilter false` set explicitly for determinism):
    // `dkod init` → no dkod-archive remote; origin carries the plain index
    // refspec; stderr mentions the fallback.
}

#[test]
fn init_subscribe_none_wires_nothing() {
    // `dkod init --subscribe none` → no dkod-archive remote, no index refspec
    // on origin (capture-only writer, §9.2).
}

#[test]
fn init_subscribe_full_forces_origin_subscription() {
    // filter-capable bare remote + `dkod init --subscribe full` →
    // origin has the plain refspec; no dkod-archive remote.
}
```

Write the full bodies (each ~25 lines) with `assert_cmd` + a
`fn git_config_get(repo, key) -> String` helper using
`git config --get <key>`.

- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement in `init.rs`.**

```rust
/// The dedicated lazy-fetch remote (§9.2): same URL as origin, separate
/// config so `remote.<name>.partialclonefilter` can never blob-filter the
/// user's CODE fetches.
const ARCHIVE_REMOTE: &str = "dkod-archive";
const ARCHIVE_FILTER: &str = "blob:limit=100k";

/// Subscription tier (§9.2): none → index-lazy (default when the host
/// supports filters) → full.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Subscribe {
    Lazy,
    Full,
    None,
}
```

Replace the Phase-2 refspec block in `run` with `wire_subscription(cwd, remote, subscribe)`:

1. Always migrate-away the legacy broad refspec on every remote (Phase 2 logic, kept).
2. `Subscribe::None` → also remove any dkod-managed index refspec from the
   target remote and remove the `dkod-archive` remote if its URL matches the
   target's (cleanup so tier switches are idempotent); done.
3. `Subscribe::Full` → Phase-2 behavior (plain index refspec on the target
   remote); remove `dkod-archive` if present and dkod-managed (URL == target
   URL and fetch == index refspec).
4. `Subscribe::Lazy` (default) → resolve the target remote's URL
   (`git remote get-url <target>`; missing remote → print skip message, done);
   then:
   - `git remote add dkod-archive <url>` (or `git remote set-url dkod-archive <url>` if it exists),
   - `git config remote.dkod-archive.fetch refs/dkod/index:refs/dkod/index` (replace-all, not add),
   - `git config remote.dkod-archive.partialclonefilter blob:limit=100k`,
   - `git config remote.dkod-archive.promisor true`,
   - **probe**: `git fetch --quiet dkod-archive refs/dkod/index:refs/dkod/probe-tmp`;
     classify: exit 0 → supported; stderr containing `couldn't find remote ref`
     → supported (empty archive, filter accepted); anything else (notably
     `filter`/`server does not support`) → unsupported. Always
     `git update-ref -d refs/dkod/probe-tmp` afterwards.
   - unsupported → remove the `dkod-archive` remote (`git remote remove`),
     wire the FULL tier instead, and print:
     `dkod init: this git host does not support partial-clone filters — using the full-content index subscription on origin (refs + negotiation benefits still apply)`.
   - supported → ensure origin does NOT also carry the plain index refspec
     (`--fixed-value --unset-all`, accept exits 0/5): never both (§9.1).
5. `discover_remote_sessions` (Phase 2 version) fetches via `dkod-archive`
   when that remote exists, else `origin` — one-line change to pick the remote
   name before the existing fetch.

`main.rs`: extend `Init`:

```rust
    Init {
        /// Remote to subscribe to the session index (default: origin).
        #[arg(long)]
        remote: Option<String>,
        /// Subscription tier: lazy (metadata always, bodies on demand —
        /// default), full (mirror everything), none (capture-only writer).
        #[arg(long, value_enum, default_value_t = SubscribeArg::Lazy)]
        subscribe: SubscribeArg,
    },
```

with a `SubscribeArg` `ValueEnum` mirroring `Subscribe` (same pattern as the
existing `ScopeArg`), and `cmd::init::run(cwd, remote, subscribe)` threading.

- [ ] **Step 4: Run green.** Full suite (the Phase-2 init tests still pass: lazy
  probe against a filterless test remote falls back to exactly the Phase-2
  full wiring — set `uploadpack.allowFilter false` is NOT needed in those old
  tests because file remotes without the key reject filters by default).
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-cli/src/cmd/init.rs`, `crates/dkod-cli/src/main.rs`, `crates/dkod-cli/tests/e2e_lazy_init.rs`.
  Message: `feat(init): lazy/full/none subscription tiers with dkod-archive promisor remote + filter probe`

### Task 3.2: body backfill + offline messaging (`dkod fetch`, `show`, drift detail)

**Files:**
- Create: `crates/dkod-cli/src/cmd/fetch.rs`
- Modify: `crates/dkod-cli/src/cmd/mod.rs` (`pub mod fetch;`), `crates/dkod-cli/src/main.rs`
- Modify: `crates/dkod-cli/src/cmd/show.rs`
- Modify: `crates/dkod-cli/src/cmd/drift.rs` (detail mode only)
- Modify: `crates/dkod-core/src/store.rs` (expose the body-blob oid)

- [ ] **Step 1: Write the failing tests.**

Core (`store.rs`):

```rust
#[test]
fn body_blob_oid_resolves_for_indexed_session() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let s = fixture_session();
    write_session(tmp.path(), &s).unwrap();
    let oid = body_blob_oid(tmp.path(), &s.id).expect("oid");
    let repo = gix::open(tmp.path()).unwrap();
    let data = repo.find_object(oid).unwrap().detach().data;
    assert_eq!(serde_json::from_slice::<Session>(&data).unwrap(), s);
    assert!(body_blob_oid(tmp.path(), "missing-id").is_none());
}
```

CLI e2e (`tests/e2e_lazy_init.rs`, append):

```rust
#[test]
fn show_prints_offline_header_when_body_blob_is_missing() {
    // 1. repo with an indexed session (write_session), dual-write OFF via
    //    .dkod/config.toml so the legacy fallback can't serve the body.
    // 2. surgically delete the body blob's loose object file
    //    (.git/objects/<aa>/<rest>) — simulating an unfetched promisor blob.
    // 3. `dkod show <id>` → exit 0; stdout contains the prompt summary AND
    //    "body not fetched — run `dkod fetch".
}

#[test]
fn dkod_fetch_pulls_the_index_from_the_subscribed_remote() {
    // bare remote (allowFilter true) seeded by repo A (write_session + dkod
    // push); repo B: dkod init (wires dkod-archive); `dkod fetch` → exit 0;
    // `dkod log` in B lists A's session id.
}
```

- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement.**

`store.rs`:

```rust
/// Oid of `sessions/<date>/<id>/body.json` at the index tip — used by the
/// CLI to backfill a lazily-deferred body via `git fetch dkod-archive <oid>`
/// (§9.2) and to detect "indexed but body object missing" (offline lazy mode).
pub fn body_blob_oid(repo_path: &Path, id: &str) -> Option<gix::ObjectId>
```

(index tip → `body_path(id)` direct, then `find_session_dir_by_scan` — the
same resolution order as `read_session`, returning the oid instead of bytes.)

`cmd/fetch.rs`:

```rust
//! `dkod fetch [<session-id>]` — without an id: fetch refs/dkod/index from
//! the subscribed remote (dkod-archive if wired, else origin). With an id:
//! backfill that session's body blob through the promisor remote (§9.2 —
//! the "body not fetched" recovery path printed by `dkod show`).

use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Command;

/// dkod-archive when configured, else origin.
pub(crate) fn subscribed_remote(cwd: &Path) -> String {
    let ok = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["remote", "get-url", "dkod-archive"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if ok { "dkod-archive".into() } else { "origin".into() }
}

pub fn run(cwd: &Path, id: Option<&str>) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;
    let remote = subscribed_remote(cwd);
    match id {
        None => {
            let out = Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(["fetch", "--quiet", &remote, "refs/dkod/index:refs/dkod/index"])
                .output()
                .map_err(|e| anyhow!("spawn git fetch: {e}"))?;
            if !out.status.success() {
                return Err(anyhow!(
                    "fetching refs/dkod/index from {remote} failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            println!("dkod: fetched the session index from {remote}");
        }
        Some(id) => {
            if !backfill_body(cwd, id)? {
                return Err(anyhow!("session {id} has no indexed body to fetch"));
            }
            println!("dkod: fetched the body for session {id}");
        }
    }
    Ok(())
}

/// Fetch the exact body blob oid from the promisor remote. Ok(false) when
/// the session isn't in the index at all. pub(crate): `show`/drift-detail
/// retry through this when a body read fails and we're plausibly online.
pub(crate) fn backfill_body(cwd: &Path, id: &str) -> Result<bool> {
    let Some(oid) = dkod_core::store::body_blob_oid(cwd, id) else {
        return Ok(false);
    };
    let remote = subscribed_remote(cwd);
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["fetch", "--quiet", &remote, &oid.to_string()])
        .output()
        .map_err(|e| anyhow!("spawn git fetch: {e}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "backfill fetch from {remote} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(true)
}
```

`main.rs`:

```rust
    /// Fetch the session index from the subscribed remote; with a session
    /// id, backfill that session's lazily-deferred body.
    Fetch {
        /// Session id whose body to backfill; omit to fetch the index.
        id: Option<String>,
    },
```

dispatch: `Cmd::Fetch { id } => cmd::fetch::run(&std::env::current_dir()?, id.as_deref()),`.

`show.rs` — wrap the body read (§ failure table "Lazy body missing while offline"):

```rust
pub fn run(cwd: &Path, id: &str) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;
    let s = match dkod_core::store::read_session(cwd, id) {
        Ok(s) => s,
        Err(read_err) => {
            // Body unreadable. If the session is indexed, try one promisor
            // backfill, then degrade to the metadata header (offline lazy
            // mode) instead of failing.
            if crate::cmd::fetch::backfill_body(cwd, id).unwrap_or(false) {
                if let Ok(s) = dkod_core::store::read_session(cwd, id) {
                    return print_full(&s);
                }
            }
            match dkod_core::store::read_session_meta(cwd, id) {
                Ok(meta) => return print_meta_only(&meta, id),
                Err(_) => return Err(read_err).with_context(|| format!("read session {id}")),
            }
        }
    };
    print_full(&s)
}
```

Factor today's printing into `fn print_full(s: &dkod_core::Session) -> Result<()>`
(existing body, unchanged — including the `redactions:` line; ALSO add, right
after it, `if let Ok(meta) = ... read_session_meta` → when `meta.truncated`
print `truncated: tool outputs were size-budgeted at capture` — the §7
disclosure), and:

```rust
fn print_meta_only(m: &dkod_core::SessionMeta, id: &str) -> Result<()> {
    println!("session {}", m.id);
    println!("agent   {}", dkod_core::agent_label(&m.agent));
    println!("created {}  duration_ms={}", m.created_at, m.duration_ms);
    println!("summary {}", m.prompt_summary);
    if !m.commits.is_empty() {
        println!("commits {}", m.commits.join(", "));
    }
    if !m.files_touched.is_empty() {
        println!("files   {}", m.files_touched.join(", "));
    }
    println!();
    println!("body not fetched — run `dkod fetch {id}` when online");
    Ok(())
}
```

`drift.rs` detail mode (`run` with `Some(id)`): on `read_session` failure, try
`backfill_body` + retry once; if still failing but `read_session_meta`
succeeds, print the meta header line + `body not fetched — run `dkod fetch
<id>` when online; drift needs the transcript body` and return Ok. The listing
pass already only needs sessions that read fully — leave it as-is.

- [ ] **Step 4: Run green.** Full suite.
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-core/src/store.rs`, `crates/dkod-cli/src/cmd/fetch.rs`, `crates/dkod-cli/src/cmd/mod.rs`, `crates/dkod-cli/src/main.rs`, `crates/dkod-cli/src/cmd/show.rs`, `crates/dkod-cli/src/cmd/drift.rs`, `crates/dkod-cli/tests/e2e_lazy_init.rs`.
  Message: `feat(lazy): dkod fetch + body backfill + offline metadata degradation in show/drift`

### Task 3.3: lazy-deferral proof e2e + phase close-out

**Files:**
- Test: `crates/dkod-cli/tests/e2e_lazy_init.rs` (append)

- [ ] **Step 1: Write the test** — the honest §9.2 claim ("bodies above the
  limit are deferred, metadata mirrors always"):

```rust
#[test]
fn lazy_subscription_defers_large_bodies_and_backfills_on_demand() {
    // 1. bare remote, allowFilter true.
    // 2. repo A: session whose body.json serializes > 100k (one Tool message
    //    with ~200 KiB of incompressible output, built with the noisy()
    //    pattern from finalize tests; budget doesn't apply — write_session
    //    directly, not finalize). dkod push.
    // 3. repo B: git init; remote add origin <bare>; dkod init (lazy);
    //    `dkod fetch`.
    // 4. PROOF OF DEFERRAL: `git -C B rev-list --objects --missing=print
    //    refs/dkod/index | grep -c '^?'` ≥ 1 (at least one promisor-missing
    //    object — the big body).
    // 5. `dkod log` in B works offline-from-metadata (lists the session id).
    // 6. `dkod fetch <id>` then `dkod show <id>` prints the transcript.
}
```

Write the full body. If step 4 proves flaky across git versions, assert
instead that the body blob oid (`dkod_core::store::body_blob_oid`) is NOT
present via `git cat-file -e <oid>` exit status with
`GIT_NO_LAZY_FETCH=1` in the env (git ≥2.42) — pick whichever works, keep the
deferral assertion.

- [ ] **Step 2: Run green.** Full suite.
- [ ] **Step 3: Gate & commit ritual.** Files: `crates/dkod-cli/tests/e2e_lazy_init.rs`.
  Message: `test(lazy): end-to-end deferral + backfill proof for the dkod-archive subscription`
- [ ] **Step 4: Phase close-out.** Gates; `/coderabbit:review --base main`;
  `gh auth switch --user haim-ari`; push `feat/storage-v2-phase3`; PR
  `feat: storage v2 phase 3 — lazy metadata-only propagation`; merge; sync.

---

## Phase 4 — retention + legacy retirement (§15.4)

Branch `feat/storage-v2-phase4` off updated `main`.

### Task 4.1: gc core — pruned orphan snapshot + epoch bump (§8.1)

**Files:**
- Modify: `crates/dkod-core/src/index.rs`

- [ ] **Step 1: Write the failing tests** (in `index.rs`):

```rust
#[cfg(test)]
mod gc_tests {
    use super::*;
    use tempfile::TempDir;

    fn v7_at(secs: u64) -> String {
        uuid::Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, secs, 0)).to_string()
    }

    /// Seed a session id with a commit pointer into a fresh repo's index.
    fn seed(repo_path: &std::path::Path, id: &str, sha: &str) {
        let mut b = IndexBatch::new(format!("seed {id}"));
        b.insert(format!("{}/meta.json", session_dir_for(id, 0)), b"{}".to_vec());
        b.insert(format!("{}/body.json", session_dir_for(id, 0)), b"{}".to_vec());
        b.insert(commit_pointer_path(sha), format!("{id}\n").into_bytes());
        apply_batch(repo_path, &b).unwrap();
    }

    #[test]
    fn gc_prune_drops_old_sessions_their_pointers_and_bumps_epoch() {
        let tmp = TempDir::new().unwrap();
        let mut r = gix::init(tmp.path()).unwrap();
        crate::store::ensure_committer(&mut r).unwrap();
        let old_id = v7_at(1735689600); // 2025-01-01
        let new_id = v7_at(1781049600); // 2026-06-10
        seed(tmp.path(), &old_id, "00000000000000000000000000000000000000aa");
        seed(tmp.path(), &new_id, "00000000000000000000000000000000000000bb");

        // cutoff: 2026-01-01 → drops the 2025 session
        let outcome = gc_prune(tmp.path(), 1767225600 * 1000, false, "30d").unwrap();
        assert_eq!(outcome.kept_sessions, 1);
        assert_eq!(outcome.dropped_sessions, 1);
        assert_eq!(outcome.dropped_pointers, 1);
        assert_eq!(outcome.new_epoch, 1);

        let repo = gix::open(tmp.path()).unwrap();
        let tip = index_tip(&repo).unwrap();
        let c = repo.find_object(tip).unwrap().try_into_commit().unwrap();
        assert_eq!(c.parent_ids().count(), 0, "orphan snapshot — history reset (§8.1)");
        let root = commit_tree(&repo, tip).unwrap();
        assert!(read_tree_blob(&repo, root, &body_path(&new_id).unwrap()).is_some());
        assert!(read_tree_blob(&repo, root, &body_path(&old_id).unwrap()).is_none());
        assert!(
            read_tree_blob(&repo, root, &commit_pointer_path("00000000000000000000000000000000000000aa")).is_none(),
            "pointer to a dropped session is dropped"
        );
        assert!(
            read_tree_blob(&repo, root, &commit_pointer_path("00000000000000000000000000000000000000bb")).is_some()
        );
        assert_eq!(read_tree_blob(&repo, root, "epoch").unwrap(), b"1\n".to_vec());
        assert_eq!(read_epoch(&repo, tip), Some(1));
    }

    #[test]
    fn gc_prune_dry_run_reports_without_moving_the_ref() {
        let tmp = TempDir::new().unwrap();
        let mut r = gix::init(tmp.path()).unwrap();
        crate::store::ensure_committer(&mut r).unwrap();
        let old_id = v7_at(1735689600);
        seed(tmp.path(), &old_id, "00000000000000000000000000000000000000aa");
        let before = index_tip(&gix::open(tmp.path()).unwrap()).unwrap();
        let outcome = gc_prune(tmp.path(), 1767225600 * 1000, true, "30d").unwrap();
        assert_eq!(outcome.dropped_sessions, 1);
        assert_eq!(index_tip(&gix::open(tmp.path()).unwrap()), Some(before), "dry run");
    }

    #[test]
    fn is_ancestor_walks_parent_links() {
        let tmp = TempDir::new().unwrap();
        let mut r = gix::init(tmp.path()).unwrap();
        crate::store::ensure_committer(&mut r).unwrap();
        let t1 = apply_batch(tmp.path(), &{
            let mut b = IndexBatch::new("1");
            b.insert("a".into(), b"1".to_vec());
            b
        })
        .unwrap()
        .unwrap();
        let t2 = apply_batch(tmp.path(), &{
            let mut b = IndexBatch::new("2");
            b.insert("b".into(), b"2".to_vec());
            b
        })
        .unwrap()
        .unwrap();
        let repo = gix::open(tmp.path()).unwrap();
        assert!(is_ancestor(&repo, t1, t2));
        assert!(!is_ancestor(&repo, t2, t1));
        assert!(is_ancestor(&repo, t1, t1), "self is its own ancestor");
    }
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p dkod-core gc_` — FAIL.
- [ ] **Step 3: Implement** in `index.rs`:

```rust
/// `dkod gc --keep` outcome (§8.1).
#[derive(Debug)]
pub struct GcOutcome {
    pub kept_sessions: usize,
    pub dropped_sessions: usize,
    pub dropped_pointers: usize,
    pub new_epoch: u64,
    /// None on dry runs.
    pub new_tip: Option<gix::ObjectId>,
}

/// `epoch` blob at an index tip, parsed.
pub fn read_epoch(repo: &gix::Repository, tip: gix::ObjectId) -> Option<u64> {
    let root = commit_tree(repo, tip).ok()?;
    let bytes = read_tree_blob(repo, root, "epoch")?;
    String::from_utf8_lossy(&bytes).trim().parse().ok()
}

/// Manual parent-walk ancestry test (no gix `revision` feature). True iff
/// `anc` is reachable from `desc` (inclusive).
pub fn is_ancestor(repo: &gix::Repository, anc: gix::ObjectId, desc: gix::ObjectId) -> bool {
    let mut queue = std::collections::VecDeque::from([desc]);
    let mut seen = std::collections::HashSet::new();
    while let Some(oid) = queue.pop_front() {
        if oid == anc {
            return true;
        }
        if !seen.insert(oid) {
            continue;
        }
        let Ok(obj) = repo.find_object(oid) else { continue };
        let Ok(commit) = obj.try_into_commit() else { continue };
        for p in commit.parent_ids() {
            queue.push_back(p.detach());
        }
    }
    false
}

/// §8.1 steps 1+3+4: drop session date-dirs older than the cutoff (the
/// v7-id timestamp is authoritative for boundary days; non-v7 ids fall back
/// to their date-dir name), drop every commits/patchid pointer whose embedded
/// session id was dropped, bump `epoch`, write the pruned tree as a NEW
/// ORPHAN ROOT commit (history reset — old blobs become unreachable so hosts
/// can actually reclaim), and move the local ref (dkod-driven non-FF, §5.5).
/// Bundle export and force-push are the CLI's job (§8.1 step 2).
/// `keep_label` is the human `--keep` argument (e.g. "90d"), used only in
/// the §4.1 commit message.
pub fn gc_prune(
    repo_path: &Path,
    cutoff_unix_ms: u64,
    dry_run: bool,
    keep_label: &str,
) -> Result<GcOutcome>
```

Body specification:

1. Open repo, `ensure_committer`, take the `IndexLock`.
2. `tip = index_tip` (no index → `Err("no index to gc — run dkod reindex first")`),
   `root = commit_tree`.
3. Decide survivors: `cutoff_date = date_from_unix_ms(cutoff_unix_ms)`. For
   each `date` in `list_tree_dir(repo, root, "sessions")`: `date < cutoff_date`
   (string compare — ISO dates sort) → all its ids are candidates;
   `date == cutoff_date` → per-id check `uuid_v7_unix_ms(id)` (None → keep:
   the dir name already equals the cutoff date and we cannot do better);
   `date > cutoff_date` → keep all. Collect `kept: BTreeSet<String>` (ids) and
   `dropped: BTreeSet<String>`.
4. Build the pruned tree FROM SCRATCH with `upsert_path` starting at `None`:
   re-insert `version` (existing blob oid), `epoch` (NEW blob
   `format!("{}\n", old_epoch + 1)`), every kept session's `meta.json` +
   `body.json` (existing blob oids — no blob rewriting), and every pointer in
   `commits/*`/`patchid/*` whose blob content (trimmed) is a kept id (read
   each pointer blob; count the dropped ones). Walk pointers via
   `list_tree_dir(repo, root, "commits")` (buckets) +
   `tree_entries` of each bucket (blob entries).
5. `dry_run` → return counts with `new_tip: None`, current epoch + 1 reported,
   ref untouched.
6. Write the orphan commit (`build_commit(repo, None, ...)` ALREADY seeds
   version/epoch — so instead call the lower-level pieces: write the pruned
   tree from step 4, then a `gix::objs::Commit` with `parents: SmallVec::new()`
   and the §4.1 message
   `format!("dkod: gc --keep {keep_label} (kept {kept}, dropped {dropped})")`),
   and move the ref with `commit_adopt(repo, new_tip, Some(tip))`.
7. Return the outcome.

Implementation note for step 6: factor a
`fn write_snapshot_commit(repo, tree: gix::ObjectId, message: &str) -> Result<gix::ObjectId>`
(signature + commit object + `write_object`) so `build_commit` and gc share
the commit-writing tail; `build_commit` keeps its seeding behavior.

- [ ] **Step 4: Run green.** `cargo test -p dkod-core gc_` — 3 PASS.
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-core/src/index.rs`.
  Message: `feat(gc): index prune to an orphan snapshot with epoch bump (core)`

### Task 4.2: `dkod gc --keep <duration>` CLI (bundle, dry-run, force-push, caveats)

**Files:**
- Create: `crates/dkod-cli/src/cmd/gc.rs`
- Modify: `crates/dkod-cli/src/cmd/mod.rs` (`pub mod gc;`), `crates/dkod-cli/src/main.rs`
- Test: `crates/dkod-cli/tests/e2e_gc.rs` (new)

- [ ] **Step 1: Write the failing unit + e2e tests.**

In `gc.rs` (unit):

```rust
#[cfg(test)]
mod tests {
    use super::parse_keep;

    #[test]
    fn parses_days_and_weeks() {
        assert_eq!(parse_keep("90d").unwrap(), 90 * 86_400);
        assert_eq!(parse_keep("12w").unwrap(), 12 * 7 * 86_400);
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "90", "d", "90x", "-3d", "1.5d"] {
            assert!(parse_keep(bad).is_err(), "{bad:?} must be rejected");
        }
    }
}
```

`tests/e2e_gc.rs`:

```rust
//! End-to-end §8: gc drops out-of-window sessions, exports a restorable
//! bundle first, force-pushes the pruned index, and prints the host-side
//! reclamation caveat.

#[test]
fn gc_keep_drops_old_sessions_and_force_pushes() {
    // 1. bare origin; work repo with dual-write OFF (.dkod/config.toml:
    //    [storage] write_legacy_refs = false — so the prune is observable
    //    through dkod log; gc does not touch legacy refs and SAYS so).
    // 2. seed an OLD session: Session literal whose id is
    //    uuid::Uuid::new_v7(Timestamp::from_unix(NoContext, 1735689600, 0))
    //    (2025-01-01) and a NEW session (Session::new_id()); write_session
    //    both; `dkod push`.
    // 3. `dkod gc --keep 30d --bundle archive.bundle`:
    //    - exit 0; stdout mentions "kept 1, dropped 1";
    //    - stdout contains "Hosts reclaim unreachable objects on their own
    //      schedule" (the §8.2 caveat);
    //    - archive.bundle exists and `git bundle verify archive.bundle` passes;
    //    - `dkod log` no longer lists the old id, still lists the new id;
    //    - `git ls-remote <bare> refs/dkod/index` equals the LOCAL new tip
    //      (force-push happened).
    // 4. `dkod gc --keep 30d --dry-run` now reports dropped 0.
}

#[test]
fn gc_warns_when_legacy_refs_would_resurrect_dropped_sessions() {
    // repo with dual-write ON (default) and one old legacy+indexed session:
    // `dkod gc --keep 30d` exit 0 and stderr/stdout contains
    // "legacy refs remain" + "reindex --delete-legacy".
}
```

- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement `gc.rs`.**

```rust
//! `dkod gc --keep <duration> [--bundle <path>] [--dry-run]` (storage-v2 §8).

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::Command;

/// Parse `<n>d` / `<n>w` into seconds. Days and weeks only — months are
/// ambiguous and YAGNI.
pub(crate) fn parse_keep(s: &str) -> Result<u64> {
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let n: u64 = num.parse().map_err(|_| anyhow!("invalid --keep {s:?} (use e.g. 90d or 12w)"))?;
    match unit {
        "d" => Ok(n * 86_400),
        "w" => Ok(n * 7 * 86_400),
        _ => Err(anyhow!("invalid --keep {s:?} (use e.g. 90d or 12w)")),
    }
}

pub fn run(cwd: &Path, keep: &str, bundle: Option<&Path>, dry_run: bool) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;
    let keep_secs = parse_keep(keep)?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock before 1970")?
        .as_millis() as u64;
    let cutoff_ms = now_ms.saturating_sub(keep_secs * 1000);

    // §8.1 step 2: bundle BEFORE anything is destroyed.
    if let (Some(path), false) = (bundle, dry_run) {
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["bundle", "create"])
            .arg(path)
            .arg("refs/dkod/index")
            .output()
            .context("spawn git bundle")?;
        if !out.status.success() {
            return Err(anyhow!(
                "bundle export failed (nothing was pruned): {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        println!("dkod gc: wrote pre-prune archive to {}", path.display());
    }

    let o = dkod_core::index::gc_prune(cwd, cutoff_ms, dry_run, keep)?;
    let verb = if dry_run { "would drop" } else { "dropped" };
    println!(
        "dkod gc: --keep {keep}: kept {}, {verb} {} session(s) ({} stale pointer(s)); epoch -> {}",
        o.kept_sessions, o.dropped_sessions, o.dropped_pointers, o.new_epoch
    );
    if dry_run {
        return Ok(());
    }

    // Legacy refs are NOT touched by gc — warn when they would resurrect
    // dropped sessions through the permanent fallback (§6.2).
    let legacy = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["for-each-ref", "--count=1", "refs/dkod/sessions/"])
        .output()
        .context("spawn git for-each-ref")?;
    if !String::from_utf8_lossy(&legacy.stdout).trim().is_empty() {
        println!(
            "dkod gc: legacy refs remain (refs/dkod/sessions/*) — dropped sessions stay readable \
             through them until you run `dkod reindex --delete-legacy`"
        );
    }

    // Force-push: the ONLY force-push in dkod (§8.1 step 4); clients adopt
    // via the epoch check (§8.3).
    let push = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["push", "--quiet", "--force", "origin", "refs/dkod/index:refs/dkod/index"])
        .output()
        .context("spawn git push")?;
    if push.status.success() {
        println!("dkod gc: force-pushed the pruned index to origin");
    } else {
        println!(
            "dkod gc: force-push failed ({}) — re-run `dkod gc --keep {keep}` when online \
             (idempotent), or push manually: git push --force origin refs/dkod/index",
            String::from_utf8_lossy(&push.stderr).trim()
        );
    }

    // §8.2, printed verbatim so nobody files "gc didn't shrink my repo":
    println!(
        "dkod gc: note — Hosts reclaim unreachable objects on their own schedule: GitHub size \
         reporting lags days-to-weeks; GitLab needs housekeeping; Gitea/Bitbucket follow their \
         own gc cadence. Local repos reclaim via `git gc`. Quota-capped hosts under acute \
         pressure may need a support ticket to realize the reduction promptly."
    );
    Ok(())
}
```

`main.rs`:

```rust
    /// Drop sessions older than the retention window from the index
    /// (tree prune + history reset, then force-push). Legacy refs are not
    /// touched; see reindex --delete-legacy.
    Gc {
        /// Retention window, e.g. 90d or 12w.
        #[arg(long)]
        keep: String,
        /// Write a `git bundle` archive of the pre-prune index first.
        #[arg(long)]
        bundle: Option<std::path::PathBuf>,
        /// Report what would be dropped without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
```

dispatch: `Cmd::Gc { keep, bundle, dry_run } => cmd::gc::run(&std::env::current_dir()?, &keep, bundle.as_deref(), dry_run),`.

- [ ] **Step 4: Run green.** Full suite.
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-cli/src/cmd/gc.rs`, `crates/dkod-cli/src/cmd/mod.rs`, `crates/dkod-cli/src/main.rs`, `crates/dkod-cli/tests/e2e_gc.rs`.
  Message: `feat(gc): dkod gc --keep with bundle export, force-push, and host-reclamation caveats`

### Task 4.3: epoch adoption after gc (§8.3)

**Files:**
- Modify: `crates/dkod-cli/src/cmd/push.rs` (the reconcile branch)
- Modify: `crates/dkod-cli/src/cmd/fetch.rs` (index fetch path)
- Test: `crates/dkod-cli/tests/e2e_gc.rs` (append)

- [ ] **Step 1: Write the failing e2e:**

```rust
#[test]
fn stale_client_adopts_gced_index_and_replays_local_sessions() {
    // 1. bare origin; repo A (dual-write off) seeds OLD (2025 v7 id) + pushes.
    // 2. repo B: clone-equivalent (git init + remote add + `dkod fetch`);
    //    B seeds a LOCAL session SB (unpushed).
    // 3. A: `dkod gc --keep 30d` (drops OLD, epoch 0→1, force-push).
    // 4. B: `dkod push` → must succeed: B detects remote-not-descendant +
    //    remote epoch 1 > local 0 → adopts the pruned tip, replays SB on top,
    //    pushes.
    // 5. `git ls-remote <bare> refs/dkod/index` resolved in a fresh clone C:
    //    `dkod log` lists SB, not OLD.
    // 6. B: `dkod fetch` after all this is a no-op success.
}

#[test]
fn unrelated_equal_epoch_histories_are_refused() {
    // bare origin seeded by repo A (epoch 0). Repo B builds its OWN index
    // root (epoch 0, unrelated history) and runs `dkod push` → after the
    // §8.3 check the push must FAIL with a message containing
    // "diverged at the same epoch" and "dkod doctor".
}
```

Wait — §5.4 says a push race replays onto the remote tip; §8.3's refusal is
scoped to histories that diverge WITHOUT an epoch step. The resolution this
plan implements (recorded in "Design ambiguities resolved"): on push
reconcile, if the remote tip is a descendant of the local tip → up-to-date;
if local is ancestor of remote or they share history → replay (§5.4); if
histories are UNRELATED (no common root) AND epochs are equal → refuse with
the doctor message (§8.3); if remote epoch > local epoch → adopt + replay
(§8.3); if remote epoch < local epoch → our gc is newer → force-push.

- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement.** In `push.rs::try_push`, replace the unconditional
  `reconcile_onto` call with:

```rust
        let local_tip = dkod_core::index::index_tip(&repo).expect("checked above");
        let local_epoch = dkod_core::index::read_epoch(&repo, local_tip).unwrap_or(0);
        let remote_epoch = dkod_core::index::read_epoch(&repo, remote_tip).unwrap_or(0);
        let related = dkod_core::index::is_ancestor(&repo, remote_tip, local_tip)
            || dkod_core::index::is_ancestor(&repo, local_tip, remote_tip)
            || dkod_core::index::share_root(&repo, local_tip, remote_tip);
        if remote_epoch > local_epoch {
            // §8.3: a gc happened — adopt the pruned tip, replay local-only
            // sessions on top (one non-negotiated fetch of the now-small
            // archive already happened above).
            dkod_core::index::reconcile_onto(cwd, remote_tip)?;
        } else if remote_epoch < local_epoch {
            // our gc is newer than the remote — force-push the pruned index
            let f = git_out(cwd, &["push", "--quiet", "--force", "origin", "refs/dkod/index:refs/dkod/index"])?;
            if f.status.success() {
                return Ok(PushOutcome::Pushed);
            }
            last = String::from_utf8_lossy(&f.stderr).trim().to_string();
        } else if related {
            dkod_core::index::reconcile_onto(cwd, remote_tip)?; // plain §5.4 race
        } else {
            return Err(anyhow!(
                "local and remote session indexes diverged at the same epoch — \
                 this should not happen; run `dkod doctor` (or `dkod reindex` to rebuild)"
            ));
        }
```

Add to `index.rs` (with a unit test in `gc_tests` asserting two
`apply_batch`-built tips share a root and an independently `build_commit`-built
root does not):

```rust
/// True iff the two commits reach a common root (cheap shared-history test:
/// walk both parent chains, compare ROOT sets — index roots are unique per
/// §4.1 unless someone rebuilt independently).
pub fn share_root(repo: &gix::Repository, a: gix::ObjectId, b: gix::ObjectId) -> bool {
    fn roots(repo: &gix::Repository, from: gix::ObjectId) -> std::collections::HashSet<gix::ObjectId> {
        let mut queue = std::collections::VecDeque::from([from]);
        let mut seen = std::collections::HashSet::new();
        let mut roots = std::collections::HashSet::new();
        while let Some(oid) = queue.pop_front() {
            if !seen.insert(oid) {
                continue;
            }
            let Ok(obj) = repo.find_object(oid) else { continue };
            let Ok(commit) = obj.try_into_commit() else { continue };
            let mut parents = commit.parent_ids().peekable();
            if parents.peek().is_none() {
                roots.insert(oid);
            }
            for p in parents {
                queue.push_back(p.detach());
            }
        }
        roots
    }
    !roots(repo, a).is_disjoint(&roots(repo, b))
}
```

In `fetch.rs` (index fetch path, `id == None`): fetch into `FETCH_TMP_REF`
(reuse the constant — move it to `fetch.rs` and re-export to `push.rs`, or
duplicate the literal with a comment), then run the SAME epoch/relatedness
decision via a shared helper. Factor it:

```rust
/// Shared §8.3 adoption decision used by push (after a rejected push) and
/// fetch (after fetching a remote tip). Moves the local ref as required.
pub(crate) fn adopt_or_reconcile(cwd: &Path, remote_tip: gix::ObjectId) -> Result<AdoptOutcome>
pub(crate) enum AdoptOutcome { UpToDate, Reconciled, AdoptedAfterGc, LocalAhead, RefusedUnrelated }
```

Put `adopt_or_reconcile` in `push.rs`, call it from both; `fetch.rs` maps
`RefusedUnrelated` to the same doctor error and prints
`dkod: a retention gc happened upstream — adopted the pruned index` on
`AdoptedAfterGc`. `dkod fetch`'s plain fast-forward case is `Reconciled`/`UpToDate`.

- [ ] **Step 4: Run green.** Full suite (notably `e2e_push.rs`: the
  divergent-writers test now flows through the `related` branch — same
  observable behavior).
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-cli/src/cmd/push.rs`, `crates/dkod-cli/src/cmd/fetch.rs`, `crates/dkod-core/src/index.rs`, `crates/dkod-cli/tests/e2e_gc.rs`.
  Message: `feat(gc): epoch-aware adoption after retention gc in push/fetch (§8.3)`

### Task 4.4: `dkod reindex --delete-legacy` (§10.5)

**Files:**
- Modify: `crates/dkod-core/src/store.rs` (verification pass)
- Modify: `crates/dkod-cli/src/cmd/reindex.rs`, `crates/dkod-cli/src/main.rs`
- Test: `crates/dkod-cli/tests/e2e_reindex.rs` (append)

- [ ] **Step 1: Write the failing tests.**

Core (`store.rs`):

```rust
#[test]
fn verify_legacy_indexed_reports_unindexed_refs() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let s = fixture_session();
    write_session(tmp.path(), &s).unwrap(); // index + legacy
    assert!(verify_legacy_indexed(tmp.path()).unwrap().is_empty(), "fully indexed");

    // Add a legacy-only ref (plumbing-style) → must be reported.
    let repo = gix::open(tmp.path()).unwrap();
    let mut s2 = fixture_session();
    s2.id = Session::new_id();
    let blob = repo.write_blob(&serde_json::to_vec(&s2).unwrap()).unwrap().detach();
    write_link_ref(&repo, crate::refs::session_ref(&s2.id), blob, "seed".into()).unwrap();
    let missing = verify_legacy_indexed(tmp.path()).unwrap();
    assert_eq!(missing, vec![crate::refs::session_ref(&s2.id)]);
}
```

e2e (`e2e_reindex.rs`):

```rust
#[test]
fn delete_legacy_removes_refs_remotely_and_locally_after_verification() {
    // 1. bare origin; work repo seeds 1 session via write_session (dual-write
    //    → index + legacy); push EVERYTHING:
    //    `git push origin '+refs/dkod/*:refs/dkod/*'`.
    // 2. `dkod reindex --delete-legacy` → exit 0; stdout contains the
    //    "all readers must run" warning (§10.5/§12).
    // 3. local: `git for-each-ref refs/dkod/sessions refs/dkod/commits
    //    refs/dkod/patchid` is empty; refs/dkod/index remains.
    // 4. remote: `git ls-remote <bare> 'refs/dkod/sessions/*'` empty;
    //    `refs/dkod/index` present.
    // 5. `dkod show <id>` still works (index serves it).
}

#[test]
fn delete_legacy_refuses_when_a_legacy_ref_is_not_indexed() {
    // seed one indexed session AND one plumbing-only legacy ref that was
    // never reindexed... then run `dkod reindex --delete-legacy`. Note the
    // command reindexes FIRST (it folds the stray ref), so to force the
    // refusal, seed a legacy ref whose blob is NOT valid session JSON
    // (reindex skips it with a warning) → --delete-legacy must refuse with
    // "not reachable from the index" and delete NOTHING.
}
```

- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement.**

`store.rs`:

```rust
/// §10.5 verification pass: every legacy ref whose CONTENT is not reachable
/// from the current index tip. Sessions verify by body-blob oid equality
/// (or scan-path presence for non-v7 ids); commit/patchid refs verify by
/// pointer-path presence naming the same session id. Empty result = safe to
/// delete legacy refs.
pub fn verify_legacy_indexed(repo_path: &Path) -> Result<Vec<String>>
```

`cmd/reindex.rs::run(cwd, dry_run, delete_legacy)`:

1. Run the existing fold (always — it makes deletion safe by construction).
2. If `!delete_legacy` → done (existing output).
3. Print the §12 warning and proceed (non-interactive by design; the flag IS
   the consent): `dkod reindex: --delete-legacy retires the v1 refs — every reader of this repo must run a dkod version with storage v2 (>= the version shipping this flag). Old dkod CLIs will no longer see sessions.`
4. `verify_legacy_indexed` → non-empty → `Err` listing up to 10 offending
   refs + `run dkod reindex again or remove the corrupt refs manually; nothing was deleted`.
5. Collect all legacy refs (three prefixes). Remote deletion first, batched
   100 per invocation: `git push origin --delete <ref>...`; a failure (no
   remote, offline) prints
   `dkod reindex: remote deletion failed/skipped (<reason>) — local refs deleted; re-run when online to retire them remotely`
   but continues. Local deletion via one `git update-ref --stdin` feed of
   `delete <ref>\n` lines (`Stdio::piped`).
6. Set `write_legacy_refs = false` alongside `format = "v2"` in
   `.dkod/config.toml` (the team just committed to v2 — §10.5).

`main.rs`: add `#[arg(long)] delete_legacy: bool` to `Reindex` and thread it.

- [ ] **Step 4: Run green.** Full suite.
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-core/src/store.rs`, `crates/dkod-cli/src/cmd/reindex.rs`, `crates/dkod-cli/src/main.rs`, `crates/dkod-cli/tests/e2e_reindex.rs`.
  Message: `feat(reindex): --delete-legacy retires v1 refs after index verification`

### Task 4.5: dual-write default flips OFF (§12, minor version)

**Files:**
- Modify: `crates/dkod-core/src/config.rs`
- Modify: the test fixtures listed below (adding explicit config, NOT changing assertions)
- Modify: `Cargo.toml` workspace version fields are NOT here — version bumps happen at release tagging per repo convention; note it in the PR body instead.

- [ ] **Step 1: Flip + failing test.** In `config.rs`, change
  `StorageConfig::default()` to `write_legacy_refs: false` and update the
  Task 1.1 test `defaults_storage_to_dual_write_on_with_no_format` → rename to
  `defaults_storage_to_index_only_with_no_format`, asserting
  `!c.storage.write_legacy_refs` (this is the ONE Phase-1-created test whose
  assertion legitimately flips — it specifies the default).
- [ ] **Step 2: Inventory the fallout.** Run `cargo test` and list every
  failure. Expected failures are exactly the tests that assert legacy refs
  exist after using the PUBLIC writers (they relied on the default):
  - `crates/dkod-core/src/store.rs`: `write_creates_session_ref`,
    `link_session_to_commit_writes_ref_pointing_at_session_blob`,
    `write_session_with_commit_links_populates_commits_and_links` (the
    `commit_ref` assertions), `relink_commit_*` (legacy-ref assertions),
    `link_session_to_patchid_*`, `reindex_legacy_refs_folds_sessions_and_links_idempotently`
    (seeds via dual-write).
  - `crates/dkod-cli/tests/`: `finalize_session.rs` (the `sref`/`cref`
    assertions + the Task 1.8 patchid-for-each-ref assertion),
    `e2e_relink.rs`, `e2e_blame.rs` + `e2e_patchid_blame.rs` only if they
    assert refs rather than behavior (they seed via `link_session_to_commit`
    and assert dkod OUTPUT — re-check; output-asserting tests keep passing
    via the index), `init_session_discovery.rs` (source repo pushes
    `+refs/dkod/*`), `e2e_reindex.rs`, `e2e_gc.rs`
    (`gc_warns_when_legacy_refs_would_resurrect_dropped_sessions`),
    `smoke.rs`/`cli.rs` if they assert session refs.
  For EVERY failing test: do NOT weaken the assertion — add this fixture line
  to the test repo setup instead (the test now explicitly exercises the
  dual-write configuration it always implicitly assumed):

```rust
std::fs::create_dir_all(repo_path.join(".dkod")).unwrap();
std::fs::write(repo_path.join(".dkod/config.toml"), "[storage]\nwrite_legacy_refs = true\n").unwrap();
```

  In `store.rs` unit tests, add a tiny `fn enable_dual_write(repo_path: &Path)`
  helper doing exactly that and call it after `gix::init` in the affected
  tests. The Task 1.6 test
  `write_session_with_legacy_disabled_writes_index_only` keeps its explicit
  `false` config and stays green; add its mirror
  `write_session_with_legacy_enabled_writes_both` if not already covered by
  the updated `write_creates_session_ref`.
- [ ] **Step 3: Verify the count.** After fixture updates, `cargo test` is
  fully green and `git diff --stat` touches ONLY test code + `config.rs`.
  Sanity bound: if more than ~15 test files needed the fixture, something else
  broke — stop and investigate.
- [ ] **Step 4: Release-note marker.** Append to the PR body draft (and the
  phase commit message body): `BREAKING-ish: new captures no longer write
  refs/dkod/sessions|commits|patchid/* by default. Old dkod CLIs cannot see
  NEW sessions in repos that capture with this version unless
  .dkod/config.toml sets [storage] write_legacy_refs = true. Reads of
  existing legacy refs are unaffected (permanent fallback).`
- [ ] **Step 5: Gate & commit ritual.** Files: `crates/dkod-core/src/config.rs` + the touched test files.
  Message: `feat(storage)!: dual-write defaults off — index-only captures (legacy readable forever)`
- [ ] **Step 6: Phase close-out.** Gates; `/coderabbit:review --base main`;
  `gh auth switch --user haim-ari`; push `feat/storage-v2-phase4`; PR
  `feat: storage v2 phase 4 — retention gc, epoch adoption, legacy retirement`
  with the release note from Step 4 verbatim; merge; sync main. Tag the minor
  release per repo convention (`git tag -a v<x.Y+1.0>`) after merge.

---

## Phase 5 (optional polish — cut freely if the months-2–3 timeline is tight, §15.5)

Branch `feat/storage-v2-phase5` off updated `main`.

### Task 5.1: `dkod doctor` — index integrity check

**Files:**
- Create: `crates/dkod-cli/src/cmd/doctor.rs`
- Modify: `crates/dkod-core/src/index.rs` (the check core), `crates/dkod-cli/src/cmd/mod.rs`, `crates/dkod-cli/src/main.rs`
- Test: `crates/dkod-cli/tests/e2e_doctor.rs` (new)

- [ ] **Step 1: Failing core tests** (`index.rs`):

```rust
#[cfg(test)]
mod doctor_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn healthy_index_reports_no_problems() {
        let tmp = TempDir::new().unwrap();
        let mut r = gix::init(tmp.path()).unwrap();
        crate::store::ensure_committer(&mut r).unwrap();
        let id = uuid::Uuid::now_v7().to_string();
        let mut b = IndexBatch::new("seed");
        b.insert(format!("{}/meta.json", session_dir_for(&id, 0)), b"{}".to_vec());
        b.insert(format!("{}/body.json", session_dir_for(&id, 0)), b"{}".to_vec());
        b.insert(commit_pointer_path("00000000000000000000000000000000000000aa"), format!("{id}\n").into_bytes());
        apply_batch(tmp.path(), &b).unwrap();
        assert!(check_index(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn dangling_pointer_and_bad_version_are_reported() {
        let tmp = TempDir::new().unwrap();
        let mut r = gix::init(tmp.path()).unwrap();
        crate::store::ensure_committer(&mut r).unwrap();
        let mut b = IndexBatch::new("seed");
        // pointer to a session that does not exist
        b.insert(commit_pointer_path("00000000000000000000000000000000000000aa"),
                 format!("{}\n", uuid::Uuid::now_v7()).into_bytes());
        b.insert("version".into(), b"99\n".to_vec());
        apply_batch(tmp.path(), &b).unwrap();
        let problems = check_index(tmp.path()).unwrap();
        assert!(problems.iter().any(|p| p.contains("version")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("dangling")), "{problems:?}");
    }

    #[test]
    fn no_index_is_not_a_problem() {
        let tmp = TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        assert!(check_index(tmp.path()).unwrap().is_empty(), "v1 repos are healthy");
    }
}
```

(Note `apply_batch` skips inserts whose oid already matches — writing
`version` = `99\n` DIFFERS from the seeded `1\n`, so the upsert applies; this
is deliberate corruption.)

- [ ] **Step 2: Implement core:**

```rust
/// Read-only integrity scan (§15.5 / §8.3's "diagnose" half): returns
/// human-readable problem strings, empty = healthy. Checks: version blob is
/// "1"; epoch parses; every sessions/<date>/<id>/ dir has meta.json AND
/// body.json with parseable JSON meta; every commits/ + patchid/ pointer
/// names a session present in the index; date dir names parse as dates.
pub fn check_index(repo_path: &Path) -> Result<Vec<String>>
```

Body: open repo; no tip → `Ok(vec![])`. Walk per the doc-comment using
`list_tree_dir`/`tree_entries`/`read_tree_blob`; dangling pointer message
format: `commits/<bucket>/<rest>: dangling pointer to unknown session <id>`.

- [ ] **Step 3: CLI** `cmd/doctor.rs`:

```rust
//! `dkod doctor` — read-only index integrity report. Exit 0 healthy, exit 1
//! with findings (scripts can gate on it). Rebuild guidance only — never
//! mutates.

use anyhow::{anyhow, Result};
use std::path::Path;

pub fn run(cwd: &Path) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;
    let problems = dkod_core::index::check_index(cwd)?;
    if problems.is_empty() {
        println!("dkod doctor: index healthy");
        return Ok(());
    }
    for p in &problems {
        println!("dkod doctor: {p}");
    }
    println!(
        "dkod doctor: {} problem(s). Recovery: `git fetch origin +refs/dkod/index:refs/dkod/index` \
         to restore from the remote, or `dkod reindex` to rebuild from legacy refs/outbox, or \
         `dkod reindex --from-bundle <path>` from a gc archive.",
        problems.len()
    );
    Err(anyhow!("index integrity problems found"))
}
```

`main.rs`: visible `Doctor` variant (`/// Check the session index for corruption.`),
dispatch `Cmd::Doctor => cmd::doctor::run(&std::env::current_dir()?),`.

- [ ] **Step 4: e2e** (`tests/e2e_doctor.rs`): `doctor_healthy_exits_zero`
  (seeded repo → exit 0, stdout "healthy") and `doctor_reports_dangling_pointer`
  (corrupt via the core test's recipe written through `dkod_core::index`
  test-only path is not possible from e2e — instead seed a pointer with
  `git update-ref`-style plumbing? Not expressible: pointers live IN the tree.
  Simplest honest e2e: run doctor on a healthy repo and on a v1-only repo →
  both exit 0; the corruption paths are covered by the core unit tests).
- [ ] **Step 5: Run green; Gate & commit ritual.** Files: `crates/dkod-core/src/index.rs`, `crates/dkod-cli/src/cmd/doctor.rs`, `crates/dkod-cli/src/cmd/mod.rs`, `crates/dkod-cli/src/main.rs`, `crates/dkod-cli/tests/e2e_doctor.rs`.
  Message: `feat(doctor): read-only index integrity check with rebuild guidance`

### Task 5.2: `dkod reindex --from-bundle` + side-repo how-to doc

**Files:**
- Modify: `crates/dkod-cli/src/cmd/reindex.rs`, `crates/dkod-cli/src/main.rs`
- Create: `docs/side-repo.md`
- Test: `crates/dkod-cli/tests/e2e_reindex.rs` (append)

- [ ] **Step 1: Failing e2e:**

```rust
#[test]
fn reindex_from_bundle_restores_a_gced_archive() {
    // 1. repo A: 1 session; `dkod gc --keep 0d --bundle a.bundle` is wrong
    //    (0d drops everything) — instead: write session, then
    //    `git bundle create a.bundle refs/dkod/index` directly.
    // 2. fresh repo B (git init only): `dkod reindex --from-bundle a.bundle`
    //    → exit 0; `dkod show <id>` works in B.
    // 3. repo C with its OWN session: --from-bundle must MERGE (reconcile
    //    machinery), not clobber: afterwards `dkod log` lists both ids.
}
```

- [ ] **Step 2: Implement.** `reindex.rs` gains
  `from_bundle: Option<&Path>`; when set, BEFORE the legacy fold:
  `git fetch --quiet <bundle-path> +refs/dkod/index:refs/dkod/bundle-tmp`
  (git fetches from bundle files natively), read the tmp ref tip, delete the
  tmp ref, then `crate::cmd::push::adopt_or_reconcile(cwd, bundle_tip)` —
  bundle restore is exactly remote adoption (epoch rules included). Map
  `RefusedUnrelated` to a clear error advising `dkod doctor`. `main.rs`:
  `#[arg(long)] from_bundle: Option<std::path::PathBuf>` on `Reindex`.
- [ ] **Step 3: Write `docs/side-repo.md`** (the §2 "documented escape hatch",
  ~40 lines, no code changes): when to use a side repo (`<repo>-dkod`) —
  Bitbucket hard caps / policy-restricted main repos; setup recipe
  (`git init --bare` the side repo on the host, `dkod init --remote` pointing
  origin at it via a second remote entry named `origin` in a one-line
  caveat… keep it concrete:
  `git remote add dkod-origin <side-url>` + `dkod init --remote dkod-origin`,
  `dkod push` then targets it via config — note plainly that `dkod push`
  targets `origin` in this version and pushing the index to `dkod-origin` is
  `git push dkod-origin refs/dkod/index`); state that the index format is
  identical in a side repo (§2).
- [ ] **Step 4: Run green; Gate & commit ritual.** Files: `crates/dkod-cli/src/cmd/reindex.rs`, `crates/dkod-cli/src/main.rs`, `crates/dkod-cli/tests/e2e_reindex.rs`, `docs/side-repo.md`.
  Message: `feat(reindex): --from-bundle restore via the adoption machinery + side-repo how-to`
  (Note in the report: CodeRabbit does not meaningfully review the `.md` half.)

### Task 5.3: drift benchmark opt-in v2 mode

**Files:**
- Modify: `benchmarks/drift/run.sh`

The benchmark's default path stays plumbing-seeded legacy refs FOREVER (it is
the live guard for the permanent fallback, paired with
`tests/e2e_legacy_fallback.rs`). This task only adds an opt-in second mode so
the index read path gets the same benchmark coverage.

- [ ] **Step 1: Implement.** In `run.sh`, after the `git update-ref` line, add:

```bash
  # Optional storage-v2 mode: fold the legacy ref into the rollup index and
  # delete the legacy ref, so the benchmark exercises the index read path.
  # Default (unset) keeps the historical legacy-ref path — which doubles as
  # the live regression guard for dkod's permanent fallback chain.
  if [[ -n "${DKOD_BENCH_V2:-}" ]]; then
    (cd "$repo" && "$DKOD_BIN" reindex >/dev/null)
    git -C "$repo" update-ref -d "refs/dkod/sessions/$id"
  fi
```

and document both modes in the header comment (`DKOD_BENCH_V2=1 benchmarks/drift/run.sh`).

- [ ] **Step 2: Verify.** Run `benchmarks/drift/run.sh` and
  `DKOD_BENCH_V2=1 benchmarks/drift/run.sh` — identical confusion matrices
  (the analyzer is storage-agnostic; only the read path differs).
- [ ] **Step 3: Gate & commit ritual** (the cargo gates still run; the diff is
  shell-only — note that CodeRabbit reviews shell). Files: `benchmarks/drift/run.sh`.
  Message: `bench(drift): opt-in DKOD_BENCH_V2 mode exercising the index read path`
- [ ] **Step 4: Phase close-out.** Gates; `/coderabbit:review --base main`;
  `gh auth switch --user haim-ari`; push `feat/storage-v2-phase5`; PR
  `feat: storage v2 phase 5 — doctor, bundle restore, side-repo doc, benchmark v2 mode`;
  merge; sync main.

---

## Design ambiguities resolved (decisions an executor must NOT re-open)

1. **Where `truncated: true` comes from (§7 vs §5.2's frozen signatures).**
   The budget runs in the CLI before `write_session_full`, but `Session`
   carries no `truncated` field and gains none (schema blast radius).
   Resolution: `SessionMeta::derive` detects the truncation marker
   (`TRUNCATION_MARKER_PREFIX`) in tool outputs — derived, honest, and keeps
   both the `Session` schema and the `store` signatures untouched.
2. **CAS naming.** The design says `PreviousValue::MustBeExactly`; gix 0.66
   spells it `PreviousValue::MustExistAndMatch(Target)` (and `MustNotExist`
   for root creation). Semantics identical.
3. **Compressed-size measurement needs a deflate implementation.** The design
   mandates measuring compressed size; the tree has no direct flate2 dep.
   Resolution: add `flate2 = "1"` (already compiled via gix's zlib backend —
   no new supply-chain surface). This is the plan's single new dependency.
4. **Push-race replay vs §8.3's "refuse unrelated equal-epoch histories".**
   Two teammates can legitimately create unrelated index roots before either
   pushes (both at epoch 0) — refusing would break first-team-onboarding.
   Resolution (Task 4.3): equal epochs + SHARED root → plain §5.4 replay;
   equal epochs + UNRELATED roots → refuse with the doctor message (§8.3's
   corruption case)… with the explicit carve-out that `reconcile_onto` replay
   is what BOTH the related case and the epoch-step adoption use, so the
   unrelated-roots-at-epoch-0 first-push race resolves like this: the FIRST
   push wins; the second pusher hits the refusal and the error message tells
   them to run `dkod reindex` (which folds their legacy/local content) — and
   since their sessions are local index commits, the practical recovery is
   documented in the error string. If field reports show this is too strict,
   loosening to "replay when both epochs are 0" is a one-line change in
   `try_push` — do not pre-loosen.
5. **Promisor auto-backfill.** The design says git's promisor machinery
   backfills "the moment `dkod show` reads the blob" — true for `git`-binary
   reads, not for gix object reads. Resolution: `dkod show`/drift-detail call
   `cmd::fetch::backfill_body` (an explicit `git fetch <remote> <oid>`) once
   on a failed body read, then degrade to the meta-only header offline. Core
   stays shell-out-free (the repo's layering convention).
6. **gc vs legacy refs.** §8 prunes the INDEX; legacy refs are not gc'd (the
   fallback would resurrect dropped sessions and remote legacy refs would
   still hold host storage). Resolution: `dkod gc` prints the
   "legacy refs remain … reindex --delete-legacy" notice (Task 4.2) instead
   of silently lying about reclamation; retention + retirement compose as
   §15.4 intends.
7. **Reindex push (Phase 1).** §10.4 says reindex pushes, but `dkod push`
   ships in Phase 2. Resolution: Phase 1 reindex prints the manual
   `git push origin refs/dkod/index` hint; Task 2.5 does NOT retrofit an
   auto-push into reindex (explicit command, explicit flush — and capture's
   auto-push carries the index forward anyway).
8. **Opportunistic legacy folding (§12 matrix row 1)** is implemented as
   (a) outbox folding on every write (Task 1.6) and (b) full legacy folding
   only in `dkod reindex` — NOT a full legacy scan on every capture. The
   matrix's "next capture folds local legacy refs" convergence is therefore
   approximated by "next `dkod reindex` run"; rationale: a per-capture
   O(legacy-refs) scan on the hook hot path is the exact latency footgun the
   capture path avoids everywhere else. If the design owner wants true
   per-capture folding, it bolts onto `write_session_full` behind a config
   key later.
9. **`--keep` duration grammar:** `<n>d` and `<n>w` only (months are
   calendar-ambiguous; hours are pointless for retention). Design only ever
   writes `90d`.
10. **`dkod fetch <id>` remote:** uses `dkod-archive` when wired, else
    `origin` — exact-oid fetches require the promisor remote anyway, and the
    full-tier fallback has the blob already (backfill is then a no-op fetch).

## Self-review checklist (controller, before dispatch)

- **Spec coverage vs design §15:** Phase 1 = `IndexBatch`/tree codec (T1.2–1.4),
  dual-write `write_session*` (T1.6), index-first reads + fallback (T1.7),
  `dkod reindex` no-delete (T1.9), lock/CAS (T1.4) ✓. Phase 2 =
  redact→truncate + config (T2.1–2.3), best-effort push + fetch-replay +
  non-forced refspec migration in init (T2.4–2.6) ✓. Phase 3 = dkod-archive
  wiring, probe, tiers, backfill + offline messaging (T3.1–3.3) ✓. Phase 4 =
  gc --keep + bundle + epoch adoption + --delete-legacy + dual-write flip
  (T4.1–4.5) ✓. Phase 5 = doctor, bundle restore, side-repo doc (+ benchmark
  mode) (T5.1–5.3) ✓. §4 format (version/epoch/date-fanout/hex-fanout/
  meta+body/pointer blobs) all in T1.2–1.6 ✓. §5.5 non-forced refspec ✓ T2.4.
  §6.2 permanent fallback ✓ T1.7 + T1.10 guard. §7 ordering
  (redact-then-truncate) ✓ T2.3. §8.2 caveats printed ✓ T4.2. §9.2 option (b)
  meta/body + blob:limit=100k + second remote ✓ T3.1. §10 idempotent reindex
  ✓ T1.9. §12 matrix ✓ (dual-write default T1.1→T4.5; loud warnings T4.4/T4.5).
- **Type/name consistency across tasks (checked):** `IndexBatch::{new,insert,is_empty,len}`;
  `index::{INDEX_REF, INDEX_VERSION, session_dir, session_dir_for, meta_path,
  body_path, commit_pointer_path, patchid_pointer_path, index_tip, commit_tree,
  apply_batch, build_commit, commit_inserts, commit_adopt, read_tree_blob,
  blob_oid_at, list_tree_dir, list_index_session_ids, find_session_dir_by_scan,
  local_only_inserts, reconcile_onto, read_epoch, is_ancestor, share_root,
  gc_prune, GcOutcome, check_index}`; `store::{write_session_full,
  relink_commits, read_session_meta, lookup_commit_session,
  lookup_patchid_session, body_blob_oid, reindex_legacy_refs, ReindexReport,
  verify_legacy_indexed, spill_to_outbox}`; `SessionMeta::derive`,
  `TRUNCATION_MARKER_PREFIX`; `budget::{enforce_budget, BudgetOutcome,
  truncate_output}`; `config::{StorageConfig, CaptureConfig,
  load_storage_config}`; CLI `cmd::{reindex, push, fetch, gc, doctor}::run`
  signatures match their `main.rs` dispatch arms; `PushOutcome`/`AdoptOutcome`
  stay `pub(crate)`.
- **Placeholder scan:** every task has either complete code or a numbered
  exact behavioral spec with signatures; the comment-skeleton e2e tests
  (T1.9 Step 5, T2.5 Step 5, T2.6 Step 2, T3.1 Step 1, T3.2 Step 1 e2e half,
  T3.3 Step 1, T4.2 Step 1 e2e half, T4.3 Step 1, T4.4 Step 1 e2e half,
  T5.2 Step 1) each carry numbered step-by-step assertions that fully
  determine the body — no TBDs, no "similar to task N".
- **Existing-test integrity:** Phases 1 and 3 modify zero existing tests;
  Phase 2 modifies exactly the 6 init-behavior sites enumerated in Background
  facts; Phase 4 Task 4.5 adds config FIXTURES (never weakens assertions) to
  the enumerated legacy-asserting tests; the benchmark + `e2e_legacy_fallback`
  guard the permanent fallback in all phases.
- **Risk flags carried into tasks:** gix `Entry` ordering note (T1.3),
  `IndexLock` timeout-line compile fix (T1.4), probe classification of
  "couldn't find remote ref" (T3.1), `GIT_NO_LAZY_FETCH` fallback assertion
  (T3.3), the §8.3-vs-§5.4 decision table (T4.3), bounded blast-radius check
  (T4.5 step 3).




