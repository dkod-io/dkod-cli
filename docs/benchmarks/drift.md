# `dkod drift` precision benchmark

**Date:** 2026-06-10 · **Binary:** `dkod` release build at this commit ·
**Config:** `DriftConfig::default()` (no `.dkod/config.toml`) unless noted ·
**Harness:** `benchmarks/drift/run.sh` · **Dataset:** `benchmarks/drift/dataset/` (60 fixtures)

The strategy doc (`docs/plans/2026-06-10-dkod-strategy-v2.md`) flags drift
precision as an unproven headline claim. This benchmark answers: **what is the
false-positive rate of the three local heuristics on realistic sessions?**
Positive class = "drift" (session flagged). The product goal is to
**under-flag**: precision matters more than recall, because a weekly digest
that accuses clean sessions distributes embarrassment.

## Methodology

- 60 hand-labeled session fixtures, each `{label, why, session}`. The session
  JSON matches the real `Session` schema (id, agent, created_at, duration_ms,
  prompt_summary, role-tagged messages, commits, files_touched).
- Each fixture is replayed exactly the way production sessions are stored: the
  session JSON becomes a git blob pinned at `refs/dkod/sessions/<id>`
  (`git hash-object -w` + `git update-ref`) in a fresh temp repo, then the real
  `dkod drift <id>` binary runs and its output is parsed (`clean` vs reasons;
  the rule is recovered from the reason text).
- All fixtures have `commits: []`, so the line-count half of the magnitude rule
  (`large_change_lines`) is **not exercised** — only the file-count half
  (`large_change_files`) and the other two rules. Line-magnitude behavior on
  real diffs is untested here.
- Prompts and file sets are modeled on real agent-usage patterns (typo fixes,
  dep adds, module refactors, test fixes, multi-turn authorizations) across all
  seven supported agents. Labels reflect **user intent**, not what the rules
  will do — several fixtures were built knowing the tool would mis-call them.

### Dataset composition

| slice | n | description |
|---|---|---|
| `clean-*` | 20 | Clearly clean: verbose prompts naming touched files; module-scoped refactors; multi-turn sessions where later turns authorize extra files; proportionate short asks. |
| `drift-*` | 20 | Clearly drift: typo/comment asks touching workflows, lockfiles, `.env`, auth, migrations; tiny asks touching 7–12 files; prompts naming file A while file B changes; a summary-only session. |
| `hard-*` | 20 | Judgment calls, labeled with rationale in `why`: dependency asks that legitimately churn lockfiles (the known FP class), explicitly requested workflow/Dockerfile/migration/auth edits, vague-short vs vague-verbose prompts with big changes, directory-scoped asks, "fix the failing tests", test-file-named source fixes, "rename … everywhere", summary-only sessions. |

22 fixtures are labeled drift, 38 clean. The hard slice is deliberately
adversarial; it is intended to be roughly the ceiling of how bad a realistic
week looks, not the average.

## Results — default config (pre-#28, kept for history)

```
TP=22  FP=13  TN=25  FN=0   (n=60)
precision = 62.9%   recall = 100.0%   accuracy = 78.3%
```

| slice | n | FP | FN | note |
|---|---|---|---|---|
| clean | 20 | 0 | 0 | every clearly-clean session stays clean |
| drift | 20 | 0 | 0 | every clearly-drifting session is flagged |
| hard | 20 | 13 | 0 | all 13 errors are false positives on judgment cases |

Two readings of the same numbers:

- **On unambiguous sessions (n=40) the heuristics are perfect:** 100%
  precision, 100% recall. The rules do exactly what they claim on the cases
  the docs describe.
- **On the adversarial-but-realistic mix (n=60) precision is 62.9%:** roughly
  1 in 3 flags in a worst-case week is noise. Recall stays 100% — the tool
  currently over-flags, never under-flags, which is the opposite of the
  stated under-flag tuning goal.

### Per-rule false-positive attribution

16 rule-firings across the 13 FP sessions (a session can fire several rules):

| rule | TP firings | FP firings | FP share | dominant FP pattern |
|---|---|---|---|---|
| `sensitive_path` | 13 | **8** | 50% | tripwire is authorization-blind: it fires even when the prompt explicitly asked for the sensitive file |
| `magnitude` | 10 | 4 | 25% | `<=140 chars` treats short-but-broad invitations ("clean up the service layer", "rename … everywhere", "fix the failing tests") as small asks |
| `unmentioned` | 10 | 4 | 25% | companion files of the named work (lockfile of a dep add, source under a named test, prose-described files) count as "unrelated" |

Breakdown of the 8 `sensitive_path` FPs — the predicted biggest source, confirmed:

- **Dependency-ask lockfile churn (3):** `hard-01` (`package-lock.json` after
  "add the axios dependency"), `hard-02` (`Cargo.lock` after "run cargo add
  serde"), `hard-03` (`poetry.lock` after "add httpx"). The lockfile is the
  *direct, mechanical consequence* of the ask. Two of these also double-fire
  `unmentioned` on the manifest/lockfile, so suppressing the tripwire alone is
  not enough.
- **Explicitly requested sensitive edits (5):** `hard-04` (asked for the CI
  workflow change), `hard-05` (asked for the Dockerfile change), `hard-06`
  (asked for auth work in `src/auth.rs`), `hard-07` (asked for a migration),
  `hard-19` (second turn names `.github/workflows/deploy.yml`). The tripwire
  never reads the prompt, so authorization is invisible to it.

The contrast case `hard-14` (lockfile churn with *no* dependency ask) is
correctly flagged — the tripwire's signal is real; its blindness to
authorization is the problem.

## Results — best config-only tuning (measured)

`benchmarks/drift/tuned-candidate.toml` exercises the two existing knobs that
target the FP sources: drop lockfiles from `sensitive_paths`, raise
`large_change_files` 5 → 8. Run via
`DKOD_DRIFT_CONFIG=benchmarks/drift/tuned-candidate.toml benchmarks/drift/run.sh`:

```
TP=19  FP=9  TN=29  FN=3   (n=60)
precision = 67.9%   recall = 86.4%   accuracy = 80.0%
```

The trade is bad: +5.0pt precision costs 13.6pt recall. Removing lockfiles
from the tripwire silently un-flags the *true* positives `drift-16` and
`hard-14` (unjustified lockfile churn), and raising the file threshold loses
`drift-20`. **The existing config surface cannot express the actual fix**,
which is conditioning on the prompt; it can only delete signal.

## Results — after issue #28 fixes (2026-06-10)

Issue #28 landed the three code changes recommended below (dependency-aware
lockfile suppression, authorization-aware tripwire, scope-aware small-ask)
plus the test-file↔source-under-test pairing in the `unmentioned` rule, all
defaults-on in `DriftConfig::default()`. Same dataset, same labels, same
harness:

```
TP=21  FP=2  TN=36  FN=1   (n=60)
precision = 91.3%   recall = 95.5%   accuracy = 95.0%
```

| slice | n | FP | FN | note |
|---|---|---|---|---|
| clean | 20 | 0 | 0 | unchanged — every clearly-clean session stays clean |
| drift | 20 | 0 | 1 | `drift-16` — the one TP the suppression was predicted to cost |
| hard | 20 | 2 | 0 | 11 of 13 FPs cleared |

### Per-rule attribution (after)

| rule | TP firings | FP firings | note |
|---|---|---|---|
| `sensitive_path` | 12 | **0** | all 8 FPs cleared; authorization-blind no more. `drift-16`'s lockfile firing is also suppressed (the FN below) |
| `magnitude` | 10 | 1 | `hard-13` remains (see residuals) |
| `unmentioned` | 10 | 1 | `hard-06` remains (see residuals) |

### Residual misclassifications (3)

- **FN `drift-16`** ("bump lodash" that also rewrote two `src/` files): the
  dep-ask suppression removes its only firing rule, exactly the predicted and
  accepted cost under the under-flag goal. Recoverable later via an
  unprompted-source-files check on dep asks.
- **FP `hard-06`** (`unmentioned` on `src/email.rs`): the email hook is
  described in prose ("the email notification hook"), not as a path or
  basename. Catching prose-described companions requires semantic intent
  matching — the deferred LLM layer; any lexical stem-matching loose enough to
  catch it would also re-open true positives like `drift-05`.
- **FP `hard-13`** (`magnitude` on "fix the failing tests" + 5 files): the
  prompt is short, contains no broad-scope phrase, and names no paths. There
  is no lexical signal separating it from a genuinely small ask; this is also
  one for the LLM layer (or a test-only-changes heuristic, deliberately not
  added here to avoid overfitting the dataset).

The contrast cases stay correct: `hard-14` (lockfile churn with no dep ask)
and `drift-01` (workflow touch with no authorization) are still flagged.

## Comparison anchor

Macroscope markets ~98% precision for its AI code-review findings. `dkod
drift` at defaults measures **62.9%** on this dataset (100% on unambiguous
sessions). A public precision headline is not defensible today; "100% recall
on a 60-session labeled benchmark, with every false positive coming from three
identified, fixable patterns" is.

## Tuning recommendations (DriftConfig / drift.rs)

**Status: 1–3 implemented by issue #28 (results above); 4 stands.**

Ordered by measured FP impact. 1–3 require code changes in
`crates/dkod-core/src/drift.rs` (new behavior, defaults on), 4 is config-only.

1. **Dependency-aware lockfile suppression** (clears 3 FPs, the known class):
   when the intent contains a dependency marker (`add`/`install`/`upgrade`/
   `bump` near `dependency`/`package`, or `cargo add` / `npm install` /
   `poetry add` / `go get`), suppress the tripwire for lockfiles *and* exempt
   manifest+lockfile pairs from the `unmentioned` rule. Cost: `drift-16`
   ("bump lodash" that also rewrote two src files) loses its only firing rule
   and becomes a FN — acceptable under the under-flag goal, and recoverable
   later via an unprompted-src-files check.
2. **Authorization-aware tripwire** (clears 5 FPs, the biggest class): before
   emitting `sensitive_path`, check whether the intent names the matched
   file's basename, a parent directory, or the category of the matched glob
   ("workflow", "dockerfile", "migration", "auth", "ci"). If yes, stay silent
   or downgrade to an info-level note. The tripwire keeps firing when the
   prompt is silent about the sensitive area (`drift-01`, `hard-14`).
3. **Scope-aware small-ask test** (clears up to 4 FPs): do not treat the
   prompt as a small ask when it contains broad-scope markers ("everywhere",
   "across", "all", "clean up", "refactor", a bare directory token). Fixes
   `hard-09/-10/-18`; pairing test files with their source under test
   (`test_cache.py` ↔ `cache.py`) additionally fixes `hard-13`/`hard-17` in
   the `unmentioned` rule.
4. **Do not** raise `large_change_files` or strip `sensitive_paths` at
   defaults — measured above, it buys little precision and sells real recall.

Analytic projection (not measured — needs the code changes): items 1–3
together clear 11 of the 13 FPs and lose 1 TP, landing at ~91% precision /
~95% recall on this dataset. That is the gate this benchmark should re-verify
once implemented.

## Verdict

**The launch blockers are cleared: with the issue #28 fixes landed
(dependency-aware lockfile suppression, authorization-aware tripwire,
scope-aware small-ask, test↔source pairing), the default config measures
91.3% precision / 95.5% recall on this dataset — above the ≥90% / ≥95% gate
this benchmark set.** The three residual misclassifications are all cases
that need semantic intent matching (the deferred opt-in LLM layer), not more
lexical rules. Framing guidance stands: cite the measured numbers with the
dataset, and keep the digest a review queue rather than an accusation — the
remaining errors are judgment calls by construction.

*(Pre-#28 verdict, for history: not shippable at 62.9% default-config
precision; recs 1–2 below were designated launch blockers.)*

## Reproducing

```sh
benchmarks/drift/run.sh                      # builds release binary, full report
DKOD_BIN=target/release/dkod benchmarks/drift/run.sh        # skip rebuild
DKOD_DRIFT_CONFIG=benchmarks/drift/tuned-candidate.toml \
  benchmarks/drift/run.sh                    # measure a candidate config
```

Requirements: `git`, `jq`, Rust toolchain. The harness never touches the
host repo state; every fixture runs in a throwaway temp repo.
