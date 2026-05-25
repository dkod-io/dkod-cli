# Spike: Factory AI (Droid) hooks

**Verdict:** HOOKS-SUPPORTED

## What I checked

- `crates/dkod-core/src/capture/factory_ai.rs` — current adapter shells out to
  `droid exec --output-format stream-json` and parses NDJSON. No native event
  bridge today; capture is invocation-scoped, not session-scoped.
- `crates/dkod-core/testdata/factory_ai/synthetic-events.jsonl` — sample
  transcript matching the `droid exec` stream-json schema (system / user /
  assistant / tool_use / tool_result / result records).
- `~/.factory/settings.json` — already contains a populated `hooks` block with
  `PostToolUse` matchers (`Bash|Execute`) pointing at user-installed shell
  scripts. Proves the hook engine ships and the schema is live in production.
- `~/.factory/hooks/post-pr-greptile-hook.sh` — real hook the user runs; reads
  `CLAUDE_TOOL_INPUT` / `CLAUDE_TOOL_OUTPUT` env vars (note the `CLAUDE_`
  prefix — Droid borrowed Anthropic's contract verbatim).
- `~/.factory/commands/`, `~/.factory/skills/`, `~/.factory/sessions/`,
  `~/.factory/history.json`, `~/.factory/mcp.json` — full Claude-Code-style
  agent surface mirrored under `~/.factory/`.
- `droid --help` + `droid exec --help` — no `--hooks` flag, no plugin
  registration CLI; hooks are configured purely via `settings.json`.
- Web docs: `docs.factory.ai/cli/configuration/hooks-guide`,
  `docs.factory.ai/reference/hooks-reference`,
  `docs.factory.ai/cli/configuration/settings`.

## What I found

- Droid exposes a first-class, JSON-on-stdin hook API modeled directly on
  Claude Code's. Events: `PreToolUse`, `PostToolUse`, `UserPromptSubmit`,
  `Notification`, `Stop`, `SubagentStop`, `PreCompact`, **`SessionStart`**,
  **`SessionEnd`**.
- Hook input JSON always carries `session_id`, `transcript_path`, `cwd`,
  `permission_mode`, and `hook_event_name`. `SessionStart` adds a `source`
  discriminator (`startup` | `resume` | `clear` | `compact`). `SessionEnd`
  adds a `reason` (`clear` | `logout` | `prompt_input_exit` | `other`).
- `transcript_path` lands hooks directly on the on-disk NDJSON our existing
  `parse_events` already consumes — no schema bridge required.
- Configuration is layered: `~/.factory/settings.json` (user) and
  `<project>/.factory/settings.json` (project), with `settings.local.json`
  overrides at either level. Matchers use pipe-separated tool names (e.g.
  `Bash|Execute`) and `""` / `*` for "all tools".
- Exit codes are blocking-aware: 0 = OK (stdout shown / injected as context
  for `SessionStart` and `UserPromptSubmit`), 2 = block + stderr fed back to
  the agent, other = non-blocking warning. `SessionEnd` cannot block.
- The existing `PostToolUse` hook the user runs today already proves the
  contract works in this exact environment — we are not blocked on a vendor
  rollout.

## Recommended install method for dkod

Wire Droid capture through native hooks rather than wrapping `droid exec`.
`dkod init` (or a new `dkod install factory-ai`) should append a `SessionEnd`
hook to `~/.factory/settings.json` (additive merge — do not clobber the
existing `hooks` map) that invokes `dkod ingest factory-ai --transcript
"$TRANSCRIPT_PATH" --session-id "$SESSION_ID" --cwd "$CWD"`, reading the
fields from stdin JSON via `jq`. Add a paired `SessionStart` hook only if we
want to mark a session "in-progress" in the dashboard. Keep the current
`capture_factory_ai` wrapper as a fallback for users who script `droid exec`
in CI. Project-scoped installs land in `<repo>/.factory/settings.json` and
should be the default from `dkod init` so dkod attaches automatically when
Droid is run inside a dkod-enabled repo.

## Sources

- https://docs.factory.ai/cli/configuration/hooks-guide
- https://docs.factory.ai/reference/hooks-reference
- https://docs.factory.ai/cli/configuration/settings
- https://docs.factory.ai/cli/droid-exec/overview
- https://docs.factory.ai/reference/cli-reference
- Local: `~/.factory/settings.json`, `~/.factory/hooks/post-pr-greptile-hook.sh`
