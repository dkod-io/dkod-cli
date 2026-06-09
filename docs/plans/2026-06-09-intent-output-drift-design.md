# Intent-vs-Output Drift — Design

**Date:** 2026-06-09
**Status:** approved (design), pending spec review → implementation plan

## TL;DR

`dkod drift` flags captured sessions where the agent did materially more or
other than the prompt asked for — "told to fix a typo, also touched
`.github/workflows/` and 12 files." dkod already stores the prompt (intent)
and the files/commits (output) together, so this is computable **locally**,
with **no network and no new dependencies**, from data we already have plus
git diff stats.

v1 is **rule-based and local** — three explainable heuristics, each emitting a
human-readable reason. A semantic LLM layer (which would force outbound HTTP +
API keys + a privacy reconciliation against dkod's "transcripts never leave
your git host" pitch) is explicitly **deferred** as a future opt-in increment.

## Locked decisions (from brainstorming)

1. **Mechanism:** rule-based, fully local, zero-network, zero-new-dep. LLM
   semantic comparison deferred (opt-in, user's own key, off by default).
2. **Surface:** a standalone `dkod drift` command, computed **on-read** over
   stored sessions (re-tuning the config re-evaluates everything; capture is
   untouched). Mirrors the `log`/`blame` read-command pattern.
3. **Verdict:** structured reasons. A session is `clean` (no reasons) or
   `flagged` (≥1 reason); each reason is concrete and human-readable. No opaque
   score, no bare boolean.
4. **Intent baseline:** all `Message::User` contents concatenated, with
   `prompt_summary` as fallback when there are no user messages (a session's
   asks accumulate across turns; the union is the fair baseline).

## What we compare

### Intent (from the stored `Session`, no I/O)
Concatenate every `Message::User { content }`; if none, use `prompt_summary`.
From this lowercase text we derive:
- **length** (chars) — for the "small-sounding ask" signal.
- **small-scope keyword present?** — typo / rename / comment / bump / tweak /
  format / lint / whitespace / one-liner / "small fix" (configurable).
- **mentioned paths** — tokens that look like a file path (contain `/` and a
  dotted extension) OR whose basename equals a `files_touched` basename. Used
  only by the unmentioned-file heuristic, which fires *only* when this set is
  non-empty.

### Output footprint
- `session.files_touched` — always present in the session blob → file-level
  heuristics always work.
- **`DiffStats { files, insertions, deletions }`** — derived in the CLI layer
  via `git diff --numstat` over the session's commits. `Option`: when the
  commits are absent (rewritten/gone), it's `None` and line-magnitude checks
  skip; file-level checks still run.

## Heuristics (v1 — exactly three; each emits a `DriftReason` when it trips)

1. **Sensitive-path tripwire.** Any touched file matches a configured sensitive
   glob. Fires **regardless of the prompt** — a pure oversight signal. Default
   globs: `.github/workflows/**`, `**/Dockerfile`, `**/*.pem`, `**/*.key`,
   `**/.env*`, `**/secrets*`, `**/Cargo.lock`, `**/package-lock.json`,
   `**/yarn.lock`, `**/poetry.lock`, `**/go.sum`, `**/migrations/**`,
   `**/auth*`.
   Reason: `touched sensitive path: <file> (matched <glob>)` (one reason per
   matched file, capped at a small number to avoid flooding).

2. **Small-ask / large-change magnitude.** Prompt is small-sounding
   (`len ≤ small_ask_max_chars` OR contains a small-scope keyword) **AND** the
   change is large (`files_touched.len() ≥ large_change_files` OR, when
   `DiffStats` is present, `insertions + deletions ≥ large_change_lines`).
   Reason: `small-sounding request but N files / M lines changed`
   (omit the "/ M lines" clause when `DiffStats` is `None`).

3. **Unmentioned-file drift.** Only when mentioned-paths is non-empty: the
   touched files whose basename is not among the mentioned basenames. Avoids
   crying wolf on vague prompts (which name nothing).
   Reason: `prompt referenced <a, b>; also changed unrelated <x, y, z>`
   (lists capped).

That is the entire v1 rule set (YAGNI). Each rule is a pure function of
`(intent-derived inputs, files_touched, Option<DiffStats>, DriftConfig)` and is
independently unit-testable.

## Verdict & rendering

```rust
pub struct DriftReason { pub rule: &'static str, pub detail: String }
pub struct DriftVerdict { pub reasons: Vec<DriftReason> }
impl DriftVerdict { pub fn is_clean(&self) -> bool { self.reasons.is_empty() } }
```

- `dkod drift` — list flagged sessions newest-first (same sort as `log`):
  a header line `<id>  <agent>  <prompt_summary>` then each reason indented.
  Clean sessions are omitted unless `--all` is passed.
- `dkod drift <session-id>` — that one session's verdict: `clean`, or the full
  reason list (plus the touched-file count and diff stats when available).
- **Exit code is always 0** — `dkod drift` is a report, not a gate. (A
  `--exit-code`/CI-gate mode is a deliberate future layer, not v1.)

## Architecture (units)

- **`dkod_core::drift`** (new module, pure): `analyze(session: &Session,
  diff_stats: Option<&DiffStats>, cfg: &DriftConfig) -> DriftVerdict`, plus
  `DriftReason`, `DriftVerdict`, `DiffStats`, and `DriftConfig`. No I/O, no git,
  no network — the testable heart. **Glob matching is a small dependency-free
  internal matcher** (verified: no `glob`/`globset` is in the tree, and the
  zero-new-dep decision stands). It supports exactly what the default patterns
  need: paths normalized to forward slashes and matched segment-by-segment,
  where a `**` segment matches zero or more path segments and `*` within a
  segment matches any run of non-`/` characters (so `**/*.pem`,
  `.github/workflows/**`, `**/.env*`, `**/auth*`, and exact basenames like
  `**/Cargo.lock` all work). This matcher is its own pure, unit-tested helper.
- **`dkod-cli::cmd::drift`** (new): resolves sessions via
  `store::list_sessions` / `read_session` (mirrors `cmd::log`), computes
  `DiffStats` from `git diff --numstat` over `session.commits` (shell-out in
  the CLI layer, like `blame`/`patchid`), calls `drift::analyze`, renders.
  Wired as a normal (visible) `Drift { session_id: Option<String>, all: bool }`
  subcommand in `main.rs`; runs `maybe_warn_drift()` like other read commands.
- **Config:** extend `dkod_core::config::Config` with `drift: DriftConfig`
  (all fields `#[serde(default)]`, so zero-config works). Fields: `enabled`
  (default true), `sensitive_paths`, `small_ask_max_chars` (default 140),
  `small_ask_keywords`, `large_change_files` (default 5), `large_change_lines`
  (default 150). When `enabled = false`, `dkod drift` reports every session
  clean (engine short-circuits).

## Data flow

```text
dkod drift [id] [--all]
  → load session(s)                                    (store::list/read_session)
  → diff_stats = git diff --numstat over session.commits → Option<DiffStats>  (cmd::drift)
  → drift::analyze(&session, diff_stats.as_ref(), &cfg.drift) → DriftVerdict   (core, pure)
  → render: list flagged (newest-first) | single-session detail               (cmd::drift)
```

## Testing

- **Core `analyze` unit tests** (the heart):
  - sensitive-path: a touched `.github/workflows/x.yml` trips; a benign
    `src/lib.rs` does not.
  - small-ask × large-change matrix: small ask + large change → flagged; small
    ask + small change → clean; large change + long/verbose ask → clean (the
    magnitude rule needs *both*); `DiffStats=None` path uses file-count only.
  - unmentioned-file: prompt names `auth.rs`, agent also touches `billing.rs`
    → reason names `billing.rs`; vague prompt (no mentioned paths) → rule does
    not fire even on a broad change.
  - fully-clean session → empty verdict; `enabled=false` → empty verdict.
  - reason strings contain the offending path / counts (assert on substrings).
- **`DiffStats` git helper** (CLI): a real temp repo with a 2-commit session
  range → correct files/insertions/deletions; missing commit → `None`/graceful.
- **e2e `dkod drift`** (real binary, assert_cmd): seed two sessions in a temp
  repo — one "fix typo" prompt whose commit touched `.github/workflows/ci.yml`
  (flagged, reason shown), one ordinary matching session (clean, hidden without
  `--all`, shown with `--all`). Assert the command exits 0 and the output
  distinguishes them.

## Known limitations (documented)

1. **Heuristic, not semantic.** Won't catch subtle same-magnitude semantic
   drift ("fixed login, also refactored payments" where both are similar-sized
   code edits). That is the deferred **opt-in LLM layer**.
2. **Tuning trade-off.** Thresholds trade false positives vs misses; defaults
   aim to **under-flag** (precision over recall) so the signal stays trusted.
3. **Line-magnitude needs the commits present**; file-level checks don't.
4. **Mentioned-path extraction is shallow** (path-shaped tokens / basenames) —
   it can miss paraphrased intent ("the auth module"); this only weakens the
   unmentioned-file rule, which is conservative by design (fires only when
   explicit paths are present).

## Out of scope (v1)

- LLM semantic comparison (opt-in, future).
- A CI/pre-commit gate mode (`--exit-code`); the engine is built to support it
  later, but v1 only reports.
- Per-reason severity levels (info/warn/high) — reasons are flat in v1.
- Drift verdicts stored in the session blob — v1 computes on-read only.
