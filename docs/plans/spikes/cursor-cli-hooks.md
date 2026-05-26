# Spike: Cursor CLI hooks

**Verdict:** HOOKS-SUPPORTED (partial — no `sessionStart` / `sessionEnd` / `stop` in CLI yet; shell + file-edit hooks are the only reliable lifecycle anchors today)

## What I checked

- `crates/dkod-core/src/capture/cursor.rs` — current adapter spawns `cursor-agent -p --output-format stream-json` and parses the NDJSON stream from stdout (active wrapper, not hook-driven).
- `crates/dkod-core/testdata/cursor/synthetic-events.jsonl` — fixture for the stream-json parser.
- `~/.cursor/` on this machine: contains `argv.json`, `mcp.json`, `extensions/`, `plugins/`, `skills/`, `projects/`, `ai-tracking/`. No `hooks.json` present (default state). No `~/.config/cursor/` (Cursor uses `~/.cursor/` on macOS).
- Local CLI surface: `cursor --help` (Cursor 2.6.20) lists an `agent` subcommand. `cursor agent --help` exposes flags `--print`, `--output-format {text|json|stream-json}`, `--plugin-dir <path>`, `--resume [chatId]`, `--continue`, `--worktree`, plus subcommands `mcp`, `worker`, `create-chat`, `generate-rule`, `ls`, `resume`. The standalone `cursor-agent` binary auto-installs from `https://cursor.com/install` on first invocation. No `--hook` / `--hooks-file` flag — hooks are config-file driven.
- Upstream docs and forum threads:
  - https://cursor.com/docs/hooks
  - https://forum.cursor.com/t/hooks-for-cursor-cli-aka-cursor-agent/137847
  - https://forum.cursor.com/t/cursor-cli-hooks/148511

## What I found

- Cursor ships a first-class native hooks system (introduced in Cursor 1.7, Sep 2025). Config lives at `~/.cursor/hooks.json` (user) or `<project>/.cursor/hooks.json` (project), with Enterprise/Team overrides on top. Priority: Enterprise > Team > Project > User.
- Documented event surface (IDE): `sessionStart`, `sessionEnd`, `preToolUse`, `postToolUse`, `postToolUseFailure`, `subagentStart`, `subagentStop`, `beforeShellExecution`, `afterShellExecution`, `beforeMCPExecution`, `afterMCPExecution`, `beforeReadFile`, `afterFileEdit`, `beforeSubmitPrompt`, `preCompact`, `stop`, `afterAgentResponse`, `afterAgentThought`. Tab hooks (`beforeTabFileRead`, `afterTabFileEdit`) and `workspaceOpen` also exist.
- **CLI gap (verified against forum testing, Nov 2025 - Feb 2026):** in `cursor-agent`, only these events actually fire today — `beforeShellExecution`, `afterShellExecution`, `beforeMCPExecution`, `afterMCPExecution`, `afterFileEdit`. The events we'd most want for session capture — `sessionStart`, `sessionEnd`, `stop`, `beforeSubmitPrompt`, `afterAgentResponse`, `afterAgentThought`, `beforeReadFile` — are defined in the type system but the CLI's agent loop never invokes them ("only works through the resource accessor wrappers"). Cursor staff (Nov 26 2025) acknowledged the gap and stated they plan to bring the full IDE hook set to the CLI; no ETA.
- Each hook is an executable. Cursor streams a JSON envelope on stdin (`conversation_id`, `generation_id`, `model`, `hook_event_name`, `cursor_version`, `workspace_roots`, `user_email`, `transcript_path`) and expects JSON on stdout. Exit `0` = success, exit `2` = block, other exit codes fail-open. `transcript_path` is the key field for dkod: it points at the on-disk session transcript, which lets us avoid duplicating the stream-json parser inside the hook.
- `cursor agent --plugin-dir <path>` exists and lets us load a local plugin directory at agent startup — orthogonal to `hooks.json` but worth noting if hooks regress.
- Community examples confirming the shape: GitButler (`afterFileEdit` + `stop` for auto-commit, IDE only), `hamzafer/cursor-hooks`, `1Password/agent-hooks`, `waynesutton/cursor-cli-sync-plugin`.

## Recommended install method for dkod

Adopt the same pattern as Claude Code / Gemini CLI: have the seamless capture wizard write `~/.cursor/hooks.json` (user-scope, the default) pointing at `dkod capture-hook --agent cursor --event <event>` registered for `afterFileEdit`, `afterShellExecution`, and `afterMCPExecution`. Per-repo hooks (`.cursor/hooks.json`) are installed by `dkod init` when initialising a specific repository — the three events confirmed to fire in the CLI today. The hook reads `conversation_id` and `transcript_path` from stdin, fires-and-forgets a JSON event to a lazy-spawned local dkod recorder over a Unix socket (timeout < 100 ms, exit `0`), and lets the recorder open `transcript_path` to materialise the full `refs/dkod/sessions/<id>` ref. Because there is no working `sessionStart` / `sessionEnd` / `stop` in the CLI yet, the recorder must derive session boundaries from `conversation_id` transitions and an inactivity timeout — not ideal, but workable. Keep the current `cursor-agent -p --output-format stream-json` wrapper as the non-interactive capture path and as a fallback when hooks don't fire (e.g. older CLI builds, remote `cursor agent worker` runs where forum reports say hooks don't work). Re-evaluate once Cursor ships full CLI hook parity; at that point we add `sessionStart` / `sessionEnd` and retire the inactivity-timeout heuristic.

## Sources

- https://cursor.com/docs/hooks
- https://forum.cursor.com/t/hooks-for-cursor-cli-aka-cursor-agent/137847
- https://forum.cursor.com/t/cursor-cli-hooks/148511
- https://forum.cursor.com/t/agent-plugins-isolated-packaging-lifecycle-management-for-sub-agents-skills-hooks-rules-incl-agent-md-across-cursor-ide-cli/151250
- https://www.infoq.com/news/2025/10/cursor-hooks/
- https://skywork.ai/blog/how-to-cursor-1-7-hooks-guide/
- https://blog.gitbutler.com/cursor-hooks-integration
- https://github.com/hamzafer/cursor-hooks
- https://github.com/1Password/agent-hooks
- https://github.com/waynesutton/cursor-cli-sync-plugin
