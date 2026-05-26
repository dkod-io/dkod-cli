## Spike: Gemini CLI hooks

**Verdict:** HOOKS-SUPPORTED

## What I checked

- `crates/dkod-core/src/capture/gemini_cli.rs` — current adapter spawns `gemini -p --output-format stream-json` and parses NDJSON from stdout (active capture model, not hook-driven).
- `crates/dkod-core/testdata/gemini_cli/synthetic-events.jsonl` — fixture for the active stream-json parser.
- `~/.gemini/` — exists (`GEMINI.md`, `skills/`, `antigravity/`) but no `settings.json` yet on this machine; `~/.config/gemini*` absent. `gemini` binary not installed locally — verified hook docs against the upstream repo.
- Upstream docs:
  - https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/index.md
  - https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/writing-hooks.md
  - https://github.com/google-gemini/gemini-cli/blob/main/docs/get-started/configuration.md
  - https://geminicli.com/docs/hooks/
  - https://developers.googleblog.com/tailor-gemini-cli-to-your-workflow-with-hooks/
- Tracking issues #9070, #11703, #14449 (extensions hook support, since landed).

## What I found

- Gemini CLI ships a first-class native hooks system, enabled by default since v0.26.0. Events: `SessionStart`, `SessionEnd`, `BeforeAgent`, `AfterAgent`, `BeforeModel`, `AfterModel`, `BeforeToolSelection`, `BeforeTool`, `AfterTool`, `PreCompress`, `Notification`.
- Hooks are configured under `"hooks"` in `settings.json`, merged from three layers: project (`.gemini/settings.json`), user (`~/.gemini/settings.json`), and system (`/etc/gemini-cli/settings.json`). Extensions can also bundle a `hooks/hooks.json` that the `ExtensionManager` auto-discovers.
- Two hook types: `command` (shell command, stdin/stdout JSON, exit codes) — fully shipped; and `plugin` (npm packages tagged `geminicli-plugin`) — for richer integrations.
- Environment passed to each command hook includes `GEMINI_SESSION_ID`, `GEMINI_PROJECT_DIR`, `GEMINI_CWD`, `GEMINI_PLANS_DIR`, and a `CLAUDE_PROJECT_DIR` compatibility alias — sufficient for dkod to scope a capture to the right session and worktree.
- `SessionStart` / `SessionEnd` fire on startup/resume/clear and exit/clear respectively — ideal anchor points for spawning a dkod recorder and finalizing the `refs/dkod/sessions/<id>` ref. `AfterTool` (with matchers like `write_file|replace`) gives us file-touch deltas without re-parsing stdout.
- Per-hook `timeout` defaults to 60000 ms; we should pick a low timeout (e.g. 2000 ms) and have the hook fire-and-forget into a dkod daemon to avoid blocking the agent loop.

## Recommended install method for dkod

Treat Gemini CLI exactly like Claude Code's hook model: have `dkod init` write a `SessionStart` + `SessionEnd` (+ optional `AfterTool` for file-touch breadcrumbs) entry into `~/.gemini/settings.json` (user scope by default, project scope when `--project` is passed) pointing at a small `dkod gemini-hook` subcommand. The hook reads `GEMINI_SESSION_ID` / `GEMINI_CWD` from env, posts a JSON event to a lazy-spawned local dkod recorder over a Unix socket, and exits in <100 ms. This obsoletes the current `gemini -p --output-format stream-json` wrapper for interactive sessions (we can keep `capture_gemini_cli` as the non-interactive path) and lines up with the always-on capture story tracked in issue #6. No shim or PATH hijack required.

## Sources

- https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/index.md
- https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/writing-hooks.md
- https://github.com/google-gemini/gemini-cli/blob/main/docs/get-started/configuration.md
- https://github.com/google-gemini/gemini-cli/issues/9070
- https://github.com/google-gemini/gemini-cli/issues/11703
- https://github.com/google-gemini/gemini-cli/issues/14449
- https://geminicli.com/docs/hooks/
- https://developers.googleblog.com/tailor-gemini-cli-to-your-workflow-with-hooks/
- https://github.com/gemini-cli-extensions
