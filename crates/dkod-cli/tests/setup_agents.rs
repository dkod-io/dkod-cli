//! Smoke tests for the per-agent installer trait.
//!
//! Full per-agent install tests land alongside each agent's full
//! implementation in a follow-up PR. This file covers:
//! 1. Detection behavior on an empty home vs. a synthetic one.
//! 2. The Claude Code user-scope install (the silent-consent path).
//! 3. The Codex shim install (the explicit-consent path), against a
//!    scripted prompter so we exercise the consent decision tree.
//! 4. Non-interactive short-circuit for consent agents.

use dkod_cli::cmd::setup::agents::{
    claude_code::ClaudeCode, codex::Codex, AgentInstaller, InstallContext, InstallOutcome,
};
use dkod_cli::cmd::setup::consent::{NonInteractivePrompter, Prompter};
use dkod_cli::cmd::setup::state::{AgentState, Consent, DkodConfig, Scope};
use tempfile::TempDir;

fn empty_agent_state() -> AgentState {
    AgentState {
        installed: false,
        scope: None,
        hook_path: None,
        installed_version: None,
        fingerprint: None,
        last_check: None,
        consent: None,
        detected_at: None,
        extra: Default::default(),
    }
}

fn touch(path: &std::path::Path) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, "{}").unwrap();
}

#[test]
fn claude_code_detect_returns_none_on_empty_home() {
    let home = TempDir::new().unwrap();
    assert!(ClaudeCode.detect(home.path()).is_none());
}

#[test]
fn claude_code_detect_finds_user_settings() {
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".claude/settings.json"));
    let detected = ClaudeCode.detect(home.path()).expect("should detect");
    assert_eq!(detected.name, "claude-code");
}

#[test]
fn claude_code_install_user_scope_writes_hooks_with_sentinel() {
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".claude/settings.json"));
    let mut state = empty_agent_state();
    let mut prompter = NonInteractivePrompter::new("y");
    let mut ctx = InstallContext {
        home: home.path(),
        scope: Scope::User,
        repo_root: None,
        prompter: &mut prompter as &mut dyn Prompter,
        non_interactive: true,
    };
    let outcome = ClaudeCode.install(&mut ctx, &mut state).unwrap();
    assert_eq!(outcome, InstallOutcome::Installed);
    assert!(state.installed);
    assert_eq!(state.scope, Some(Scope::User));

    // The settings.json must now contain a SessionStart hook with the
    // dkod sentinel and our capture-hook command.
    let body = std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    assert!(body.contains("_dkod"), "missing sentinel: {body}");
    assert!(
        body.contains("SessionStart"),
        "missing SessionStart: {body}"
    );
    assert!(
        body.contains("dkod capture-hook"),
        "missing capture-hook command: {body}"
    );
}

#[test]
fn claude_code_install_is_idempotent() {
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".claude/settings.json"));
    let mut prompter = NonInteractivePrompter::new("y");

    for _ in 0..3 {
        let mut state = empty_agent_state();
        let mut ctx = InstallContext {
            home: home.path(),
            scope: Scope::User,
            repo_root: None,
            prompter: &mut prompter as &mut dyn Prompter,
            non_interactive: true,
        };
        ClaudeCode.install(&mut ctx, &mut state).unwrap();
    }

    let body = std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    // Exactly one entry per event in the dkod sentinel set — re-running
    // must not duplicate entries.
    let session_start_dkod_entries = body.matches("\"_dkod\": true").count();
    // 6 events configured in dkod_hook_events.
    assert_eq!(
        session_start_dkod_entries, 6,
        "expected exactly 6 dkod entries (one per event), got {session_start_dkod_entries} in:\n{body}"
    );
}

#[test]
fn codex_install_non_interactive_records_skipped() {
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".codex/config.toml"));
    let mut state = empty_agent_state();
    let mut prompter = NonInteractivePrompter::new("y");
    let mut ctx = InstallContext {
        home: home.path(),
        scope: Scope::User,
        repo_root: None,
        prompter: &mut prompter as &mut dyn Prompter,
        non_interactive: true,
    };
    let outcome = Codex.install(&mut ctx, &mut state).unwrap();
    assert_eq!(outcome, InstallOutcome::SkippedNoninteractive);
    assert_eq!(state.consent, Some(Consent::SkippedNoninteractive));
    assert!(!state.installed);
}

#[test]
fn codex_install_consent_yes_writes_shim() {
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".codex/config.toml"));
    // Pre-seed both .zshrc and .bashrc so we can assert the install
    // touches every shell rc it finds, not just the first.
    std::fs::write(home.path().join(".zshrc"), "# my zsh config\n").unwrap();
    std::fs::write(home.path().join(".bashrc"), "# my bash config\n").unwrap();

    let mut state = empty_agent_state();
    let mut prompter = NonInteractivePrompter::new("y");
    let mut ctx = InstallContext {
        home: home.path(),
        scope: Scope::User,
        repo_root: None,
        prompter: &mut prompter as &mut dyn Prompter,
        non_interactive: false,
    };
    let outcome = Codex.install(&mut ctx, &mut state).unwrap();
    assert_eq!(outcome, InstallOutcome::Installed);

    let shim = home.path().join(".dkod/bin/codex");
    assert!(shim.exists(), "shim was not written");
    let shim_body = std::fs::read_to_string(&shim).unwrap();
    assert!(shim_body.contains("exec dkod capture codex"));

    let zshrc = std::fs::read_to_string(home.path().join(".zshrc")).unwrap();
    assert!(zshrc.contains("dkod managed block"));
    assert!(zshrc.contains("$HOME/.dkod/bin"));

    let bashrc = std::fs::read_to_string(home.path().join(".bashrc")).unwrap();
    assert!(bashrc.contains("dkod managed block"), "bashrc not patched");
    assert!(
        bashrc.contains("$HOME/.dkod/bin"),
        "bashrc missing PATH export"
    );
}

#[test]
fn codex_install_consent_never_records_permanent_decline() {
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".codex/config.toml"));

    let mut state = empty_agent_state();
    let mut prompter = NonInteractivePrompter::new("never");
    let mut ctx = InstallContext {
        home: home.path(),
        scope: Scope::User,
        repo_root: None,
        prompter: &mut prompter as &mut dyn Prompter,
        non_interactive: false,
    };
    let outcome = Codex.install(&mut ctx, &mut state).unwrap();
    assert_eq!(outcome, InstallOutcome::DeclinedByUser);
    assert_eq!(state.consent, Some(Consent::Never));
    assert!(!home.path().join(".dkod/bin/codex").exists());
}

#[test]
fn codex_install_respects_previous_never_without_prompting() {
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".codex/config.toml"));

    let mut state = empty_agent_state();
    state.consent = Some(Consent::Never);

    // PanickyPrompter would explode if asked — use one that returns
    // garbage so we can still construct the ctx, but the install path
    // must short-circuit before consulting it.
    let mut prompter = NonInteractivePrompter::new("y");
    let mut ctx = InstallContext {
        home: home.path(),
        scope: Scope::User,
        repo_root: None,
        prompter: &mut prompter as &mut dyn Prompter,
        non_interactive: false,
    };
    let outcome = Codex.install(&mut ctx, &mut state).unwrap();
    assert_eq!(outcome, InstallOutcome::DeclinedByUser);
    assert!(!home.path().join(".dkod/bin/codex").exists());
}

#[test]
fn unused_config_helpers_smoke_compile() {
    // Belt-and-braces compile check that DkodConfig is still wired into
    // the agents path; the orchestrator (Wave 4) reads state through it.
    let _: DkodConfig = DkodConfig::default();
}
