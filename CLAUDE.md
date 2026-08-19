# dkod-cli — ARCHIVED (superseded by dkod-signals, 2026-08-18)

This repository is archived and read-only. It was the first dkod product: a
git-native "flight recorder" that captured AI agent sessions into custom git
refs (`refs/dkod/sessions/<id>`). That direction was retired on 2026-08-18.

**Active dkod work lives in [`dkod-io/dkod-signals`](https://github.com/dkod-io/dkod-signals)** —
a single static Rust binary IT admins run once per device (macOS/Linux/Windows,
via MDM) that inventories AI-built apps, computes metrics-only signals per app
(stack, git posture, secrets, auth, exposure, data, activity), scores risk and
`dkod_fit`, and writes one privacy-safe JSON report. Aggregation/dashboard is a
separate repo. First release: v0.1.0.

- Spec: `dkod-signals/docs/superpowers/specs/2026-08-18-dkod-signals-v1-design.md`
- Plan: `dkod-signals/docs/superpowers/plans/2026-08-18-dkod-signals-v1.md`
- The larger product this feeds (governed build pipeline): "DKOD · Slice 1 — The
  Digest Vertical" design spec (private artifact).

## What remains useful here

- `crates/dkod-core/src/capture/*` — knowledge of where each AI coding tool keeps
  local session state (Claude Code, Codex, Cursor, Gemini CLI, Copilot CLI,
  opencode, Factory). This informed dkod-signals' discovery layer.
- `crates/dkod-core/src/redact.rs` and `docs/redaction.md` — secret-shape and
  entropy rules, ported into dkod-signals' `rules/secrets.toml`.
- `docs/plans/` — the pivot design, strategy v2, and implementation history.

Do not add features here. Open issues and PRs against dkod-signals instead.

## Git identity (unchanged, if you must touch this repo)

Commit and push as `haim-ari <haimari1@gmail.com>` (author AND committer):

```sh
git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "..."
```
