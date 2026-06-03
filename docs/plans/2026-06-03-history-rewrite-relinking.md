# History-Rewrite Re-linking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore `dkod blame` provenance after history rewrites (rebase / `commit --amend` / squash / reword) via a per-repo git `post-rewrite` hook that pipes git's `old→new` SHA pairs to a new hidden `dkod relink` subcommand, which re-points `refs/dkod/commits/<new>` at the session blob.

**Architecture:** A core helper `dkod_core::store::relink_commit(old, new)` re-points a commit-ref (mirrors `link_session_to_commit`). A hidden `dkod relink` CLI command reads `old new` pairs from stdin and calls it per pair (best-effort, always exits 0). `dkod init` installs a sentinel-guarded `.git/hooks/post-rewrite` script (`exec dkod relink`) that git fires after rewrites. Squash is last-writer-wins for free (overwrite-on-collision); refs only (no blob rewrite).

**Tech Stack:** Rust (edition 2021), `gix` 0.66, `clap`, `anyhow`; `git` CLI via `std::process::Command` (init hook-dir resolution + tests); `assert_cmd` + `tempfile` for tests.

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
6. **YAGNI:** implement only what the design specifies. No patch-id fallback,
   no blob rewrite, no `dkod gc`, no wizard install.

---

## Background facts (verified — do not re-discover)

- `crates/dkod-core/src/store.rs` `pub fn link_session_to_commit(repo_path: &Path, session_id: &str, commit_sha: &str) -> Result<()>` opens the repo, calls `ensure_committer(&mut repo)?`, resolves `find_reference(&refs::session_ref(session_id))`, takes `.id().detach()` as the blob OID, and `edit_reference`s `refs::commit_ref(commit_sha)` → `Target::Object(blob_id)`. `relink_commit` mirrors this but reads the OLD commit-ref's blob.
- `crates/dkod-core/src/refs.rs`: `pub fn commit_ref(sha: &str) -> String` → `refs/dkod/commits/{sha}`; `pub fn session_ref(...)`. No `parse_commit_ref` exists — don't assume one.
- `ensure_committer(&mut gix::Repository)` is `pub(crate)` in store.rs; `relink_commit` lives in store.rs so it calls it directly.
- `crates/dkod-cli/src/main.rs`: clap `Cmd` enum; `CaptureHook { .. }` is `#[command(hide = true)]`. `main` calls `maybe_warn_drift()` gated by `if !matches!(cli.cmd, Cmd::Setup { .. } | Cmd::CaptureHook { .. }) { maybe_warn_drift(); }` (exact form may differ slightly — read it). Dispatch is `match cli.cmd { ... }`.
- `crates/dkod-cli/src/cmd/mod.rs` declares `pub mod ...` submodules + a `load_config` helper.
- `crates/dkod-cli/src/cmd/init.rs` `pub fn run(cwd: &Path) -> Result<()>`: `gix::discover(cwd)` guard → write `.dkod/config.toml` → `load_config` → `maybe_suggest_setup_wizard()` → `ensure_dkod_refspec(cwd)` → claude `install_hooks_at_init`. It already shells out to git via `std::process::Command::new("git").arg("-C").arg(cwd)...`. Because `gix::discover` allows `cwd` to be a subdir, resolve the hooks dir via git, not `cwd/.git`.
- Test helpers `git(repo, args)` (sets GIT_AUTHOR_*/COMMITTER_* env) and `head(repo)` and `fixture_session(prompt)` exist verbatim in `crates/dkod-cli/tests/finalize_session.rs` — copy them into new test files. `Session` literal: `dkod_core::Session { id: dkod_core::Session::new_id(), agent: dkod_core::Agent::Codex, created_at: 0, duration_ms: 0, prompt_summary: prompt.into(), messages: vec![], commits: vec![], files_touched: vec![] }`.
- `dkod_core` is a dev-dependency of `dkod-cli` (used by existing integration tests).
- Capture-hook conventions to mirror (`crates/dkod-cli/src/cmd/capture/hook.rs`): hidden internal command, validate 40-char lowercase hex, best-effort, **always exit 0**.

---

## Phase 1 — core helper `relink_commit`

### Task 1.1: `relink_commit` in `dkod_core::store`

**Files:**
- Modify: `crates/dkod-core/src/store.rs` (add fn + tests in existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing tests.** Append to the `#[cfg(test)] mod tests` block in `store.rs`. (The module already imports what `fixture_session`, `write_session`, `link_session_to_commit`, `TempDir`, `gix` need — mirror the neighboring tests; `fixture_session()` already exists there.)

```rust
#[test]
fn relink_commit_repoints_new_to_same_blob() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let s = fixture_session();
    write_session(tmp.path(), &s).unwrap();

    let old = "0000000000000000000000000000000000000001";
    let new = "0000000000000000000000000000000000000002";
    link_session_to_commit(tmp.path(), &s.id, old).unwrap();

    let did = relink_commit(tmp.path(), old, new).unwrap();
    assert!(did, "relink should report it acted");

    let repo = gix::open(tmp.path()).unwrap();
    let old_ref = repo.find_reference(&crate::refs::commit_ref(old)).unwrap();
    let new_ref = repo.find_reference(&crate::refs::commit_ref(new)).unwrap();
    let sess_ref = repo.find_reference(&crate::refs::session_ref(&s.id)).unwrap();
    // new ref points at the same blob as old ref and the session ref.
    assert_eq!(new_ref.id(), old_ref.id());
    assert_eq!(new_ref.id(), sess_ref.id());
}

#[test]
fn relink_commit_absent_old_returns_false() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let new = "0000000000000000000000000000000000000002";
    let did = relink_commit(tmp.path(), "0000000000000000000000000000000000000001", new).unwrap();
    assert!(!did, "absent old ref means nothing to relink");
    let repo = gix::open(tmp.path()).unwrap();
    assert!(repo.find_reference(&crate::refs::commit_ref(new)).is_err());
}

#[test]
fn relink_commit_keeps_old_ref() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let s = fixture_session();
    write_session(tmp.path(), &s).unwrap();
    let old = "0000000000000000000000000000000000000001";
    let new = "0000000000000000000000000000000000000002";
    link_session_to_commit(tmp.path(), &s.id, old).unwrap();
    relink_commit(tmp.path(), old, new).unwrap();
    // old ref is additive — still resolves after relink.
    let repo = gix::open(tmp.path()).unwrap();
    assert!(repo.find_reference(&crate::refs::commit_ref(old)).is_ok());
}

#[test]
fn relink_commit_last_writer_wins() {
    let tmp = TempDir::new().unwrap();
    gix::init(tmp.path()).unwrap();
    let a = fixture_session();
    let mut b = fixture_session();
    b.id = Session::new_id();
    // ensure distinct ids/blobs
    std::thread::sleep(std::time::Duration::from_millis(2));
    write_session(tmp.path(), &a).unwrap();
    write_session(tmp.path(), &b).unwrap();

    let old1 = "0000000000000000000000000000000000000011";
    let old2 = "0000000000000000000000000000000000000022";
    let new = "0000000000000000000000000000000000000099";
    link_session_to_commit(tmp.path(), &a.id, old1).unwrap();
    link_session_to_commit(tmp.path(), &b.id, old2).unwrap();

    relink_commit(tmp.path(), old1, new).unwrap();
    relink_commit(tmp.path(), old2, new).unwrap(); // last wins

    let repo = gix::open(tmp.path()).unwrap();
    let new_ref = repo.find_reference(&crate::refs::commit_ref(new)).unwrap();
    let b_ref = repo.find_reference(&crate::refs::session_ref(&b.id)).unwrap();
    assert_eq!(new_ref.id(), b_ref.id(), "last relink (sessionB) must win");
}
```

> If `fixture_session()` in the store test module takes an argument or has a different shape than expected, read it and adapt the calls — do not change the fixture.

- [ ] **Step 2: Run to verify failure.**
  Run: `cargo test -p dkod-core relink_commit`
  Expected: FAIL — `relink_commit` not found.

- [ ] **Step 3: Implement `relink_commit`.** Add to `store.rs`, next to `link_session_to_commit`:

```rust
/// Re-point `refs/dkod/commits/<new_sha>` at whatever session blob
/// `refs/dkod/commits/<old_sha>` currently points at. Returns `Ok(true)` if the
/// old ref existed and the new ref was written, `Ok(false)` if the old ref is
/// absent (nothing to re-link). The old ref is left in place (additive) so an
/// undone rewrite (`git reset --hard ORIG_HEAD`) still resolves.
pub fn relink_commit(repo_path: &Path, old_sha: &str, new_sha: &str) -> Result<bool> {
    use gix::refs::{
        transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
        Target,
    };

    let mut repo = gix::open(repo_path).context("open repo")?;
    ensure_committer(&mut repo)?;

    let old_ref = match repo.find_reference(&refs::commit_ref(old_sha)) {
        Ok(r) => r,
        Err(_) => return Ok(false),
    };
    let blob_id = old_ref.id().detach();

    let ref_name = refs::commit_ref(new_sha);
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("dkod: relink commit {old_sha} -> {new_sha}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(blob_id),
        },
        name: ref_name.try_into().context("invalid commit ref name")?,
        deref: false,
    })
    .context("edit commit ref")?;
    Ok(true)
}
```

- [ ] **Step 4: Run green.**
  Run: `cargo test -p dkod-core relink_commit`
  Expected: 4 tests PASS.

- [ ] **Step 5: Gates + CodeRabbit + commit.**
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings, re-run until clean
  git add crates/dkod-core/src/store.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(store): relink_commit re-points a commit-ref to its session blob"
  coderabbit --type committed --plain
  ```

---

## Phase 2 — `dkod relink` command + wiring

### Task 2.1: `cmd/relink.rs` with pure `parse_pairs` + `run`

**Files:**
- Create: `crates/dkod-cli/src/cmd/relink.rs`
- Modify: `crates/dkod-cli/src/cmd/mod.rs` (add `pub mod relink;`)

- [ ] **Step 1: Register the module.** In `crates/dkod-cli/src/cmd/mod.rs`, add `pub mod relink;` in alphabetical position among the existing `pub mod` lines (it sits after `pub mod log;`/before `pub mod setup;` — match the file's ordering).

- [ ] **Step 2: Write `relink.rs` with the failing parser tests first.**

```rust
//! `dkod relink` — hidden, invoked by the git `post-rewrite` hook.
//!
//! Reads `old new` commit-SHA pairs from stdin (git's post-rewrite format,
//! one per rewritten commit) and re-points each session's commit-ref from the
//! old SHA to the new one, so `dkod blame` keeps resolving rewritten lines.
//! Always exits 0 — a misbehaving re-link must never break the user's rebase.

use anyhow::Result;
use std::io::Read;
use std::path::Path;

/// True iff `s` is exactly 40 lowercase hex chars (a full git SHA-1).
fn is_hex40(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Parse git `post-rewrite` stdin into `(old, new)` SHA pairs. Each line is
/// whitespace-split; only lines with exactly two 40-hex tokens are kept.
/// Blank and malformed lines are skipped.
pub(crate) fn parse_pairs(input: &str) -> Vec<(String, String)> {
    input
        .lines()
        .filter_map(|line| {
            let mut toks = line.split_whitespace();
            let old = toks.next()?;
            let new = toks.next()?;
            if toks.next().is_some() {
                return None; // more than two tokens — malformed
            }
            if is_hex40(old) && is_hex40(new) {
                Some((old.to_string(), new.to_string()))
            } else {
                None
            }
        })
        .collect()
}

/// Read stdin, parse pairs, and re-link each. Best-effort: per-pair errors are
/// ignored and the command always returns `Ok(())`.
pub fn run(cwd: &Path) -> Result<()> {
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf); // best-effort
    let pairs = parse_pairs(&buf);
    let mut relinked = 0usize;
    for (old, new) in &pairs {
        if let Ok(true) = dkod_core::store::relink_commit(cwd, old, new) {
            relinked += 1;
        }
    }
    if relinked > 0 {
        eprintln!("dkod: re-linked {relinked} session commit-ref(s) after history rewrite");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0000000000000000000000000000000000000001";
    const B: &str = "0000000000000000000000000000000000000002";
    const C: &str = "0000000000000000000000000000000000000003";

    #[test]
    fn parses_well_formed_pairs() {
        let input = format!("{A} {B}\n{B} {C}\n");
        assert_eq!(
            parse_pairs(&input),
            vec![(A.to_string(), B.to_string()), (B.to_string(), C.to_string())]
        );
    }

    #[test]
    fn skips_blank_and_single_token_lines() {
        let input = format!("\n{A}\n   \n{A} {B}\n");
        assert_eq!(parse_pairs(&input), vec![(A.to_string(), B.to_string())]);
    }

    #[test]
    fn rejects_non_hex_and_wrong_length() {
        let input = format!("{A} zzzz\nshort {B}\n{A} {B}extra\n");
        assert!(parse_pairs(&input).is_empty());
    }

    #[test]
    fn rejects_three_token_lines() {
        let input = format!("{A} {B} {C}\n");
        assert!(parse_pairs(&input).is_empty());
    }

    #[test]
    fn preserves_order_for_many_to_one() {
        // Two pairs mapping to the same new sha — order preserved so the
        // downstream relink_commit calls apply last-writer-wins.
        let input = format!("{A} {C}\n{B} {C}\n");
        assert_eq!(
            parse_pairs(&input),
            vec![(A.to_string(), C.to_string()), (B.to_string(), C.to_string())]
        );
    }

    #[test]
    fn is_hex40_validates() {
        assert!(is_hex40(A));
        assert!(!is_hex40("ABCDEF0000000000000000000000000000000001")); // uppercase
        assert!(!is_hex40("000")); // too short
        assert!(!is_hex40("g000000000000000000000000000000000000000")); // non-hex
    }
}
```

- [ ] **Step 3: Run to verify failure.**
  Run: `cargo test -p dkod-cli --lib relink`
  Expected: FAIL — module/`parse_pairs` not found until mod.rs + file land; once they compile, tests run. (If the lib test harness name differs, `cargo test -p dkod-cli parse` also targets them.)

- [ ] **Step 4: (implementation is already in Step 2's file).** Ensure it compiles: `cargo build`.

- [ ] **Step 5: Run green.**
  Run: `cargo test -p dkod-cli relink`
  Expected: the 6 `relink::tests` PASS.

- [ ] **Step 6: Gates + CodeRabbit + commit.**
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-cli/src/cmd/relink.rs crates/dkod-cli/src/cmd/mod.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(relink): dkod relink command parses post-rewrite pairs and re-links"
  coderabbit --type committed --plain
  ```

### Task 2.2: Wire the hidden `Relink` subcommand in `main.rs`

**Files:**
- Modify: `crates/dkod-cli/src/main.rs`

- [ ] **Step 1: Read `main.rs`.** Locate the `Cmd` enum (note the `#[command(hide = true)] CaptureHook { .. }` variant), the `match cli.cmd { ... }` dispatch, and the `maybe_warn_drift()` guard line `if !matches!(cli.cmd, Cmd::Setup { .. } | Cmd::CaptureHook { .. }) { maybe_warn_drift(); }`.

- [ ] **Step 2: Add the variant.** In the `Cmd` enum, add (place near `CaptureHook`):

```rust
    /// Internal: invoked by the git post-rewrite hook to re-link sessions
    /// after a history rewrite. Not for direct use.
    #[command(hide = true)]
    Relink,
```

- [ ] **Step 3: Exclude from drift warning.** Update the `maybe_warn_drift()` guard so `Relink` is excluded (it must stay fast + silent during a git rewrite, like `CaptureHook`):

```rust
    if !matches!(cli.cmd, Cmd::Setup { .. } | Cmd::CaptureHook { .. } | Cmd::Relink) {
        maybe_warn_drift();
    }
```

(Match the exact existing form — if the guard is written differently, add `| Cmd::Relink` to its `matches!` pattern.)

- [ ] **Step 4: Add the dispatch arm.** In `match cli.cmd { ... }`, add:

```rust
        Cmd::Relink => cmd::relink::run(&std::env::current_dir()?),
```

- [ ] **Step 5: Build + verify the command is hidden.**
  Run: `cargo build && ./target/debug/dkod --help`
  Expected: build clean; `relink` does NOT appear in help output (hidden), same as `capture-hook`.

- [ ] **Step 6: Gates + CodeRabbit + commit.**
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-cli/src/main.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(cli): wire hidden dkod relink subcommand"
  coderabbit --type committed --plain
  ```

---

## Phase 3 — install the `post-rewrite` hook in `dkod init`

### Task 3.1: `install_post_rewrite_hook` + call site

**Files:**
- Modify: `crates/dkod-cli/src/cmd/init.rs`
- Test: `crates/dkod-cli/tests/init_post_rewrite_hook.rs` (new)

- [ ] **Step 1: Write the failing integration tests.** Create `crates/dkod-cli/tests/init_post_rewrite_hook.rs`:

```rust
//! `dkod init` installs a sentinel-guarded post-rewrite hook, idempotently,
//! and never clobbers a foreign hook.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
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

fn hooks_dir(repo: &Path) -> PathBuf {
    // Resolve the path git uses for hooks (honors worktrees).
    let out = std::process::Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "rev-parse", "--git-path", "hooks"])
        .output()
        .unwrap();
    let rel = String::from_utf8(out.stdout).unwrap().trim().to_string();
    let p = PathBuf::from(&rel);
    if p.is_absolute() { p } else { repo.join(p) }
}

fn dkod_init(repo: &Path) {
    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo)
        .arg("init")
        .assert()
        .success();
}

#[test]
#[cfg(unix)]
fn init_writes_executable_post_rewrite_hook() {
    use std::os::unix::fs::PermissionsExt;
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    dkod_init(repo.path());

    let hook = hooks_dir(repo.path()).join("post-rewrite");
    assert!(hook.exists(), "hook should be written");
    let body = std::fs::read_to_string(&hook).unwrap();
    assert!(body.contains("dkod-managed"), "missing sentinel:\n{body}");
    assert!(body.contains("exec dkod relink"), "missing relink call:\n{body}");
    let mode = std::fs::metadata(&hook).unwrap().permissions().mode();
    assert!(mode & 0o111 != 0, "hook must be executable, mode={mode:o}");
}

#[test]
#[cfg(unix)]
fn init_is_idempotent() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    dkod_init(repo.path());
    let hook = hooks_dir(repo.path()).join("post-rewrite");
    let first = std::fs::read_to_string(&hook).unwrap();
    dkod_init(repo.path()); // second run
    let second = std::fs::read_to_string(&hook).unwrap();
    assert_eq!(first, second, "re-running init must not change the managed hook");
}

#[test]
#[cfg(unix)]
fn init_does_not_clobber_foreign_hook() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    let hook = hooks_dir(repo.path()).join("post-rewrite");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\necho i was here first\n").unwrap();

    dkod_init(repo.path());

    let body = std::fs::read_to_string(&hook).unwrap();
    assert_eq!(body, "#!/bin/sh\necho i was here first\n", "foreign hook must be left intact");
}
```

- [ ] **Step 2: Run to verify failure.**
  Run: `cargo test -p dkod-cli --test init_post_rewrite_hook`
  Expected: FAIL — no hook written (the install fn doesn't exist yet).

- [ ] **Step 3: Implement `install_post_rewrite_hook` in `init.rs`.** Add the constant and function:

```rust
/// Sentinel comment marking the dkod-managed post-rewrite hook so re-running
/// `dkod init` refreshes our hook but never overwrites a foreign one.
const POST_REWRITE_SENTINEL: &str = "# dkod-managed: re-link sessions after history rewrite";

/// The full managed hook script.
const POST_REWRITE_SCRIPT: &str =
    "#!/bin/sh\n# dkod-managed: re-link sessions after history rewrite\nexec dkod relink\n";

/// Install `.git/hooks/post-rewrite` so a rebase/amend/squash re-links session
/// commit-refs via `dkod relink`. Idempotent; never clobbers a foreign hook.
/// Honors `core.hooksPath` by warning (not installing) when hooks are redirected.
fn install_post_rewrite_hook(cwd: &Path) -> Result<()> {
    // If hooks are redirected, our .git/hooks file would never fire — warn instead.
    let hp = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .context("invoke `git config --get core.hooksPath`")?;
    if hp.status.success() {
        let path = String::from_utf8_lossy(&hp.stdout).trim().to_string();
        if !path.is_empty() {
            eprintln!(
                "dkod init: core.hooksPath is set ({path}); install a post-rewrite hook there \
                 manually (exec dkod relink) to enable session re-linking after history rewrites."
            );
            return Ok(());
        }
    }

    // Resolve the hooks dir git actually uses.
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--git-path", "hooks"])
        .output()
        .context("invoke `git rev-parse --git-path hooks`")?;
    if !out.status.success() {
        return Err(anyhow!(
            "`git rev-parse --git-path hooks` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let hooks_dir = {
        let p = std::path::PathBuf::from(&rel);
        if p.is_absolute() {
            p
        } else {
            cwd.join(p)
        }
    };
    std::fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("create hooks dir {}", hooks_dir.display()))?;
    let hook = hooks_dir.join("post-rewrite");

    // Foreign-hook guard: only write if absent or our own (sentinel present).
    if hook.exists() {
        let existing = std::fs::read_to_string(&hook).unwrap_or_default();
        if !existing.contains(POST_REWRITE_SENTINEL) {
            eprintln!(
                "dkod init: existing post-rewrite hook found at {}; not overwriting. \
                 Add `exec dkod relink` to it to enable session re-linking.",
                hook.display()
            );
            return Ok(());
        }
    }

    std::fs::write(&hook, POST_REWRITE_SCRIPT)
        .with_context(|| format!("write {}", hook.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod 755 {}", hook.display()))?;
    }
    Ok(())
}
```

- [ ] **Step 4: Call it from `run`, non-fatally.** In `init.rs::run`, after the claude `install_hooks_at_init` block (near the end, before `Ok(())`), add:

```rust
    // Install the post-rewrite hook so history rewrites re-link sessions
    // (dkod blame keeps resolving rewritten lines). Non-fatal: init still
    // succeeds if the hook can't be written (e.g. read-only hooks dir).
    if let Err(e) = install_post_rewrite_hook(cwd) {
        eprintln!("dkod init: could not install post-rewrite hook: {e:#}");
    }
```

- [ ] **Step 5: Run green.**
  Run: `cargo test -p dkod-cli --test init_post_rewrite_hook`
  Expected: 3 tests PASS.

- [ ] **Step 6: Gates + CodeRabbit + commit.**
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-cli/src/cmd/init.rs crates/dkod-cli/tests/init_post_rewrite_hook.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(init): install sentinel-guarded post-rewrite hook"
  coderabbit --type committed --plain
  ```

---

## Phase 4 — end-to-end re-link → blame proof

### Task 4.1: `e2e_relink.rs`

**Files:**
- Test: `crates/dkod-cli/tests/e2e_relink.rs` (new)

Proves the `relink → blame` chain through the real `dkod` binary. To stay
hermetic we do NOT depend on git auto-firing the hook (the hook calls bare
`dkod`, which may not be on PATH in the test sandbox). Instead we drive
`dkod relink` directly with the `old new` pair on stdin — exactly what the hook
would pipe — then assert blame resolves the rewritten line.

- [ ] **Step 1: Write the test.**

```rust
//! End-to-end: after a history rewrite, `dkod relink` (fed the old→new pair
//! git's post-rewrite hook would emit) restores `dkod blame` provenance on the
//! rewritten commit.

use assert_cmd::Command;
use std::path::Path;
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
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
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string()
}

#[test]
fn relink_restores_blame_after_amend() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "ai line\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "original"]);
    let old = head(repo.path());

    // Seed a session linked to the original commit.
    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 0,
        duration_ms: 0,
        prompt_summary: "write the greeting".into(),
        messages: vec![],
        commits: vec![old.clone()],
        files_touched: vec!["f.txt".into()],
    };
    dkod_core::store::write_session(repo.path(), &s).unwrap();
    dkod_core::store::link_session_to_commit(repo.path(), &s.id, &old).unwrap();

    // Rewrite history: amend changes the commit SHA.
    git(repo.path(), &["commit", "--amend", "-qm", "reworded"]);
    let new = head(repo.path());
    assert_ne!(old, new, "amend must change the sha");

    // Before relink, blame can't resolve the rewritten line.
    let before = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["blame", "f.txt"])
        .assert()
        .success();
    let before_out = String::from_utf8(before.get_output().stdout.clone()).unwrap();
    assert!(before_out.contains("(human)"), "expected (human) before relink:\n{before_out}");

    // Feed the post-rewrite pair to `dkod relink` via stdin (what the hook does).
    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .arg("relink")
        .write_stdin(format!("{old} {new}\n"))
        .assert()
        .success();

    // After relink, blame attributes the rewritten line to the session.
    let after = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .args(["blame", "f.txt"])
        .assert()
        .success();
    let after_out = String::from_utf8(after.get_output().stdout.clone()).unwrap();
    assert!(after_out.contains("claude_code"), "expected agent label after relink:\n{after_out}");
    assert!(after_out.contains("write the greeting"), "expected prompt summary after relink:\n{after_out}");
}
```

> If the `blame` output column format differs (e.g. agent label rendering), read `crates/dkod-cli/src/cmd/blame.rs` and assert on the actual rendered tokens (agent label from `dkod_core::agent_label`, short session id, prompt summary). Do not weaken the assertion to a trivial check.

- [ ] **Step 2: Run.**
  Run: `cargo test -p dkod-cli --test e2e_relink`
  Expected: PASS. If the "before" assertion is flaky because blame renders the original commit's short sha rather than `(human)` for a single self-authored commit, adjust the "before" assertion to assert the absence of the agent label / prompt summary instead — but keep the "after" assertions strict (they are the real proof).

- [ ] **Step 3: Gates + CodeRabbit + commit.**
  ```bash
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-cli/tests/e2e_relink.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "test(relink): e2e proof relink restores blame after amend"
  coderabbit --type committed --plain
  ```

---

## Phase 5 — docs + PR

### Task 5.1: Update the positioning roadmap

**Files:**
- Modify: `docs/plans/2026-05-27-macroscope-competitive-positioning.md`

- [ ] **Step 1: Read the `dkod blame` roadmap entry** (the "### 1. `dkod blame`" block and its carried-forward limitation about history rewrite).

- [ ] **Step 2: Edit the limitation.** State that history-rewrite re-linking now ships: rebase / `commit --amend` / squash / reword re-link session commit-refs via a per-repo `post-rewrite` hook (installed by `dkod init`) feeding `dkod relink`. Document the residue as remaining limitations: `git filter-repo`/external/pre-`init` rewrites are not auto-relinked; squash is lossy (last-writer-wins); `core.hooksPath` users wire manually; `dkod show` may show a pre-rewrite SHA (refs-only). Reference the design doc `docs/plans/2026-06-03-history-rewrite-relinking-design.md`.

- [ ] **Step 3: Commit (docs-only).**
  ```bash
  git add docs/plans/2026-05-27-macroscope-competitive-positioning.md
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "docs: history-rewrite re-linking shipped (post-rewrite hook + dkod relink)"
  ```
  (CodeRabbit does not meaningfully review docs — note this in your report; skip the CodeRabbit passes for this docs-only commit.)

### Task 5.2: Open the PR and drive it to merge

- [ ] **Step 1: Final local verification.**
  Run: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check`
  Expected: all green.

- [ ] **Step 2: Full-branch CodeRabbit.**
  Run: `coderabbit --type all --base main --plain`; resolve every actionable finding (new commits, re-run until clean).

- [ ] **Step 3: Push.**
  ```bash
  gh auth switch --user haim-ari
  git push -u origin feat/history-rewrite-relinking
  ```

- [ ] **Step 4: Open the PR.**
  Title (≤70 chars): `feat: re-link sessions after history rewrites`
  Body covers: the `post-rewrite` hook → `dkod relink` → `relink_commit` chain; install at `dkod init` (sentinel-guarded, foreign-hook-safe, `core.hooksPath` warning); squash last-writer-wins; refs-only; and the documented residual limitations. Reference the design doc.

- [ ] **Step 5: Drive to green and merge.**
  Trigger CodeRabbit (`@coderabbitai review` comment), wait for CI + CodeRabbit
  server-side review, fix findings (re-`gh auth switch` before each push), repeat
  until CI green + CodeRabbit approved, then squash-merge with `--delete-branch`.
  After merge, sync local `main`
  (`git checkout main && git fetch origin && git reset --hard origin/main`).

---

## Self-review checklist (controller, before dispatch)

- Spec coverage: `relink_commit` (Phase 1) ✓; `dkod relink` command + wiring (Phase 2) ✓; `post-rewrite` hook install at `dkod init` with sentinel / foreign-hook / `core.hooksPath` (Phase 3) ✓; e2e relink→blame (Phase 4) ✓; docs (Phase 5) ✓. Squash last-writer-wins covered by `relink_commit_last_writer_wins` + `preserves_order_for_many_to_one`. Refs-only honored (no blob writes anywhere). Always-exit-0 honored (`run` returns `Ok(())` unconditionally).
- Type/name consistency: `relink_commit(repo_path, old_sha, new_sha) -> Result<bool>`, `parse_pairs(&str) -> Vec<(String,String)>`, `is_hex40`, `Cmd::Relink`, `cmd::relink::run(&Path)`, `refs::commit_ref`, `POST_REWRITE_SENTINEL`/`POST_REWRITE_SCRIPT` — consistent across tasks.
- No placeholders: every code step shows complete code.

## Known limitations (carry in the PR description)

1. Coverage limited to git `post-rewrite` triggers (amend/rebase/squash/reword); `filter-repo`, external, and pre-`init` rewrites are not auto-relinked.
2. Squash is lossy (last-writer-wins; other sessions' provenance for that commit is dropped).
3. `core.hooksPath` users must install the hook manually (init warns).
4. `dkod show` may list a pre-rewrite SHA (refs-only; blame unaffected).
