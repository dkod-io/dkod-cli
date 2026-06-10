# Storage v2 — Rollup Index, Size Budgets, GC, Lazy Propagation

**Date:** 2026-06-10
**Status:** design (no implementation yet)
**Prereq reading:** `docs/plans/2026-06-10-dkod-strategy-v2.md` (the "Storage v2"
build item and the verified scaling facts), `crates/dkod-core/src/refs.rs`,
`crates/dkod-core/src/store.rs`, `crates/dkod-cli/src/cmd/init.rs`,
`crates/dkod-cli/src/cmd/relink.rs`, `crates/dkod-cli/src/cmd/blame.rs`,
`crates/dkod-cli/src/cmd/capture/mod.rs`.

---

## 1. The verified problem

Live testing (strategy v2, "Storage v2" section) found the scaling bomb in the
V1 storage layout. The facts, all verified:

- **Median session ≈ 600 KB zlib-compressed** (not the assumed 50 KB).
  Tool-output spam dominates the bytes.
- **2–3 refs per session**: `refs/dkod/sessions/<id>`, one
  `refs/dkod/commits/<sha>` per produced commit, one
  `refs/dkod/patchid/<id>` per produced commit.
- A 50-dev org at ~10 sessions/dev/day ≈ **126k sessions/yr** ≈
  **250–375k refs/yr** and **tens of GB/yr** of session blobs.
- That breaches **Bitbucket's 4 GB hard repo cap in weeks** and **GitLab
  Free's 10 GiB in 1–2 months**.
- Every subscribed client pays **~100 B/ref of advertisement per fetch** —
  **~30 MB of ref advertisement at 300k refs**, on *every* `git fetch`,
  before a single object moves.
- The session refs point **directly at blobs** (`Target::Object(blob_id)` in
  `store::write_session`), so there is **no commit negotiation**: git cannot
  say "I already have everything up to X" — each fetch re-evaluates every ref.
- `dkod init` (`ensure_dkod_refspec`) wires `+refs/dkod/*:refs/dkod/*` into
  **every remote's fetch refspec**, so every teammate's every vanilla
  `git fetch` mirrors the whole archive, paying all of the above.

Strategy v2 makes Storage v2 a hard gate: *"required before any 50-dev org
lands"*, with the kill-trigger *"any org >20 devs onboarding before Storage v2
ships → prioritize the rollup index immediately."*

## 2. Goals and non-goals

### Goals

1. **O(1) refs** per repo regardless of session count (kill ref-advertisement
   cost and per-ref fetch evaluation).
2. **Real incremental fetch** via commit negotiation (the index history is a
   commit chain).
3. **Bounded per-session size** (~256 KiB compressed default) with a
   full-fidelity opt-out.
4. **Retention** (`dkod gc --keep <duration>`) that actually reclaims host
   storage.
5. **Cheap default propagation**: a teammate's fetch costs one ref and the
   metadata delta, not the archive.
6. **Existing repos in the wild keep working unmodified** — the read path
   falls back to legacy per-session refs forever; migration is opt-in.
7. Public `dkod-core::store` function signatures stay stable for callers.

### Non-goals (explicit)

- **Indexer changes** — out of scope. Section 10 notes the one assumption it
  must update; the trust boundary is unchanged.
- **Side-repo (`<repo>-dkod`) and sessions-only-remote deployment modes** —
  strategy items; documented escape hatches, separate design. Nothing here
  precludes them (the index ref works identically in a side repo).
- **Changing the `Session` JSON schema.** The body blob is the same JSON
  `store::read_session` deserializes today.
- Cross-repo dedup, transcript encryption, Agent Trace export, semantic
  compaction of messages, rewriting legacy session blobs in place.
- Multi-remote index reconciliation beyond `origin` (V2 reconciles against
  the remote each ref was fetched from, same as V1's per-remote refspecs;
  cross-remote merge is YAGNI until someone runs two write remotes).

## 3. Design overview

One new ref, `refs/dkod/index`, points at a normal git **commit chain**. Each
commit's **tree** is the complete current state of the archive: every
session's metadata and body, plus the commit-sha and patch-id lookup tables,
as tree paths. Capture appends **one index commit per finalized session**
(session + all its commit links + all its patch-id links, batched). Reads
resolve tree paths against the index tip, falling back to the legacy
`refs/dkod/sessions|commits|patchid/*` refs when a path is absent.

Because the index is a commit chain:

- Fetch advertises **one ref** (~100 B) instead of 300k.
- Fetch **negotiates**: a client that has yesterday's tip downloads only the
  new commits, the few changed tree nodes, and the new session blobs.
- Lazy propagation becomes a **filter problem** (section 9), not a
  ref-explosion problem.
- `dkod gc` becomes a **tree rewrite + history reset** (section 8).

## 4. The rollup index format (`dkod-index/1`)

### 4.1 Ref and commit

- Ref: `refs/dkod/index`. Target: a commit. Parent: the previous index tip
  (root commit has no parent). This is the Gerrit NoteDb / git-notes pattern:
  application data in a ref's commit history, invisible to branches.
- **Committer/author:** the existing `ensure_committer` fallback identity
  (`dkod <noreply@dkod.io>`) when the repo has none, else the repo's
  configured identity — exactly the policy `store.rs` applies today.
- **Commit message** (human-readable convention, tree is the source of
  truth — messages are never parsed):
  - capture: `dkod: add session <id> (<agent>, <n> commit(s))`
  - relink: `dkod: relink <n> commit(s) after history rewrite`
  - reconcile: `dkod: reconcile <n> session(s)` (push-race replay, §5.4)
  - reindex: `dkod: reindex <n> legacy session(s)`
  - gc: `dkod: gc --keep <duration> (kept <n>, dropped <m>)`

### 4.2 Tree layout

```
/
├── version                              # blob, contents "1\n"
├── epoch                                # blob, decimal integer, bumped by gc (§8)
├── sessions/
│   └── <YYYY-MM-DD>/                    # date derived from the UUIDv7 timestamp
│       └── <session-id>/
│           ├── meta.json                # small metadata blob (~600 B)
│           └── body.json                # full Session JSON (today's schema)
├── commits/
│   └── <aa>/                            # first 2 hex chars of the commit sha
│       └── <remaining-38-hex>           # pointer blob: "<session-id>\n"
└── patchid/
    └── <aa>/
        └── <remaining-38-hex>           # pointer blob: "<session-id>\n"
```

**Why date fan-out for sessions, not hex fan-out.** Session ids are UUID v7
(`Session::new_id()` → `uuid::Uuid::now_v7()`): the first 48 bits are a
millisecond timestamp, so the leading hex characters are *time-clustered* —
a hex fan-out like `sessions/ab/cd/<id>` would put **decades** of sessions in
one bucket (the first two hex chars roll over every ~35 years). Date
directories instead give:

- bounded directory size (~400 entries/day for a 50-dev org → ~30 KB tree
  object, which delta-compresses to near nothing in packs);
- write locality (only today's tree node churns; historical tree nodes are
  byte-identical across commits and stored once);
- **gc as tree pruning**: dropping sessions older than the cutoff is dropping
  whole date directories (§8).

**Path date is derived from the UUID v7 timestamp**, not `created_at`,
so the path is computable from the id alone — `read_session(id)` extracts the
embedded ms-timestamp and goes straight to
`sessions/<date>/<id>/body.json` with no scan. Non-v7 ids (foreign imports,
hand-rolled tests) fall back to a date-directory scan, then to the legacy
ref (§6.2).

**Why hex fan-out for commits/patchid.** Commit shas and patch-ids are
uniformly distributed hashes, so the standard 2-hex/256-bucket fan-out works
(git's own objects/ layout). At 252k links/yr that is ~1k entries per bucket
after a year — fine, and gc rewrites these tables wholesale anyway.

### 4.3 The two session blobs: `meta.json` + `body.json`

- **`body.json`** — the full `Session` serialized exactly as
  `store::write_session` serializes it today. Zero schema change;
  `read_session` keeps deserializing the same bytes. Median ~600 KB
  compressed today, ≤256 KiB after the size budget (§7).
- **`meta.json`** — a small derived header written alongside:

  ```json
  {
    "id": "...", "agent": "claude_code", "created_at": 1760000000,
    "duration_ms": 84000, "prompt_summary": "fix the auth bug",
    "commits": ["<sha>", "..."], "files_touched": ["src/auth.rs"],
    "redaction_count": 3, "body_bytes": 215040, "truncated": true
  }
  ```

The split is what makes lazy propagation honest (§9): `dkod log`, `dkod
blame`, and `dkod drift`'s listing pass need only `meta.json` (agent, short
id, prompt summary, commits, files); only `dkod show` / drift detail /
full-text search need `body.json`. Metadata stays tiny enough to mirror
always; bodies can be blob-filtered and backfilled on demand. The ~600 B
duplication per session is noise.

### 4.4 Pointer blobs

`commits/<fanout>` and `patchid/<fanout>` blobs contain exactly
`<session-id>\n`. Properties:

- Many commits from one session → identical blob contents → **one object**
  in the store (content addressing dedups them for free).
- The session id embeds its date (v7), so a pointer resolves to the meta/body
  paths without any reverse map — and gc can decide a pointer's fate from its
  contents alone (§8).
- Last-writer-wins on collisions, matching today's `PreviousValue::Any`
  policy in `write_link_ref` (two sessions producing the same patch-id: the
  later index commit's tree write wins).

### 4.5 Why this enables commit negotiation

Today's refs point at blobs; git's fetch negotiation works on **commit**
reachability, so blob-target refs are all-or-nothing every time. With the
index, `refs/dkod/index` points at a commit whose parents chain back to the
first capture. A fetch sends "have <old tip>", the server walks the chain,
and the pack contains only: new index commits, the handful of tree nodes that
changed (root + `sessions/` + one date dir + one fanout bucket per link),
and the new meta/body/pointer blobs. Steady-state fetch cost is proportional
to **new sessions since last fetch**, not archive size.

## 5. Write path

### 5.1 One batched index commit per session

Today `finalize_session` (cli `capture/mod.rs`) performs 1 session-ref write +
N commit-ref writes + N patchid-ref writes. In v2 the same logical content
becomes **one index commit**:

- new internal core type `store::IndexBatch`: an ordered list of
  `(tree_path, blob_bytes)` inserts, applied to the current index tip's tree
  in a single read-modify-write, producing one child commit.
- A finalized session contributes: `sessions/<date>/<id>/meta.json`,
  `sessions/<date>/<id>/body.json`, N × `commits/...`, N × `patchid/...` —
  all in the one batch. No per-session batching-across-sessions (a capture
  flush is already the natural batch; cross-session batching adds latency and
  loss windows for nothing — YAGNI).

### 5.2 Public signatures stay stable

| Function | v2 behavior |
|---|---|
| `write_session(repo, &Session)` | Builds meta+body, applies a 2-path IndexBatch. Same signature, same `Result<()>`. Also writes the legacy `refs/dkod/sessions/<id>` ref **iff dual-write is on** (§12). |
| `write_session_with_commit_links(repo, &mut Session, head_at_start)` | Discovers commits (unchanged `new_commits_since`), then applies **one** IndexBatch covering session + commit pointers. Same signature; still returns linked shas; session write remains the only fatal step, link entries best-effort (a link whose pointer path can't be built is dropped from the batch, not fatal). |
| `link_session_to_commit` / `link_session_to_patchid` | Single-path IndexBatch (one index commit) + legacy ref iff dual-write. Standalone callers (tests, future tooling) keep working; `finalize_session` migrates to the batched path so the patchid links land in the *same* commit as the session instead of separate calls. |
| `relink_commit(repo, old, new)` | Reads the pointer at `commits/<old>` (falling back to the legacy `refs/dkod/commits/<old>` ref), writes `commits/<new>` in an IndexBatch. Old path kept (additive), exactly today's undo-friendly semantics. |
| `list_sessions` / `read_session` | §6. |

`dkod relink` (the post-rewrite hook) batches **all** stdin pairs into one
index commit (`dkod: relink <n> commit(s)…`) instead of N ref edits, and
keeps its always-exit-0 contract.

### 5.3 Local concurrency

Two capture daemons (e.g. Claude Code + Codex sessions ending together) can
finalize concurrently. Two layers:

1. **Lock file** `.git/dkod/index.lock` (advisory, `O_CREAT|O_EXCL` with
   stale-lock takeover after a timeout) serializes local index updates — the
   common case never conflicts.
2. **Ref CAS** as the backstop: the index ref edit uses
   `PreviousValue::MustBeExactly(<tip read at batch start>)` instead of
   `Any`. On CAS failure (lock bypassed/stale): re-read tip, re-apply the
   batch, retry, **bounded at 5 attempts** with small jitter.

If all retries fail (pathological), the session JSON is spilled to
`.git/dkod/outbox/<id>.json` and folded into the index by the next successful
finalize (or `dkod reindex`). A capture must never be lost to contention.

### 5.4 Remote propagation: push, race, offline

Capture finalize ends with a **best-effort push** of `refs/dkod/index` to
`origin` (config `[storage] auto_push = true` default; today users push
`+refs/dkod/*` by hand, which stops scaling the moment the team grows).
Failure handling:

- **Non-fast-forward rejection** (a teammate pushed first): fetch the remote
  tip into a temp ref (`refs/dkod/fetch-tmp`, deleted after), **replay** all
  local-only path inserts onto it as a single reconcile commit
  (`dkod: reconcile <n> session(s)`), reset local `refs/dkod/index` to it,
  push again. **Bounded at 3 fetch-replay-push rounds.** Replay is trivially
  safe because index writes are path-level inserts: distinct sessions touch
  distinct paths; the only possible same-path collision is a pointer-blob
  collision, which is last-writer-wins by policy (§4.4).
- **Retries exhausted / offline / auth failure**: the local index commit
  simply stays local (it is already durable in the local repo — no outbox
  needed for this case) and the next capture's push attempt carries it. An
  explicit `dkod push` flushes manually. Nothing blocks the agent's session.
- This is the same reconciliation shape Gerrit uses for NoteDb meta refs:
  fetch-rebase-retry with bounded attempts, queue on failure.

### 5.5 Vanilla-fetch safety (a real footgun, designed out)

If init wired a **forced** refspec (`+refs/dkod/index:refs/dkod/index`), a
teammate's plain `git fetch` while the local index is *ahead* (unpushed
sessions) would clobber the local tip and orphan those sessions. Therefore:

- init wires the refspec **without** the `+`:
  `refs/dkod/index:refs/dkod/index`. Vanilla fetch fast-forwards only; a
  local-ahead index is left untouched (git prints a non-FF warning, harmless).
- Only **dkod-driven** operations move the index ref non-fast-forward, and
  only after folding local-only sessions (push-race replay §5.4, gc epoch
  adoption §8.3).

## 6. Read path

### 6.1 Lookups

- `read_session(id)`: v7-timestamp → date → `sessions/<date>/<id>/body.json`
  at the index tip. Deserializes the same `Session` JSON as today.
- `list_sessions()`: walk `sessions/*/` date dirs at the index tip, collect
  ids; then **union** with legacy `refs/dkod/sessions/*` (dedup by id, index
  wins). Date-dir ordering gives `dkod log` chronological listing for free
  using only tree walks + meta blobs.
- `blame` (`session_for_commit`): `commits/<fanout(sha)>` pointer →
  `meta.json` (agent label, short id, prompt summary — blame never needs the
  body, which keeps it fast under lazy mode). Patch-id fallback unchanged in
  spirit: compute patch-id, look up `patchid/<fanout(pid)>`. One blame run
  opens the index tree once and reuses it across line lookups (replacing the
  per-sha `find_reference` calls).

### 6.2 Fallback chain (REQUIRED, permanent)

Every lookup is: **index tree first, legacy per-session refs second.**

```
read_session:   index sessions/<date>/<id>/body.json
                → (non-v7 id) scan index date dirs
                → refs/dkod/sessions/<id>
blame commit:   index commits/<fanout> → refs/dkod/commits/<sha>
blame patchid:  index patchid/<fanout> → refs/dkod/patchid/<pid>
list_sessions:  index walk ∪ refs/dkod/sessions/* (dedup, index wins)
```

A repo that never runs `dkod reindex` and never upgrades its writers keeps
working **unmodified, forever**. A missing `refs/dkod/index` ref simply means
the index branch of every chain is skipped — i.e., v1 behavior exactly. The
fallback is not a deprecation period; it is a permanent compatibility floor
(it is ~30 lines of code and the cost of keeping it is near zero).

## 7. Size budget (capture-time)

### 7.1 Defaults and mechanism

```toml
[capture]
session_budget_kib = 256     # target compressed size; 0 = unlimited
full_fidelity = false        # true => never truncate (opt-out)
```

Enforced inside `finalize_session`, **after redaction, before write**:

1. Serialize the redacted session; deflate it (cheap, one pass). Under
   budget → write as-is.
2. Over budget → cap each `Message::Tool.output` at **16 KiB: 8 KiB head +
   8 KiB tail** with an explicit marker splice:
   `…[dkod: truncated 412 KiB of tool output]…`. Re-check.
3. Still over → tighten the per-output cap to 4 KiB (2 head + 2 tail),
   largest outputs first. Re-check.
4. Still over (rare: enormous user/assistant text) → **write anyway** and
   print a one-line warning. User prompts, assistant text, and reasoning are
   the forensic core — dkod never truncates them. `meta.json` records
   `"truncated": true` so `dkod show` can disclose it.

Tool outputs are the verified bloat source (strategy v2: "bloat is
tool-output spam", plausible 5–10× cut), and head+tail preserves the two
forensically useful parts of a tool result: what it started saying and how it
ended (errors, exit status).

### 7.2 Interaction with redaction (ordering is load-bearing)

**Redact first, truncate second.** Reasons:

- Redaction (`redact_session`, default ON) must see the full, contiguous
  text: truncating first could split a secret across the head/tail cut so
  the pattern never matches the kept fragments. Redact-then-truncate
  guarantees every byte that *persists* was scanned in full context
  (including the entropy ruleset, which needs intact tokens).
- `redaction_count` stays an honest audit number for the whole transcript,
  not just the kept bytes.
- Bytes dropped by truncation were already redacted, so the truncation
  marker can never be hiding an unredacted secret — and dropped middles never
  persist anywhere regardless.

The truncation marker's byte count refers to redacted text (post-redaction
sizes), which is the only text that ever existed on disk.

## 8. Retention: `dkod gc --keep <duration>`

```
dkod gc --keep 90d [--bundle <path>] [--dry-run]
```

### 8.1 What it does

1. Cutoff = now − duration. Candidate sessions = date dirs older than the
   cutoff date (the date fan-out makes this a directory-name comparison; the
   v7-id timestamp is authoritative for boundary days).
2. **Optional bundle export first**: `--bundle` writes a `git bundle` of the
   current `refs/dkod/index` (full history, full objects) so the dropped
   sessions remain restorable offline / archivable to cold storage before
   anything is destroyed. (Indexer-side archival is the hosted complement;
   out of scope here.)
3. Build the pruned tree: drop out-of-window `sessions/<date>/` dirs; walk
   `commits/` and `patchid/` pointer blobs and drop every pointer whose
   embedded session id resolves to a dropped session (the id's v7 timestamp
   answers this without a reverse map).
4. Write the pruned tree as a **new orphan root commit** (no parent — a
   snapshot), bump the `epoch` blob (`epoch` = previous + 1), point local
   `refs/dkod/index` at it, and **force-push** `refs/dkod/index`.

The orphan reset is not optional polish: if gc merely added a "pruned" commit
on top of the chain, every dropped blob would remain reachable through
history and **no host would ever reclaim a byte**. Actually shrinking
requires the old commits to become unreachable.

### 8.2 Host-side reclamation caveats (documented, not solved)

- Servers reclaim unreachable objects on **their** schedule: GitHub repo-size
  reporting lags days-to-weeks behind an unreachable-object purge; GitLab
  needs housekeeping (automatic eventually, or via the housekeeping API);
  Gitea/Bitbucket on their own gc cadence. `dkod gc` prints exactly this so
  nobody files "gc didn't shrink my repo" bugs.
- Quota-capped hosts under acute pressure may need a support ticket /
  manual housekeeping run to realize the reduction promptly.
- Local repos reclaim via normal `git gc` (the old index commits are
  unreachable locally too once the ref moves).

### 8.3 Client behavior after gc (epoch adoption)

A client's next dkod-driven fetch sees a remote index that is **not a
descendant** of its local index. It reads the remote root's `epoch` blob:

- remote epoch > local epoch → a gc happened. Adopt the remote tip; replay
  any **local-only unpushed sessions** on top (same replay machinery as
  §5.4); reset the local ref (dkod-driven, allowed to move non-FF per §5.5).
- epochs equal but histories unrelated → corruption/misuse; refuse and tell
  the user to run `dkod reindex --doctor` (diagnose-and-rebuild).

Cost: the one fetch after a gc loses negotiation (full re-download of the
pruned archive — which gc just made small). Vanilla `git fetch` simply
declines the non-FF update (§5.5) until dkod handles it; stale local index
data is read-only harmless in the interim.

## 9. Propagation default: what `dkod init` wires, and the lazy-fetch truth

### 9.1 Stop subscribing the world

`ensure_dkod_refspec` today adds `+refs/dkod/*:refs/dkod/*` to **every**
remote — every teammate's every fetch mirrors the whole archive. V2 default:

- subscribe to **only** `refs/dkod/index:refs/dkod/index` (non-forced, §5.5),
  wired on **exactly one** remote entry depending on the subscription tier
  (§9.2): the filtered `dkod-archive` remote in lazy mode, or `origin` in
  full mode. Never both — a plain-refspec copy on `origin` alongside the
  filtered one would let a vanilla `git fetch origin` pull every body blob
  unfiltered and silently defeat lazy mode. Other remotes opt in via
  `dkod init --remote <name>`.
- migrate-in-place: init removes an existing `+refs/dkod/*:refs/dkod/*`
  entry it previously wrote and wires the tier-appropriate subscription
  (exact-match detection, same idempotency approach as
  `remote_already_has_dkod_refspec`), leaving foreign refspecs alone.
- `discover_remote_sessions` switches from ls-remote'ing
  `refs/dkod/sessions/*` (O(sessions) advertisement) to ls-remote'ing the
  single index ref + reading the session count from a fetched index tip;
  repos that only have legacy refs keep the old count path.

### 9.2 The honest analysis: one ref ≠ cheap fetch by itself

Subscribing the single index ref kills the **ref advertisement** cost and
buys **negotiation**, but a plain fetch of that ref still downloads the full
reachable closure — every body blob ever — *that's the full archive again*,
just spelled differently. The fix has to live in object filtering. Options
considered:

| Option | Verdict |
|---|---|
| **(a) `--filter=blob:none` on the combined index fetch** | Trees+commits only; *every* blob lazy. But metadata is in blobs too, so `dkod log`/`blame` would trigger one network round-trip per session header — a listing of 1k sessions becomes 1k lazy fetches. Unusable offline. Rejected as the whole story. |
| **(b) Split index: metadata always mirrored, bodies on demand** | `meta.json` blobs are ~600 B; a year of a 50-dev org is ~76 MB uncompressed, fetched incrementally (~ tens of KB/day). `log`, `blame`, `drift` triage work **fully offline** from metadata. Only `dkod show`/drift-detail backfill a body. **Chosen.** |

**Recommendation: (b), implemented with the meta/body split of §4.3 plus a
size-cutoff filter** — and crucially, the lazy fetch goes through a
**dedicated second remote entry**, not `origin`:

```
git remote add dkod-archive <origin-url>          # same URL, separate config
git config remote.dkod-archive.fetch  refs/dkod/index:refs/dkod/index
git config remote.dkod-archive.partialclonefilter blob:limit=100k
git config remote.dkod-archive.promisor true
```

- `blob:limit=100k` mirrors everything small (all meta blobs, pointer blobs,
  trees, small bodies) and defers only large bodies. Bodies above the limit
  are backfilled automatically by git's promisor machinery the moment
  `dkod show` reads the blob.
- **Why a second remote:** `remote.<name>.partialclonefilter` applies to all
  fetches from that remote — putting it on `origin` would blob-filter the
  user's *code* fetches. Scoping promisor-ness to `dkod-archive` leaves
  `origin` completely untouched. (The repo does gain
  `extensions.partialClone` semantics — `git fsck`/`gc` treat
  promisor-referenced missing objects as expected. That is the unavoidable,
  disclosed cost of lazy blobs; it is scoped to objects from dkod fetches.)
- **Host support probe at init:** partial clone requires
  `uploadpack.allowFilter` server-side (GitHub, GitLab, Gitea ≥1.17: yes;
  others vary). Init probes with a trial filtered fetch; on failure it falls
  back to the plain (full-content) index refspec on `origin` and says so.
  Full mirror of a *budgeted, gc'd* archive is the acceptable degraded mode —
  the index alone already fixed refs + negotiation.
- `dkod init --subscribe=full` forces the full mirror (CI runners, airgap
  prep); `--subscribe=none` wires nothing (capture-only writers).

So the subscription tiers are: **none** → **index-lazy (default when host
supports filters)** → **full**.

## 10. Migration: `dkod reindex`

```
dkod reindex [--delete-legacy] [--dry-run]
```

1. Enumerate local legacy refs: `refs/dkod/sessions/*`, `refs/dkod/commits/*`,
   `refs/dkod/patchid/*` (plus any `.git/dkod/outbox/*.json` spills).
2. Fold into the index as one batched commit
   (`dkod: reindex <n> legacy session(s)`): session blob → `body.json` (bytes
   copied verbatim — same object id, so no new blob storage), derived
   `meta.json`, link refs → pointer blobs. Legacy sessions keep their
   original size (the budget is capture-time only; reindex never mutates
   captured history).
3. **Idempotent**: a path already present with the same blob OID is skipped;
   re-running on a fully-indexed repo writes nothing (no empty commit).
   Safe to run concurrently with capture (it's just another IndexBatch under
   the same lock/CAS).
4. Push the index (same §5.4 machinery).
5. **`--delete-legacy` (opt-in, never default):** after a verification pass
   confirms every legacy ref's content is reachable from the pushed index
   tip, delete the legacy refs **remotely**
   (`git push origin --delete <refs…>`, batched) and locally. Without the
   flag, legacy refs stay — harmless duplicates the fallback chain dedups.
   The flag is the moment a team commits to "every reader is on v2" (§12);
   the command prints exactly that warning before acting.

Non-v7 session ids found in legacy refs are indexed under the date derived
from their `created_at` (and found again via the §6.2 scan fallback).

## 11. Trust boundary: unchanged; indexer note

The indexer's contract is unchanged: it reads the same `refs/dkod/*`
namespace from the customer's git host under the user's token, persists
metadata only, and remains a disposable cache (strategy v2, "Infra"). The one
indexer-side assumption that must change — **out of scope here, tracked on
dkod-indexer** — is its reconciler's `git ls-remote … refs/dkod/*` +
per-session-ref ingestion (see the dkod-indexer ingest/reconciler doc's
"sessions are refs" assumption): with v2 it sees a single `refs/dkod/index`
ref whose tip SHA *is* the change cursor (tip unchanged → nothing to ingest;
changed → fetch the index delta, walk new meta blobs). That is strictly
cheaper for the indexer than 300k-ref ls-remote diffing. Until updated, an
old indexer keeps working against legacy refs (and reads nothing from
index-only repos — one more reason `--delete-legacy` is opt-in).

## 12. Mixed-version compatibility story

Repo-level coordination lives in the committed `.dkod/config.toml`:

```toml
[storage]
format = "v2"              # written by `dkod reindex` / new init; absent = v1
write_legacy_refs = true   # dual-write toggle; default true in Phase 1/2
```

Old CLIs deserialize unknown keys harmlessly (serde ignores unknown fields).
The matrix:

| Writer | Reader | Result |
|---|---|---|
| old CLI (legacy refs) | new CLI | Read via fallback chain (§6.2); new CLI's next capture opportunistically folds local legacy refs it hasn't indexed (reindex-lite, same idempotent batch) so the index converges. |
| new CLI, dual-write ON | old CLI | Old CLI sees the legacy refs it expects. Full interop; ref growth continues until the team flips dual-write off. |
| new CLI, dual-write OFF | old CLI | Old CLI **cannot see new sessions** — this is the one genuinely incompatible cell. Guarded twice: dual-write defaults ON until the team opts out, and `--delete-legacy`/`write_legacy_refs=false` print "all readers must run ≥ vX.Y". |
| new ↔ new | | Index only; steady state. |

## 13. The math, before and after

50-dev org, ~10 sessions/dev/day, 252 working days ≈ **126k sessions/yr**,
~2 commits/session.

| Metric | v1 (today) | v2 |
|---|---|---|
| Refs after 1 yr | 250–375k | **1** (`refs/dkod/index`) + legacy until reindexed |
| Ref advertisement per fetch | ~25–37 MB, every fetch, every client | **~100 B** |
| Fetch negotiation | none (blob-target refs) | commit-chain; delta = new sessions only |
| Session data per yr (median 600 KB) | ~75 GB | budgeted ≤256 KiB, realistic post-truncation median ~60–120 KB → **~8–15 GB/yr**; with `gc --keep 90d` steady-state **~2–4 GB total** |
| Teammate subscription cost | full archive, growing, on every fetch | metadata only: ~600 B/session → **~76 MB/yr** uncompressed, fetched incrementally; bodies on demand |
| Bitbucket 4 GB cap | breached in weeks | inside the cap with 90d retention (and Bitbucket is a dropped segment per strategy v2; side-repo escape hatch documented) |
| GitLab Free 10 GiB | breached in 1–2 months | years with budget alone; indefinitely with retention |
| Objects per session | 1 blob + 2–3 loose refs | ~2 blobs + ~4–6 tree nodes + 1 commit (trees delta-compress to near zero) |
| Index commit chain | n/a | ~126k commits/yr — trivial for git |

## 14. Failure-mode table

| Failure | Behavior |
|---|---|
| Two local capture daemons finalize simultaneously | Lock file serializes; CAS backstop with 5 bounded retries; final fallback spills to `.git/dkod/outbox/`, folded in by next finalize. No session lost. |
| Push race (teammate pushed first) | Fetch remote tip → replay local-only inserts as one reconcile commit → push; 3 bounded rounds; on exhaustion stay local-ahead, retried at next capture / `dkod push`. |
| Offline capture | Index commit is locally durable; push deferred. Vanilla fetch can't clobber the local-ahead ref (non-forced refspec, §5.5). |
| Interrupted fetch | Git atomicity: ref not moved until the pack lands; retry is a plain re-fetch. |
| Lazy body missing while offline | `dkod show` prints the session header from `meta.json` plus "body not fetched — run `dkod fetch <id>` when online". `log`/`blame` are unaffected (metadata-only). |
| Mixed versions: old CLI writes legacy refs | New CLI reads them via fallback and folds them into the index opportunistically (idempotent). |
| Mixed versions: new CLI index-only, old reader | The one breaking cell — gated behind explicit opt-out of dual-write + loud warnings (§12). |
| gc force-push vs stale clients | Epoch check (§8.3): adopt remote, replay unpushed local sessions on top. One non-negotiated fetch of the (now small) pruned archive. Vanilla fetch harmlessly declines the non-FF until dkod handles it. |
| Same-path write collision (patch-id from two sessions) | Last writer wins — the documented v1 policy, preserved. |
| Host without partial-clone support | Init probe fails → full-content index subscription on `origin`, message printed; refs + negotiation benefits retained. |
| Index ref corrupted / deleted locally | Re-fetch from origin; if remote also gone, `dkod reindex` rebuilds from legacy refs/outbox where they exist. |
| Hook bypassed rewrite (no post-rewrite event) | Unchanged from v1: patch-id fallback path resolves diff-preserving rewrites (§6.1). |

## 15. Phasing (each phase shippable)

1. **Phase 1 — index core + dual-write + read fallback + reindex.**
   `IndexBatch`, tree codec, format spec doc; `write_session*` writes index
   **and** legacy refs (dual-write ON); all reads go index-first with
   fallback; `dkod reindex` (no `--delete-legacy` yet); local lock/CAS.
   Shippable: zero behavior change for old readers, index correctness soaks
   in the wild.
2. **Phase 2 — capture size budget + auto-push/reconcile.**
   Redact→truncate pipeline + config; best-effort push with
   fetch-replay-retry and the non-forced refspec switch in init (init also
   migrates the old broad refspec). Shippable: the 5–10× size cut and
   one-ref fetch land for every team, still fully v1-compatible.
3. **Phase 3 — lazy propagation.** Meta/body split is already in the format;
   this phase adds the `dkod-archive` remote wiring, the init filter probe,
   subscription tiers, and body backfill + offline messaging in
   `show`/drift-detail. Shippable: big-org fetch cost drops to metadata.
4. **Phase 4 — retention + legacy retirement.** `dkod gc --keep` with bundle
   export, epoch adoption in the fetch path; `reindex --delete-legacy`;
   dual-write default flips OFF (minor version, release-noted). Shippable:
   quota-capped hosts get a steady-state ceiling.
5. **Phase 5 (optional polish) — `dkod doctor` for index integrity,**
   reindex-from-bundle restore, and the side-repo how-to doc. Cut freely if
   the strategy timeline (months 2–3) is tight.

## 16. Self-consistency notes

- §5.5 (non-forced refspec) deliberately constrains §8 (gc force-push): only
  dkod-driven fetches adopt a gc'd index, which is why epoch adoption lives
  in dkod's fetch path and not in git config.
- §4.3's meta/body split exists *because of* §9.2's analysis — if lazy
  propagation were dropped, a single body blob would suffice; the split is
  cheap (~600 B/session) so it ships in the format from Phase 1 to avoid a
  format bump later.
- The §7 budget applies at capture only; §10 reindex preserves legacy bytes
  verbatim — so "≤256 KiB" claims are about new captures, and the before/after
  table says so.
- `write_session` standalone still produces a 2-path index commit while
  `write_session_with_commit_links` produces one combined commit — callers
  composing the two would create two commits; that is why `finalize_session`
  moves to the batched entry point (§5.2) while signatures stay stable.
