# All-Agent Commit Linking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend HEAD-watching session→commit linkage from Claude-Code-only to all six agent wrappers (codex, copilot-cli, cursor, factory-ai, gemini-cli, opencode) so `dkod blame` attributes lines regardless of which agent wrote them.

**Architecture:** Promote the `head_sha` helper into `dkod_core::store` (canonical home, reachable everywhere). Add one `pub fn finalize_session` in the CLI capture module that does the redact + link-write tail every wrapper currently duplicates. Rewire each of the six synchronous wrappers to record HEAD *before* spawning the agent and call `finalize_session` after. The linking logic is tested once against the shared helper; one real-wrapper end-to-end test (fake `codex` via `DKOD_CODEX_BIN`) proves the head-before-agent ordering works through an actual `dkod capture` invocation.

**Tech Stack:** Rust (edition 2021), `gix` 0.66 (commit-graph walk, already a dep), `git` CLI via `std::process::Command` in tests only, `anyhow`, `assert_cmd` + `tempfile` for tests.

---

## Hard rules (apply to EVERY task; paste verbatim into any subagent prompt)

1. **Git identity:** every commit and push uses
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
6. **YAGNI:** build only what a task specifies.

---

## Background facts (already verified — do not re-discover)

- `dkod_core::store::new_commits_since(repo, start: Option<&str>) -> Result<Vec<String>>`
  and `dkod_core::store::write_session_with_commit_links(repo, &mut Session, head_at_start: Option<&str>) -> Result<Vec<String>>`
  already exist and are tested (shipped in PR #19). `write_session_with_commit_links`
  sets `session.commits`, writes the session blob (the **only** fatal step), then
  best-effort writes `refs/dkod/commits/<sha>` per commit, returning the linked shas.
- `head_sha` currently lives at `crates/dkod-core/src/capture/claude_code.rs` (~line 132)
  as `pub(crate) fn head_sha(path: &Path) -> Option<String>`, with 3 unit tests in that
  file's test module: `head_sha_returns_none_for_non_repo`,
  `head_sha_returns_none_for_unborn_repo`, `head_sha_returns_sha_after_commit`.
- The Claude Code **server** path (`crates/dkod-cli/src/cmd/capture/claude_code.rs::handle_finished_session`)
  already links commits. **DO NOT change its behavior.** Phase 1 only relocates `head_sha`;
  the server keeps working by calling the new location.
- The six wrappers are structurally identical. Their exact current bodies:
  - `codex.rs`: bin `DKOD_CODEX_BIN` default `"codex"`; resolves `CODEX_HOME`
    (`std::env::var_os("CODEX_HOME")` or `$HOME/.codex`); `CaptureOptions { args, codex_bin, codex_home, cwd }`.
  - `copilot_cli.rs`: bin `DKOD_COPILOT_BIN` default `"copilot"`; resolves `COPILOT_HOME`
    (or `$HOME/.copilot`); `CaptureOptions { args, copilot_bin, copilot_home, cwd }`.
  - `cursor.rs`: bin `DKOD_CURSOR_BIN` default `"cursor-agent"`; `CaptureOptions { args, cursor_bin, cwd }`.
  - `factory_ai.rs`: bin `DKOD_FACTORY_BIN` (with `.filter(|v| !v.is_empty())`) default `"droid"`;
    `CaptureOptions { args, factory_bin, cwd }`.
  - `gemini_cli.rs`: bin `DKOD_GEMINI_BIN` default `"gemini"`; `CaptureOptions { args, gemini_bin, cwd }`.
  - `opencode.rs`: bin `DKOD_OPENCODE_BIN` default `"opencode"`; `CaptureOptions { args, opencode_bin, cwd }`.
  Each wrapper's tail is identical:
  ```rust
  dkod_core::redact::redact_session(&mut session, &cfg.redact);
  dkod_core::store::write_session(cwd, &session).context("write session")?;
  eprintln!("dkod: captured session {}", session.id);
  Ok(())
  ```
- `crates/dkod-cli/src/lib.rs` exposes `pub mod cmd;`, so integration tests can reach
  `dkod_cli::cmd::capture::finalize_session` once it is `pub`.
- `dkod_core` is already a dev-dependency of `dkod-cli` (used by `tests/e2e_blame.rs`).
- `Session` fields (from `crates/dkod-core/src/session.rs`): `id, agent, created_at: i64,
  duration_ms: u64, prompt_summary: String, messages: Vec<Message>, commits: Vec<String>,
  files_touched: Vec<String>`. `Session::new_id()` mints a fresh id.
- `Config::default()` is `dkod_core::config::Config::default()`.

---

## Phase 1 — Promote `head_sha` to `dkod_core::store`

### Task 1.1: Move `head_sha` and its tests into `store.rs`

**Files:**
- Modify: `crates/dkod-core/src/store.rs` (add `head_sha` + move its 3 tests in)
- Modify: `crates/dkod-core/src/capture/claude_code.rs` (delete local `head_sha`, its 3 tests; call `crate::store::head_sha`)

- [ ] **Step 1: Read the current helper and tests.**
  Open `crates/dkod-core/src/capture/claude_code.rs`. Find `pub(crate) fn head_sha(path: &Path) -> Option<String>` (~line 132) and copy its exact body. In the same file's `#[cfg(test)] mod tests`, find the 3 tests `head_sha_returns_none_for_non_repo`, `head_sha_returns_none_for_unborn_repo`, `head_sha_returns_sha_after_commit` and copy them verbatim. Note every call site of `head_sha` in this file (the SessionStart handler) — these will become `crate::store::head_sha`.

- [ ] **Step 2: Add `head_sha` to `store.rs`.**
  In `crates/dkod-core/src/store.rs`, add (place it next to `new_commits_since`):

  ```rust
  /// Current HEAD commit SHA of the git repo at `path`, or None if `path`
  /// isn't a git repo or HEAD is unborn (no commits yet). Used by the capture
  /// flows to record where a session started, so the commits it produces can be
  /// linked via `write_session_with_commit_links`.
  pub fn head_sha(path: &Path) -> Option<String> {
      gix::open(path).ok()?.head_id().ok().map(|id| id.detach().to_string())
  }
  ```

  (If the original body differed, prefer the original body — it is known-correct. The
  signature must be exactly `pub fn head_sha(path: &Path) -> Option<String>`.)

- [ ] **Step 3: Move the 3 tests into `store.rs`'s test module.**
  Paste the 3 copied tests into the `#[cfg(test)] mod tests` block in `store.rs`. They
  reference `super::head_sha`, `gix`, `tempfile::TempDir`, and the `ensure_committer` +
  `commit_as` pattern already used by other tests in that module — confirm the imports
  they need are present in that test module (TempDir is already used there). Adjust any
  `super::` path so it resolves to `store::head_sha`.

- [ ] **Step 4: Delete the local copy + tests from `claude_code.rs` and repoint callers.**
  In `crates/dkod-core/src/capture/claude_code.rs`: delete the `head_sha` fn and its 3
  tests. Replace every `head_sha(...)` call with `crate::store::head_sha(...)`. If the file
  had a `use` bringing it into scope, fix it.

- [ ] **Step 5: Run the moved tests.**
  Run: `cargo test -p dkod-core head_sha`
  Expected: 3 tests pass, all now located in the store module.

- [ ] **Step 6: Full gates.**
  Run: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check`
  Expected: all green (the Claude server path still compiles and its tests pass — proves
  the relocation didn't change behavior).

- [ ] **Step 7: CodeRabbit + commit.**
  ```bash
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-core/src/store.rs crates/dkod-core/src/capture/claude_code.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "refactor(store): promote head_sha to store for reuse across agents"
  coderabbit --type committed --plain
  ```

---

## Phase 2 — Shared `finalize_session` helper

### Task 2.1: Add `finalize_session` to `cmd/capture/mod.rs`

**Files:**
- Modify: `crates/dkod-cli/src/cmd/capture/mod.rs`
- Test: `crates/dkod-cli/tests/finalize_session.rs` (new)

- [ ] **Step 1: Read `cmd/capture/mod.rs`.**
  Open `crates/dkod-cli/src/cmd/capture/mod.rs`. It currently only declares submodules
  (`pub mod claude_code; pub mod codex; ...`). Confirm there is no existing `finalize_session`.

- [ ] **Step 2: Write the failing integration test.**
  Create `crates/dkod-cli/tests/finalize_session.rs`:

  ```rust
  //! Integration test for the shared `finalize_session` helper that every
  //! agent wrapper uses to redact, persist, and commit-link a session.

  use dkod_cli::cmd::capture::finalize_session;
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
      assert!(
          out.status.success(),
          "git {:?}: {}",
          args,
          String::from_utf8_lossy(&out.stderr)
      );
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

  fn fixture_session(prompt: &str) -> dkod_core::Session {
      dkod_core::Session {
          id: dkod_core::Session::new_id(),
          agent: dkod_core::Agent::Codex,
          created_at: 0,
          duration_ms: 0,
          prompt_summary: prompt.into(),
          messages: vec![],
          commits: vec![],
          files_touched: vec![],
      }
  }

  #[test]
  fn finalize_links_commits_made_since_head_at_start() {
      let repo = TempDir::new().unwrap();
      git(repo.path(), &["init", "-q"]);
      std::fs::write(repo.path().join("f.txt"), "a\n").unwrap();
      git(repo.path(), &["add", "."]);
      git(repo.path(), &["commit", "-qm", "A"]);
      let a = head(repo.path());

      // The "agent" makes commit B after head_at_start was recorded.
      std::fs::write(repo.path().join("f.txt"), "a\nb\n").unwrap();
      git(repo.path(), &["add", "."]);
      git(repo.path(), &["commit", "-qm", "B"]);
      let b = head(repo.path());

      let cfg = dkod_core::config::Config::default();
      let mut session = fixture_session("do the thing");
      let linked = finalize_session(repo.path(), &mut session, Some(&a), &cfg).unwrap();

      assert_eq!(linked, vec![b.clone()]);
      assert_eq!(session.commits, vec![b.clone()]);

      // Session blob persisted with the commit list.
      let back = dkod_core::store::read_session(repo.path(), &session.id).unwrap();
      assert_eq!(back.commits, vec![b.clone()]);

      // refs/dkod/commits/<B> exists and points at the same blob as the session ref.
      let r = gix::open(repo.path()).unwrap();
      let sref = r
          .find_reference(&dkod_core::refs::session_ref(&session.id))
          .unwrap();
      let cref = r
          .find_reference(&dkod_core::refs::commit_ref(&b))
          .unwrap();
      assert_eq!(sref.id(), cref.id());
  }

  #[test]
  fn finalize_with_none_head_links_nothing_but_writes_session() {
      let repo = TempDir::new().unwrap();
      git(repo.path(), &["init", "-q"]);
      std::fs::write(repo.path().join("f.txt"), "a\n").unwrap();
      git(repo.path(), &["add", "."]);
      git(repo.path(), &["commit", "-qm", "A"]);

      let cfg = dkod_core::config::Config::default();
      let mut session = fixture_session("no head");
      let linked = finalize_session(repo.path(), &mut session, None, &cfg).unwrap();

      assert!(linked.is_empty());
      assert!(session.commits.is_empty());
      // Session is still written even when nothing is linked.
      assert_eq!(
          dkod_core::store::read_session(repo.path(), &session.id)
              .unwrap()
              .id,
          session.id
      );
  }
  ```

- [ ] **Step 3: Run to verify failure.**
  Run: `cargo test -p dkod-cli --test finalize_session`
  Expected: FAIL — `finalize_session` not found (does not resolve in `dkod_cli::cmd::capture`).

- [ ] **Step 4: Implement `finalize_session`.**
  In `crates/dkod-cli/src/cmd/capture/mod.rs`, add (keep the existing `pub mod ...`
  declarations; add the needed `use` for `Context`):

  ```rust
  use anyhow::{Context, Result};

  /// Redact, persist, and commit-link a freshly-captured session. The session
  /// write is the only fatal step; commit-linking is best-effort (see
  /// `dkod_core::store::write_session_with_commit_links`). `head_at_start` is the
  /// repo HEAD recorded *before* the agent ran. Returns the shas successfully
  /// linked (empty when there was no new commit / no recorded start HEAD).
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

- [ ] **Step 5: Run green.**
  Run: `cargo test -p dkod-cli --test finalize_session`
  Expected: both tests PASS.

- [ ] **Step 6: Full gates.**
  Run: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check`
  Expected: all green.

- [ ] **Step 7: CodeRabbit + commit.**
  ```bash
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-cli/src/cmd/capture/mod.rs crates/dkod-cli/tests/finalize_session.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(capture): shared finalize_session helper (redact + commit-link)"
  coderabbit --type committed --plain
  ```

---

## Phase 3 — Rewire the six wrappers + one real end-to-end proof

### Task 3.1: Rewire all six wrappers to record HEAD and call `finalize_session`

**Files (modify all six):**
- `crates/dkod-cli/src/cmd/capture/codex.rs`
- `crates/dkod-cli/src/cmd/capture/copilot_cli.rs`
- `crates/dkod-cli/src/cmd/capture/cursor.rs`
- `crates/dkod-cli/src/cmd/capture/factory_ai.rs`
- `crates/dkod-cli/src/cmd/capture/gemini_cli.rs`
- `crates/dkod-cli/src/cmd/capture/opencode.rs`

For EACH wrapper apply the same two edits. **Do not change how the wrapper resolves its
bin/opts or calls `capture_<agent>`.** Read each file first and preserve its exact
`CaptureOptions` construction.

- [ ] **Step 1 (per file): record HEAD before the agent runs.**
  Immediately after `let cfg = super::super::load_config(cwd)?;` (and before resolving the
  bin / building `CaptureOptions`), insert:

  ```rust
  // Record HEAD before the agent runs so we can link the commits it produces.
  let head_at_start = dkod_core::store::head_sha(cwd);
  ```

- [ ] **Step 2 (per file): replace the redact+write+eprintln tail.**
  Replace these three lines:

  ```rust
  dkod_core::redact::redact_session(&mut session, &cfg.redact);
  dkod_core::store::write_session(cwd, &session).context("write session")?;
  eprintln!("dkod: captured session {}", session.id);
  ```

  with:

  ```rust
  let linked = super::finalize_session(cwd, &mut session, head_at_start.as_deref(), &cfg)?;
  eprintln!(
      "dkod: captured session {} ({} commit link(s))",
      session.id,
      linked.len()
  );
  ```

  Note: `redact_session` now happens inside `finalize_session`, so the standalone redact
  line is removed. If removing the `write_session`/`redact` calls makes a `use` unused,
  clippy `-D warnings` will flag it — remove the now-unused import. (`Context` may become
  unused in a wrapper that no longer calls `.context(...)` directly — check each file; the
  `capture_<agent>(...).context("capture <agent> session")?` call usually keeps `Context`
  used, but verify.)

- [ ] **Step 3: Build + clippy + fmt after all six are edited.**
  Run: `cargo build && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check`
  Expected: clean. (No new unit test here — the linking logic is covered by Task 2.1's
  `finalize_session` tests; Task 3.2 adds the real-wrapper end-to-end proof.)

- [ ] **Step 4: Full test suite.**
  Run: `cargo test`
  Expected: all green (existing wrapper-related tests still pass; behavior is a superset —
  sessions are still written, now also linked).

- [ ] **Step 5: CodeRabbit + commit.**
  ```bash
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-cli/src/cmd/capture/codex.rs \
          crates/dkod-cli/src/cmd/capture/copilot_cli.rs \
          crates/dkod-cli/src/cmd/capture/cursor.rs \
          crates/dkod-cli/src/cmd/capture/factory_ai.rs \
          crates/dkod-cli/src/cmd/capture/gemini_cli.rs \
          crates/dkod-cli/src/cmd/capture/opencode.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(capture): link commits to sessions for all six agent wrappers"
  coderabbit --type committed --plain
  ```

### Task 3.2: Real end-to-end proof through the `codex` wrapper (fake binary)

**Files:**
- Test: `crates/dkod-cli/tests/e2e_codex_linking.rs` (new)

Proves the head-before-agent ordering works through an actual `dkod capture codex` run.
`parse_rollout` is lenient (it skips unknown record types; a one-line rollout parses to a
valid Session), and `locate_rollout` globs
`<codex_home>/sessions/*/*/*/rollout-*-<thread_id>.jsonl` requiring exactly one match — so
the fake codex only needs to write one rollout file matching that glob, emit a
`thread.started` event, make a commit, and exit 0.

- [ ] **Step 1: Write the test.**
  Create `crates/dkod-cli/tests/e2e_codex_linking.rs`:

  ```rust
  //! End-to-end: `dkod capture codex` links the commit the agent made to the
  //! captured session, via a fake codex binary (DKOD_CODEX_BIN).

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
      assert!(
          out.status.success(),
          "git {:?}: {}",
          args,
          String::from_utf8_lossy(&out.stderr)
      );
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
  #[cfg(unix)]
  fn capture_codex_links_agent_commit() {
      let repo = TempDir::new().unwrap();
      git(repo.path(), &["init", "-q"]);
      std::fs::write(repo.path().join("f.txt"), "start\n").unwrap();
      git(repo.path(), &["add", "."]);
      git(repo.path(), &["commit", "-qm", "start"]);

      let codex_home = TempDir::new().unwrap();
      let rollout_dir = codex_home.path().join("sessions/2026/05/29");
      std::fs::create_dir_all(&rollout_dir).unwrap();

      // Fake codex: make a commit (the "agent's" work), write a one-line rollout
      // matching locate_rollout's glob, emit thread.started, exit 0.
      let tid = "deadbeefcafe";
      let rollout = rollout_dir.join(format!("rollout-2026-05-29T00-00-00-{tid}.jsonl"));
      let fake = codex_home.path().join("fake-codex.sh");
      let script = format!(
          r#"#!/bin/sh
git -C "{repo}" -c user.name=t -c user.email=t@e.com commit --allow-empty -m agentwork >/dev/null 2>&1
printf '%s\n' '{{"type":"session_meta","payload":{{"cli_version":"99.0.0"}}}}' > "{rollout}"
printf '%s\n' '{{"type":"thread.started","thread_id":"{tid}"}}'
exit 0
"#,
          repo = repo.path().display(),
          rollout = rollout.display(),
          tid = tid,
      );
      std::fs::write(&fake, script).unwrap();
      use std::os::unix::fs::PermissionsExt;
      std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

      Command::cargo_bin("dkod")
          .unwrap()
          .current_dir(repo.path())
          .env("DKOD_CODEX_BIN", &fake)
          .env("CODEX_HOME", codex_home.path())
          .args(["capture", "codex", "--", "noop"])
          .assert()
          .success();

      // The agent's commit (now HEAD) must be linked: refs/dkod/commits/<HEAD> exists.
      let agent_commit = head(repo.path());
      let r = gix::open(repo.path()).unwrap();
      let cref = r.find_reference(&dkod_core::refs::commit_ref(&agent_commit));
      assert!(
          cref.is_ok(),
          "expected refs/dkod/commits/{agent_commit} to exist after capture"
      );
      // And it resolves to a session blob.
      let cref = cref.unwrap();
      let obj = r.find_object(cref.id()).unwrap().detach();
      let session: dkod_core::Session = serde_json::from_slice(&obj.data).unwrap();
      assert!(session.commits.contains(&agent_commit));
  }
  ```

- [ ] **Step 2: Run the test.**
  Run: `cargo test -p dkod-cli --test e2e_codex_linking`
  Expected: PASS.

  **Fallback clause:** if constructing a parser-accepted rollout proves too fiddly and the
  test cannot be made to pass cleanly within reasonable effort, STOP and report it. The
  `finalize_session` integration tests (Task 2.1) plus the existing Claude e2e are accepted
  as sufficient coverage for the linking logic; downgrade this codex e2e to best-effort
  (mark `#[ignore]` with a comment explaining why, or omit the file) rather than rabbit-hole
  on the fake-codex rollout protocol. Do not let this block the wrapper rewire.

- [ ] **Step 3: Full gates.**
  Run: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check`
  Expected: all green.

- [ ] **Step 4: CodeRabbit + commit.**
  ```bash
  coderabbit --type uncommitted --plain   # resolve findings
  git add crates/dkod-cli/tests/e2e_codex_linking.rs
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "test(capture): e2e proof codex wrapper links the agent's commit"
  coderabbit --type committed --plain
  ```

---

## Phase 4 — Docs

### Task 4.1: Update the positioning roadmap

**Files:**
- Modify: `docs/plans/2026-05-27-macroscope-competitive-positioning.md`

- [ ] **Step 1: Read the `dkod blame` roadmap entry.**
  Open the file and find "### 1. `dkod blame`" (~line 112) and its "Status: implemented" /
  "Carried-forward limitations" block (notes it links only the Claude Code capture path).

- [ ] **Step 2: Edit the limitation.**
  Update the status block so it states `dkod blame` now links commits across **all** agents
  (Claude Code server path + the six synchronous wrappers: codex, copilot-cli, cursor,
  factory-ai, gemini-cli, opencode). Remove the "only the Claude Code capture path links
  commits" limitation. KEEP the history-rewrite limitation (rebase/squash/amend rewrites
  SHAs and dangles `refs/dkod/commits/<old-sha>`; re-linking on observed rewrites is a
  follow-up).

- [ ] **Step 3: Commit (docs-only).**
  ```bash
  git add docs/plans/2026-05-27-macroscope-competitive-positioning.md
  git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "docs: dkod blame now links commits across all agents"
  ```
  (CodeRabbit does not meaningfully review docs — skip the CodeRabbit passes for this
  docs-only commit, but say so in your report.)

---

## Phase 5 — PR

### Task 5.1: Open the PR and drive it to merge

- [ ] **Step 1: Final local verification.**
  Run: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check`
  Expected: all green.

- [ ] **Step 2: Full-branch CodeRabbit.**
  Run: `coderabbit --type all --base main --plain`
  Resolve every actionable finding (new commits, re-run until clean).

- [ ] **Step 3: Push.**
  ```bash
  gh auth switch --user haim-ari
  git push -u origin <branch>     # the feature branch this work is on
  ```

- [ ] **Step 4: Open the PR.**
  Title (≤70 chars): `feat: link commits to sessions for all agents`
  Body covers: the shared `finalize_session` helper, `head_sha` promotion, the six-wrapper
  rewire, the codex e2e proof (or the fallback note if downgraded), and that `dkod blame`
  now attributes lines regardless of agent. Note the carried-forward history-rewrite
  limitation.

- [ ] **Step 5: Drive to green and merge.**
  Trigger CodeRabbit (`@coderabbitai review` comment if needed), wait for CI + CodeRabbit
  server-side review, fix findings (re-`gh auth switch` before each push), repeat until CI
  green + CodeRabbit approved, then squash-merge with `--delete-branch`. After merge, sync
  local `main` (`git checkout main && git fetch origin && git reset --hard origin/main`).

---

## Self-review checklist (controller, before dispatch)

- Spec coverage: head_sha promotion (Phase 1) ✓; shared finalize_session + test (Phase 2) ✓;
  six wrappers rewired (Phase 3.1) ✓; real end-to-end proof (Phase 3.2) ✓; docs (Phase 4) ✓;
  PR→merge (Phase 5) ✓.
- Type/name consistency: `head_sha`, `finalize_session`, `write_session_with_commit_links`,
  `new_commits_since`, `refs::commit_ref`, `refs::session_ref`, `Config::default()`,
  `Session` field set — all match the verified codebase signatures above.
- No placeholders: every code step shows complete code; the six-wrapper edit is identical
  and fully specified; the fake-codex script is complete.

## Known limitations (carry in the PR description)

1. **History rewrite breaks links** (unchanged from the blame V1): rebase/squash/amend after
   capture rewrites commit SHAs; `refs/dkod/commits/<old-sha>` then dangles.
2. **`git` runtime dependency for `dkod blame`** (unchanged): the read command shells out to
   `git blame`.
