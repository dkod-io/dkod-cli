# History-Rewrite Re-linking — Design

**Date:** 2026-06-03
**Status:** approved (design), pending spec review → implementation plan

## TL;DR

When a developer rewrites history (rebase, `commit --amend`, interactive
squash/reword), commit SHAs change. dkod's provenance links
(`refs/dkod/commits/<old-sha>` → session blob) then point at commits that
are no longer in the branch, so `dkod blame` can't resolve the rewritten
lines and falls back to `(human)` — provenance is silently lost.

This design restores provenance by installing a git `post-rewrite` hook
(per-repo, at `dkod init`) that pipes git's `old→new` SHA pairs to a new
hidden `dkod relink` subcommand. For each pair, if the old commit-ref
resolves to a session, `relink` writes a new commit-ref for the rewritten
SHA pointing at the same session blob. Blame then resolves the rewritten
lines normally.

This is the carried-forward limitation from PR #19 (dkod blame) and PR #20
(all-agent commit linking), both merged.

## Decisions (locked during brainstorming)

1. **Mechanism: git `post-rewrite` hook only.** No patch-id fallback in v1.
   The hook is purpose-built, gives exact `old→new` pairs, and is the *only*
   mechanism that conveys the many→one squash signal. Rewrites it doesn't
   cover (filter-repo, external, pre-install) degrade gracefully to today's
   `(human)` behavior and are documented limitations.
2. **Squash: last-writer-wins, 1:1 model preserved.** No data-model change.
   When N old SHAs map to one new SHA, each re-link overwrites the new
   commit-ref; the last pair processed wins. No special-casing — falls out
   of the existing overwrite-on-collision `edit_reference`.
3. **Refs only.** Re-linking writes commit-refs; it does NOT rewrite the
   session blob's `commits` list. Blame reads the ref, so blame is correct.
   `dkod show` may display a pre-rewrite SHA — documented.
4. **Install at `dkod init` only.** Per-repo (`.git/hooks/post-rewrite`),
   idempotent, alongside the existing `.git/config` fetch-refspec wiring.
   Not wired through the seamless wizard in v1.

## Architecture & data flow

```text
git rebase / commit --amend / squash / reword
        │
        ▼
.git/hooks/post-rewrite          (tiny dkod-managed script: exec dkod relink)
        │  stdin, one line per rewritten commit:
        │     <old-sha> <new-sha>\n
        ▼
dkod relink                      (new hidden subcommand; logic in Rust)
        │  for each pair:
        │    if refs/dkod/commits/<old> resolves to a session blob,
        │    write refs/dkod/commits/<new> → that same blob OID
        ▼
refs/dkod/commits/<new>  →  session blob  →  dkod blame resolves the rewritten line
```

The hook script is deliberately trivial (`#!/bin/sh` + `exec dkod relink`)
so all logic lives in testable Rust — mirroring the existing capture-hook
pattern (`dkod capture-hook`). The handler **always exits 0**: a misbehaving
re-link must never break the user's `git rebase`/`amend`.

## Components

### New: `dkod relink` (hidden subcommand)

- **Location:** `crates/dkod-cli/src/cmd/relink.rs`, dispatched from
  `crates/dkod-cli/src/main.rs` as a `#[command(hide = true)]` `Relink`
  variant (internal, invoked by the hook — not user-facing), consistent
  with how `capture-hook` is hidden.
- **Input:** reads `stdin` line-by-line. Git's `post-rewrite` format is
  `<old-sha> SP <new-sha>` (one per rewritten commit). The hook also
  receives a single argument (`amend` or `rebase`) which the handler
  ignores. Blank lines and malformed lines are skipped.
- **Per-pair behavior:** validate both tokens are 40-char lowercase hex
  (defensive, like the capture-hook's repo-hash validation). Look up
  `refs/dkod/commits/<old>`; if it resolves to a blob, write
  `refs/dkod/commits/<new>` pointing at the same blob OID via the same
  `edit_reference` machinery `link_session_to_commit` uses. If the old ref
  is absent (the rewritten commit had no session), skip silently.
- **Squash (many→one):** no special code. `old1→new`, `old2→new`,
  `old3→new` each overwrite `refs/dkod/commits/<new>`; the last pair in
  stdin order wins (last-writer-wins).
- **Old refs kept (additive):** `refs/dkod/commits/<old>` is NOT deleted.
  If the user undoes the rewrite (`git reset --hard ORIG_HEAD`), the old
  link still resolves. The cost is dangling commit-refs accumulating; a
  future `dkod gc` can prune commit-refs whose SHA is unreachable.
- **Always returns Ok / exits 0.** Internal errors are logged best-effort
  (to a per-repo log path, matching the capture-hook's error-logging
  convention) and swallowed.

### New core helper: `dkod_core::store::relink_commit`

A small, unit-testable function:

```text
relink_commit(repo_path, old_sha, new_sha) -> Result<bool>
  - find refs/dkod/commits/<old_sha>; if absent → Ok(false) (nothing to do)
  - else write refs/dkod/commits/<new_sha> → same blob OID; Ok(true)
```

This keeps the git surgery in `dkod-core` next to `link_session_to_commit`
(which already resolves a ref's blob and writes a commit-ref), and lets the
`relink` CLI command stay a thin stdin-parsing + orchestration layer. The
two share the ref-writing path.

### Modified: `dkod init` — install the hook

- After the existing refspec wiring, write `.git/hooks/post-rewrite`
  (mode 0755) containing the dkod-managed script with a sentinel marker
  line (e.g. `# dkod-managed: re-link sessions after history rewrite`).
- **Idempotent:** if the file is absent, or present and carries the dkod
  sentinel, (over)write it. If a **foreign** `post-rewrite` hook exists
  (no sentinel), **skip and print guidance** — never clobber, never
  silently chain. (Same surgical/sentinel philosophy as the Claude
  settings-hook installer.)
- **`core.hooksPath` caveat:** if the repo/user has redirected hooks via
  `core.hooksPath`, `.git/hooks/post-rewrite` will not fire. Detect this
  cheaply at init (read the config value) and print a one-line warning with
  manual wire-up guidance. Do not attempt to write into the custom hooks
  dir in v1.

## Error handling

- `dkod relink` is best-effort and exits 0 on every path. A failed ref
  write for one pair does not abort the others.
- Hook install failures in `dkod init` are surfaced as warnings, not fatal
  — `dkod init` must still succeed (refspec wiring, config) even if the
  hook can't be written (e.g. read-only `.git/hooks`).
- SHA validation rejects non-40-hex tokens before any ref operation.

## Testing

### Unit (`dkod_core::store::relink_commit`)
- Re-link happy path: seed `refs/dkod/commits/<old>` → session blob; call
  `relink_commit(old, new)`; assert `refs/dkod/commits/<new>` resolves to
  the same blob OID; returns `Ok(true)`.
- Old ref absent → `Ok(false)`, no new ref written.
- Old ref kept after re-link (additive): both old and new resolve.

### Unit (`relink` stdin parsing)
- Note on git's invocation shape: `post-rewrite` passes the trigger name
  (`amend` or `rebase`) as a command-line **argument** (`$1`), and the
  `<old> <new>` pairs on **stdin** (one per rewritten commit). `dkod relink`
  parses pairs from stdin and ignores any positional argument.
- Parses well-formed `<old> <new>` stdin lines into pairs.
- Skips blank and malformed lines; rejects non-hex SHAs.
- Many→one (squash): three pairs to the same new SHA → new ref resolves to
  the last-processed session (last-writer-wins).

### E2E (real binary, real git)
- Init a repo, capture/seed a session linked to a commit, install the hook
  via `dkod init`, run `git commit --amend` (and a separate interactive
  squash case if tractable), then `dkod blame <file>` shows the session on
  the rewritten line (previously `(human)`).
- Fallback clause: if driving an interactive squash in a hermetic test is
  too fiddly, the amend e2e + the unit-level many→one test are accepted as
  sufficient coverage; document and move on (do not rabbit-hole).

### Install test
- `dkod init` writes `.git/hooks/post-rewrite` (executable, contains the
  sentinel); re-running `dkod init` is idempotent (no duplication, no
  error).
- A pre-existing foreign `post-rewrite` hook is left byte-intact and a
  warning is printed.

## Known limitations (documented in code + PR)

1. **Coverage:** only the rewrites git's `post-rewrite` fires for (amend,
   rebase, squash, fixup, reword). `git filter-repo`/`filter-branch`,
   externally-rewritten history, and history rewritten before `dkod init`
   are not auto-relinked — blame falls back to `(human)`, exactly as today.
   A patch-id fallback is a clean future add if these gaps prove painful.
2. **Squash is lossy:** the squashed commit attributes to a single session
   (last-writer-wins); the other sessions' provenance for that commit is
   dropped. A many-sessions-per-commit data model is a deliberate non-goal
   for v1.
3. **`core.hooksPath`:** users who redirect hooks must wire the
   `post-rewrite` hook manually; `dkod init` warns but does not install
   into a custom hooks dir.
4. **`dkod show`:** the session blob's `commits` list is not rewritten, so
   `dkod show` may display a pre-rewrite SHA. Blame is unaffected (it reads
   the ref). Refreshing the blob is a possible future pass.

## Out of scope (v1)

- patch-id matching / lazy re-resolution fallback.
- Many-sessions-per-commit data model.
- Rewriting `session.commits` in the blob.
- `dkod gc` for pruning unreachable commit-refs.
- Wizard-driven (user-scope) hook installation.
