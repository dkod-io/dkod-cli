# dkod-cli

The first product repo for the dkod project pivot. Captures every AI agent session
(Claude Code, Codex) into custom git refs (`refs/dkod/sessions/<id>`) inside the
user's repo. Pure Rust, gitoxide-backed, MIT licensed, distributed via cargo / curl
install.sh / GitHub Releases.

Sibling repos:
- `dkod-app` — Tauri viewer (planned, not yet built)
- `dkod-indexer` — hosted federated team-layer indexer / search (planned — this is
  the revenue piece; see `docs/plans/2026-05-03-dkod-pivot-design.md` Section "dkod
  Indexer")
- `dkod-web` — landing + docs + dashboard (existing repo, repurpose pending)

The full design lives in `docs/plans/2026-05-03-dkod-pivot-design.md`. The V1
implementation plan lives in `docs/plans/2026-05-03-dkod-cli-v1-implementation.md`.

## Git identity (CRITICAL — every commit, every push, no exceptions)

Every commit AND push, in this repo and any sibling dkod-* repo, must be authored
AND committed by `haim-ari <haimari1@gmail.com>`. This applies to the main
session, every dispatched subagent, every fix loop, every `--amend`, and every
release. NEVER:

- Use `--author` alone (sets author but not committer; GitHub displays the committer)
- Add `Co-Authored-By:` lines (Claude, Anthropic, agent IDs, model attribution — none of these)
- Commit as a different email (e.g. work emails — known footgun)

Apply explicitly on the command line so subagents can't inherit the wrong default:

```sh
git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit -m "..."
```

When dispatching a subagent that will commit, paste this rule verbatim in the
prompt's hard-rules section.

## Code review (CodeRabbit — always-on, no exceptions for non-trivial commits)

Use the **CodeRabbit Claude Code plugin** (`/coderabbit:review`), NOT the raw
`coderabbit` / `cr` CLI. The plugin auto-selects the right output mode — do NOT
pass `--agent` / `--plain` / `--interactive`. Plugin install: `/plugin install
coderabbit`. Auth: one-time `coderabbit auth login` in the terminal.

**Run CodeRabbit at every commit boundary:**

1. **Before commit** — `/coderabbit:review uncommitted` on staged + working-tree
   changes. Resolve findings before committing. Applies to chore, fix, feat,
   docs, config — every commit type. Only literal one-line typo fixes are
   exempt; even then, prefer running it.
2. **After commit** — `/coderabbit:review committed` on the just-landed commit.
   Catches things the pre-commit pass missed.
3. **Before opening any PR** — `/coderabbit:review --base main` on the full
   branch diff. Never open a PR with an unreviewed branch.
4. **After PR opens** — wait for CodeRabbit's server-side review, fix every
   actionable finding, push, wait for re-review, repeat until clean. Do NOT
   merge with open findings.

When dispatching subagents that commit code, paste the pre-commit step verbatim
in the prompt's hard-rules section.

**Scope caveat:** CodeRabbit reviews code, not docs or config. For commits that
contain ONLY `.md` / `.yaml` / `.toml` / `.json`, the review returns 0 findings
because it effectively skipped — do NOT claim "reviewed clean." For mixed
code+config commits (common), still run it.

## Tooling

- **Build:** `cargo build` / `cargo test` / `cargo clippy --all-targets -- -D warnings` / `cargo fmt --all -- --check`. Pinned toolchain: `rust-toolchain.toml` says `stable`.
- **Install path test (private repo):** the install.sh script reads `$GH_TOKEN`. For testing locally, store the token in a 600-mode file (e.g. `/tmp/test-pat`) and pass via `GH_TOKEN=$(cat /tmp/test-pat)`. Never paste tokens in chat.
- **Releases:** `git tag -a v<x.y.z> -m "..."; git push origin v<x.y.z>` triggers `release.yml`. Default `prerelease: false`. Promotion via `gh release edit` is no longer required.

## Open follow-up issues (V1.5)

- #4 — Bump GitHub Actions to Node 24 before 2026-09-16
- #5 — `dkod init` should write `refs/dkod/*` fetch refspec to `.git/config`
- #6 — Always-on Claude Code capture (lazy-spawn server from hook)
