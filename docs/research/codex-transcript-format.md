# Codex CLI transcript format — research

**Date:** 2026-05-03
**Status:** input to Task 15 (Codex adapter)

## TL;DR

Codex (the Rust CLI in `openai/codex`, subdirectory `codex-rs/`, currently
shipping versions in the `0.128.x` line on May 1, 2026) emits a
**stable, well-typed JSONL event stream** in two places:

1. **stdout** — when invoked as `codex exec --json …`, each line is a
   `ThreadEvent` (defined in `codex-rs/exec/src/exec_events.rs`). Its
   shape is exported via `ts-rs` and is treated by upstream as a public
   protocol surface.
2. **on-disk rollouts** — every session (interactive, exec, IDE) writes
   a JSONL "rollout" file to `~/.codex/sessions/YYYY/MM/DD/rollout-<ISO-ts>-<uuid>.jsonl`,
   with a richer internal schema (`session_meta`, `response_item`,
   `event_msg`, `turn_context`). The desktop app, the TUI, and `codex exec`
   all write the same rollout format.

There is **no socket / IPC hook surface** we can attach to from outside the
process. There **is** a config-driven lifecycle hook system
(`session_start`, `user_prompt_submit`, `pre_tool_use`, `post_tool_use`,
permission requests) that runs subprocesses configured in
`~/.codex/config.toml`, but that is for *injecting* context, not for
streaming the transcript out.

The cleanest capture path is therefore **(c) hybrid**: shell out to
`codex exec --json` for live signal + final output, and post-process the
matching `rollout-*.jsonl` for the canonical, durable transcript
(reasoning, tool calls, file changes, token counts).

## Stdout format

`codex exec` is the non-interactive entry point. From `codex --help`:

```
Commands:
  exec    Run Codex non-interactively [aliases: e]
  proto   Run the Protocol stream via stdin/stdout [aliases: p]
  …
```

`codex exec --help` exposes the relevant flags:

```
      --json
          Print events to stdout as JSONL

      --output-last-message <LAST_MESSAGE_FILE>
          Specifies file where the last message from the agent should be written

      --color <COLOR>            [default: auto]   [values: always, never, auto]
```

Default mode prints only the agent's final natural-language message to
stdout (with progress on stderr). With `--json`, every event is one JSON
object per line. Upstream commits to this contract in
`codex-rs/exec/src/lib.rs`:

```rust
// - In the default output mode, it is paramount that the only thing written to
//   stdout is the final message (if any).
// - In --json mode, stdout must be valid JSONL, one event per line.
// For both modes, any other output must be written to stderr.
#![deny(clippy::print_stdout)]
```

The event vocabulary is the `ThreadEvent` enum in
`codex-rs/exec/src/exec_events.rs`:

```rust
#[serde(tag = "type")]
pub enum ThreadEvent {
    #[serde(rename = "thread.started")]   ThreadStarted(ThreadStartedEvent),   // { thread_id }
    #[serde(rename = "turn.started")]     TurnStarted(TurnStartedEvent),
    #[serde(rename = "turn.completed")]   TurnCompleted(TurnCompletedEvent),   // { usage: Usage }
    #[serde(rename = "turn.failed")]      TurnFailed(TurnFailedEvent),
    #[serde(rename = "item.started")]     ItemStarted(ItemStartedEvent),       // { item: ThreadItem }
    #[serde(rename = "item.updated")]     ItemUpdated(ItemUpdatedEvent),
    #[serde(rename = "item.completed")]   ItemCompleted(ItemCompletedEvent),
    #[serde(rename = "error")]            Error(ThreadErrorEvent),             // { message }
}
```

Each `ThreadItem` carries an `id` plus a typed `details` payload
(`#[serde(tag = "type", rename_all = "snake_case")]`):

| `type`              | Carries                                                     |
|---------------------|-------------------------------------------------------------|
| `agent_message`     | `text` — final/streamed assistant text                      |
| `reasoning`         | `text` — reasoning summary                                  |
| `command_execution` | `command`, `aggregated_output`, `exit_code`, `status`       |
| `file_change`       | `changes: [{path, kind: add/delete/update}]`, `status`      |
| `mcp_tool_call`     | `server`, `tool`, `arguments`, `result`, `error`, `status`  |
| `collab_tool_call`  | spawn/send/wait/close agent traffic                         |
| `web_search`        | `id`, `query`, `action`                                     |
| `todo_list`         | `items: [{text, completed}]`                                |
| `error`             | `message`                                                   |

`Usage` on `turn.completed` includes
`input_tokens / cached_input_tokens / output_tokens / reasoning_output_tokens`.

This is exactly the shape we need to populate `Message` records
(role, content, tool_calls, file_changes, token totals) without parsing
free-form text.

## On-disk history

There are three on-disk artifacts under `$CODEX_HOME` (default `~/.codex`):

1. **`~/.codex/sessions/YYYY/MM/DD/rollout-<ISO-ts>-<uuid>.jsonl`** — the
   canonical per-session transcript. Confirmed locally by listing real
   sessions from this machine:

   ```
   ~/.codex/sessions/2026/04/20/rollout-2026-04-20T09-04-26-019da97d-7bdc-7971-96d9-a2125d3b586f.jsonl
   ~/.codex/sessions/2025/09/12/rollout-2025-09-12T21-14-57-634ca982-07e0-432b-a220-ccc67fa48227.jsonl
   ```

   Top-level shape (every line is one of):

   - `{"timestamp": …, "type": "session_meta",  "payload": {…}}` — once at the start: `id`, `cwd`, `originator`, `cli_version`, `source`, `model_provider`, `base_instructions`, `dynamic_tools`.
   - `{"timestamp": …, "type": "turn_context",  "payload": {…}}` — `cwd`, `model`, `effort`, `approval_policy`, `sandbox_policy`, etc.
   - `{"timestamp": …, "type": "response_item", "payload": {…}}` — model API records: `message` (role=user/assistant/developer), `reasoning`, `function_call`, `function_call_output`.
   - `{"timestamp": …, "type": "event_msg",     "payload": {…}}` — coarser orchestration events: `task_started`, `task_complete`, `user_message`, `agent_message`, `token_count`, `error`, `thread_name_updated`, `thread_rolled_back`.

   Distinct event-type histogram from a real 289-line rollout:

   ```
   72  event_msg | token_count
   66  response_item | function_call
   66  response_item | function_call_output
   38  response_item | reasoning
   23  response_item | message
   13  event_msg | agent_message
   10  event_msg | user_message
    1  session_meta
   ```

   Tool calls and file edits are first-class. `function_call` records
   look like:

   ```json
   {"timestamp":"…","type":"response_item","payload":{
     "type":"function_call","name":"shell",
     "arguments":"{\"command\":[\"bash\",\"-lc\",\"ls -la\"],\"timeout_ms\":120000}",
     "call_id":"call_KrOyvrheD5Dox9qEXEyOcRJ1"}}
   ```

   File edits ride on the same `shell` function with `apply_patch`
   as the argv head, which we can detect cleanly:

   ```json
   "arguments":"{\"command\":[\"apply_patch\",\"*** Begin Patch\\n*** Add File: AGENTS.md\\n+# …\"]}"
   ```

   The matching `function_call_output` carries a JSON-stringified
   `{"output": "…", "metadata": {"exit_code": …, "duration_seconds": …}}`.

2. **`~/.codex/sessions/session_index.jsonl`** — one line per known
   session: `{"id":"…","thread_name":"…","updated_at":"…"}`. Useful
   purely as a directory.

3. **`~/.codex/history.jsonl`** — global cross-session prompt history
   (`{session_id, ts, text}`), indexed for the TUI's reverse-prompt
   recall. Not a transcript — only user prompts.

The rollout writer is in the upstream `codex-rs/rollout` crate
(`RolloutRecorder`, re-exported via `codex-rs/core/src/rollout.rs`); the
corresponding directory constant is `SESSIONS_SUBDIR`. The repo also
defines an `ARCHIVED_SESSIONS_SUBDIR`, so don't be surprised if some
files end up under an `archived/` parallel tree.

`$CODEX_HOME` overrides the location; otherwise it's `~/.codex`. There
is also a `~/.codex/state_5.sqlite` / `logs_2.sqlite` pair, but those
are app-server bookkeeping (automations, telemetry), not the transcript
of interest.

## Hooks / plugin API

Codex has a **lifecycle hook system**, but it is not a streaming /
external-observer surface. The hook surface lives in the
`codex-rs/hooks/` crate and is consumed at runtime by
`codex-rs/core/src/hook_runtime.rs`. The hook event names are:

```rust
SessionStartOutcome
UserPromptSubmitOutcome      / UserPromptSubmitRequest
PreToolUseOutcome            / PreToolUseRequest
PostToolUseOutcome           / PostToolUseRequest
PermissionRequestDecision    / PermissionRequestRequest / PermissionRequestOutcome
```

Hooks are configured in `~/.codex/config.toml` and **run as
subprocesses** Codex invokes synchronously to *inject extra context* or
*authorize/block tool use*. They are not push-style transcript taps.
The `notify` mechanism is documented as deprecated:

> `notify` is deprecated and will be removed in a future release.
> Existing configurations still work for compatibility, but new
> automation should use lifecycle hooks instead.

Practical implications for dkod:

- We **could** register a `post_tool_use` hook that writes the
  finalized turn to a fifo / unix socket dkod owns. That earns us
  push-style capture, but it's invasive (mutates the user's
  `config.toml`) and doesn't see the *final* assistant message
  (no `final_response` hook exists today).
- **Far simpler**: don't touch the hook surface in V1. Read the
  rollout file. It's the same data Codex itself uses to resume a
  session, so it's load-bearing for upstream — they are not going
  to silently drop it.

There is a `codex proto` mode (alias `p`) that runs a Protocol stream
over stdin/stdout, but it's a low-level "drive Codex from another
program" interface — interesting for a future replacement of the
chat UI, not for passive capture of an existing user session.

There is a `codex mcp` mode marked **Experimental: run Codex as an MCP
server** — same story: a way to *call* Codex, not to *observe* a user's
Codex session.

## Tool calls and file edits

Yes, both stdout (`--json`) and on-disk rollouts emit tool calls and
file edits as **structured records**. We do *not* need to diff the
working tree to recover what happened.

- In the **stdout JSONL** stream:
  - shell invocations → `item.completed` with `details.type = "command_execution"` and full `command`, `aggregated_output`, `exit_code`.
  - file edits → `item.completed` with `details.type = "file_change"` and `changes: [{path, kind: add|delete|update}]`. (Note: the JSON event reports *which* paths changed and *what kind* of change, not the patch body. The patch body is on stdout in the underlying `apply_patch` shell call captured in the rollout.)
  - MCP tool calls → `details.type = "mcp_tool_call"` with `server`, `tool`, `arguments`, `result`/`error`.

- In the **rollout JSONL** file:
  - shell invocations → `response_item` with `payload.type = "function_call"`, `name = "shell"`, `arguments` is a JSON-stringified `{command: [argv…], timeout_ms}`. Followed by a `function_call_output` with the matching `call_id`.
  - file edits → same as shell, but `command[0] == "apply_patch"`. The full unified-diff-ish patch body is right there in `arguments[1]` as `*** Begin Patch / *** End Patch`. Trivial to extract `files_touched` from this without invoking gix at all (still useful as a cross-check / for non-`apply_patch` writes).

Either source is sufficient for `Message::content` + `Message::file_changes`.
The rollout is richer (it also has `reasoning` items and the verbatim
`apply_patch` body); the stdout stream is timelier.

## Stability / version pin

Stability looks reasonable but not glacial. Observations:

- The rollout file format has been recognizably stable (same `session_meta` / `response_item` / `event_msg` envelope) on this machine across a 7-month gap (2025-09-12 → 2026-04-20).
- The `ThreadEvent` enum on stdout is exported via `ts-rs` and is treated by upstream as a public protocol surface — they ship TypeScript bindings off it, so silent breaking changes are unlikely.
- Release cadence is **fast**. From the GitHub releases API as of 2026-05-01:
  - `rust-v0.129.0-alpha.2` (2026-05-01)
  - `rust-v0.128.0` (2026-04-30)
  - `rust-v0.126.0-alpha.{12..17}` (2026-04-28 → 04-30)
  Multiple alpha releases per day; one stable `0.X.0` roughly every couple of weeks.

Recommended pin for V1: **codex-cli ≥ 0.34.0** for `codex exec --json`
(works on Haim's installed version). Document the rollout schema we
parse; treat any unknown event/item type as a no-op rather than a hard
error so a new minor version doesn't break capture. Keep a small
version-snapshot fixture (one real rollout + one `--json` stream)
checked into the test corpus so regressions show up as test diffs.

## Recommended capture strategy

**(c) Hybrid** — but with the rollout file as the primary source and
`codex exec --json` purely as a wrapper-level "is it done yet, what was
the final answer, what did it cost" signal.

Justification: the rollout file is the canonical, durable form, contains
strictly more information (reasoning items + verbatim patch bodies + token
accounting), is written by every Codex entry point (TUI, exec, IDE,
desktop), and is the file Codex itself uses for thread resume — so
upstream has a strong incentive to keep it stable. `codex exec --json`
gives us the spawn-time things the rollout doesn't surface as cleanly:
the spawn arguments, our own wall-clock start/end, exit status, and the
file path of the rollout to read (we discover it from
`thread.started.thread_id` + `~/.codex/session_index.jsonl`).

Constraints:

- Requires `codex-cli >= 0.34.0` for `--json` and `--output-last-message`.
- Requires read access to `$CODEX_HOME` (default `~/.codex`); if user
  has overridden `CODEX_HOME` we must honor it (read env var, fall back
  to `~/.codex`).
- We only capture sessions that ran **through our wrapper** in V1.
  Capturing arbitrary already-completed Codex sessions is possible
  (they're all sitting on disk) but is a separate "import" feature, not
  the V1 capture path.

## What this means for Task 15

`crates/dkod-core::capture::codex` (the adapter) needs to:

- Expose a `capture_codex(prompt, cwd, args, …) -> Result<Session>` entry
  point that:
  1. Spawns `codex exec --json --skip-git-repo-check -C <cwd> [user args] -- <prompt>`. Plumb `--output-last-message <tmpfile>` so the final assistant text is also recoverable from a deterministic file.
  2. Streams stdout line-by-line, parses each line as `ThreadEvent`
     (single `#[serde(tag = "type")]` enum match). On `thread.started`
     capture the `thread_id`. On any unknown variant, log + ignore
     (don't fail).
  3. On the *child's* exit, locate the rollout file:
     - read `${CODEX_HOME:-$HOME/.codex}/sessions/session_index.jsonl`,
       find the row with matching `id == thread_id`, then glob
       `…/sessions/*/*/*/rollout-*-<thread_id>.jsonl`.
  4. Parse the rollout JSONL into our `Session` / `Message` model:
     - `session_meta` → `Session.metadata` (cwd, model_provider, cli_version, originator).
     - `event_msg.user_message` / `event_msg.agent_message` / `response_item.message` → `Message { role, content }`.
     - `response_item.function_call` (`name = "shell"`) → `Message::ToolCall { tool: "shell", argv, timeout_ms }` plus, when `argv[0] == "apply_patch"`, an extracted `apply_patch` body.
     - `response_item.function_call_output` (matched via `call_id`) → attach to the corresponding tool call: `output`, `exit_code`, `duration_seconds`.
     - `response_item.reasoning` → `Message::Reasoning { text }` (we may want to redact-but-keep these by default, controlled by config).
     - `event_msg.token_count` → roll up into `Session.usage`.
  5. Compute `files_touched`: prefer parsing the `apply_patch` bodies
     out of the rollout (deterministic, no I/O); fall back to gix
     working-tree diff (Task 16) only when no `apply_patch` was used
     (e.g. the agent ran `mv` / `sed -i` directly). Agree both signals
     should be unioned.
  6. Tag the resulting session with `provider = "codex"` and
     `provider_version = session_meta.payload.cli_version`.

- Have a unit-level "parse this rollout fixture into a Session" test
  using a real (sanitized) rollout file checked into
  `crates/dkod-core/testdata/codex/`.

- Be tolerant of:
  - Rollout file showing up *after* exec exits (briefly — the writer
    is fsync-on-drop). Add a small bounded retry loop (e.g. up to
    1s, 50ms ticks) before giving up.
  - `event_msg.thread_rolled_back` — drop and re-replay messages
    after the rollback marker (matches Codex's own resume semantics).
  - Multi-turn sessions: the JSONL is append-only; honor
    `turn_context.turn_id` if we want per-turn metadata.

## Open questions for Haim (the controller)

1. **Reasoning capture default.** The rollout includes
   `response_item.reasoning` items (model's chain-of-thought summary).
   Do we (a) always store, (b) always drop, (c) store but redact under
   the same rules as content? Default proposal: (c).

2. **Existing-session import.** All historical Codex sessions on disk
   (`~/.codex/sessions/**`) are captureable today without a wrapper.
   In scope for V1 (a `dkod capture codex --import-existing` mode), or
   strictly post-V1?

3. **Hook-based "passive" capture.** A `post_tool_use` hook in
   `~/.codex/config.toml` would let us capture sessions the user runs
   *outside* our `dkod capture codex` wrapper. Want this in V1, or
   wrapper-only for now? Default proposal: wrapper-only; hook-based
   capture is a follow-up once we trust the rollout-parse path.

4. **Version pin policy.** Hard-fail on `codex --version <
   $minimum`, or warn-and-continue? With the alpha-heavy release
   cadence, hard-fail seems aggressive. Default proposal: warn on
   first run if `cli_version` from `session_meta` is outside
   `[0.34.0, current_tested_major]`.

## Sources consulted

- Local install: `codex --help`, `codex exec --help`, `codex proto --help`, `codex --version` (0.34.0)
- Local artifacts: `~/.codex/sessions/2026/04/20/rollout-2026-04-20T09-04-26-019da97d-7bdc-7971-96d9-a2125d3b586f.jsonl`, `~/.codex/sessions/2025/09/12/rollout-2025-09-12T21-14-57-634ca982-07e0-432b-a220-ccc67fa48227.jsonl`, `~/.codex/session_index.jsonl`, `~/.codex/history.jsonl`
- https://github.com/openai/codex
- https://github.com/openai/codex/tree/main/codex-rs
- https://raw.githubusercontent.com/openai/codex/main/codex-rs/exec/src/lib.rs
- https://raw.githubusercontent.com/openai/codex/main/codex-rs/exec/src/exec_events.rs
- https://raw.githubusercontent.com/openai/codex/main/codex-rs/exec/src/event_processor_with_jsonl_output.rs
- https://raw.githubusercontent.com/openai/codex/main/codex-rs/core/src/rollout.rs
- https://raw.githubusercontent.com/openai/codex/main/codex-rs/core/src/hook_runtime.rs
- https://api.github.com/repos/openai/codex/contents/codex-rs/hooks/src
- https://raw.githubusercontent.com/openai/codex/main/docs/exec.md
- https://raw.githubusercontent.com/openai/codex/main/docs/config.md
- https://api.github.com/repos/openai/codex/releases (versions ≤ `rust-v0.129.0-alpha.2`, 2026-05-01)
- https://developers.openai.com/codex/noninteractive (referenced by `docs/exec.md`)
