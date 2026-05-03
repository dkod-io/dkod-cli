# dkod Pivot — Design

**Date:** 2026-05-03
**Status:** approved, ready for implementation planning

## TL;DR

dkod is pivoting 180° away from the agent-native parallel-execution platform. The new product is a team layer for AI-coded git history. A free, MIT-licensed CLI captures every agent session into the developer's git repo as a custom git ref. A hosted indexer service authenticates as a GitHub App, federates across an org's repos, and serves a search dashboard so teams can find every agent session across every repo without the transcripts ever leaving their git host.

The free CLI and desktop viewer drive adoption. The hosted team dashboard is the business.

## Why pivot

The previous bet — multi-agent parallel execution with AST-level merging — is heavy infrastructure with an unclear adoption curve. The new bet trades infrastructure depth for a simpler, agent-agnostic value prop that fits any team already using AI coding tools, regardless of which one. It also keeps a defensible privacy story (git is the source of truth; transcripts never leave the customer's git host) that future Cursor/Copilot/etc. team features cannot easily match.

## Wedge

**Public sentence:** *Every agent session, every commit, every repo — searchable across your whole org, without the transcripts ever leaving your git host.*

The free CLI + desktop app is solo-dev parity. The wedge is the team dashboard: cross-repo session search, org-wide views, and (V1.5) PR-review surfaces that attach the originating session to a PR.

## Architecture

Four pieces, owned by four repos.

```
┌──────────────────────────────────────────────────────────────┐
│                       Developer's machine                     │
│                                                                │
│   Agent (Claude Code / Codex / …)                              │
│       │                                                        │
│       │  hooks / wrappers / transcript files                   │
│       ▼                                                        │
│   ┌──────────────┐    git push       ┌───────────────────┐    │
│   │   dkod CLI   │ ───────────────►  │  GitHub / GitLab  │    │
│   │  (Rust)      │                   │   refs/dkod/...   │    │
│   └──────┬───────┘                   └─────────┬─────────┘    │
│          │ local read                          │              │
│          ▼                                     │              │
│   ┌──────────────┐                             │              │
│   │  dkod App    │                             │              │
│   │  (Tauri)     │                             │              │
│   └──────────────┘                             │              │
└────────────────────────────────────────────────┼──────────────┘
                                                 │
                            GitHub App (read-only)│
                                                 ▼
                                     ┌──────────────────────┐
                                     │   dkod Indexer        │
                                     │  (Rust)               │
                                     │  ─ pulls session refs │
                                     │  ─ indexes metadata   │
                                     │  ─ serves dashboard   │
                                     └──────────┬────────────┘
                                                │
                                                ▼
                                     ┌──────────────────────┐
                                     │   dkod-web            │
                                     │  landing + docs +     │
                                     │  team dashboard UI    │
                                     └──────────────────────┘
```

### Data flow

1. **Capture.** dkod CLI sits between the dev and their agent. When the agent finishes a turn (or the dev commits), the CLI grabs the transcript + the diff and writes both as a git object under `refs/dkod/sessions/<id>`. Object stays inside the repo.
2. **Push.** When the dev pushes the branch, the CLI also pushes the session ref. Standard git, no special protocol.
3. **Solo browse.** dkod App reads session refs locally via gitoxide and renders a timeline + replay UI. Zero network, zero account needed.
4. **Team browse.** Org admin installs the dkod GitHub App on selected repos. Indexer subscribes to push webhooks, pulls new session refs, builds a search index. Dashboard queries the indexer.
5. **Trust boundary.** The indexer persists metadata + embeddings. When a user opens a session in the dashboard, the indexer fetches the full content from GitHub on demand using the user's OAuth token — reads honor GitHub's permission model, transcripts are never persisted.

### Architectural commitments

- Git is the source of truth. Always.
- The indexer is a cache + search index, not a database of record. If the indexer is lost, customers lose nothing — re-index from git.
- The desktop app works fully offline against any single repo. No auth, no team plan required.
- The team plan only adds: cross-repo search, org analytics, shared dashboards, eventual PR integrations.

## Components

### dkod CLI (`dkod-cli`)

**Stack:** Rust, gitoxide for any git operation. Single static binary. MIT licensed. Distribution: `curl install.sh`, Homebrew, `cargo install`.

**Surface — four commands.**

```
dkod init                  # Add dkod to a repo: install agent hook + .dkod/config
dkod capture <agent>       # Wrap an agent invocation; capture transcript + diff
dkod log                   # List sessions in this repo (like `git log` for sessions)
dkod show <session-id>     # Print a session's prompt, response, and diff
```

`dkod push` and `dkod pull` are not separate commands — sessions ride on normal `git push` / `git fetch` because they live in refs.

**Capture, V1 adapters:**

| Agent | Capture path |
|---|---|
| Claude Code | Plugin / hook that streams the SDK's NDJSON transcript to dkod over a UNIX socket. |
| Codex (OpenAI CLI) | Wrapper: `dkod capture codex -- <args>` execs codex, tees stdout/stderr, parses transcript. |

Cursor, Copilot CLI, Gemini CLI, OpenCode, FactoryAI ship in V1.1+ as adapter PRs.

**Storage layout inside the repo:**

```
refs/dkod/sessions/<session-id>            blob (compressed JSON: transcript + metadata)
refs/dkod/commits/<commit-sha>             ref pointing to the session id that produced it
.dkod/config.toml                          repo config (which agents to capture, redaction rules)
```

Sessions are git **blobs** under custom refs, not commits, so they don't appear in `git log` and don't bloat the working tree. Linking to a commit is a separate ref so a session can map to multiple commits and vice versa.

**Redaction (default ON).** `.dkod/config.toml` lets the repo owner specify regex patterns to strip from transcripts before storage. Runs at capture time, before the blob is written. V1 builtin ruleset:

- AWS access keys
- GitHub tokens
- OpenAI keys
- Stripe keys
- Generic `KEY=value` env-style assignments

```toml
[redact]
enabled = true                                 # default
patterns = ["builtin:aws", "builtin:github_token",
            "builtin:openai_key", "builtin:stripe",
            "builtin:env_assignment"]
custom = []
```

Redaction-on-by-default is the right call: the failure mode of leaking a credential into git history is irreversible; the failure mode of an over-eager redaction is annoying but fixable.

**Auth:** none. CLI is fully local. The user's existing `git` credentials handle push.

### dkod App (`dkod-app`)

**Stack:** Tauri (Rust core + web frontend). Cargo-workspace member alongside `dkod-cli`, sharing a `dkod-core` crate.

**What it is:** local-only, zero-account viewer. Opens against any folder that's a git repo with `refs/dkod/*` in it.

**Three primary views:**

1. **Repo timeline** — vertical timeline of sessions, newest first. Per row: timestamp, agent name + icon, one-line prompt summary, commit sha produced, files-touched count. Click → session view.
2. **Session view** — three-pane: left = chat transcript (markdown rendered, tool calls collapsed by default), middle = the diff this session produced, right = metadata. "Replay" button steps through the transcript message-by-message synced with the diff growing as you scrub.
3. **Search** — local fuzzy search across this repo's sessions: by prompt text, file path touched, agent, date range. (Cross-repo search lives in the web dashboard.)

**Secondary surfaces, V1:**

- "Open in dkod" deep link from web dashboard → desktop app jumps to a specific session.
- Drag-drop or "open repo" — that's the entire onboarding.

**Explicitly NOT in V1:**

- Launching agents from the app. The app is a *viewer/browser*, not an agent host. Agents run in the user's terminal/IDE; dkod App reads what they wrote.
- Editing files. Read-only.
- Multi-repo project workspaces. One repo per window for V1.

This reframe — viewer not host — cuts ~80% of the prior dkod-app surface.

### dkod Indexer (`dkod-indexer`)

**Stack:** Rust (axum + sqlx). Postgres for metadata. Search via pg full-text to start; graduate to Meilisearch when search volume justifies it. S3-compatible blob store for *ephemeral* session-content cache only — never permanent storage.

**Why Rust:** shares the `dkod-core` crate (session schema, ref layout, redaction, gitoxide helpers) cleanly with CLI/app. One source of truth for the data model.

**What it does:**

1. **GitHub App.** Installed by an org admin on N selected repos. Receives `push` webhooks. Authenticates as the installation to fetch refs.
2. **Ingest loop.** On webhook: fetch new objects under `refs/dkod/*` for that repo. Parse session metadata (agent, author, timestamp, commit links, file paths touched). Write metadata + a content-addressed pointer to Postgres. Generate embeddings for prompt + transcript summary. Drop the full transcript content from cache after embedding — content always re-fetched live from GitHub.
3. **Query API.** REST + WebSocket for the dashboard. Endpoints: search sessions, list per repo / per author / per date, fetch a single session (proxies through to GitHub using the *user's* OAuth token).
4. **Auth.** Sign in with GitHub OAuth on dkod-web. Session tokens scoped to the orgs/repos the GitHub App is installed on.

**Privacy commitment (load-bearing claim):**

- We persist: metadata (session id, repo, author, timestamp, file paths touched, commit shas), prompt summary, embeddings.
- We do **not** persist: full transcripts, full diffs, secret values. Cached during ingest only, evicted after embedding.
- Reading session content always goes through the user's GitHub token — same access model as `git clone`.

**Self-host path (V2 / enterprise):** the same Rust binary runs in customer infra against GitHub Enterprise. Same code, different deploy target. Built into the design from day one even if not shipped V1.

### dkod-web

**Stack:** keep the existing dkod-web stack unless friction surfaces during implementation. Single repo with three jobs:

1. **Landing** — `dkod.io` homepage, replaces existing copy.
2. **Docs** — install, capture commands, adapter list, redaction config, GitHub App setup, privacy model.
3. **Team dashboard** (gated behind GitHub OAuth) — cross-repo session search, per-author/per-repo views, session detail proxied through GitHub.

One deploy, shared design system. Splitting landing and dashboard buys nothing for V1.

## Repos

| Repo | Purpose | Lang | Status |
|---|---|---|---|
| `dkod-cli` | The CLI binary | Rust + gitoxide | **NEW** |
| `dkod-app` | Desktop viewer | Tauri | **NEW** (clean repo, name reused after archive) |
| `dkod-indexer` | Hosted indexer service | Rust | **NEW** |
| `dkod-web` | Landing + docs + dashboard | existing stack | **Keep, repurpose** |
| All other `dkod-*` | Old platform | — | **Archive** |

**Workspace layout:** `dkod-cli` and `dkod-app` share a Cargo workspace and a `dkod-core` crate (session schema, ref layout, redaction, gitoxide helpers). The indexer pulls in `dkod-core` for ref parsing too.

**Archive list:** `dkod-engine`, `dkod-engine-backup-20260316`, `dkod-engine-epicb`, `dkod-plugin`, `dkod-harness`, `dkod-swarm`, `dkod-demo`, `dkod-demo-swarm`, `dkod-e2e`, `dkod-e2e-testbed`, `dkod-io` (replaced by repurposed `dkod-web`).

## V1 ship list

The smallest cut that tells the full story:

- `dkod-cli`: `init`, `capture`, `log`, `show`. Adapters: Claude Code + Codex.
- `dkod-app`: timeline + session view + local search. Single-repo, read-only.
- `dkod-indexer`: GitHub App, ingest, search API, federated content fetch.
- `dkod-web`: landing + docs + team dashboard with cross-repo search across one org.
- Free tier: CLI + app, forever. Paid: team dashboard ($X / seat / month, price decided later).

## V1.5

- PR check-run that posts the originating session as a review comment.
- Cursor, Copilot CLI, Gemini CLI, OpenCode, FactoryAI adapters.
- Org analytics view (sessions/week, agent mix, top files).

## V2

- Self-host indexer for enterprise (hybrid mode: customer-owned indexer, same binary, GitHub Enterprise).
- Multi-org dashboards.
- GitLab support.

## Open questions deferred to implementation

- Exact pricing for the team plan.
- Whether to use Meilisearch from V1 or start with pg-FTS.
- `dkod-web` framework decision (keep current vs. swap) — decide on first commit.
- Specific GitHub App permission scopes (minimum: contents:read on selected repos).

## Decisions locked

| Decision | Choice |
|---|---|
| Form factor | CLI engine + Tauri desktop IDE on top |
| Wedge | Team layer (cross-repo search, eventual PR review) |
| Architecture | Git-native federated for MVP; hybrid self-host indexer in V2 |
| MVP scope | (b) — cross-repo search across an authorized GitHub org |
| CLI language | Rust + gitoxide |
| Indexer language | Rust |
| Brand | Keep `dkod` |
| Salvage | Archive all `dkod-*` except `dkod-web`; build new repos from scratch |
| V1 adapters | Claude Code + Codex only |
| Redaction default | ON, with builtin ruleset for AWS/GitHub/OpenAI/Stripe/env-assignment |
| App role | Viewer/browser, not agent host |
| External inspirations | Not named publicly anywhere — marketing, docs, READMEs, commits |
