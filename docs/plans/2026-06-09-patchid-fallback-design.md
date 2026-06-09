# Patch-ID Fallback for `dkod blame` — Design

**Date:** 2026-06-09
**Status:** approved (design calls made autonomously per standing directive), pending implementation plan

## TL;DR

`dkod blame` resolves a line's commit SHA → session via `refs/dkod/commits/<sha>`.
The post-rewrite hook (PR #21) re-points that ref after *local* rewrites it
observes. But rewrites the hook never sees — history rewritten **before**
`dkod init` installed the hook, rewrites on a **clone without the hook**, and
`git filter-repo` — leave the rewritten commit with no `refs/dkod/commits/<sha>`,
so blame falls back to `(human)` and provenance is lost.

This adds a **patch-id fallback**: at capture time, alongside each
`refs/dkod/commits/<sha>` we also write `refs/dkod/patchid/<patch-id>` → the same
session blob, where `<patch-id>` is the stable `git patch-id` of the commit's
diff. At blame time, when the commit-ref is absent, blame computes the line's
commit patch-id and looks up `refs/dkod/patchid/<patch-id>`. Because a
diff-preserving rewrite (rebase, reword, filter-repo) keeps the same patch-id,
provenance is recovered without any hook having fired.

## Why this is complementary to the hook (not redundant)

| Rewrite kind | Hook/relink (PR #21) | patch-id fallback (this) |
|---|---|---|
| Local rebase/amend/reword (hook installed) | ✅ re-points commit-ref | ✅ (also works, unused) |
| **Squash** (N→1, diffs combine) | ✅ last-writer-wins via old→new pairs | ❌ patch-id changes |
| **Pre-`init` history rewrite** | ❌ hook didn't exist | ✅ patch-id matches |
| **Clone without the hook** | ❌ no hook | ✅ |
| **`git filter-repo` / external** | ❌ not per-commit | ✅ if diff-preserving |
| Content-changing amend | ✅ (hook re-points) | ❌ patch-id changes (correctly no match) |

The hook owns squash and live local rewrites; patch-id owns diff-preserving
rewrites that bypass the hook. Together they cover the realistic gap.

## Verified facts (spike, 2026-06-09)

- `git diff-tree -p --root <sha> | git patch-id --stable` → a 40-hex patch-id.
- **Stable across a diff-preserving rewrite** (reword/amend that doesn't change
  the diff): old and new commit yield the *same* patch-id. ✅
- The **`--root` flag is essential**: without it, a root (parentless) commit
  produces empty output. With it, root commits get a real patch-id. ✅
- A **content-changing** amend yields a *different* patch-id — so the fallback
  correctly does NOT match when the diff actually changed. ✅
- Empty-diff commits (empty commit, some merges) produce **empty** patch-id
  output → must be skipped (never write/lookup an empty patch-id, or all
  empty-diff commits would collide onto one ref).

## Components

### `dkod_core::refs::patchid_ref(patch_id: &str) -> String`
Returns `refs/dkod/patchid/<patch_id>`. Mirrors `commit_ref`.

### `dkod_core::store::link_session_to_patchid(repo, session_id, patch_id) -> Result<()>`
Pure gix (no shell-out): mirrors `link_session_to_commit` exactly, but writes
the `patchid_ref` instead of the `commit_ref`. Overwrite-on-collision
(`PreviousValue::Any`) → last-writer-wins, consistent with commit-ref semantics.

### `dkod_cli::cmd::patchid::compute_patch_id(cwd, sha) -> Option<String>`
The git shell-out lives in the CLI layer (where `init.rs`/`blame.rs` already
shell out to git), keeping `dkod-core/store` gix-only. Runs
`git -C <cwd> diff-tree -p --root <sha>` piped into `git patch-id --stable`,
returns the first whitespace token iff it is non-empty 40-lowercase-hex; else
`None`. Best-effort: any spawn/parse failure → `None`.

### Write path: `finalize_session` (cmd/capture/mod.rs)
After `write_session_with_commit_links` returns the `linked` commit SHAs, for
each linked SHA compute its patch-id and, when present, call
`link_session_to_patchid`. Best-effort — a patch-id failure must never fail
capture. (Only the commits the session *produced* get patch-id refs, same set
the commit-refs cover.)

### Read path: `blame.rs::session_for_commit`
Restructure: try `refs/dkod/commits/<sha>` first (unchanged primary path). On
miss, compute the sha's patch-id and try `refs/dkod/patchid/<patch-id>`. Either
hit resolves to the session blob and renders identically. Blame already caches
`session_for_commit` per unique SHA, so the extra subprocess runs at most once
per unique dangling commit, only on the fallback path.

## Data flow

```text
capture (finalize_session):
  for each commit the session produced:
    refs/dkod/commits/<sha>      -> session blob   (existing)
    refs/dkod/patchid/<patchid>  -> session blob   (new; patchid = patch-id of <sha>'s diff)

blame (per line's commit sha):
  refs/dkod/commits/<sha> present? -> session            (primary)
  else patch-id(<sha>) -> refs/dkod/patchid/<patchid>?    (fallback)
  else (human)
```

## Decisions & rationale

- **Store at capture, keyed by patch-id (a ref), not in the session blob.** The
  blame lookup is "given a patch-id, find the session" → needs a patch-id→session
  index, which a ref keyed by patch-id provides in O(1). Storing the patch-id
  *inside* the blob would force scanning every session at blame time.
- **patch-id refs travel with the repo.** They live under `refs/dkod/*`, which
  the existing fetch refspec (`+refs/dkod/*:refs/dkod/*`, written by `dkod init`)
  already pushes/fetches — so a clone recovers provenance via patch-id even if it
  never had the hook. This is the point.
- **Empty patch-id → skip.** Guard both write and lookup on non-empty 40-hex.
- **Collisions = last-writer-wins**, identical to the commit-ref squash policy;
  documented.
- **git shell-out in the CLI layer only.** `dkod-core/store` stays gix-only;
  `cmd::patchid` owns the subprocess, consistent with `init.rs`/`blame.rs`.
- **No relink change.** A hook-driven rewrite already gets a fresh commit-ref
  (primary path), so relink needn't write patch-id refs.

## Testing

- Unit (`store::link_session_to_patchid`): writes `refs/dkod/patchid/<id>` →
  session blob; overwrite/last-writer-wins; mirrors the commit-ref tests.
- Unit (`refs::patchid_ref`): path format.
- Unit (`cmd::patchid::compute_patch_id`): real temp repo — non-empty 40-hex for
  a normal commit; stable across a reword; `None`/empty handled for an empty
  commit; root commit yields a value (via `--root`).
- Integration (write path): `finalize_session` writes a patch-id ref for a
  produced commit (assert the ref resolves to the session blob).
- E2E (the payoff, hook-independent): capture-link a session to commit OLD (with
  patch-id ref written), `git commit --amend` a **reword** → NEW (diff
  preserved), do **NOT** run relink/hook, then `dkod blame` resolves NEW's line
  to the session **via the patch-id fallback**. Also assert a **content-changing**
  amend does NOT resolve (patch-id differs → `(human)`), proving the fallback is
  diff-precise, not a false-positive.

## Known limitations (documented)

1. Only **diff-preserving** rewrites are recoverable via this fallback. Squash
   and content-changing rewrites are not (the hook handles squash; content
   changes are genuinely different work).
2. patch-id **collisions** (two sessions, identical diffs) → last-writer-wins.
3. Empty-diff commits get no patch-id ref (skipped).
4. Adds a `git` subprocess on the blame **fallback** path (once per unique
   dangling commit; cached). Negligible on the common (commit-ref-hit) path.

## Out of scope (v1)

- Backfilling patch-id refs for sessions captured before this ships (they have
  commit-refs only; a future `dkod reindex` could add patch-id refs).
- Rename/whitespace-tolerant matching beyond what `git patch-id --stable` does.
- Pruning patch-id refs (`dkod gc`).
