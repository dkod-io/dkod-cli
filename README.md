> **Archived (2026-08-18).** dkod-cli is superseded by [dkod-signals](https://github.com/dkod-io/dkod-signals) — a device scanner that inventories AI-built apps and emits a metrics-only report. This repo is read-only; see `CLAUDE.md` for what remains useful here.

# dkod — the git-native flight recorder for AI coding agents

Transcripts are never stored outside your git host.

![dkod demo](docs/demo/dkod-demo.gif)

dkod captures every AI agent session — prompt, reasoning, tool calls — into
your repo's own git refs (`refs/dkod/sessions/*`). `dkod blame` answers
"which prompt wrote this line?"; `dkod drift` flags sessions where the agent
did materially more than it was asked. It works with 7 agents (Claude Code,
Codex, Copilot CLI, Cursor, Factory droid, Gemini CLI, opencode). MIT
licensed, single static binary, zero network at capture time.

## Install

```sh
brew install dkod-io/tap/dkod
```

```sh
curl -fsSL https://raw.githubusercontent.com/dkod-io/dkod-cli/main/install.sh | sh
```

```sh
cargo install --git https://github.com/dkod-io/dkod-cli dkod-cli
```

## Quickstart

```sh
dkod setup        # wizard: detects installed agents, wires capture everywhere
# — or, for a single repo —
dkod init         # wire capture in the current repo
```

From then on, sessions accumulate automatically as you use your agents:

```sh
dkod log               # list captured sessions in this repo
dkod show <id>         # full transcript of one session
dkod blame <path>      # per line: which session (and prompt) wrote it
dkod drift             # sessions where the agent exceeded its brief
dkod drift <id> --card # shareable boxed card for one session
dkod export agent-trace [<id>]  # sessions as Agent Trace records (docs/agent-trace.md)
```

### Import existing transcripts

Claude Code deletes local transcripts after ~30 days. Rescue them (and your
Codex rollouts) into permanent git refs — works in any git repo, no setup
needed, redacted like live captures, idempotent to re-run:

```sh
dkod import claude-code   # ~/.claude/projects/<this repo>/*.jsonl
dkod import codex         # ~/.codex/sessions rollouts recorded in this repo
```

`dkod init` also writes a `.dkod.toml` breadcrumb at the repo root, so
teammates browsing the repo learn the session history exists. After cloning,
they run `dkod init` once — it detects existing sessions on `origin` and
fetches the history.

## Privacy & redaction

Sessions live in **your** repo as git refs and travel with normal git
push/fetch. The optional hosted team layer persists metadata only and fetches
content on demand under your own token.

Capture-time secret redaction is **on by default**: known key formats,
`ENV=value` assignments, an entropy rule for generic credentials, plus your
own custom patterns. Every replacement is tallied in a per-session audit
count shown by `dkod show`. Redaction is best-effort, not a guarantee — see
[`docs/redaction.md`](docs/redaction.md) for the full rule set and
limitations.

## Supported agents

| Agent | Captured via |
| --- | --- |
| Claude Code | Hooks (`~/.claude/settings.json`) |
| Codex | PATH shim wrapper (consent-gated — no hook API) |
| Copilot CLI | Hooks (`~/.copilot/hooks/`) |
| Cursor | Hooks (`.cursor/hooks.json`) |
| Factory droid | Hooks (`~/.factory/settings.json`) |
| Gemini CLI | Hooks (`~/.gemini/settings.json`) |
| opencode | Hooks (`opencode.json`) |

Every agent can also be wrapped explicitly:
`dkod capture <agent> -- <args>`.

## How it works

Each session is stored as a blob under `refs/dkod/sessions/<id>`; the commits
it produced are linked via `refs/dkod/commits/<sha>`. Custom refs stay out of
`git branch -a` and your normal workflow. A post-rewrite hook re-links
sessions after rebases, with a patch-id fallback for rewrites the hook can't
see — so `dkod blame` survives history rewrites.

Design docs: [pivot design](docs/plans/2026-05-03-dkod-pivot-design.md) ·
[V1 implementation plan](docs/plans/2026-05-03-dkod-cli-v1-implementation.md).
(Regenerate the demo GIF with `vhs docs/demo/blame.tape`.)

---

MIT licensed · [dkod.io](https://dkod.io) · Built in the open — issues and
PRs welcome.
