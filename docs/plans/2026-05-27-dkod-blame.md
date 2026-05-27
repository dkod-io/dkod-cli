# dkod blame (git-blame-for-AI) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Ship `dkod blame <path>` — for each line of a file, show the AI agent session (agent + short session id + prompt summary) that produced it, falling back to ordinary blame info for human/unlinked commits.

**Architecture:** Two parts. **Part 1 (prerequisite)** wires the currently-dead session→commit linking: the capture server records repo HEAD at session start, and at session end links every new commit reachable from HEAD to the session via `refs/dkod/commits/<sha>` (populating `session.commits` too). **Part 2** is the read command: shell out to `git blame --porcelain`, map each line's commit SHA through `refs/dkod/commits/<sha>` → session blob, and render an annotated listing.

**Tech Stack:** Rust (edition 2021), `gix` 0.66 (commit-graph walk for Part 1), `git` CLI via `std::process::Command` (porcelain blame for Part 2 — see Phase 0), `anyhow`, `assert_cmd` + `tempfile` for tests.

---

## Hard rules (apply to EVERY task; paste verbatim into any subagent prompt)

1. **Git identity:** every commit and push uses
   `git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit ...`.
   No `--author`-only. No `Co-Authored-By:` lines, ever. No model/agent attribution.
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
6. **YAGNI:** build only what a task specifies. No speculative flags, no
   abstractions with a single caller (except the one DRY extraction called
   for explicitly in Task 4).

---

## Phase 0 — Blame capability spike (decision already made; verify, don't re-litigate)

**Finding (verified 2026-05-27):** the workspace pins `gix = "0.66"`, which has
**no blame support** — `gix-blame` is absent from `Cargo.lock` and did not ship
in the gix 0.66 line. Bumping gix risks a workspace-wide migration (store.rs,
worktree_diff.rs, and the capture server all rely on gix 0.66-specific APIs, some
called out in comments).

**Decision:** Part 2 shells out to `git blame --porcelain`. Precedent: 
`crates/dkod-cli/src/cmd/init.rs` already shells out to `git` (`git remote`,
`git config`). The dkod-core "no shell-out to git" rule was scoped to the capture
hot path, **not** CLI read commands. `dkod blame` is a read-only command where a
`git` runtime dependency is acceptable.

### Task 0.1: Confirm the spike finding and record it

**Files:**
- Modify: `docs/plans/2026-05-27-dkod-blame.md` (this file — tick the box below)

**Step 1:** Run `grep -c 'name = "gix-blame"' Cargo.lock` — expect `0`.
**Step 2:** Run `git blame --porcelain --help 2>/dev/null | head -1 || git --version` — confirm `git` is on PATH (it is the install/runtime assumption already).
**Step 3:** No code changes. This phase is a documented decision gate.

- [ ] Spike confirmed: gix 0.66 has no blame; Part 2 uses `git blame --porcelain`.

(No commit for Phase 0 unless you ticked the box above; if so, fold it into the Task 1.1 commit or commit as a docs-only change.)

---

## Phase 1 — HEAD-watching commit association (the prerequisite)

This makes `refs/dkod/commits/<sha>` real in production. Three tasks: a pure
commit-diff helper in dkod-core, threading the start-HEAD through the server,
and the link-on-finish wiring.

### Task 1.1: `new_commits_since` helper in dkod-core::store

Returns the commit SHAs reachable from the repo's current HEAD but **not** from a
given start commit. If `start` is `None` (we never recorded a start HEAD), return
an empty Vec — never over-attribute.

**Files:**
- Modify: `crates/dkod-core/src/store.rs`
- Test: add to the existing `#[cfg(test)] mod tests` in `crates/dkod-core/src/store.rs`

**Step 1: Write the failing test.** Append to `store.rs` tests:

```rust
#[test]
fn new_commits_since_returns_commits_after_start() {
    use gix::ObjectId;
    let tmp = TempDir::new().unwrap();
    let mut repo = gix::init(tmp.path()).unwrap();
    super::ensure_committer(&mut repo).unwrap();

    let sig = gix::actor::SignatureRef {
        name: "t".into(),
        email: "t@e.com".into(),
        time: gix::date::Time::now_utc(),
    };
    let tree = repo.empty_tree().id().into();

    // commit A (the "start" point)
    let a = repo
        .commit_as(sig, sig, "HEAD", "a", tree, Vec::<ObjectId>::new())
        .unwrap()
        .detach();
    // commits B, C on top
    let b = repo.commit_as(sig, sig, "HEAD", "b", tree, vec![a]).unwrap().detach();
    let c = repo.commit_as(sig, sig, "HEAD", "c", tree, vec![b]).unwrap().detach();

    let got = super::new_commits_since(tmp.path(), Some(&a.to_string())).unwrap();
    // B and C are new; A is the start and excluded. Order: newest-first is fine,
    // assert as a set.
    let set: std::collections::BTreeSet<String> = got.into_iter().collect();
    assert_eq!(
        set,
        [b.to_string(), c.to_string()].into_iter().collect()
    );
}

#[test]
fn new_commits_since_none_start_returns_empty() {
    let tmp = TempDir::new().unwrap();
    let mut repo = gix::init(tmp.path()).unwrap();
    super::ensure_committer(&mut repo).unwrap();
    let sig = gix::actor::SignatureRef { name: "t".into(), email: "t@e.com".into(), time: gix::date::Time::now_utc() };
    let tree = repo.empty_tree().id().into();
    repo.commit_as(sig, sig, "HEAD", "a", tree, Vec::<gix::ObjectId>::new()).unwrap();
    assert!(super::new_commits_since(tmp.path(), None).unwrap().is_empty());
}

#[test]
fn new_commits_since_no_new_commits_returns_empty() {
    use gix::ObjectId;
    let tmp = TempDir::new().unwrap();
    let mut repo = gix::init(tmp.path()).unwrap();
    super::ensure_committer(&mut repo).unwrap();
    let sig = gix::actor::SignatureRef { name: "t".into(), email: "t@e.com".into(), time: gix::date::Time::now_utc() };
    let tree = repo.empty_tree().id().into();
    let a = repo.commit_as(sig, sig, "HEAD", "a", tree, Vec::<ObjectId>::new()).unwrap().detach();
    // start == current HEAD → nothing new
    assert!(super::new_commits_since(tmp.path(), Some(&a.to_string())).unwrap().is_empty());
}
```

**Step 2: Run to verify failure.**
Run: `cargo test -p dkod-core new_commits_since`
Expected: FAIL — `new_commits_since` not found.

**Step 3: Implement.** Add to `store.rs`:

```rust
/// Commit SHAs reachable from the repo's current HEAD but NOT reachable from
/// `start`. When `start` is `None`, returns an empty Vec — we never recorded a
/// session-start HEAD, so attributing any commit to the session would be a
/// guess. Used by the capture flow to link a session to the commits it produced.
///
/// Note: a rebase/squash/amend that rewrites SHAs *after* this runs will leave
/// the `refs/dkod/commits/<old-sha>` links pointing at commits that no longer
/// exist on the branch. Re-linking on observed history rewrites is a future
/// follow-up; V1 accepts the stale links.
pub fn new_commits_since(repo_path: &Path, start: Option<&str>) -> Result<Vec<String>> {
    let start = match start {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };
    let repo = gix::open(repo_path).context("open repo")?;
    let head_id = match repo.head_id() {
        Ok(id) => id,
        Err(_) => return Ok(Vec::new()), // unborn branch / detached with no commit
    };
    let start_oid = gix::ObjectId::from_hex(start.as_bytes())
        .context("parse start commit sha")?;
    if head_id.detach() == start_oid {
        return Ok(Vec::new());
    }

    // Walk ancestors of HEAD, stopping when we reach `start_oid` (exclusive).
    // gix 0.66: `rev_walk` yields commit ids in topological/chronological order.
    let mut out = Vec::new();
    let walk = repo
        .rev_walk([head_id])
        .all()
        .context("start rev walk")?;
    for info in walk {
        let info = info.context("walk commit")?;
        let oid = info.id;
        if oid == start_oid {
            break;
        }
        out.push(oid.to_string());
    }
    Ok(out)
}
```

> **Executor note:** verify the exact gix 0.66 `rev_walk` API while implementing
> (method names like `.all()` / iterator item type may differ). The algorithm is
> what matters: ancestors of HEAD, stop at `start_oid` exclusive. If `rev_walk`
> ergonomics fight you, a manual parent-walk (load commit, push parents, stop at
> `start_oid`, guard against cycles with a visited set) is an acceptable fallback.

**Step 4: Run green.**
Run: `cargo test -p dkod-core new_commits_since`
Expected: PASS (all three).

**Step 5: Gates + commit.**
```bash
cargo build && cargo test -p dkod-core && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
coderabbit --type uncommitted --plain   # resolve findings
git add crates/dkod-core/src/store.rs docs/plans/2026-05-27-dkod-blame.md
git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "feat(store): new_commits_since for session->commit linking"
coderabbit --type committed --plain
```

### Task 1.2: Record HEAD at session start; carry it on FinishedSession

The server (`crates/dkod-core/src/capture/claude_code.rs`) tracks in-flight
sessions and emits `FinishedSession` when one ends. Add the session-start HEAD to
that flow.

**Files:**
- Modify: `crates/dkod-core/src/capture/claude_code.rs` (FinishedSession struct + the in-memory session state + SessionStart handling)

**Step 1: Read first.** Open `crates/dkod-core/src/capture/claude_code.rs` and locate:
- the `FinishedSession` struct (around line 143),
- the in-memory per-session state struct the server keeps (search for where `SessionStart` / `WireEvent::SessionStart` is handled and where `cwd` is stored),
- where `FinishedSession` is constructed when a session ends.

**Step 2: Write the failing test.** Add a unit test in that file's test module that
drives a SessionStart for a temp git repo with one commit, then a SessionEnd, and
asserts the emitted `FinishedSession.head_at_start` equals that commit's SHA.
(Model it on the existing `server_round_trip_writes_session_blob` test in
`crates/dkod-cli/src/cmd/capture/claude_code.rs` for how to drive the server; if
the server's session struct is private, assert via a smaller pure helper instead —
see note.)

> **Executor note:** if wiring a full server test is heavy, extract a tiny pure
> helper `fn head_sha(repo: &Path) -> Option<String>` and unit-test *that*
> directly (temp repo, one commit → `Some(sha)`; empty dir → `None`), then use it
> in the SessionStart handler. The pure helper is the testable unit; the server
> plumbing is exercised end-to-end in Task 3's integration test.

**Step 3: Implement.**
- Add `pub head_at_start: Option<String>` to `FinishedSession`.
- Add a matching field to the in-memory session-state struct.
- On `SessionStart`: resolve the repo at the event's `cwd` and record
  `head_sha(cwd)` (HEAD as a 40-char hex string, or `None` if unborn/not a repo).
- When constructing `FinishedSession`, copy that field through.
- Update every other construction site of `FinishedSession` (tests, watchdog path)
  to set the new field — `cargo build` will list them.

**Step 4: Run green.**
Run: `cargo test -p dkod-core` and `cargo build`
Expected: PASS; all `FinishedSession` construction sites compile.

**Step 5: Gates + commit** (same gate block + CodeRabbit as Task 1.1).
Commit message: `feat(capture): record HEAD at claude-code session start`

### Task 1.3: Link session to its commits on finish

**Files:**
- Modify: `crates/dkod-cli/src/cmd/capture/claude_code.rs` (`handle_finished_session`, ~line 530)

**Step 1: Write the failing integration test.** In
`crates/dkod-cli/tests/` (new file `capture_commit_linking.rs` or extend an
existing capture test): build a temp git repo, write a session via the server path
(or call a small seam), make a commit after `head_at_start` was recorded, run the
finish handler, then assert:
- `store::read_session` shows the new commit in `session.commits`, and
- `refs/dkod/commits/<sha>` exists and points at the session blob
  (mirror the assertion in `store.rs::link_session_to_commit_writes_ref_pointing_at_session_blob`).

> **Executor note:** `handle_finished_session` is currently private. Either make it
> `pub(crate)` and test through a thin seam, or factor the new linking logic into a
> small `pub` function in dkod-core (e.g. `store::link_session_commits(repo, session_id, head_at_start)`)
> and test *that* directly — preferred, because it keeps the testable logic in the
> library and `handle_finished_session` just calls it. If you take this route,
> Task 1.1's helper and this can live side by side in `store.rs`.

**Step 2: Run to verify failure.**
Run: `cargo test -p dkod-cli commit_linking`
Expected: FAIL — `session.commits` empty / commit ref absent.

**Step 3: Implement.** In `handle_finished_session`, after `write_session`
succeeds (the session blob MUST exist before linking, since the commit-ref points
at that blob):

```rust
// Link the commits this session produced (HEAD-watching). Best-effort:
// a failure here must not lose the already-written session.
match dkod_core::store::new_commits_since(repo_root, fs.head_at_start.as_deref()) {
    Ok(commits) if !commits.is_empty() => {
        for sha in &commits {
            if let Err(e) = dkod_core::store::link_session_to_commit(repo_root, &id, sha) {
                eprintln!("dkod: claude-code: failed to link commit {sha}: {e:#}");
            }
        }
        // also persist the commit list onto the session blob for `dkod show`
        // (re-write the session with commits populated).
        session.commits = commits;
        if let Err(e) = dkod_core::store::write_session(repo_root, &session) {
            eprintln!("dkod: claude-code: failed to update session commits: {e:#}");
        }
    }
    Ok(_) => {}
    Err(e) => eprintln!("dkod: claude-code: commit linking skipped: {e:#}"),
}
```

> **Executor note:** mind the borrow of `session` — `id` and `n` are already taken
> before `write_session` in the current code; you may need to reorder so `session`
> is still owned when you set `session.commits`. Keep the existing
> "captured … -> dkod show" log line working.

**Step 4: Run green.**
Run: `cargo test -p dkod-cli commit_linking` then full `cargo test`.
Expected: PASS.

**Step 5: Gates + commit.**
Commit message: `feat(capture): link claude-code sessions to the commits they produce`

---

## Phase 2 — `dkod blame` read command

### Task 2.1: Porcelain blame parser (pure, in the new cmd module)

**Files:**
- Create: `crates/dkod-cli/src/cmd/blame.rs`
- Modify: `crates/dkod-cli/src/cmd/mod.rs` (add `pub mod blame;`)
- Test: unit tests inside `blame.rs`

**Step 1: Write the failing test.** In `blame.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Minimal `git blame --porcelain` sample: two lines from two commits.
    const SAMPLE: &str = "\
0000000000000000000000000000000000000001 1 1 1
author Alice
\tfn main() {
0000000000000000000000000000000000000002 2 2 1
author Bob
\tprintln!(\"hi\");
";

    #[test]
    fn parses_porcelain_into_lines() {
        let lines = parse_porcelain(SAMPLE);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].sha, "0000000000000000000000000000000000000001");
        assert_eq!(lines[0].content, "fn main() {");
        assert_eq!(lines[1].sha, "0000000000000000000000000000000000000002");
        assert_eq!(lines[1].content, "println!(\"hi\");");
    }
}
```

**Step 2: Run to verify failure.**
Run: `cargo test -p dkod-cli parses_porcelain`
Expected: FAIL — `parse_porcelain` not found.

**Step 3: Implement** the parser. Porcelain format: a header line begins with a
40-hex SHA followed by line numbers; metadata lines follow; the actual source
line is the one prefixed with a TAB (`\t`).

```rust
pub(crate) struct BlameLine {
    pub sha: String,
    pub content: String,
}

/// Parse `git blame --porcelain` output into one BlameLine per source line.
/// The current commit SHA is the most recent header (`<40-hex> <orig> <final> <count>`);
/// the source text is the line that starts with a TAB.
pub(crate) fn parse_porcelain(out: &str) -> Vec<BlameLine> {
    let mut cur_sha: Option<String> = None;
    let mut lines = Vec::new();
    for raw in out.lines() {
        if let Some(rest) = raw.strip_prefix('\t') {
            if let Some(sha) = &cur_sha {
                lines.push(BlameLine { sha: sha.clone(), content: rest.to_string() });
            }
        } else if let Some((maybe_sha, _)) = raw.split_once(' ') {
            if maybe_sha.len() == 40 && maybe_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                cur_sha = Some(maybe_sha.to_string());
            }
        }
    }
    lines
}
```

**Step 4: Run green.**
Run: `cargo test -p dkod-cli parses_porcelain`
Expected: PASS.

**Step 5: Gates + commit.**
Commit message: `feat(blame): porcelain git-blame parser`

### Task 2.2: `dkod blame` command — annotate lines with sessions

**Files:**
- Modify: `crates/dkod-cli/src/cmd/blame.rs`
- Test: `crates/dkod-cli/tests/e2e_blame.rs` (binary-level, via `assert_cmd`)

**Step 1: Write the failing integration test.** `e2e_blame.rs`:

```rust
use assert_cmd::Command;
use tempfile::TempDir;

fn git(repo: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args).current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@e.com")
        .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@e.com")
        .output().expect("git");
    assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
}

#[test]
fn blame_annotates_ai_lines_and_passes_through_others() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "ai line\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "ai commit"]);

    // Capture the commit sha, write a dkod session, and link it.
    let sha = String::from_utf8(
        std::process::Command::new("git").args(["rev-parse","HEAD"])
            .current_dir(repo.path()).output().unwrap().stdout).unwrap().trim().to_string();

    // Seed a session + commit link using the dkod binary's own plumbing is not
    // exposed on the CLI, so drive dkod_core directly via a tiny helper binary is
    // overkill — instead use the library in the test crate:
    let s = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::ClaudeCode,
        created_at: 0, duration_ms: 0,
        prompt_summary: "add greeting".into(),
        messages: vec![], commits: vec![sha.clone()], files_touched: vec!["f.txt".into()],
    };
    dkod_core::store::write_session(repo.path(), &s).unwrap();
    dkod_core::store::link_session_to_commit(repo.path(), &s.id, &sha).unwrap();

    let out = Command::cargo_bin("dkod").unwrap()
        .current_dir(repo.path())
        .args(["blame", "f.txt"])
        .assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("claude_code"), "AI annotation missing:\n{stdout}");
    assert!(stdout.contains("add greeting"), "prompt summary missing:\n{stdout}");
    assert!(stdout.contains("ai line"), "source line missing:\n{stdout}");
}
```

> **Executor note:** the test crate already depends on `dkod_core` (used in
> `e2e_setup_cli.rs` / `capture_hook_routing.rs`). If not, add it under
> `[dev-dependencies]` in `crates/dkod-cli/Cargo.toml`.

**Step 2: Run to verify failure.**
Run: `cargo test -p dkod-cli --test e2e_blame`
Expected: FAIL — `blame` subcommand unknown.

**Step 3: Implement `run`.** In `blame.rs`:

```rust
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::Command;

pub fn run(cwd: &Path, path: &str) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    let out = Command::new("git")
        .arg("-C").arg(cwd)
        .args(["blame", "--porcelain", "--", path])
        .output()
        .context("run git blame")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git blame failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let lines = parse_porcelain(&text);

    // Cache sha -> Option<(agent, short_id, summary)> so we read each session once.
    let mut cache: std::collections::HashMap<String, Option<(String, String, String)>> =
        std::collections::HashMap::new();

    let mut lineno = 0usize;
    for bl in &lines {
        lineno += 1;
        let annotation = cache.entry(bl.sha.clone()).or_insert_with(|| {
            session_for_commit(cwd, &bl.sha)
        });
        match annotation {
            Some((agent, short, summary)) => {
                println!("{lineno:>5} {agent:<11} {short} {summary} | {}", bl.content);
            }
            None => {
                let short = &bl.sha[..bl.sha.len().min(8)];
                println!("{lineno:>5} {:<11} {short} {} | {}", "-", "(human)", bl.content);
            }
        }
    }
    Ok(())
}

/// If `sha` has a `refs/dkod/commits/<sha>` link, resolve the session and return
/// (agent_label, short_session_id, prompt_summary). Otherwise None.
fn session_for_commit(cwd: &Path, sha: &str) -> Option<(String, String, String)> {
    let repo = gix::open(cwd).ok()?;
    let r = repo.find_reference(&dkod_core::refs::commit_ref(sha)).ok()?;
    let obj = repo.find_object(r.id()).ok()?.detach();
    let session: dkod_core::Session = serde_json::from_slice(&obj.data).ok()?;
    let short = session.id.get(..8).unwrap_or(&session.id).to_string();
    Some((
        dkod_core::agent_label(&session.agent).to_string(),
        short,
        session.prompt_summary,
    ))
}
```

**Step 4: Wire the subcommand** in `crates/dkod-cli/src/main.rs`:
- Add to the `Cmd` enum:
  ```rust
  /// Show, per line of a file, the AI agent session that produced it.
  Blame {
      /// Path to the file to annotate (relative to the repo).
      path: String,
  },
  ```
- Add the dispatch arm (note: this is a normal command, so the existing
  `maybe_warn_drift()` self-heal at the top of `main` already applies — do NOT add
  it to the `Setup`/`CaptureHook` exclusion list):
  ```rust
  Cmd::Blame { path } => cmd::blame::run(&std::env::current_dir()?, &path),
  ```

**Step 5: Run green.**
Run: `cargo test -p dkod-cli --test e2e_blame` then full `cargo test`.
Expected: PASS.

**Step 6: Gates + commit.**
Commit message: `feat(blame): dkod blame annotates lines with their AI session`

---

## Phase 3 — DRY cleanup + docs

### Task 3 (was Task 4): Extract the agent→label match into dkod-core

`agent_label` is now needed in three places (`cmd/log.rs`, `cmd/show.rs`,
`cmd/blame.rs`). Extract one helper.

**Files:**
- Modify: `crates/dkod-core/src/session.rs` (add the helper next to `Agent`)
- Modify: `crates/dkod-cli/src/cmd/log.rs`, `crates/dkod-cli/src/cmd/show.rs`, `crates/dkod-cli/src/cmd/blame.rs`

**Step 1: Write the failing test** in `session.rs` tests:

```rust
#[test]
fn agent_label_is_stable_snake_case() {
    assert_eq!(agent_label(&Agent::ClaudeCode), "claude_code");
    assert_eq!(agent_label(&Agent::OpenCode), "open_code");
    assert_eq!(agent_label(&Agent::FactoryAi), "factory_ai");
}
```

**Step 2: Run to verify failure.**
Run: `cargo test -p dkod-core agent_label` → FAIL (not found).

**Step 3: Implement** in `session.rs`:

```rust
/// Stable snake_case label for an agent. Single source of truth for the
/// CLI's human-readable agent column (log / show / blame).
pub fn agent_label(a: &Agent) -> &'static str {
    match a {
        Agent::ClaudeCode => "claude_code",
        Agent::Codex => "codex",
        Agent::CopilotCli => "copilot_cli",
        Agent::Cursor => "cursor",
        Agent::FactoryAi => "factory_ai",
        Agent::GeminiCli => "gemini_cli",
        Agent::OpenCode => "open_code",
    }
}
```

Ensure it's re-exported if the crate root re-exports `Session`/`Agent` (check
`crates/dkod-core/src/lib.rs`; add `pub use session::agent_label;` if that's the
pattern).

**Step 4: Replace** the inline `match` blocks in `log.rs` and `show.rs` with
`dkod_core::agent_label(&s.agent)`, and use the same in `blame.rs`'s
`session_for_commit`.

> **Note:** `log.rs`/`show.rs` currently emit `"open_code"` for `OpenCode` — keep
> that exact string so output doesn't change. (The capture `Agent` serde uses
> `open_code` too; the helper matches.)

**Step 5: Run green.** `cargo test` (whole workspace) → PASS. Eyeball that
`dkod log` / `dkod show` output is byte-identical to before.

**Step 6: Gates + commit.**
Commit message: `refactor: single agent_label helper for log/show/blame`

### Task 3.2: Update the design doc roadmap status

**Files:**
- Modify: `docs/plans/2026-05-27-macroscope-competitive-positioning.md`

Mark roadmap item #1 (`dkod blame`) as "implemented (V1: HEAD-watching linkage,
Claude Code capture path only)" and note the carried-forward limitation
(rebase/squash SHA rewrite breaks links) and the gap (only the Claude Code capture
path links commits today; other agents’ adapters still set `commits: vec![]` — a
follow-up extends HEAD-watching to them).

Commit message: `docs: mark dkod blame shipped in positioning roadmap`
(docs-only — note "CodeRabbit does not meaningfully review docs").

---

## Phase 4 — PR

### Task 4.1: Open the PR

**Step 1:** Ensure the whole suite is green and formatted:
`cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check`.
**Step 2:** Full-branch CodeRabbit: `coderabbit --type all --base main --plain`; resolve every actionable finding (new commits, re-run until clean).
**Step 3:** Branch + push:
```bash
git switch -c feat/dkod-blame   # if not already on a feature branch
gh auth switch --user haim-ari
git push -u origin feat/dkod-blame
```
**Step 4:** Open the PR (title ≤70 chars), body covering: the two-part design,
the HEAD-watching approach + its known SHA-rewrite limitation, the
git-blame-shell-out decision (gix 0.66 has no blame), and that only the Claude
Code capture path links commits in V1.
**Step 5:** Drive the PR to green: wait for CI + CodeRabbit server-side review,
fix findings, push (re-`gh auth switch` first), repeat until CI green + CodeRabbit
approved. Do not self-merge unless the user asks.

---

## Known limitations (carried in the PR description)

1. **History rewrite breaks links.** rebase/squash/amend after capture rewrites
   commit SHAs; `refs/dkod/commits/<old-sha>` then dangles. Re-linking on observed
   rewrites is a follow-up.
2. **Claude Code only (V1).** Only the Claude Code capture path records
   `head_at_start` and links commits. Other agents’ adapters still set
   `commits: vec![]`; extending HEAD-watching to them is a follow-up.
3. **`git` runtime dependency for `dkod blame`.** The read command shells out to
   `git blame`. Acceptable per the init.rs precedent; revisit if/when gix gains
   blame and the workspace bumps.

## Open seams the executor must confirm while implementing

- Exact gix 0.66 `rev_walk` API shape (Task 1.1 executor note).
- Where the server stores per-session state and how `FinishedSession` is
  constructed (Task 1.2 — read first).
- Whether to make `handle_finished_session` testable directly or factor the
  linking into a `pub` dkod-core fn (Task 1.3 — the factored fn is preferred).
