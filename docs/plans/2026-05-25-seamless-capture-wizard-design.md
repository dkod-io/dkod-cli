# Seamless capture wizard — design

**Status:** draft, brainstormed 2026-05-25
**Supersedes:** issue #6 (always-on Claude Code capture)
**Related:** `docs/plans/2026-05-03-dkod-pivot-design.md`, `docs/plans/2026-05-03-dkod-cli-v1-implementation.md`

## Problem

Today, capturing an agent session requires two manual commands: `dkod init` once
per repo, and `dkod capture <agent>` per terminal session for each agent the user
runs. The capture command is a long-lived foreground process, so forgetting it
means the session goes uncaptured. This violates dkod's value prop ("every
session, automatically") and forces the user to think about dkod when they should
not have to.

## Goal

After a single `curl … | sh`, every supported agent session on the machine is
captured automatically — for every repo, forever — without further user action.
New agents installed later are detected and added on the next `dkod` invocation.

## Non-goals

- Windows support (V2+). Wizard prints "not yet supported."
- Capturing agents we have no adapter for. Wizard only handles known agents.
- Replacing the existing `dkod capture <agent>` foreground server — it stays as
  a debug/manual mode.

## Six load-bearing decisions

These were made during brainstorming and shape the design.

1. **All-agent unified wizard** — detect every supported agent (Claude Code,
   Codex, Cursor, Gemini CLI, Copilot CLI, OpenCode, Factory AI) and configure
   each via its native mechanism in one flow.
2. **Hybrid install scope** — the user picks user-global (hooks everywhere) or
   per-repo (hooks only in `dkod init`'d repos) at setup time, with a per-repo
   override file.
3. **Self-healing on every `dkod` invocation** — every subcommand runs a sub-ms
   detection pass and prints a one-line nudge if a new agent appears or the
   installed agent set drifts. Auto-fix if `DKOD_AUTO_SETUP=1`. No daemon.
4. **Tiered consent** — silent install for agents with native hook APIs; explicit
   per-agent prompt for agents that require a PATH shim. Per-agent consent
   (`yes` / `no` / `never` / `ask`) persisted across runs.
5. **Inline with install.sh** — the wizard runs at the end of `install.sh` when a
   TTY is present. In non-interactive contexts (CI, piped scripts), install.sh
   only drops the binary and prints "Run `dkod setup` later."
6. **Vault + auto-init for orphan sessions** — a hook firing inside a git repo
   triggers a silent `dkod init` of that repo (one gitignored
   `.dkod/config.toml`) and captures locally; a hook firing outside any git repo
   captures into a personal `~/.dkod/vault/` git repo dkod manages. Nothing is
   silently dropped.

## Architecture

```text
                    ┌─────────────────────────────────────────────┐
                    │  curl -fsSL https://dkod.io/install.sh | sh │
                    └────────────────────┬────────────────────────┘
                                         │
                          install binary; if TTY:
                                         ▼
                              ┌────────────────────┐
                              │   dkod setup       │ ◄──── self-heal calls ────┐
                              │   (interactive)    │       (every `dkod ...`)  │
                              └─────────┬──────────┘                            │
                                        │                                       │
        ┌───────────────┬───────────────┼───────────────┬──────────────┐       │
        ▼               ▼               ▼               ▼              ▼       │
   detect agents   ask scope     install hooks    install shims   write state ─┘
                                 (silent)         (explicit       (~/.dkod/
                                                   consent)        config.toml)
```

Three entry points, one code path: `install.sh`, `dkod setup`, and the
`selfheal::ensure_setup_current()` call at the top of every dkod subcommand.

**At capture time** (independent of setup), each installed hook or shim invokes
`dkod capture-hook --agent <name>`, which routes the session to:

- the current repo's `refs/dkod/sessions/*` if `.dkod/config.toml` exists, or
- a silent `dkod init` of the current repo if it is a git repo without dkod, or
- `~/.dkod/vault/` if the current directory is not a git repo.

## Invariants

- **No session is silently dropped.** Vault catches everything that does not fit
  in a repo.
- **The wizard is idempotent and replayable.** Re-runs are no-ops unless drift is
  detected.
- **Self-heal is cheap.** Sub-ms on warm cache (stat a handful of well-known
  paths, compare a fingerprint hash).
- **Per-agent state is independent.** A failed install for one agent never rolls
  back a successful install for another.
- **We never blindly overwrite a config the user owns.** Unparseable configs are
  refused with a clear message; existing non-dkod hooks at the same event are
  merged into, not replaced.

## Components

New module: `crates/dkod-cli/src/cmd/setup/`

```text
setup/
├── mod.rs              # `dkod setup` subcommand entry, orchestrator
├── detect.rs           # per-agent presence/version detection
├── install.rs          # per-agent install routines
├── consent.rs          # interactive prompts + non-interactive mode
├── state.rs            # read/write ~/.dkod/config.toml + drift fingerprint
├── selfheal.rs         # cheap ensure_setup_current() from every cmd
├── vault.rs            # ensure ~/.dkod/vault exists + initialized
└── install_sh.rs       # --inline / --non-interactive modes for install.sh
```

State file `~/.dkod/config.toml`:

```toml
schema_version = 1

[scope]
default = "per-repo"          # or "user"

[vault]
path = "~/.dkod/vault"

[agent.claude_code]
installed = true
scope = "user"
hook_path = "~/.claude/settings.json"
installed_version = "1.0.123"
fingerprint = "sha256:..."
last_check = "2026-05-25T10:00:00Z"

[agent.cursor]
installed = false
consent = "never"             # yes | no | never | ask
detected_at = "2026-05-25T10:00:00Z"
```

Modified files:

- `crates/dkod-cli/src/cmd/mod.rs` — register `setup` subcommand; wire
  `selfheal::ensure_setup_current()` into the dispatcher.
- `crates/dkod-cli/src/cmd/init.rs` — when `dkod init` runs in a fresh repo,
  also call `setup::install_for_repo()` so per-repo hooks land.
- `crates/dkod-core/src/capture/mod.rs` — add `route_session()` for the
  auto-init / vault routing.
- `install.sh` — append `dkod setup --inline` (TTY) or `--non-interactive`.

No new dependencies. Reuses `gix`, `serde`, `toml`, `clap`, `dirs`, `sha2`.

## Per-agent strategy matrix

Confirmed via Wave 0 spikes (`docs/plans/spikes/`). Six of seven agents support
hooks natively; only Codex still requires a PATH shim.

| Agent | Native API | Install method | Consent | Spike |
|---|---|---|---|---|
| Claude Code | `hooks` in `~/.claude/settings.json` (user) or `.claude/settings.local.json` (repo) | Write `SessionStart`/`Stop`/etc. pointing at `dkod capture-hook --agent claude-code`. Existing code reused from `cmd/capture/claude_code.rs::install_hooks_at_init` | Silent | n/a (existing) |
| OpenCode | `opencode.json` `hooks` block | Write `SessionStart`/`SessionEnd` entries pointing at `dkod capture-hook --agent opencode` | Silent | n/a |
| Copilot CLI | JSON v1 hooks at `~/.copilot/hooks/*.json` (honors `$COPILOT_HOME`) or `.github/hooks/*.json` | Write `~/.copilot/hooks/dkod-capture.json` registering `sessionEnd` + `agentStop` invoking `dkod capture-hook --agent copilot-cli` with `timeoutSec: 5` | Silent | `copilot-cli-hooks.md` |
| Codex (OpenAI) | None | PATH shim `~/.dkod/bin/codex` → `dkod capture codex --` | **Explicit** | n/a |
| Cursor CLI | Partial — `.cursor/hooks.json` with reliable `afterShellExecution`, `afterMCPExecution`, `afterFileEdit` (5 events fire today; lifecycle events not yet wired in `cursor-agent`) | Write `~/.cursor/hooks.json` wiring the 3 working `after*` events to `dkod capture-hook --agent cursor`; derive session boundaries from `conversation_id` + inactivity timeout | Silent (partial) | `cursor-cli-hooks.md` |
| Gemini CLI | `hooks` block in `~/.gemini/settings.json` (user) or `.gemini/settings.json` (repo); default-on since v0.26.0 | Write `SessionStart` + `SessionEnd` entries pointing at `dkod capture-hook --agent gemini-cli`; payload arrives via env (`GEMINI_SESSION_ID`, `GEMINI_CWD`, `GEMINI_PROJECT_DIR`) + stdin JSON | Silent | `gemini-cli-hooks.md` |
| Factory AI (Droid) | Claude-Code-style hooks at `~/.factory/settings.json` and `<project>/.factory/settings.json`; stdin carries `session_id`, `transcript_path`, `cwd`, `hook_event_name` | Additively merge `SessionEnd` hook calling `dkod capture-hook --agent factory-ai` (reads `transcript_path` from stdin and ingests the NDJSON our existing `parse_events` already consumes) | Silent | `factory-ai-hooks.md` |

**Net effect on the wizard UX:** on a clean machine with all seven agents
installed, the wizard installs six silent hook configs and asks one consent
question (for the Codex PATH shim). On most users' machines, zero consent
prompts will appear.

**Shim mechanics** (explicit-consent rows):

1. Wizard prompts: "Codex doesn't support hooks. Install a PATH shim at
   `~/.dkod/bin/codex` and add it to your shell? [y/N/never]"
2. On `y`: write a 3-line shim that `exec`s `dkod capture codex -- "$@"`,
   append one managed block to existing rc files:

   ```sh
   # >>> dkod managed block — do not edit
   export PATH="$HOME/.dkod/bin:$PATH"
   # <<< dkod managed block
   ```

3. Block delimited so `dkod setup --uninstall` removes it surgically.
4. rc files touched: only the ones that exist among `~/.zshrc`, `~/.bashrc`,
   `~/.config/fish/config.fish`.

**Hook installation safety** (silent-consent rows):

- Parse existing config (JSON/YAML/TOML).
- Same dkod fingerprint → no-op.
- Different dkod fingerprint (upgrade/drift) → replace.
- Non-dkod hook at the same event → merge into the array (Claude Code allows
  multiple hooks per event), never overwrite.
- Unparseable → refuse with clear message; never blindly rewrite.

## Capture-time routing

A single `dkod capture-hook --agent <name>` subcommand consumes the hook payload
on stdin and:

1. Reads the current working directory and walks up looking for a `.git`.
2. If a git repo is found:
   - If `.dkod/config.toml` exists → write the session to that repo's
     `refs/dkod/sessions/*`.
   - Else if `[scope].default = "user"` and `[per_repo_overrides].auto_init !=
     false` → silent `dkod init`, then write.
   - Else → write to vault.
3. If no git repo is found → write to vault.

This generalizes the existing `crates/dkod-cli/src/cmd/capture/claude_code.rs::
hook_command` across agents.

## Edge cases

| Situation | Behavior |
|---|---|
| `! [ -t 0 ]` (CI / pipe) | install.sh prints `Run \`dkod setup\` later` and exits 0. `dkod setup --non-interactive` installs only silent-consent agents; explicit-consent ones recorded as `consent = "skipped-noninteractive"`. |
| Self-heal sees a new agent | One-line nudge. Suppressed if `consent = "never"`. Auto-runs if `DKOD_AUTO_SETUP=1`. |
| Self-heal sees an agent uninstalled | Mark `installed = false`; keep hook entry for one cycle (handles temporary uninstalls); then clean up. |
| User edits hook config by hand | Fingerprint mismatch detected. Next dkod run prints `Run \`dkod setup --reconcile\``. Never auto-overwritten. |
| Vault missing or corrupt | `vault::ensure()` creates it. Corrupt → refuse capture with clear error; never silently re-init. |
| Auto-init in unwanted repo | `.dkod/config.toml` line 1 is a comment: `# Created automatically by dkod. Remove this file to disable capture.` Per-repo override available via the hybrid toggle from decision 2. |
| Hook fires mid-upgrade | Hook buffers event to `.dkod/pending/<uuid>.json` first. Next dkod invocation drains buffer. |
| rc file read-only / symlinked | Detect, refuse to write, print exact line for the user to add manually. |
| Two `dkod setup` race | `~/.dkod/.setup.lock` flock — second waits or exits clearly. |
| Windows | Print "not yet supported" and exit; binary works elsewhere. |

## Testing

- Unit tests per detector and installer using `tempfile` for isolated `$HOME`
  (matches the existing pattern in `crates/dkod-core/testdata/`).
- Snapshot tests (`insta`) on generated hook configs per agent.
- Integration test: end-to-end install in temp `$HOME`, simulate hook fire via
  direct `dkod capture-hook`, assert the session lands in the right destination
  (repo vs. vault).
- install.sh test: shellcheck plus a CI matrix run faking TTY (`script`) and
  no-TTY, asserting correct tail behavior.
- Self-heal microbenchmark: `ensure_setup_current()` must return in <5 ms on a
  warm cache.

## Migration

- `dkod capture <agent>` foreground server stays as a debug/manual mode.
- Users who manually wrote hooks pointing at the old `dkod capture-hook`
  continue to work — new code is a superset.
- Issue #6 closed/superseded by this work.

## Open questions for plan time

1. Should the wizard offer to migrate existing manual hook configs into our
   managed block, or always coexist?
2. Which hook events per agent? Claude Code: `SessionStart`, `Stop`,
   `PostToolUse` minimum; also `UserPromptSubmit`?
3. Vault GC — grow forever, or prune sessions older than N days?
4. Telemetry on install — default: no. Confirm.
5. `dkod setup --uninstall` for shared hook arrays: remove only our entry,
   leave the rest intact.

These do not change the architecture. The plan can resolve them cheaply or punt.
