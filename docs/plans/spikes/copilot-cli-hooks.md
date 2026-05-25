# Spike: Copilot CLI hooks

**Verdict:** HOOKS-SUPPORTED

## What I checked

- `crates/dkod-core/src/capture/copilot_cli.rs` — current adapter; spawns
  `copilot -p --output-format json --no-ask-user`, captures stdout JSONL, and
  falls back to reading `$COPILOT_HOME/session-state/<session_id>/events.jsonl`.
- `crates/dkod-core/testdata/copilot_cli/synthetic-events.jsonl` — adapter knows
  event types: `session.start`, `user.message` / `user_message`,
  `assistant.message` / `agent_message`, `reasoning`, `tool.execution_start` /
  `tool_use`, `tool.execution_complete` / `tool_result`, `file.change` /
  `file_change`, `session.shutdown` / `session.end`, `error`.
- `~/.copilot/` — present (contains `skills/`); no `hooks/` or `settings.json`
  yet. `~/.config/copilot/` and `~/.config/gh-copilot/` — absent.
- GitHub Docs:
  - https://docs.github.com/en/copilot/reference/hooks-configuration
  - https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/use-hooks
  - https://docs.github.com/en/copilot/tutorials/copilot-cli-hooks
  - https://docs.github.com/en/copilot/concepts/agents/copilot-cli/about-cli-plugins
- GitHub Changelog (2026-05-06): enterprise-managed plugins for Copilot CLI in
  public preview — plugins bundle Skills, Hooks, MCP, LSP configs.

## What I found

- Copilot CLI ships a **native, first-class hooks system** — not a shim. Hooks
  are external commands run at lifecycle points by the CLI itself.
- Hook events relevant to dkod capture: `sessionStart`, `sessionEnd`,
  `userPromptSubmitted`, `preToolUse`, `postToolUse`, `agentStop`,
  `errorOccurred`, plus `subagentStart` / `subagentStop`, `preCompact`,
  `permissionRequest`, `notification`.
- Config is **JSON, version 1**. Load order: repo `.github/hooks/*.json` →
  user `~/.copilot/hooks/*.json` (or `$COPILOT_HOME/hooks/`) → `hooks` field in
  `.github/copilot/settings.json` / `.github/copilot/settings.local.json` →
  `~/.copilot/settings.json` → plugin-declared `hooks.json`. Changes only take
  effect on CLI restart.
- Each hook entry: `{ "type": "command", "bash": "...", "powershell": "...",
  "command": "<fallback>", "cwd": "...", "env": { ... }, "timeoutSec": 30 }`.
  At least one of `bash` / `powershell` / `command` required.
- System env vars passed to hooks: `GITHUB_COPILOT_API_TOKEN`,
  `GITHUB_COPILOT_GIT_TOKEN`, `COPILOT_AGENT_PROMPT` (cloud only), `HOME`.
  The CLI also exposes session context to the hook process (session id, tool
  name, args) — exact wire format is on stdin per the docs/tutorials.
- Distribution path for teams already exists: **enterprise-managed plugins**
  can bundle a `hooks.json` so an org admin can roll dkod capture to every
  Copilot CLI user without touching individual machines.

## Recommended install method for dkod

Use the user-level hook config and write a single file at
`~/.copilot/hooks/dkod-capture.json` (honor `$COPILOT_HOME` if set, matching
`copilot_cli.rs`). Register `sessionEnd` (primary capture trigger — full
transcript on disk by then) and `agentStop` (fallback for non-terminated
sessions), each invoking `dkod capture copilot-cli --session-id "$..."` via the
`bash` field with a `powershell` mirror for Windows. Optionally add
`sessionStart` to record `spawn_unix` so we don't have to infer it. Keep
`timeoutSec` small (e.g. 5) so we never block the user's shell on a slow
import. For team rollout later, the same JSON drops unchanged into an
enterprise-managed plugin bundle — no second install path to maintain. Skip
the current `copilot -p ...` spawn-wrapper as the default install once the
hook is wired; keep it as a manual `dkod capture --replay` escape hatch for
sessions that started before the hook was registered.

Example file (sketch):

```json
{
  "version": 1,
  "hooks": {
    "sessionEnd": [
      {
        "type": "command",
        "bash": "dkod capture copilot-cli --session-id \"$COPILOT_SESSION_ID\" --copilot-home \"${COPILOT_HOME:-$HOME/.copilot}\" >/dev/null 2>&1 &",
        "powershell": "Start-Process -NoNewWindow dkod -ArgumentList 'capture','copilot-cli','--session-id',$env:COPILOT_SESSION_ID",
        "timeoutSec": 5
      }
    ]
  }
}
```

(Confirm the exact env-var name / stdin shape against
`docs.github.com/en/copilot/reference/hooks-configuration` during V1.5
implementation — docs hint at both `env` injection and a stdin JSON payload;
the spike didn't run a live hook to nail down which.)

## Sources

- https://docs.github.com/en/copilot/reference/hooks-configuration
- https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/use-hooks
- https://docs.github.com/en/copilot/tutorials/copilot-cli-hooks
- https://docs.github.com/en/copilot/concepts/agents/hooks
- https://docs.github.com/en/copilot/concepts/agents/copilot-cli/about-cli-plugins
- https://github.blog/changelog/2026-05-06-enterprise-managed-plugins-in-github-copilot-cli-are-now-in-public-preview/
- https://deepwiki.com/github/copilot-sdk/10.3-session-hooks-and-lifecycle-events
