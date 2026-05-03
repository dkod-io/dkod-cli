# Claude Code capture protocol

**Date:** 2026-05-03
**Status:** input to Tasks 18 (socket server) and 19 (hook + CLI wiring)

## TL;DR

Claude Code is **friendlier to capture than Codex** in two ways: it
writes a richer per-session JSONL transcript to a well-defined,
per-cwd path on disk, and it has a first-class **hooks system** (15+
event types) that can synchronously call out to a subprocess and
hand it the path to that transcript. We exploit both: V1 capture is
**strategy (D), hybrid** — a `Stop` hook wakes a long-lived `dkod`
**UNIX socket server** spawned by `dkod capture claude-code …`,
hands it the `transcript_path`, and the server reads + parses the
JSONL to build the canonical `Session`. Live `UserPromptSubmit` /
`PostToolUse` events flow over the same socket as **NDJSON** so the
server can show progress and detect orphaned sessions, but they are
**not the source of truth** — the JSONL is. The socket lives at
`$XDG_RUNTIME_DIR/dkod/<repo_hash>.sock` (Linux) /
`$TMPDIR/dkod-<repo_hash>.sock` (macOS), 0600, and is created when
`dkod capture claude-code` starts and removed on graceful shutdown.

This deliberately mirrors the Codex adapter shape (live signal +
durable on-disk transcript), but flips which side is primary: for
Codex the rollout is the canonical source and stdout is just spawn-
time signal; for Claude Code the JSONL is canonical and the hook
events are just spawn-time signal. The result is a single
`SessionWriter` interface that both adapters feed.

## Claude Code's transcript surface

### On-disk

**Path (officially documented in the SDK sessions guide).** Per session:

```
~/.claude/projects/<encoded-cwd>/<session-id>.jsonl
```

`<encoded-cwd>` is the absolute working directory with **every
non-alphanumeric character replaced by `-`**. So a session run from
`/Users/me/proj` writes to
`~/.claude/projects/-Users-me-proj/<session-id>.jsonl`. Confirmed
locally — sample directory listing:

```
~/.claude/projects/-Users-haimari-vsCode-haim-ari-github-dkod-swarm/
  6c533ba6-5970-4653-beb4-e80d5406f7e4.jsonl   (1381 lines)
  b19f83d7-1380-4945-b5c4-4fb58abb88b1.jsonl
  …
```

This **is the same file** that the hook input field `transcript_path`
points at — Claude Code passes the JSONL path to every hook
script on stdin, so hooks can read the live, in-progress transcript.

**Format.** Newline-delimited JSON. One event per line. Each line
has a top-level `type` field. Distinct top-level types observed in
real sessions (with frequencies from a 1381-line session):

| `type`                  | Count | Purpose                                                                               |
|-------------------------|-------|---------------------------------------------------------------------------------------|
| `assistant`             | 335   | Assistant turn — wraps an Anthropic-API-style `message` with `content` blocks         |
| `user`                  | 236   | User turn — same shape; `content` may be a `string` or a list of typed blocks         |
| `attachment`            | 437   | Sidechain context: hook output, skill listings, deferred tools, diagnostics, etc.     |
| `system`                | 68    | System events (`subtype: "local_command"`, `level`, `cwd`, `version`, `gitBranch`)   |
| `file-history-snapshot` | 40    | Tracked-file backup pointers used by `--checkpointing`                                |
| `custom-title`          | 53    | Session display name (`customTitle`)                                                  |
| `agent-name`            | 53    | Agent name (`agentName`)                                                              |
| `permission-mode`       | 52    | Current permission mode                                                               |
| `last-prompt`           | 50    | Pointer to the leaf UUID of the last user prompt                                      |
| `pr-link`               | 45    | Linked PR (`prNumber`, `prUrl`, `prRepository`)                                       |
| `queue-operation`       | 12    | Prompt-queue housekeeping                                                             |

Every `user` / `assistant` line carries:

- `uuid`, `parentUuid` — DAG pointers (one session is a tree, not a list — fork/branching is encoded here)
- `sessionId`, `timestamp`, `cwd`, `version`, `gitBranch`
- `isSidechain` (subagent traffic), `userType`, `entrypoint`
- `message.content[]` — Anthropic message format. Content blocks observed:

| Inner `type`   | Carries                                                                              |
|----------------|--------------------------------------------------------------------------------------|
| `text`         | `text` — assistant or user free text                                                 |
| `thinking`     | `thinking`, `signature` — extended-thinking summary (assistant only)                |
| `tool_use`     | `id`, `name`, `input`, `caller` — tool call (assistant only)                         |
| `tool_result`  | `tool_use_id`, `content` — tool output (user only, generated by Claude Code itself)  |
| `image`        | `source` — image attached by the user                                                |

Plus — on `assistant` lines — a `message.usage` block with
`input_tokens / output_tokens / cache_creation_input_tokens /
cache_read_input_tokens / server_tool_use / cache_creation /
service_tier`. The very same fields the Anthropic API returns;
adopting them is straightforward.

Concrete examples (sanitized, taken from the dkod-swarm session):

```jsonl
{"type":"assistant","uuid":"4fc1…","parentUuid":"e7a2…","sessionId":"6c53…","timestamp":"2026-04-28T17:31:35.831Z","cwd":"/Users/haimari/vsCode/haim-ari/github/dkod-swarm","message":{"model":"claude-opus-4-7","role":"assistant","content":[{"type":"thinking","thinking":"…","signature":"…"}],"usage":{"input_tokens":6,"cache_read_input_tokens":0,"output_tokens":5811,…}}}
{"type":"assistant","uuid":"8132…","parentUuid":"3402…","sessionId":"6c53…","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_01LGbfxdS6weJ22F14Jj6HAx","name":"Read","input":{"file_path":"/Users/haimari/vsCode/haim-ari/github/dkod-swarm/docs/design.md"},"caller":{"type":"direct"}}]}}
{"type":"user","uuid":"807c…","parentUuid":"8132…","sessionId":"6c53…","message":{"role":"user","content":[{"tool_use_id":"toolu_01LGbfxdS6weJ22F14Jj6HAx","type":"tool_result","content":"1\t# Design: dkod-swarm\n2\t…"}]}}
```

**Operational notes.**

- Path is **deterministic** from `(cwd, session_id)`. A capture
  process that knows the session ID can locate the file without
  hooks.
- File is **append-only during the session** and remains on disk
  after — same persistence model as Codex rollouts. SDK exposes
  `listSessions()` / `getSessionMessages()` so it's a public surface.
- Across machines: file is **local-only**. The session-resume guide
  spells this out: to resume on a different host you have to ship
  the file. We can rely on it being there on the same machine that
  ran the session.
- `parentUuid` makes the transcript a tree, not a list. Subagent
  traffic carries `isSidechain: true` — useful for filtering or
  scoping later, irrelevant to V1 (we capture the whole tree as
  one `Session`).

### Stdout (SDK headless mode)

`claude -p / --print` runs non-interactively. Three output formats:

- `text` (default) — plain final assistant text, nothing else.
- `json` — single JSON blob with `result`, `session_id`, usage
  metadata, and (with `--json-schema`) `structured_output`.
- `stream-json` — newline-delimited JSON, one event per line.

Stream events documented in
`https://code.claude.com/docs/en/headless.md`:

- **`system` / subtype `init`** — first event in the stream (unless
  `CLAUDE_CODE_SYNC_PLUGIN_INSTALL` is set, in which case
  `system/plugin_install` events precede). Carries `session_id`,
  the model, the loaded tools, MCP servers, and `plugins` /
  `plugin_errors`.
- **`system` / subtype `api_retry`** — emitted on retryable API
  errors. Fields: `attempt`, `max_retries`, `retry_delay_ms`,
  `error_status`, `error` (one of
  `authentication_failed | oauth_org_not_allowed | billing_error |
  rate_limit | invalid_request | server_error | max_output_tokens |
  unknown`), `uuid`, `session_id`.
- **`system` / subtype `plugin_install`** — when
  `CLAUDE_CODE_SYNC_PLUGIN_INSTALL` is set. Status transitions
  `started` → (per marketplace `installed` / `failed`) → `completed`.
- **`assistant`** — assistant message. Same Anthropic-shaped
  `message` body the JSONL uses.
- **`user`** — user message (including tool results). Same shape.
- **`result`** — terminal event. Carries `result` (final assistant
  text), `session_id`, `subtype` (`success` / `error_max_turns` /
  `error_max_budget_usd` / etc.), `total_cost_usd`, `usage`.
- **`stream_event`** — present only with
  `--include-partial-messages`. Carries Anthropic API SSE deltas
  (`message_start`, `content_block_start`, `content_block_delta`
  with `delta.type ∈ {text_delta, thinking_delta, …}`,
  `content_block_stop`, `message_delta`, `message_stop`).
- **hook lifecycle events** — present only with
  `--include-hook-events` (we are not relying on this in V1; the
  shape is the hook-input JSON wrapped in an envelope).

`--input-format=stream-json` lets the caller stream user messages
**in** as well, which matters if we ever want to drive Claude Code
from inside dkod (we don't, in V1).

`session_id` appears on **`system/init`**, **`result`**, and the
retry / plugin-install system events. Capture from `system/init`,
verify on `result`.

### Hook events

Documented at `https://code.claude.com/docs/en/hooks`. Every hook
input is a single-line JSON object on the hook's stdin, and hooks
**always** receive the lifecycle-common fields:

```
session_id        string  — Claude Code session UUID
transcript_path   string  — path to the on-disk JSONL (see above)
cwd               string  — current working directory
hook_event_name   string  — the lifecycle event name (e.g. "Stop")
permission_mode   string  — for tool / prompt events
```

Plus event-specific fields. Full inventory (V1-relevant
highlighted):

| Hook                 | Match by                    | Fires on                                                    |
|----------------------|-----------------------------|-------------------------------------------------------------|
| **`SessionStart`**   | `startup`/`resume`/`clear`/`compact` | New session begins, including `--resume` / `--continue` / `/resume` / `/clear` |
| `Setup`              | `init`/`maintenance`        | `--init-only` / `--init` / `--maintenance` invocations      |
| **`SessionEnd`**     | reason (`clear`/`resume`/`logout`/`prompt_input_exit`/…)| Session terminates                                          |
| **`UserPromptSubmit`** | (no matcher)              | Before Claude processes a user prompt; carries `prompt`     |
| `UserPromptExpansion`| `command_name`              | A typed slash command expands before reaching Claude        |
| **`PreToolUse`**     | `tool_name` (regex / `\|`)  | Tool params built, before tool runs; carries `tool_input`, `tool_use_id` |
| **`PostToolUse`**    | `tool_name`                 | Tool succeeded; carries `tool_input`, `tool_result`, `tool_use_id` |
| `PostToolUseFailure` | `tool_name`                 | Tool failed; carries `error`                                |
| `PostToolBatch`      | (no matcher)                | After a parallel batch resolves                             |
| `PermissionRequest`  | `tool_name`                 | Permission dialog about to show                             |
| `PermissionDenied`   | `tool_name`                 | Auto-mode classifier denied                                 |
| **`Stop`**           | (no matcher)                | Claude finishes responding (turn boundary)                  |
| `StopFailure`        | `error_type`                | Turn ended due to API error                                 |
| `SubagentStart`      | `agent_type`                | Subagent spawned                                            |
| `SubagentStop`       | `agent_type`                | Subagent finished                                           |
| `TeammateIdle`       | (no matcher)                | Agent-team teammate going idle                              |
| `TaskCreated`        | (no matcher)                | `TaskCreate` invoked                                        |
| `TaskCompleted`      | (no matcher)                | A task marked completed                                     |
| `Notification`       | `notification_type`         | Permission prompt / idle prompt / auth success / elicitation |
| `CwdChanged`         | (no matcher)                | `cd` happened — has `CLAUDE_ENV_FILE`                       |
| `FileChanged`        | filename glob               | Watched file changed                                        |
| `InstructionsLoaded` | `load_reason`               | `CLAUDE.md` / rules file loaded                             |
| `ConfigChange`       | settings source             | Settings file changed during session                        |
| **`PreCompact`**     | `manual`/`auto`             | Before context compaction                                   |
| `PostCompact`        | `manual`/`auto`             | After context compaction                                    |
| `WorktreeCreate`     | (no matcher)                | `--worktree` worktree being created                         |
| `WorktreeRemove`     | (no matcher)                | Worktree being removed                                      |
| `Elicitation` / `ElicitationResult` | MCP server name | MCP elicitation request / response                  |

For each event, the hook script's exit code and stdout JSON can
return decision control (`decision: "block"`, `permissionDecision`,
`continue: false`, `stopReason`, `additionalContext`,
`hookSpecificOutput.*`). **For dkod V1 we do not block, deny, or
inject context** — every hook script we install exits 0 with empty
stdout. The hook is for **observation only**.

Hooks are configured under `hooks: { <Event>: [{ matcher, hooks:
[{ type, command, timeout, async, … }] }] }` in
`~/.claude/settings.json` (user), `.claude/settings.json` (project),
or `.claude/settings.local.json` (local / gitignored). Handler
types include `command`, `http`, `mcp_tool`, `prompt`, and `agent`
— we use `command` exclusively in V1.

## Chosen capture strategy

**Strategy (D), hybrid: hooks + on-disk transcript, with the
transcript as the source of truth.**

The hooks fire and tell our long-lived socket server "this session
just did X, here's its `transcript_path`". The server uses the live
events for progress / orphan detection / wakeup (so `dkod show`
isn't dead until `Stop`). On `SessionEnd` (preferred) or `Stop` (if
`SessionEnd` doesn't fire — see open questions), the server reads
the JSONL at `transcript_path` end-to-end and converts it to the
canonical `Session`. The JSONL is what we trust; the live events
are scaffolding.

Why not (A) JSONL-only after-the-fact? It works — `transcript_path`
is fully deterministic given `(cwd, session_id)` — but we'd need
some way to know **when** the session ended and **which session**
to capture (the user could have multiple concurrent sessions in
the same cwd). Without hooks we'd have to inotify-watch every
project dir and heuristically decide a session is "done", which is
the kind of guess that breaks silently.

Why not (B) hook-only NDJSON? The hook stream isn't transactional
— hooks can be dropped (timeout, exec error, settings disable),
and the schema is denser than what's in the JSONL (e.g.
`PostToolUse` doesn't include the assistant's full reasoning
between turns). The JSONL is the canonical artifact; rebuilding
from a hook stream would mean reverse-engineering schema details
Anthropic explicitly publishes elsewhere.

Why not (C) wrap `claude -p` with `stream-json`? It captures
exactly one invocation. It does **not** capture the user's
ordinary interactive `claude` sessions, which is the V1 target.
Wrap-`-p`-with-stream-json is the right design for a future
"`dkod capture claude-code -- claude -p '…'`" headless mode (worth
shipping eventually as a parallel adapter, mirroring how Codex
exposes `codex exec --json`), but not the primary V1 path.

Trade-offs / what we'd need to add later:

- We don't yet capture sessions launched by other users on the
  same machine — V1 reads `~/.claude/projects/...` resolved against
  the dkod process's `$HOME`.
- Multi-cwd one-server-per-cwd is V1; cross-cwd capture (one server
  per machine) is post-V1.
- If the user has `disableAllHooks: true` in their settings, our
  capture goes dark. We must detect this on `dkod capture
  claude-code` startup and fail loudly.

## Wire protocol

### Transport

**Path (per repo / per cwd):**

- macOS: `${TMPDIR:-/tmp}/dkod-<repo_hash>.sock`
- Linux: `${XDG_RUNTIME_DIR:-/tmp}/dkod/<repo_hash>.sock`

`<repo_hash>` is `sha256(canonical_repo_root)[..12]` — same scheme
the rest of dkod uses for repo IDs. One server per repo root
keeps the socket scoped, lets us run two captures against two
repos concurrently, and avoids a global-singleton reaper.

**Lifecycle.**

- **Created** by `dkod capture claude-code` on startup. The
  command:
  1. Computes `<repo_hash>` from cwd or `--repo`.
  2. Refuses to start if the socket already exists *and* a peer
     accepts on it (another server running). Stale-socket
     detection: try to `connect()`; on `ECONNREFUSED`, `unlink()`
     and proceed.
  3. `bind()`s, `chmod 0600` on the socket, `listen()`.
  4. Writes the socket path + PID to a small heartbeat file at
     `~/.local/share/dkod/captures/<repo_hash>.json` so we can
     detect orphans next run.
  5. Installs the per-project hook (`.claude/settings.local.json`,
     scoped to this cwd) that runs
     `dkod capture-hook <repo_hash> $hook_event_name` and pipes the
     hook input straight through.
- **Removed** on graceful shutdown:
  - `SIGTERM` / `SIGINT` from `dkod capture claude-code`.
  - The server has been idle (no connected peer, no in-flight
    session) for `--idle-timeout` (default 600s).
  - `dkod capture claude-code stop --repo .`.
  Cleanup deletes the socket file, removes the hook from
  `.claude/settings.local.json` (only entries dkod installed),
  and clears the heartbeat file.
- **Crash recovery.** On startup, if the heartbeat file references
  a PID that's no longer alive, we treat the socket as stale and
  recreate it. Any partially-written sessions (where `Stop` /
  `SessionEnd` arrived but the server was killed mid-flush) get
  recovered on next start by replaying the JSONL transcript at the
  recorded `transcript_path` — same code path as the normal `Stop`
  handler.
- **Ownership.** Mode `0600`, owned by the invoking user. We do
  not allow group or world access.

### Event types

The hook side speaks **NDJSON** to the server: one event per line,
each line a single JSON object, terminated by `\n`. Connection is
short-lived (one request = one or many events = bye). We are not
keeping persistent connections from the hook script; `claude`
spawns a fresh process per hook firing, so a connection-per-event
model fits cleanly.

Common envelope fields on every event:

```jsonc
{
  "v":          1,                  // protocol version
  "kind":       "...",              // event kind, see table below
  "session_id": "uuid-v4",          // Claude Code session UUID (from hook input)
  "ts":         "2026-05-03T15:00:00.000Z", // event timestamp (server may overwrite with recv-time if missing)
  "cwd":        "/Users/.../proj"   // from hook input, kept for sanity-check
}
```

Per-kind extra fields:

| `kind`              | Extra fields                                                                                          | Hook source              | Maps to                                                                 |
|---------------------|------------------------------------------------------------------------------------------------------|--------------------------|-------------------------------------------------------------------------|
| `session_start`     | `transcript_path`, `model`, `agent_type?`, `source` (`startup`/`resume`/`clear`/`compact`)           | `SessionStart`           | starts a `SessionWriter`; provider = `claude-code`                       |
| `prompt_submitted`  | `prompt` (string), `permission_mode`                                                                 | `UserPromptSubmit`       | progress signal only (we'll get the full message from the JSONL)        |
| `tool_start`        | `tool_name`, `tool_input` (truncated to 4 KiB), `tool_use_id`                                        | `PreToolUse`             | progress signal; eventually maps to `Message::tool_call.start_at`       |
| `tool_end`          | `tool_name`, `tool_use_id`, `status` (`success` / `failure`), `duration_ms`, `error?`                | `PostToolUse` + `PostToolUseFailure` | progress signal; eventually maps to `Message::tool_call.end_at`         |
| `pre_compact`       | `trigger` (`manual` / `auto`)                                                                        | `PreCompact`             | annotation in `Session.metadata`                                        |
| `turn_stop`         | (no extras)                                                                                           | `Stop`                   | flush-trigger; if `SessionEnd` doesn't fire we treat this as the end    |
| `session_end`       | `reason` (`clear` / `resume` / `logout` / `prompt_input_exit` / `bypass_permissions_disabled` / `other`), `transcript_path` | `SessionEnd`             | **canonical end** — server reads JSONL, builds `Session`, writes blob   |

The hook script's job is mechanical: read the JSON on stdin from
Claude Code, project a few well-known fields into the dkod envelope,
truncate `tool_input` if huge, write one line to the socket, exit 0.
Everything else (parsing the message tree, reasoning, attachments,
file edits, tokens) the server reads from the JSONL.

`tool_input` truncation matters: hooks can fire on huge `Write`
payloads. The size cap keeps the socket buffer bounded; the JSONL
has the un-truncated content so we lose nothing.

We deliberately do **not** stream `assistant_message` / `reasoning`
events over the socket. Those are large, ordering-sensitive, and
already live in the JSONL. Keeping the wire protocol small means
the protocol surface stays stable — adding a new event later is
backwards-compatible (server ignores unknown `kind`s).

Server → hook: the hook script reads zero bytes back. Exit code 0
always. We do not implement an ack — if the server is gone, the
hook silently no-ops and Claude Code keeps running. (See "Error
handling" below.)

### Lifecycle (prose sequence)

1. User runs `dkod capture claude-code` in a repo. Server binds the
   socket, installs hooks in `.claude/settings.local.json`, writes
   heartbeat, starts accepting.
2. User runs `claude` in that repo. Claude Code fires the
   `SessionStart` hook with `source=startup`; our hook script
   sends `session_start` to the socket. The server registers a new
   in-flight session keyed by `session_id`, stashing
   `transcript_path`.
3. User types a prompt. `UserPromptSubmit` hook → `prompt_submitted`
   event. Server bumps progress for `dkod show` callers.
4. Claude calls a tool. `PreToolUse` → `tool_start`. Tool returns.
   `PostToolUse` (or `PostToolUseFailure`) → `tool_end`. Server
   keeps a small ring of recent tool events for inspection.
5. Claude finishes the turn. `Stop` → `turn_stop`. Server records
   the boundary but does not flush yet — multi-turn sessions stay
   open.
6. Either:
   - User exits / clears / resumes / logs out → `SessionEnd` →
     `session_end`. Server opens `transcript_path`, parses the
     JSONL, builds a `Session`, writes the blob (same code path
     the Codex adapter uses), removes the in-flight entry.
   - Claude Code is killed without `SessionEnd` (the docs warn
     this can happen — see open questions). On the next `Stop`
     the server starts a `SessionEnd` watchdog: if no further
     hook events arrive for `--orphan-grace` (default 60s) and
     the JSONL hasn't been touched in that window, we treat it
     as ended and flush. This is the analogue of the Codex
     adapter's "rollout file shows up after exec exits" retry.
7. `dkod capture claude-code` server keeps running. Idle eviction
   on `--idle-timeout` flushes any still-open sessions and exits.

### Error handling

The first principle is **never break Claude Code**. The hook script
runs synchronously inside Claude Code's tool loop; if we hang or
crash, the user's session hangs.

Hook script behavior matrix:

| Failure                                  | Hook behavior                                  |
|------------------------------------------|------------------------------------------------|
| `connect(socket)` fails (`ENOENT` / `ECONNREFUSED`) | `exit 0` immediately, log to `/tmp/dkod-hook.log` |
| `write` fails partway                    | best-effort `close()`, `exit 0`                 |
| Malformed hook input on stdin            | `exit 0` (don't gate the user's session on our parser) |
| Server is slow                           | hard 1s `connect()` timeout, 1s write timeout, then `exit 0` |

The hook script will be a tiny static Rust binary (~500 LOC of
unwrap-free, no-deps code) shipped from the same workspace as
`dkod`. We call it `dkod capture-hook` — `claude` invokes
`dkod capture-hook <repo_hash> <hook_event_name>`. (Implementation
detail for Task 19.)

Server-side error handling:

- Unknown `kind` → log + ignore.
- `session_id` that doesn't match any in-flight session → log,
  start a new in-flight entry tentatively (recover late
  `session_start`).
- JSONL parse error on flush → write what we have, attach an
  `error` annotation in `Session.metadata`, keep going.
- Disk-write error on the final blob → retry with backoff; failing
  that, leave a `.dkod-pending/<session_id>.json` for the next
  startup to pick up.

User-facing errors (exit code, log line) only on fatal startup
issues (can't bind socket, can't write hook config). Once running,
the server is silent unless `--debug`.

## What's NOT in V1

- **Multi-cwd capture.** One server per repo / cwd. Running
  `claude` in a cwd with no live `dkod capture claude-code` simply
  doesn't get captured. (We could lift this by binding a
  user-global socket and routing by `cwd`; deferred.)
- **Cross-machine capture / resume.** The JSONL is a local file.
  Capturing a session that was started on machine A and resumed on
  machine B is a `SessionStore` problem we don't solve in V1.
- **Resuming a partial capture after a server crash mid-session.**
  We recover *finished* sessions whose `Stop` / `SessionEnd` events
  arrived before the crash (heartbeat replay). For sessions that
  were still open we make a best-effort flush of whatever JSONL is
  on disk; if the user later resumes that session and continues,
  we'll capture only the post-resume slice as a new session
  (acceptable V1 behavior).
- **Capturing the desktop / VS Code IDE Claude Code surfaces.**
  Both write to the same `~/.claude/projects/...` JSONL — but they
  may bypass `~/.claude/settings.json` hooks (TBC). V1 is wrapper-
  only via terminal `claude`; an "import existing JSONL" command
  is a follow-up.
- **Subagent topology preservation.** We ingest sidechain
  (`isSidechain: true`) messages as part of the same `Session` for
  V1. Splitting them into a parent-Session-with-children tree is a
  later structural change.
- **The headless `claude -p` wrapper mode** (analogous to Codex's
  `codex exec --json`) is straightforward to add later and does
  not need a socket — same parser, fed from stdout. Not in V1.
- **Hook events as a public dkod surface.** The NDJSON wire format
  is internal to dkod. We do not document it for third-party
  consumption in V1; reserve `v: 2` for that.

## Open questions for Haim (the controller)

1. **`SessionEnd` reliability.** Anthropic's docs list `SessionEnd`
   reasons (`clear`, `resume`, `logout`, `prompt_input_exit`,
   `bypass_permissions_disabled`, `other`) but don't promise it
   fires on every termination path (e.g. `kill -9`, OS reboot).
   Default proposal: rely on `SessionEnd` for the happy path, fall
   back to "no hook events for `--orphan-grace=60s` after a `Stop`
   AND no JSONL writes in that window" as the watchdog. Want the
   60s default tuned, or want a different fallback heuristic?

2. **Hook installation scope.** Three options:
   - (a) **Project-scoped** (`.claude/settings.local.json`,
     auto-gitignored) — only this repo. Cleanest but requires the
     user to be in the repo when starting `claude`.
   - (b) **User-scoped** (`~/.claude/settings.json`) — captures
     every Claude Code invocation system-wide. Powerful but
     invasive: we'd be installing a hook that fires on every
     session even when no `dkod capture` server is up (the hook
     will short-circuit on no socket, but it's a process-spawn
     tax on every Claude Code action).
   - (c) **Hybrid** — install user-scoped on `dkod init` if the
     user opts in, project-scoped otherwise.
   Default proposal: **(a)** for V1. `dkod init --capture-claude
   --user` adds (b) later.

3. **Reasoning capture default.** Mirrors the Codex doc question.
   Claude Code's JSONL has `thinking` blocks (`assistant.message
   .content[type=thinking]`) on every extended-thinking-enabled
   session. Default proposal: store but redact under the same
   rules as content (matches what we agreed for Codex).

4. **Multi-session same-cwd.** A user can run two `claude`
   sessions in the same cwd at the same time — `~/.claude/projects/
   <encoded-cwd>/<id1>.jsonl` and `<id2>.jsonl` happily coexist.
   Our server already keys in-flight state by `session_id`, so
   this works. Confirm we're OK with both ending up under the
   *same* dkod commit-graph attachment (i.e. we don't want to
   force one capture per terminal pane).

5. **`disableAllHooks: true`.** If a teammate has this set in
   their settings (some security guides recommend it), our capture
   silently fails. Default proposal: `dkod capture claude-code`
   refuses to start if `disableAllHooks` is true at the resolved
   settings layer; print a remediation message.

6. **Hook handler binary vs. shell script.** Plan said "tiny
   binary" (`dkod capture-hook`). Alternative: a 50-line POSIX
   `sh` script shipped in `.claude/hooks/dkod-capture.sh`. Pros of
   binary: no shell-portability bugs, no `bash` / `zsh` dependency
   on the user. Pros of script: trivial to inspect, no extra
   binary in `PATH`. Default proposal: ship `dkod capture-hook`
   as a subcommand of the same `dkod` binary the user already
   has — zero extra install, easy to inspect via `dkod capture-
   hook --help`.

## What this means for Task 18

`crates/dkod-core::capture::claude_code` (the socket server)
needs to implement:

- **`SocketServer` struct** owning:
  - A `tokio` `UnixListener` bound at the platform path with
    `0600` permissions.
  - A `HashMap<SessionId, InFlightSession>` indexed by Claude Code
    session UUID. Each `InFlightSession` holds `transcript_path`,
    `started_at`, last-event timestamp, `cwd`, `model`, and a
    bounded ring of recent `tool_start` / `tool_end` events for
    `dkod show --live`.
  - A `Vec<JoinHandle<()>>` for per-connection NDJSON readers.
  - A heartbeat-file path under `~/.local/share/dkod/captures/`.

- **Stale-socket detection on bind.** Before `bind()`, try
  `connect()`; on `ECONNREFUSED`, `unlink()` and proceed. On any
  other error, abort with a clear message ("another dkod capture
  server appears to be running for this repo — `dkod capture
  claude-code stop`").

- **NDJSON line reader.** Per accepted connection: read until
  `\n`, parse as `WireEvent { v, kind, session_id, ts, cwd, .. }`,
  dispatch by `kind`. Tolerate `v != 1` by logging + dropping
  the connection (no crash).

- **Event handlers.** One per `kind`:
  - `session_start` → create or update in-flight entry.
  - `prompt_submitted` / `tool_start` / `tool_end` /
    `pre_compact` / `turn_stop` → progress accounting only.
  - `session_end` → call `flush_session(session_id)`.

- **`flush_session(session_id)`.** Locate `transcript_path`
  (from in-flight state), open the JSONL, parse line-by-line into
  `parse_jsonl_session()`, run redaction, write the
  `Session` blob via the existing `SessionWriter` (the same
  interface Codex uses), drop the in-flight entry.

- **`parse_jsonl_session(path: &Path) -> Result<Session>`.** A
  pure parser, decoupled from the socket so it's unit-testable
  with checked-in fixtures. Maps:
  - First `assistant` line's `message.model` + the session-wide
    `cwd` / `version` / `gitBranch` → `Session.metadata`.
  - `user` `message.content[type=text]` / `string` content →
    `Message::User { text }`.
  - `user` `message.content[type=tool_result]` → attach to the
    tool call by `tool_use_id`.
  - `user` `message.content[type=image]` → `Message::User {
    images }` (V1: stored as a placeholder marker; binary content
    is post-V1).
  - `assistant` `message.content[type=text]` →
    `Message::Assistant { text }`.
  - `assistant` `message.content[type=thinking]` →
    `Message::Reasoning { text }` (subject to redaction config).
  - `assistant` `message.content[type=tool_use]` →
    `Message::ToolCall { id, name, input }` and a placeholder for
    the matching `tool_result`.
  - Roll up `message.usage` across `assistant` lines into
    `Session.usage` (sum of `input_tokens` / `output_tokens`
    plus the cache breakdown).
  - Tag `provider = "claude-code"`,
    `provider_version = first system event's "version"` (e.g.
    `"2.1.121"` from real data).
  - **Skip** `attachment`, `file-history-snapshot`, `custom-title`,
    `agent-name`, `permission-mode`, `last-prompt`, `pr-link`,
    `queue-operation`, `system` lines for V1 — they're mostly
    UI scaffolding. Log `pr-link` as `Session.metadata.pr_url`
    (cheap and useful).

- **Watchdog timer.** A `tokio::time::interval` that scans
  in-flight entries: if last-event-time is older than
  `--orphan-grace` AND we've already seen at least one `turn_stop`
  AND the `transcript_path` mtime hasn't changed, flush as if
  `session_end` had arrived.

- **Graceful shutdown.** On `SIGINT` / `SIGTERM`: stop accepting
  new connections, drain the `JoinHandle`s, flush every in-flight
  session, `unlink()` the socket, remove the heartbeat,
  uninstall the project hook (if we installed it), exit.

- **Recovery on startup.** Read the heartbeat file. If it points
  at a dead PID, log it, then walk
  `~/.local/share/dkod/captures/.pending/*.json` (left by
  best-effort flush) and retry the writes.

- **Compute `files_touched`.** Existing Task 16 gix-diff path
  works unchanged. Optionally cross-check against `Edit` /
  `Write` / `MultiEdit` `tool_input.file_path` values
  recovered from the JSONL — same idea as parsing `apply_patch`
  argv in the Codex adapter, just easier here because Claude's
  edit tools already name the path explicitly.

- **Tests.** A `crates/dkod-core/testdata/claude_code/` corpus
  containing one sanitized real JSONL (we have a 1381-line
  sample sitting on this machine), and a unit test
  `parse_jsonl_session(fixture).unwrap()` that asserts message
  count, role distribution, that all `tool_use` / `tool_result`
  pairs match by `tool_use_id`, and that `Session.usage` is
  non-zero.

## What this means for Task 19

`crates/dkod-cli` needs:

- **`dkod capture claude-code` subcommand**, mirroring the
  in-progress `dkod capture codex` shape but very different
  semantics — this command **starts a server** rather than
  spawning a single agent. Flags:
  - `--repo <path>` (default: cwd, must be a git repo),
  - `--idle-timeout <seconds>` (default 600),
  - `--orphan-grace <seconds>` (default 60),
  - `--no-install-hook` (server-only mode for local testing),
  - `--user-hooks` (install into `~/.claude/settings.json`
    instead of project — gated behind question 2 above),
  - `--foreground` / default daemonize.

- **`dkod capture claude-code stop [--repo <path>]`** — sends
  SIGTERM to the heartbeat-recorded PID, waits for it to clean
  up the socket, returns.

- **`dkod capture-hook <repo_hash> <event_name>`** — internal
  subcommand the hook script invokes. Reads hook input JSON from
  stdin, projects to a `WireEvent`, connects to the socket at the
  resolved per-repo path, writes one line, exits 0. Total budget:
  500 LOC, no panics, single-second hard timeouts on connect and
  write. Self-contained — does **not** load the rest of `dkod`.

- **Hook installer** that idempotently merges into
  `.claude/settings.local.json`:

  ```jsonc
  {
    "hooks": {
      "SessionStart":     [{ "matcher": "startup",  "hooks": [{ "type": "command", "command": "dkod capture-hook <repo_hash> SessionStart",     "timeout": 1 }]}],
      "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "dkod capture-hook <repo_hash> UserPromptSubmit", "timeout": 1 }]}],
      "PreToolUse":       [{ "hooks": [{ "type": "command", "command": "dkod capture-hook <repo_hash> PreToolUse",       "timeout": 1 }]}],
      "PostToolUse":      [{ "hooks": [{ "type": "command", "command": "dkod capture-hook <repo_hash> PostToolUse",      "timeout": 1 }]}],
      "PostToolUseFailure":[{"hooks": [{ "type": "command", "command": "dkod capture-hook <repo_hash> PostToolUseFailure","timeout": 1 }]}],
      "PreCompact":       [{ "hooks": [{ "type": "command", "command": "dkod capture-hook <repo_hash> PreCompact",       "timeout": 1 }]}],
      "Stop":             [{ "hooks": [{ "type": "command", "command": "dkod capture-hook <repo_hash> Stop",             "timeout": 1 }]}],
      "SessionEnd":       [{ "hooks": [{ "type": "command", "command": "dkod capture-hook <repo_hash> SessionEnd",       "timeout": 1 }]}]
    }
  }
  ```

  Each entry tagged with a marker comment / dkod-owned key so the
  uninstaller knows what it's allowed to remove.

- **Pre-flight check** on startup: refuse to start if
  `disableAllHooks: true` is set at any settings layer that would
  affect this cwd; print remediation.

- **`dkod show --live`** integration: `dkod show` against an
  in-flight session reads the server's recent-tool ring via a
  short-lived control connection (a `kind: "_query"` message) and
  prints the most recent N tool events plus the latest
  `transcript_path` JSONL tail. Stretch; punt to V1.1 if it
  bloats Task 19.

- **End-to-end smoke test.** A shell script that:
  1. Spawns `dkod capture claude-code --foreground` in the
     background.
  2. Manually injects a `session_start` / `prompt_submitted` /
     `tool_start` / `tool_end` / `session_end` sequence over the
     socket using a fixture transcript.
  3. Asserts `dkod log` shows the captured session.
  4. Sends SIGTERM, asserts socket + heartbeat are gone.

## Sources consulted

- Local install: `claude --version` (2.1.126), `claude --help` (full output reviewed)
- Local artifacts:
  - `~/.claude/projects/-Users-haimari-vsCode-haim-ari-github-dkod-swarm/6c533ba6-5970-4653-beb4-e80d5406f7e4.jsonl` (1381 lines, 30+ event sub-types observed)
  - `~/.claude/projects/-Users-haimari-vsCode-haim-ari-github-dkod-swarm/{b19f83d7-…,02fc244e-…}.jsonl`
  - `~/.claude/settings.local.json`
- https://code.claude.com/docs/en/hooks (every hook event, input shape, output shape, settings.json layout)
- https://code.claude.com/docs/en/headless.md (`-p`, `--output-format`, `stream-json`, `system/init`, `system/api_retry`, `system/plugin_install` schemas)
- https://code.claude.com/docs/en/agent-sdk/sessions.md (canonical `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl` path; `SessionStore` adapter)
- https://code.claude.com/docs/en/agent-sdk (Agent SDK overview, hook list, session types)
- Prior dkod adapter: `crates/dkod-core/src/capture/codex.rs` (Task 15)
- Prior dkod doc: `docs/research/codex-transcript-format.md` (Task 14)
- dkod-app prior art: `sidecar/node_modules/@anthropic-ai/claude-agent-sdk/sdk.d.ts` (TypeScript typing of stream-json events)
