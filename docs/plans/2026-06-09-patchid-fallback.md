# Patch-ID Fallback for `dkod blame` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `dkod blame` recover provenance after diff-preserving rewrites the post-rewrite hook never saw (history rewritten before `dkod init`, clones without the hook, `git filter-repo`) by writing `refs/dkod/patchid/<patch-id>` → session blob at capture and falling back to a patch-id match at blame time.

**Architecture:** A pure-gix `store::link_session_to_patchid` writes the new ref namespace. A CLI-layer `cmd::patchid::compute_patch_id` owns the `git patch-id` shell-out (keeping `dkod-core/store` gix-only). `finalize_session` writes a patch-id ref per produced commit (best-effort); `blame.rs::session_for_commit` tries the commit-ref first, then the patch-id fallback. Complementary to the hook (which owns squash + live local rewrites).

**Tech Stack:** Rust (edition 2021), `gix` 0.66, `git` CLI via `std::process::Command`, `clap`, `anyhow`; `assert_cmd` + `tempfile` for tests.

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
6. **YAGNI:** implement only the design. No backfill/reindex, no `dkod gc`,
   no rename-tolerant matching.

---

## Background facts (verified — do not re-discover)

- **patch-id command (spike-verified):**
  `git -C <cwd> diff-tree -p --root <sha>` piped to `git patch-id --stable`,
  first whitespace token. The `--root` flag is ESSENTIAL — without it a root
  (parentless) commit produces empty output. The patch-id is **stable** across
  a diff-preserving rewrite (reword) and **differs** on a content-changing
  rewrite. Empty-diff commits (`--allow-empty`, some merges) produce **empty**
  output → treat as `None`.
- `crates/dkod-core/src/refs.rs` has `session_ref`, `commit_ref(sha) -> "refs/dkod/commits/{sha}"`, `parse_session_ref`, and a `#[cfg(test)] mod tests` with `commit_ref_path_is_correct`.
- `crates/dkod-core/src/store.rs` `link_session_to_commit(repo_path, session_id, commit_sha) -> Result<()>` resolves `refs::session_ref(session_id)`, takes `.id().detach()` as `blob_id`, and `edit_reference`s `refs::commit_ref(commit_sha)` → `Target::Object(blob_id)` with `use gix::refs::{transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog}, Target};`. `ensure_committer(&mut repo)` is `pub(crate)` in the same file. The test module has `fixture_session()`, uses `tempfile::TempDir`, `gix::init`, `write_session`, and has a `link_session_to_commit_writes_ref_pointing_at_session_blob`-style test plus a last-writer-wins relink test for reference.
- `crates/dkod-cli/src/cmd/blame.rs` has `fn session_for_commit(cwd: &Path, sha: &str) -> Option<(String, String, String)>` (find `commit_ref(sha)` → object → `Session` → `(agent_label, short id, prompt_summary)`), and `run` caches `session_for_commit` results in a `HashMap<String, Option<...>>` keyed by sha. It already `use`s `std::process::Command` and shells out for `git blame --porcelain`.
- `crates/dkod-cli/src/cmd/capture/mod.rs` `pub fn finalize_session(cwd, session, head_at_start, cfg) -> Result<Vec<String>>` redacts, calls `dkod_core::store::write_session_with_commit_links` (returns the linked SHAs), returns them.
- `crates/dkod-cli/src/cmd/mod.rs` declares `pub mod blame; pub mod capture; pub mod init; pub mod log; pub mod relink; pub mod setup; pub mod show;` + `load_config`.
- Test convention (`crates/dkod-cli/tests/e2e_relink.rs`, `e2e_blame.rs`): `assert_cmd::Command::cargo_bin("dkod")`, a `git(repo, args)` helper setting `GIT_AUTHOR_*`/`GIT_COMMITTER_*` env, a `head(repo)` helper, and the `Session` literal `dkod_core::Session { id: dkod_core::Session::new_id(), agent: dkod_core::Agent::ClaudeCode, created_at: 0, duration_ms: 0, prompt_summary: ..., messages: vec![], commits: vec![...], files_touched: vec![...] }`. `dkod_core` is a dev-dependency.

---

## Phase 1 — `refs::patchid_ref`

### Task 1.1: add the ref helper

**Files:**
- Modify: `crates/dkod-core/src/refs.rs`

- [ ] **Step 1: Write the failing test.** Add to the `#[cfg(test)] mod tests` in `refs.rs`:

```rust
#[test]
fn patchid_ref_path_is_correct() {
    let pid = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    assert_eq!(patchid_ref(pid), "refs/dkod/patchid/deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
}
```

- [ ] **Step 2: Run to verify failure.**
  Run: `cargo test -p dkod-core patchid_ref`
  Expected: FAIL — `patchid_ref` not found.

- [ ] **Step 3: Implement.** Add to `refs.rs` (next to `commit_ref`):

```rust
/// Ref namespace mapping a commit's `git patch-id` to the session blob, so
/// `dkod blame` can recover provenance after a diff-preserving rewrite that
/// the post-rewrite hook never observed (pre-init history, clones without the
/// hook, filter-repo).
pub fn patchid_ref(patch_id: &str) -> String {
    format!("refs/dkod/patchid/{patch_id}")
}
```

- [ ] **Step 4: Run green.**
  Run: `cargo test -p dkod-core patchid_ref`
  Expected: PASS.

- [ ] **Step 5: Gates + CodeRabbit + commit.**
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-core/src/refs.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(refs): patchid_ref namespace for patch-id provenance fallback"
  coderabbit --type committed --plain
  ```

---

## Phase 2 — `store::link_session_to_patchid`

### Task 2.1: pure-gix patch-id ref writer

**Files:**
- Modify: `crates/dkod-core/src/store.rs`

- [ ] **Step 1: Write the failing tests.** Append to the `#[cfg(test)] mod tests` in `store.rs`:

```rust
#[test]
fn link_session_to_patchid_writes_ref_pointing_at_session_blob() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let s = fixture_session();
    write_session(tmp.path(), &s).unwrap();

    let pid = "1111111111111111111111111111111111111111";
    link_session_to_patchid(tmp.path(), &s.id, pid).unwrap();

    let repo = gix::open(tmp.path()).unwrap();
    let pid_ref = repo.find_reference(&crate::refs::patchid_ref(pid)).unwrap();
    let sess_ref = repo.find_reference(&crate::refs::session_ref(&s.id)).unwrap();
    assert_eq!(pid_ref.id(), sess_ref.id(), "patchid ref must point at the session blob");
}

#[test]
fn link_session_to_patchid_last_writer_wins() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let a = fixture_session();
    let mut b = fixture_session();
    b.id = Session::new_id();
    write_session(tmp.path(), &a).unwrap();
    write_session(tmp.path(), &b).unwrap();

    let pid = "2222222222222222222222222222222222222222";
    link_session_to_patchid(tmp.path(), &a.id, pid).unwrap();
    link_session_to_patchid(tmp.path(), &b.id, pid).unwrap(); // overwrites

    let repo = gix::open(tmp.path()).unwrap();
    let pid_ref = repo.find_reference(&crate::refs::patchid_ref(pid)).unwrap();
    let b_ref = repo.find_reference(&crate::refs::session_ref(&b.id)).unwrap();
    assert_eq!(pid_ref.id(), b_ref.id(), "last writer (sessionB) must win");
}
```

> If `fixture_session()` takes an argument, read its signature and adapt. `Session` is already in scope in the store test module.

- [ ] **Step 2: Run to verify failure.**
  Run: `cargo test -p dkod-core link_session_to_patchid`
  Expected: FAIL — `link_session_to_patchid` not found.

- [ ] **Step 3: Implement.** Add to `store.rs`, immediately after `link_session_to_commit`:

```rust
/// Write `refs/dkod/patchid/<patch_id>` pointing at the same blob the session
/// ref points at — the patch-id provenance fallback for `dkod blame`. Mirrors
/// `link_session_to_commit`. Idempotent / overwrite-on-collision
/// (`PreviousValue::Any`): two sessions with the same diff → last writer wins,
/// matching the commit-ref policy.
pub fn link_session_to_patchid(repo_path: &Path, session_id: &str, patch_id: &str) -> Result<()> {
    use gix::refs::{
        transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
        Target,
    };

    let mut repo = gix::open(repo_path).context("open repo")?;
    ensure_committer(&mut repo)?;
    let session_ref = repo
        .find_reference(&refs::session_ref(session_id))
        .context("find session ref")?;
    let blob_id = session_ref.id().detach();

    let ref_name = refs::patchid_ref(patch_id);
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("dkod: link session {} to patch-id {}", session_id, patch_id)
                    .into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(blob_id),
        },
        name: ref_name.try_into().context("invalid patch-id ref name")?,
        deref: false,
    })
    .context("edit patch-id ref")?;
    Ok(())
}
```

- [ ] **Step 4: Run green.**
  Run: `cargo test -p dkod-core link_session_to_patchid`
  Expected: 2 PASS.

- [ ] **Step 5: Gates + CodeRabbit + commit.**
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-core/src/store.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(store): link_session_to_patchid writes the patch-id provenance ref"
  coderabbit --type committed --plain
  ```

---

## Phase 3 — `cmd::patchid::compute_patch_id`

### Task 3.1: git patch-id shell-out (CLI layer)

**Files:**
- Create: `crates/dkod-cli/src/cmd/patchid.rs`
- Modify: `crates/dkod-cli/src/cmd/mod.rs` (add `pub mod patchid;`)

- [ ] **Step 1: Register the module.** In `crates/dkod-cli/src/cmd/mod.rs`, add `pub mod patchid;` in alphabetical position (between `pub mod log;` and `pub mod relink;`).

- [ ] **Step 2: Create `patchid.rs` with impl + failing unit tests.**

```rust
//! Compute a commit's stable `git patch-id`, used by the `dkod blame` patch-id
//! fallback. Kept in the CLI layer (not `dkod-core`) because it shells out to
//! git, like `init.rs` and `blame.rs` already do.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// True iff `s` is exactly 40 lowercase hex chars (a git SHA-1 / patch-id).
fn is_hex40(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The stable `git patch-id` of `sha`'s diff, or `None` when it can't be
/// computed or is empty (empty-diff commits, merges with no combined diff).
///
/// Runs `git -C <cwd> diff-tree -p --root <sha>` and pipes it into
/// `git patch-id --stable`. The `--root` flag is required so a root commit
/// (no parent) still produces a diff. Best-effort: any spawn/IO failure → None.
pub(crate) fn compute_patch_id(cwd: &Path, sha: &str) -> Option<String> {
    // Stage 1: the diff. Capture to memory so we control exactly what feeds
    // stage 2 (avoids shell quoting / pipe-failure ambiguity).
    let diff = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["diff-tree", "-p", "--root", sha])
        .output()
        .ok()?;
    if !diff.status.success() || diff.stdout.is_empty() {
        return None;
    }

    // Stage 2: patch-id over that diff.
    let mut child = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(&diff.stdout).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }

    // Output: "<patch-id> <commit-sha>\n". Take the first token; validate.
    let stdout = String::from_utf8(out.stdout).ok()?;
    let pid = stdout.split_whitespace().next()?;
    if is_hex40(pid) {
        Some(pid.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
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

    fn head(repo: &Path) -> String {
        String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo).output().unwrap().stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }

    #[test]
    fn normal_commit_yields_40_hex() {
        let repo = TempDir::new().unwrap();
        git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("f.txt"), "a\nb\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "c"]);
        let pid = compute_patch_id(repo.path(), &head(repo.path()));
        let pid = pid.expect("expected a patch-id");
        assert_eq!(pid.len(), 40);
        assert!(pid.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    }

    #[test]
    fn root_commit_yields_a_patch_id() {
        // A repo whose only commit is the root — requires --root to diff.
        let repo = TempDir::new().unwrap();
        git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("f.txt"), "hello\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "root"]);
        assert!(compute_patch_id(repo.path(), &head(repo.path())).is_some());
    }

    #[test]
    fn stable_across_reword() {
        let repo = TempDir::new().unwrap();
        git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("f.txt"), "a\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "orig"]);
        let before = compute_patch_id(repo.path(), &head(repo.path())).unwrap();
        git(repo.path(), &["commit", "--amend", "-qm", "reworded"]); // diff unchanged
        let after = compute_patch_id(repo.path(), &head(repo.path())).unwrap();
        assert_eq!(before, after, "patch-id must be stable across a reword");
    }

    #[test]
    fn empty_commit_yields_none() {
        let repo = TempDir::new().unwrap();
        git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("f.txt"), "a\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "base"]);
        git(repo.path(), &["commit", "--allow-empty", "-qm", "empty"]);
        assert!(compute_patch_id(repo.path(), &head(repo.path())).is_none(),
            "empty-diff commit must yield None (no patch-id ref)");
    }
}
```

- [ ] **Step 3: Run to verify failure.**
  Run: `cargo test -p dkod-cli patchid`
  Expected: FAIL first because the module isn't registered / function absent; once Step 1 + the file land, the 4 tests run.

- [ ] **Step 4: Run green.**
  Run: `cargo test -p dkod-cli patchid`
  Expected: 4 PASS.

  > Note on dead-code: `compute_patch_id` is `pub(crate)` but has no caller until Phase 4. The `#[cfg(test)]` tests reference it, and `cargo clippy --all-targets` compiles the lib WITHOUT cfg(test) too — if clippy `-D warnings` flags `compute_patch_id`/`is_hex40` as dead_code, that is expected at THIS phase. Do NOT add `#[allow(dead_code)]`: instead, sequence the work so Phase 4 (which adds the callers) lands before you run the full `-D warnings` gate for committing this task — i.e. if the standalone gate trips on dead_code, STOP and report; the controller will fold Phase 3 + Phase 4 into one commit. (Expectation: confirm whether it trips; `is_hex40` is only used by `compute_patch_id`, and `compute_patch_id` is only used in tests this phase, so the non-test lib build may warn. If so, report rather than paper over.)

- [ ] **Step 5: Gates + CodeRabbit + commit** (only if clippy is clean; otherwise report per the note above).
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-cli/src/cmd/patchid.rs crates/dkod-cli/src/cmd/mod.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(patchid): compute_patch_id via git diff-tree | patch-id --stable"
  coderabbit --type committed --plain
  ```

> **Controller note:** if Phase 3's standalone `-D warnings` gate trips on `compute_patch_id` being unused in the non-test lib build, do not fight it — dispatch Phase 3 and Phase 4 together so the callers exist in the same commit. The TDD discipline is preserved (tests-first within the combined task).

---

## Phase 4 — wire write + read paths

### Task 4.1: write patch-id refs in `finalize_session`; add the blame fallback

**Files:**
- Modify: `crates/dkod-cli/src/cmd/capture/mod.rs` (write side)
- Modify: `crates/dkod-cli/src/cmd/blame.rs` (read side)
- Test: `crates/dkod-cli/tests/finalize_patchid.rs` (new, write-side integration)

- [ ] **Step 1 (write side): edit `finalize_session`.** Replace the body so it also writes patch-id refs for the produced commits (best-effort). The function currently is:

```rust
pub fn finalize_session(
    cwd: &std::path::Path,
    session: &mut dkod_core::Session,
    head_at_start: Option<&str>,
    cfg: &dkod_core::config::Config,
) -> Result<Vec<String>> {
    dkod_core::redact::redact_session(session, &cfg.redact);
    let linked = dkod_core::store::write_session_with_commit_links(cwd, session, head_at_start)
        .context("write session")?;
    Ok(linked)
}
```

Change it to:

```rust
pub fn finalize_session(
    cwd: &std::path::Path,
    session: &mut dkod_core::Session,
    head_at_start: Option<&str>,
    cfg: &dkod_core::config::Config,
) -> Result<Vec<String>> {
    dkod_core::redact::redact_session(session, &cfg.redact);
    let linked = dkod_core::store::write_session_with_commit_links(cwd, session, head_at_start)
        .context("write session")?;
    // Also write a patch-id provenance ref per produced commit so `dkod blame`
    // can recover after a diff-preserving rewrite the post-rewrite hook never
    // saw (pre-init history, clones without the hook, filter-repo). Best-effort:
    // a patch-id failure must never fail capture.
    for sha in &linked {
        if let Some(pid) = crate::cmd::patchid::compute_patch_id(cwd, sha) {
            let _ = dkod_core::store::link_session_to_patchid(cwd, &session.id, &pid);
        }
    }
    Ok(linked)
}
```

> `crate::cmd::patchid` is reachable from `cmd/capture/mod.rs` as `crate::cmd::patchid::compute_patch_id` (both under `cmd`). Confirm the path resolves; if `capture/mod.rs` is `crate::cmd::capture`, the sibling is `crate::cmd::patchid` — correct.

- [ ] **Step 2 (read side): restructure `session_for_commit` in `blame.rs`.** Replace it with a shared decode helper + the two-step lookup:

```rust
/// Decode the session blob a dkod ref points at into the blame annotation
/// tuple `(agent_label, short_session_id, prompt_summary)`.
fn session_from_ref_name(repo: &gix::Repository, ref_name: &str) -> Option<(String, String, String)> {
    let r = repo.find_reference(ref_name).ok()?;
    let obj = repo.find_object(r.id()).ok()?.detach();
    let session: dkod_core::Session = serde_json::from_slice(&obj.data).ok()?;
    let short = session.id.get(..8).unwrap_or(&session.id).to_string();
    Some((
        dkod_core::agent_label(&session.agent).to_string(),
        short,
        session.prompt_summary,
    ))
}

/// Resolve a blamed commit SHA to its session annotation. Primary: the
/// `refs/dkod/commits/<sha>` link. Fallback: when that's absent (a rewrite the
/// post-rewrite hook never observed), match the commit's `git patch-id` against
/// `refs/dkod/patchid/<patch-id>`, which survives diff-preserving rewrites.
fn session_for_commit(cwd: &Path, sha: &str) -> Option<(String, String, String)> {
    let repo = gix::open(cwd).ok()?;
    if let Some(hit) = session_from_ref_name(&repo, &dkod_core::refs::commit_ref(sha)) {
        return Some(hit);
    }
    let pid = crate::cmd::patchid::compute_patch_id(cwd, sha)?;
    session_from_ref_name(&repo, &dkod_core::refs::patchid_ref(&pid))
}
```

> Keep the existing per-sha `HashMap` cache in `run` unchanged — it already ensures the fallback subprocess runs at most once per unique sha. `gix::Repository` is the type returned by `gix::open`; if the helper's signature needs `&gix::Repository`, that's what `gix::open(cwd).ok()?` yields. Adjust the borrow if the compiler wants `&mut` (it should not — these are read ops).

- [ ] **Step 3 (write-side test): create `crates/dkod-cli/tests/finalize_patchid.rs`.**

```rust
//! finalize_session writes a patch-id provenance ref for each produced commit.

use dkod_cli::cmd::capture::finalize_session;
use std::path::Path;
use std::process::Command;
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

fn head(repo: &Path) -> String {
    String::from_utf8(
        Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo).output().unwrap().stdout,
    )
    .unwrap()
    .trim()
    .to_string()
}

/// Compute a commit's patch-id the same way the implementation does, so the
/// test can name the expected ref.
fn patch_id(repo: &Path, sha: &str) -> String {
    use std::io::Write;
    use std::process::Stdio;
    let diff = Command::new("git")
        .arg("-C").arg(repo)
        .args(["diff-tree", "-p", "--root", sha])
        .output().unwrap();
    let mut child = Command::new("git")
        .arg("-C").arg(repo)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(&diff.stdout).unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8(out.stdout).unwrap().split_whitespace().next().unwrap().to_string()
}

#[test]
fn finalize_writes_patchid_ref_for_produced_commit() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    // A base commit so head_at_start is a real ancestor; then the "produced" commit.
    std::fs::write(repo.path().join("f.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "base"]);
    let base = head(repo.path());
    std::fs::write(repo.path().join("f.txt"), "a\nb\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "produced"]);
    let produced = head(repo.path());

    let cfg = dkod_core::config::Config::default();
    let mut session = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: "do it".into(),
        messages: vec![],
        commits: vec![],
        files_touched: vec![],
    };
    let linked = finalize_session(repo.path(), &mut session, Some(&base), &cfg).unwrap();
    assert!(linked.contains(&produced), "expected the produced commit to be linked");

    // The patch-id ref for the produced commit must resolve to the session blob.
    let pid = patch_id(repo.path(), &produced);
    let r = gix::open(repo.path()).unwrap();
    let pid_ref = r.find_reference(&dkod_core::refs::patchid_ref(&pid)).unwrap();
    let sess_ref = r.find_reference(&dkod_core::refs::session_ref(&session.id)).unwrap();
    assert_eq!(pid_ref.id(), sess_ref.id());
}
```

- [ ] **Step 4: Run.**
  Run: `cargo test -p dkod-cli --test finalize_patchid` then the full `cargo test`.
  Expected: the new test passes; existing blame/finalize/relink tests stay green.

- [ ] **Step 5: Gates + CodeRabbit + commit.**
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-cli/src/cmd/capture/mod.rs crates/dkod-cli/src/cmd/blame.rs crates/dkod-cli/tests/finalize_patchid.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(blame): patch-id fallback (write refs at capture, match at blame)"
  coderabbit --type committed --plain
  ```

---

## Phase 5 — end-to-end payoff

### Task 5.1: `e2e_patchid_blame.rs`

**Files:**
- Test: `crates/dkod-cli/tests/e2e_patchid_blame.rs` (new)

Proves the fallback recovers provenance after a diff-preserving rewrite WITHOUT
any hook/relink, and is diff-precise (a content change does NOT match). Drives
the real `dkod` binary for blame; seeds the patch-id ref in-test (mirroring how
`e2e_relink.rs` seeds via the library) so the test does not depend on the
capture path or the hook.

- [ ] **Step 1: Write the test.**

```rust
//! End-to-end: the patch-id fallback restores `dkod blame` provenance after a
//! diff-preserving rewrite the post-rewrite hook never saw — and does NOT
//! mis-attribute after a content-changing rewrite.

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

fn head(repo: &Path) -> String {
    String::from_utf8(
        Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo).output().unwrap().stdout,
    )
    .unwrap()
    .trim()
    .to_string()
}

fn patch_id(repo: &Path, sha: &str) -> String {
    let diff = Command::new("git")
        .arg("-C").arg(repo)
        .args(["diff-tree", "-p", "--root", sha])
        .output().unwrap();
    let mut child = Command::new("git")
        .arg("-C").arg(repo)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(&diff.stdout).unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8(out.stdout).unwrap().split_whitespace().next().unwrap().to_string()
}

fn seed_session(repo: &Path, prompt: &str, old_sha: &str) -> dkod_core::Session {
    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: prompt.into(),
        messages: vec![],
        commits: vec![old_sha.to_string()],
        files_touched: vec!["f.txt".into()],
    };
    dkod_core::store::write_session(repo, &s).unwrap();
    dkod_core::store::link_session_to_commit(repo, &s.id, old_sha).unwrap();
    // The patch-id ref is what survives the rewrite (commit-ref becomes stale).
    dkod_core::store::link_session_to_patchid(repo, &s.id, &patch_id(repo, old_sha)).unwrap();
    s
}

#[test]
fn patchid_fallback_recovers_blame_after_diff_preserving_rewrite() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "ai line\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "original"]);
    let old = head(repo.path());

    seed_session(repo.path(), "write the greeting", &old);

    // Diff-preserving rewrite (reword). The commit-ref for `new` does NOT exist
    // and we deliberately do NOT run relink — only the patch-id ref can save us.
    git(repo.path(), &["commit", "--amend", "-qm", "reworded"]);
    let new = head(repo.path());
    assert_ne!(old, new, "amend must change the sha");

    let out = AssertCommand::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["blame", "f.txt"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("claude_code"), "patch-id fallback should attribute the agent:\n{stdout}");
    assert!(stdout.contains("write the greeting"), "patch-id fallback should show the prompt:\n{stdout}");
}

#[test]
fn patchid_fallback_does_not_misattribute_after_content_change() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "ai line\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "original"]);
    let old = head(repo.path());

    seed_session(repo.path(), "write the greeting", &old);

    // Content-CHANGING amend → different patch-id → fallback must NOT match.
    std::fs::write(repo.path().join("f.txt"), "ai line\nextra\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "--amend", "-qm", "changed"]);

    let out = AssertCommand::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["blame", "f.txt"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(!stdout.contains("write the greeting"),
        "a content change must NOT be attributed to the old session:\n{stdout}");
}
```

- [ ] **Step 2: Run.**
  Run: `cargo test -p dkod-cli --test e2e_patchid_blame`
  Expected: 2 PASS. If the blame agent-label token differs, read `blame.rs` and assert on the actual `dkod_core::agent_label(&Agent::ClaudeCode)` value ("claude_code"); do NOT weaken the assertions.

- [ ] **Step 3: Gates + CodeRabbit + commit.**
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-cli/tests/e2e_patchid_blame.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "test(blame): e2e patch-id fallback recovers + stays diff-precise"
  coderabbit --type committed --plain
  ```

---

## Phase 6 — docs + PR

### Task 6.1: Update the positioning roadmap

**Files:**
- Modify: `docs/plans/2026-05-27-macroscope-competitive-positioning.md`

- [ ] **Step 1: Read the `dkod blame` roadmap entry** (the "### 1. `dkod blame`" block and its history-rewrite residue paragraph from PR #21).

- [ ] **Step 2: Edit.** Note the patch-id fallback now ships: diff-preserving rewrites the post-rewrite hook never saw (history rewritten before `dkod init`, clones without the hook, `git filter-repo`) are recovered by matching the commit's `git patch-id` against `refs/dkod/patchid/<patch-id>` written at capture. Shrink the residue accordingly: what remains is squash (lossy), content-changing rewrites (genuinely different work), patch-id collisions (last-writer-wins), and sessions captured before this shipped (no patch-id ref — a future `dkod reindex`). Reference `docs/plans/2026-06-09-patchid-fallback-design.md`.

- [ ] **Step 3: Commit (docs-only).**
  ```bash
  git add docs/plans/2026-05-27-macroscope-competitive-positioning.md
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "docs: patch-id fallback shipped — shrink the history-rewrite residue"
  ```
  (CodeRabbit does not meaningfully review docs — note this; skip the CodeRabbit passes for this docs-only commit.)

### Task 6.2: Open the PR and drive to merge

- [ ] **Step 1: Final local verification.**
  Run: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` → all green.
- [ ] **Step 2: Full-branch CodeRabbit.**
  Run: `coderabbit --type all --base main --plain`; resolve every actionable finding (new commits, re-run until clean).
- [ ] **Step 3: Push.**
  ```bash
  gh auth switch --user haim-ari
  git push -u origin feat/patchid-fallback
  ```
- [ ] **Step 4: Open the PR.**
  Title (≤70 chars): `feat: patch-id fallback for dkod blame`
  Body: the write-side patch-id ref at capture + the blame fallback; why it's complementary to the hook (hook owns squash + live rewrites; patch-id owns diff-preserving rewrites the hook never saw); the documented residue. Reference the design doc.
- [ ] **Step 5: Drive to green and merge.**
  Trigger CodeRabbit (`@coderabbitai review`), wait for CI + CodeRabbit, fix findings (re-`gh auth switch` before each push), repeat until CI green + CodeRabbit approved, then squash-merge `--delete-branch`. After merge, sync local `main` (`git checkout main && git fetch origin && git reset --hard origin/main`).

---

## Self-review checklist (controller, before dispatch)

- Spec coverage: `patchid_ref` (P1) ✓; `link_session_to_patchid` (P2) ✓; `compute_patch_id` with `--root` + empty→None + 40-hex validation (P3) ✓; write-path wiring in `finalize_session` + read-path fallback in `blame.rs` (P4) ✓; e2e recover + diff-precision (P5) ✓; docs + PR (P6) ✓. Empty patch-id skipped (compute returns None → loop skips; fallback `?` short-circuits). Collisions last-writer-wins (`PreviousValue::Any`). git shell-out only in `cmd::patchid`. refs travel via existing `+refs/dkod/*` refspec (no code needed).
- Type/name consistency: `patchid_ref(&str)->String`, `link_session_to_patchid(&Path,&str,&str)->Result<()>`, `compute_patch_id(&Path,&str)->Option<String>`, `session_from_ref_name(&gix::Repository,&str)->Option<(String,String,String)>`, `crate::cmd::patchid` path — consistent across phases.
- Placeholder scan: every code step shows complete code.
- Known risk flagged: Phase 3 standalone dead-code gate (controller note to fold P3+P4 if `-D warnings` trips).

## Known limitations (carry in the PR description)

1. Recovers only **diff-preserving** rewrites; squash + content-changing rewrites are not (hook handles squash; content changes are different work).
2. patch-id **collisions** (identical diffs) → last-writer-wins.
3. Empty-diff commits get no patch-id ref.
4. Sessions captured **before** this shipped have no patch-id ref (future `dkod reindex`).
5. Adds a `git` subprocess on the blame **fallback** path only (cached per unique sha).
