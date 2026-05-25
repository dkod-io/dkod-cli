# Seamless capture wizard — implementation plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement the seamless capture wizard so that `curl install.sh | sh` results in every supported agent session being captured automatically, with no further user action.

**Architecture:** New `setup` module in `dkod-cli` exposing `dkod setup` plus a `selfheal::ensure_setup_current()` called from every subcommand. Per-agent detectors and installers (silent hook installs vs. explicit-consent PATH shims). State persisted to `~/.dkod/config.toml`. New `dkod capture-hook` routing generalized across agents with auto-init / vault fallback.

**Tech Stack:** Rust (edition 2021), `gix`, `serde`, `toml`, `clap`, `dirs`, `sha2`, `tempfile`, `insta`. No new dependencies.

**Reference:** `docs/plans/2026-05-25-seamless-capture-wizard-design.md` is the source of truth for decisions.

---

## Wave structure

The plan is organized in waves. Tasks within a wave are independent and may be dispatched in parallel via `superpowers:subagent-driven-development`. Waves are sequential — wave N+1 depends on wave N.

- **Wave 0:** Branch + spikes (sequential — informs the per-agent matrix)
- **Wave 1:** Foundation modules (parallelizable: state, vault, consent, selfheal)
- **Wave 2:** Per-agent detectors + installers (parallelizable: one per agent)
- **Wave 3:** Capture-time routing (`capture-hook` generalization)
- **Wave 4:** Wizard orchestrator + `dkod setup` subcommand
- **Wave 5:** install.sh integration + init.rs hook
- **Wave 6:** Integration tests + microbenchmark
- **Wave 7:** Pre-PR CodeRabbit pass + PR open + review loop until merged
- **Wave 8:** Smoke test post-merge

## Hard rules for every commit / subagent

Paste these verbatim into every subagent prompt:

1. **Git identity:** every commit and push uses `git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' commit ...`. No `Co-Authored-By` lines, ever.
2. **Pre-commit CodeRabbit:** before every commit that contains code, run `/coderabbit:review uncommitted`, resolve findings, then commit. For docs-only commits, note in the commit message that CodeRabbit does not meaningfully review docs.
3. **Post-commit CodeRabbit:** after every code commit, run `/coderabbit:review committed`.
4. **Pre-PR CodeRabbit:** before opening any PR, run `/coderabbit:review --base main`.
5. **Cargo gates:** every commit must pass `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all -- --check`.
6. **TDD:** write the failing test first, run to verify it fails, implement, run to verify it passes, then commit.
7. **YAGNI:** implement only what this plan calls for. No speculative abstractions.

---

## Wave 0 — Branch and spikes

### Task 0.1: Create feature branch

**Step 1:** `cd /Users/haimari/vsCode/haim-ari/github/dkod-cli`
**Step 2:** `git checkout -b feat/seamless-capture-wizard`
**Step 3:** Verify: `git status` shows clean working tree on the new branch.

### Task 0.2: Spike — Copilot CLI hook surface

**Goal:** Determine whether GitHub Copilot CLI exposes a hook API. If yes, capture exact config path and hook event names. If no, mark as shim-only.

**Steps:**
1. Read `crates/dkod-core/src/capture/copilot_cli.rs` and `crates/dkod-core/testdata/copilot_cli/` to see what the current adapter already knows.
2. Check `~/.copilot/`, `~/.config/copilot/`, `~/.config/gh-copilot/` for config files on this machine if present.
3. WebFetch `https://docs.github.com/en/copilot/github-copilot-in-the-cli` for hook documentation.
4. Record findings in `docs/plans/spikes/copilot-cli-hooks.md` with one of: `HOOKS-SUPPORTED`, `SHIM-ONLY`, `UNKNOWN`.
5. Commit: `chore(spike): record copilot cli hook surface`

### Task 0.3: Spike — Gemini CLI hook surface

Same shape as 0.2 but for Gemini CLI. Source: `crates/dkod-core/src/capture/gemini_cli.rs`, `crates/dkod-core/testdata/gemini_cli/`, `https://ai.google.dev/gemini-api/docs/cli`. Output: `docs/plans/spikes/gemini-cli-hooks.md`.

### Task 0.4: Spike — Factory AI (Droid) hook surface

Same shape. Source: `crates/dkod-core/src/capture/factory_ai.rs`, `crates/dkod-core/testdata/factory_ai/`, and Factory AI's public docs. Output: `docs/plans/spikes/factory-ai-hooks.md`.

### Task 0.5: Spike — Cursor CLI hook surface

Same shape. Source: `crates/dkod-core/src/capture/cursor.rs`, `crates/dkod-core/testdata/cursor/`, Cursor's CLI docs. Output: `docs/plans/spikes/cursor-cli-hooks.md`.

**Wave 0 gate:** Update the per-agent matrix in `docs/plans/2026-05-25-seamless-capture-wizard-design.md` with confirmed install methods. Commit as `docs: confirm per-agent install methods after spikes`.

---

## Wave 1 — Foundation modules

These four modules are independent and parallelizable.

### Task 1.1: `setup/state.rs` — config file read/write + fingerprint

**Files:**
- Create: `crates/dkod-cli/src/cmd/setup/mod.rs` (skeleton: `pub mod state;`)
- Create: `crates/dkod-cli/src/cmd/setup/state.rs`
- Create: `crates/dkod-cli/tests/setup_state.rs`

**Types to implement:**

```rust
pub struct DkodConfig {
    pub schema_version: u32,           // = 1
    pub scope: ScopeConfig,
    pub vault: VaultConfig,
    pub agents: BTreeMap<String, AgentState>,
}

pub struct ScopeConfig { pub default: Scope }
pub enum Scope { User, PerRepo }

pub struct VaultConfig { pub path: PathBuf }

pub struct AgentState {
    pub installed: bool,
    pub scope: Option<Scope>,
    pub hook_path: Option<PathBuf>,
    pub installed_version: Option<String>,
    pub fingerprint: Option<String>,
    pub last_check: Option<String>,        // RFC3339
    pub consent: Option<Consent>,
    pub detected_at: Option<String>,
}
pub enum Consent { Yes, No, Never, Ask, SkippedNoninteractive }
```

**Public API:**
- `DkodConfig::load_or_default(path: &Path) -> Result<Self>`
- `DkodConfig::save(&self, path: &Path) -> Result<()>` (atomic: write `.tmp`, rename)
- `DkodConfig::default_path() -> Result<PathBuf>` (returns `~/.dkod/config.toml`)
- `fingerprint(bytes: &[u8]) -> String` (SHA-256 hex, `"sha256:..."`)

**Tests (TDD — write first):**
1. `load_or_default` on a missing file returns a default with `schema_version = 1`.
2. Round-trip: save then load returns the same struct.
3. Atomic save: simulated mid-write crash leaves the original file intact.
4. Fingerprint determinism: same bytes → same string.
5. Unknown TOML keys are preserved on round-trip (use `#[serde(flatten)]` or a raw `toml::Value` carrier so future schema versions don't lose user data).

**Per-step protocol:** TDD as in the hard rules. Commit after green.

### Task 1.2: `setup/vault.rs` — personal vault repo manager

**Files:**
- Create: `crates/dkod-cli/src/cmd/setup/vault.rs`
- Create: `crates/dkod-cli/tests/setup_vault.rs`

**Public API:**
- `pub fn ensure(path: &Path) -> Result<()>` — if missing, init a bare-ish git repo at the path with gitoxide. If present and a valid git repo, no-op. If present and corrupt, return a `VaultCorrupt` error.
- `pub fn is_initialized(path: &Path) -> bool`
- `pub fn write_session_blob(repo: &Path, session_id: &str, blob: &[u8]) -> Result<()>` — writes to `refs/dkod/sessions/<id>` using gix.

**Tests:**
1. `ensure` creates the directory and initializes a git repo.
2. `ensure` on an already-initialized repo is a no-op.
3. `ensure` on a non-empty non-git directory returns `VaultCorrupt` (refuses to clobber).
4. `write_session_blob` round-trips: write a blob, then read it back via gix.

### Task 1.3: `setup/consent.rs` — interactive prompts + non-interactive mode

**Files:**
- Create: `crates/dkod-cli/src/cmd/setup/consent.rs`
- Create: `crates/dkod-cli/tests/setup_consent.rs`

**Public API:**
- `pub trait Prompter { fn ask(&mut self, question: &str, choices: &[&str]) -> Result<String>; }`
- `pub struct StdinPrompter;` (implements `Prompter` from stdin/stdout)
- `pub struct NonInteractivePrompter { default: String };` (always returns the default; for CI)
- `pub fn is_tty() -> bool` (wraps `atty` or equivalent — but stick to stdlib: check `io::stdin().is_terminal()`, available since Rust 1.70).

**Tests:**
1. `NonInteractivePrompter` returns the default regardless of input.
2. `StdinPrompter` with a mocked reader returns the user's choice.
3. Invalid input loops up to 3 times then errors.

### Task 1.4: `setup/selfheal.rs` — cheap drift check

**Files:**
- Create: `crates/dkod-cli/src/cmd/setup/selfheal.rs`
- Create: `crates/dkod-cli/tests/setup_selfheal.rs`

**Public API:**
- `pub fn ensure_setup_current(config: &DkodConfig) -> SelfHealResult` returning one of `Clean`, `DriftDetected(Vec<String>)`, `NeverInstalled`.
- Performs only `Path::exists()` checks against a fixed list of well-known agent config paths (no parsing).

**Tests:**
1. With no known agents present → returns `NeverInstalled` if config empty, else `Clean`.
2. With a new agent path present that isn't in config → `DriftDetected([agent_name])`.
3. Microbenchmark test: 1000 calls complete in under 50 ms (50 µs each warm). Use `std::time::Instant` rather than a bench framework — keep it portable.

**Wave 1 gate:** All four modules merged to the feature branch, each with green tests, each commit CodeRabbit-clean.

---

## Wave 2 — Per-agent detector + installer

Two tasks per agent, both run in parallel across agents. Skip rows the spikes marked `SHIM-ONLY` for the hook-installer task and vice versa. Each agent's pair of tasks (detect + install) is sequential within the agent but parallel across agents.

### Task template — Agent X detect + install

**Files (template):**
- Create: `crates/dkod-cli/src/cmd/setup/agents/<agent>.rs`
- Create: `crates/dkod-cli/tests/setup_agent_<agent>.rs`
- Modify: `crates/dkod-cli/src/cmd/setup/agents/mod.rs` (add `pub mod <agent>;`)

**Public API per agent file:**
- `pub fn detect() -> Option<DetectedAgent>` — returns presence + version + config path.
- `pub fn install_hook(state: &mut AgentState, scope: Scope) -> Result<()>` (silent rows only).
- `pub fn install_shim(state: &mut AgentState, prompter: &mut dyn Prompter) -> Result<()>` (shim rows only).
- `pub fn uninstall(state: &mut AgentState) -> Result<()>`.

**Tests (per agent):**
1. `detect` with the agent's well-known config absent → `None`.
2. `detect` with a synthetic config in a temp `$HOME` → `Some(...)`.
3. `install_hook` (silent rows): writes a hook block, second call is idempotent (fingerprint matches), produces a snapshot-test output (use `insta`).
4. `install_hook` merging: when a non-dkod hook exists at the same event, our hook is appended, theirs is preserved (snapshot).
5. `install_hook` refusal: when the existing config is unparseable, returns a `ConfigCorrupt` error and writes nothing.
6. `install_shim` (shim rows): writes `~/.dkod/bin/<agent>`, appends managed block to `~/.zshrc` (in temp `$HOME`), is idempotent on second call.
7. `uninstall` removes our entry without touching others (snapshot).

**Agent list (each gets the template above):**
- 2.1 — `claude_code` (silent)
- 2.2 — `opencode` (silent)
- 2.3 — `copilot_cli` (silent or shim per spike 0.2)
- 2.4 — `codex` (shim)
- 2.5 — `cursor` (shim or hook per spike 0.5)
- 2.6 — `gemini_cli` (per spike 0.3)
- 2.7 — `factory_ai` (per spike 0.4)

**Wave 2 gate:** Snapshot tests reviewed and committed. All agents pass `cargo test`.

---

## Wave 3 — Capture-time routing

### Task 3.1: `dkod-core::capture::route_session`

**Files:**
- Modify: `crates/dkod-core/src/capture/mod.rs`
- Create: `crates/dkod-core/src/capture/route.rs`
- Test: `crates/dkod-core/tests/capture_route.rs`

**Public API:**
- `pub enum RouteDestination { Repo(PathBuf), Vault(PathBuf), AutoInitThenRepo(PathBuf) }`
- `pub fn route_session(cwd: &Path, config: &DkodConfig) -> RouteDestination`

**Rules (verbatim from design):**
1. Walk up from `cwd` looking for `.git`.
2. If found and `.dkod/config.toml` exists in the same dir → `Repo(...)`.
3. If found and `config.scope.default == Scope::User` and the per-repo override does not opt out → `AutoInitThenRepo(...)`.
4. Else → `Vault(config.vault.path)`.

**Tests:** one per branch above, using `tempfile` to construct synthetic directory trees.

### Task 3.2: Generalize `dkod capture-hook` across agents

**Files:**
- Modify: `crates/dkod-cli/src/cmd/capture/mod.rs`
- Modify: `crates/dkod-cli/src/cmd/capture/claude_code.rs` (extract reusable bits)
- Test: `crates/dkod-cli/tests/capture_hook.rs`

**Behavior:**
- `dkod capture-hook --agent <name>` reads the hook payload on stdin, normalizes it via the agent's existing adapter, calls `route_session`, and writes the resulting blob via either `gix` (repo case) or `vault::write_session_blob` (vault case). For `AutoInitThenRepo`, call into `cmd::init` first.
- On any write failure, buffer the raw payload under `.dkod/pending/<uuid>.json` (or `~/.dkod/pending/` for the vault case) and exit 0. (No data loss — exit 0 so the host agent doesn't surface scary errors.)

**Tests:**
1. Repo case: hook payload → session ref in temp git repo.
2. Vault case: hook payload in a non-git tmp dir → session in vault.
3. AutoInit case: hook payload in a git repo without `.dkod/` → repo gets a `.dkod/config.toml` then session lands in it.
4. Failure-to-buffer: simulated write error → payload appears under `pending/`.

---

## Wave 4 — Wizard orchestrator

### Task 4.1: `setup::install` orchestrator

**Files:**
- Create: `crates/dkod-cli/src/cmd/setup/install.rs`
- Test: `crates/dkod-cli/tests/setup_install.rs`

**Public API:**
- `pub struct WizardOptions { pub scope: Scope, pub non_interactive: bool, pub auto_yes: bool }`
- `pub fn run(opts: WizardOptions, prompter: &mut dyn Prompter, config_path: &Path) -> Result<WizardReport>`
- `pub struct WizardReport { pub installed: Vec<String>, pub skipped: Vec<(String, String)>, pub errors: Vec<(String, String)> }`

**Flow:**
1. Load or default `DkodConfig`.
2. Ensure vault.
3. If `scope` not yet set, ask the user (skipped in `non_interactive`).
4. For each agent: detect → if absent, skip → if present, check consent → install (hook or shim per agent module).
5. Per-agent failures recorded in `errors`, do not abort.
6. Save updated config.
7. Print a one-page report.

**Tests:**
1. Empty machine: report shows 0 installed, 0 errors.
2. Synthetic Claude Code only: report shows 1 installed (claude_code), 0 errors.
3. Non-interactive mode with a shim-required agent: agent recorded as `SkippedNoninteractive`, no rc-file writes.
4. Re-run is idempotent: second run installs nothing new.

### Task 4.2: `dkod setup` subcommand wiring

**Files:**
- Modify: `crates/dkod-cli/src/cmd/setup/mod.rs` (add the `Setup` clap subcommand)
- Modify: `crates/dkod-cli/src/cmd/mod.rs` (register `setup`)
- Modify: `crates/dkod-cli/src/main.rs` (dispatch + call `selfheal::ensure_setup_current()` at the top of every subcommand other than `setup`)

**Clap surface:**
```
dkod setup [--non-interactive] [--scope user|per-repo] [--reconcile] [--uninstall]
```

**Tests (cli-level, via `assert_cmd`):**
1. `dkod setup --non-interactive` on a clean tmp `$HOME` exits 0.
2. `dkod log` on a clean tmp `$HOME` prints the self-heal nudge (since no setup state exists) but still runs.
3. `DKOD_AUTO_SETUP=1 dkod log` on a clean tmp `$HOME` auto-runs setup before continuing.

---

## Wave 5 — install.sh + dkod init integration

### Task 5.1: install.sh tail call

**Files:**
- Modify: `install.sh`

**Behavior:**
- After `dkod` binary lands in PATH, detect TTY: `if [ -t 0 ] && [ -t 1 ]`.
- TTY case: `exec dkod setup --inline` (or run inline and continue).
- Non-TTY: print "dkod installed. Run `dkod setup` to enable automatic capture."

**Tests:**
- `shellcheck install.sh` passes.
- CI matrix runs install.sh with TTY faked (via `script -q -c`) and without; both produce expected output. Add a new workflow in `.github/workflows/install-sh.yml` or extend the existing CI to cover this.

### Task 5.2: `dkod init` integration

**Files:**
- Modify: `crates/dkod-cli/src/cmd/init.rs`

**Behavior:**
- After existing init steps, call `setup::install::install_for_repo(repo_root, &mut config)` to write per-repo hooks where appropriate.
- Skip silently if user hasn't run `dkod setup` yet (the self-heal nudge handles that).

**Tests:** existing `dkod init` tests stay green; add one new test that asserts a per-repo hook file is created when global setup is already complete.

---

## Wave 6 — Integration tests + microbenchmark

### Task 6.1: End-to-end smoke test (in-repo)

**Files:**
- Create: `crates/dkod-cli/tests/e2e_seamless_capture.rs`

**Scenario:**
1. Create temp `$HOME`.
2. Run `dkod setup --non-interactive --scope per-repo`.
3. Create a fresh git repo in `$TMPDIR`.
4. Run `dkod init` in that repo.
5. Pipe a synthetic Claude Code hook payload into `dkod capture-hook --agent claude-code`.
6. Assert: a session ref exists under `refs/dkod/sessions/*` in the repo.

### Task 6.2: End-to-end smoke test (vault)

Same as 6.1 but step 3 uses a non-git `$TMPDIR`. Assert the session lands in `~/.dkod/vault/`.

### Task 6.3: Self-heal microbenchmark

**Files:**
- Create: `crates/dkod-cli/tests/selfheal_perf.rs`

**Behavior:** 1000 calls to `ensure_setup_current` against a fully populated config complete in under 50 ms total.

---

## Wave 7 — PR open + CodeRabbit loop until clean

### Task 7.1: Push branch and open PR

1. `git -c user.name='haim-ari' -c user.email='haimari1@gmail.com' push -u origin feat/seamless-capture-wizard`
2. Run `/coderabbit:review --base main` locally first; fix anything actionable and push again.
3. `gh pr create --title "feat: seamless capture wizard" --body "$(cat <<'EOF'\n## Summary\n<3 bullets>\n\n## Test plan\n- [ ] cargo test\n- [ ] cargo clippy --all-targets -- -D warnings\n- [ ] install.sh shellcheck + TTY/no-TTY CI runs\n- [ ] e2e smoke (in-repo)\n- [ ] e2e smoke (vault)\nEOF\n)"`

### Task 7.2: CodeRabbit server-side review loop

Repeat until clean:
1. Wait for the CodeRabbit GitHub action to post its review.
2. Fix every actionable finding.
3. Commit + push.
4. Wait for re-review.

Do NOT merge until the latest CodeRabbit pass has zero open actionable findings.

### Task 7.3: Merge

Once clean and CI green: `gh pr merge --squash --delete-branch` (or rebase if the repo convention is rebase — check `git log --oneline -5 origin/main` for the prevailing pattern). Use the personal git identity if any local commits are amended pre-merge.

---

## Wave 8 — Post-merge smoke test

### Task 8.1: Real install + hook fire

1. In a fresh temp `$HOME`, run `bash install.sh` from the merged main.
2. Confirm `dkod setup` runs, all detected agents installed.
3. Trigger a real Claude Code session in a fresh git repo.
4. Run `dkod log` and assert the session appears.
5. If the smoke test reveals a regression, file a follow-up issue (do not silently fix on main — preserve audit trail).

---

## Open items deferred (do not implement here)

These were listed as open questions in the design; implement only if the answer becomes obvious during execution, otherwise file a follow-up issue:

- Vault GC policy
- Telemetry on install
- `dkod setup --uninstall` cleanup of shared hook arrays beyond our entry
- Hook event coverage tuning per agent (start with `SessionStart` + `Stop` + `PostToolUse` for Claude Code; widen later)
