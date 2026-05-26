//! Integration tests for the `setup::orchestrator::run` end-to-end flow.
//!
//! These drive the orchestrator against a tempdir `HOME` so we can assert
//! on the exact outcomes recorded in `~/.dkod/config.toml` for each
//! agent — without touching the developer's real environment.

use dkod_cli::cmd::setup::{
    agents::InstallOutcome,
    consent::{NonInteractivePrompter, Prompter},
    orchestrator::{run, AgentResult, Options},
    state::{Consent, DkodConfig, Scope},
};
use std::path::PathBuf;
use tempfile::TempDir;

const AGENT_NAMES: [&str; 7] = [
    "claude-code",
    "codex",
    "copilot-cli",
    "cursor",
    "factory-ai",
    "gemini-cli",
    "opencode",
];

fn touch(path: &std::path::Path) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, "{}").unwrap();
}

fn make_opts<'a>(
    home: &TempDir,
    config: &TempDir,
    vault: &TempDir,
    prompter: Option<&'a mut dyn Prompter>,
    non_interactive: bool,
) -> Options<'a> {
    Options {
        home: home.path().to_path_buf(),
        scope: Scope::User,
        repo_root: None,
        non_interactive,
        config_path: Some(config.path().join("config.toml")),
        vault_path: Some(vault.path().to_path_buf()),
        prompter,
    }
}

fn outcome_for(results: &[AgentResult], name: &str) -> InstallOutcome {
    results
        .iter()
        .find(|r| r.name == name)
        .map(|r| r.outcome.clone())
        .unwrap_or_else(|| panic!("missing result for {name}"))
}

#[test]
fn empty_home_reports_every_agent_as_not_present() {
    let home = TempDir::new().unwrap();
    let config_dir = TempDir::new().unwrap();
    let vault_dir = TempDir::new().unwrap();

    // Vault dir from TempDir already exists and is empty — vault::ensure
    // accepts that. We point at a subpath so the orchestrator initialises
    // it as a fresh git repo.
    let vault_sub = vault_dir.path().join("vault");
    let opts = Options {
        vault_path: Some(vault_sub.clone()),
        ..make_opts(&home, &config_dir, &vault_dir, None, true)
    };
    let summary = run(opts).unwrap();

    assert_eq!(summary.agents.len(), AGENT_NAMES.len());
    for name in AGENT_NAMES {
        assert_eq!(
            outcome_for(&summary.agents, name),
            InstallOutcome::AgentNotPresent,
            "empty home: {name} should be AgentNotPresent"
        );
    }
    assert!(
        summary.config_path.exists(),
        "config.toml should be written on save"
    );
    assert!(vault_sub.exists(), "vault must be created");
}

#[test]
fn claude_code_only_installs_claude_code() {
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".claude/settings.json"));
    let config_dir = TempDir::new().unwrap();
    let vault_dir = TempDir::new().unwrap();
    let mut prompter = NonInteractivePrompter::new("y");

    let opts = make_opts(
        &home,
        &config_dir,
        &vault_dir,
        Some(&mut prompter as &mut dyn Prompter),
        true,
    );
    let summary = run(opts).unwrap();

    assert_eq!(
        outcome_for(&summary.agents, "claude-code"),
        InstallOutcome::Installed
    );
    for name in AGENT_NAMES.iter().filter(|n| **n != "claude-code") {
        assert_eq!(
            outcome_for(&summary.agents, name),
            InstallOutcome::AgentNotPresent,
            "only claude-code should install: but {name} != AgentNotPresent"
        );
    }
    // Settings file should now carry the dkod sentinel.
    let settings = std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    assert!(settings.contains("_dkod"));
}

#[test]
fn codex_present_with_non_interactive_records_skipped() {
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".codex/config.toml"));
    let config_dir = TempDir::new().unwrap();
    let vault_dir = TempDir::new().unwrap();

    let opts = make_opts(&home, &config_dir, &vault_dir, None, true);
    let summary = run(opts).unwrap();

    assert_eq!(
        outcome_for(&summary.agents, "codex"),
        InstallOutcome::SkippedNoninteractive
    );

    // Verify the persisted state carries consent = SkippedNoninteractive.
    let cfg = DkodConfig::load_or_default(&summary.config_path).unwrap();
    let codex_state = cfg.agents.get("codex").expect("codex state recorded");
    assert_eq!(codex_state.consent, Some(Consent::SkippedNoninteractive));
    assert!(!codex_state.installed);
}

#[test]
fn previous_never_consent_is_preserved_across_runs() {
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".codex/config.toml"));
    let config_dir = TempDir::new().unwrap();
    let vault_dir = TempDir::new().unwrap();
    let mut prompter = NonInteractivePrompter::new("never");

    // First run: with a "never" prompter, Codex records permanent decline.
    let opts1 = make_opts(
        &home,
        &config_dir,
        &vault_dir,
        Some(&mut prompter as &mut dyn Prompter),
        false,
    );
    let first = run(opts1).unwrap();
    assert_eq!(
        outcome_for(&first.agents, "codex"),
        InstallOutcome::DeclinedByUser
    );

    // Second run: even with a "y" prompter, the stored Never should be
    // respected — the installer must short-circuit before consulting the
    // prompter.
    let mut yes = NonInteractivePrompter::new("y");
    let opts2 = make_opts(
        &home,
        &config_dir,
        &vault_dir,
        Some(&mut yes as &mut dyn Prompter),
        false,
    );
    let second = run(opts2).unwrap();
    assert_eq!(
        outcome_for(&second.agents, "codex"),
        InstallOutcome::DeclinedByUser
    );

    let cfg = DkodConfig::load_or_default(&second.config_path).unwrap();
    assert_eq!(
        cfg.agents.get("codex").and_then(|s| s.consent),
        Some(Consent::Never)
    );
}

#[test]
fn stub_agents_report_not_yet_implemented_when_present() {
    // Drop a settings file that the cursor stub recognises in detect()
    // (the stub returns NotYetImplemented when invoked).
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".cursor/hooks.json"));
    let config_dir = TempDir::new().unwrap();
    let vault_dir = TempDir::new().unwrap();

    let opts = make_opts(&home, &config_dir, &vault_dir, None, true);
    let summary = run(opts).unwrap();
    assert_eq!(
        outcome_for(&summary.agents, "cursor"),
        InstallOutcome::NotYetImplemented
    );
}

#[test]
fn rerunning_is_idempotent() {
    // Claude Code install with three back-to-back runs: state and
    // settings should match across runs.
    let home = TempDir::new().unwrap();
    touch(&home.path().join(".claude/settings.json"));
    let config_dir = TempDir::new().unwrap();
    let vault_dir = TempDir::new().unwrap();

    fn dkod_count_in_settings(home: &std::path::Path) -> usize {
        let body = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
        body.matches("\"_dkod\": true").count()
    }

    for _ in 0..3 {
        let mut prompter = NonInteractivePrompter::new("y");
        let opts = make_opts(
            &home,
            &config_dir,
            &vault_dir,
            Some(&mut prompter as &mut dyn Prompter),
            true,
        );
        let _ = run(opts).unwrap();
    }

    // 6 dkod hook entries (one per event the installer registers).
    assert_eq!(dkod_count_in_settings(home.path()), 6);
}

#[test]
fn config_path_defaults_to_caller_supplied_path() {
    // Sanity: when Options::config_path is Some, the orchestrator writes
    // there (we already implicitly rely on this in every other test, but
    // make it explicit so a future refactor catches the contract).
    let home = TempDir::new().unwrap();
    let config_dir = TempDir::new().unwrap();
    let vault_dir = TempDir::new().unwrap();
    let custom = config_dir.path().join("custom.toml");

    let opts = Options {
        home: home.path().to_path_buf(),
        scope: Scope::User,
        repo_root: None,
        non_interactive: true,
        config_path: Some(custom.clone()),
        vault_path: Some(vault_dir.path().join("vault")),
        prompter: None,
    };
    let summary = run(opts).unwrap();
    assert_eq!(summary.config_path, custom);
    assert!(custom.exists());
}

#[test]
fn format_summary_renders_each_agent_line() {
    use dkod_cli::cmd::setup::orchestrator::{format_summary, WizardSummary};
    let summary = WizardSummary {
        agents: AGENT_NAMES
            .iter()
            .map(|n| AgentResult {
                name: n,
                outcome: InstallOutcome::AgentNotPresent,
            })
            .collect(),
        config_path: PathBuf::from("/tmp/cfg"),
        vault_path: PathBuf::from("/tmp/vault"),
    };
    let rendered = format_summary(&summary);
    assert!(rendered.contains("/tmp/cfg"));
    assert!(rendered.contains("/tmp/vault"));
    for name in AGENT_NAMES {
        assert!(rendered.contains(name), "rendered missing {name}");
    }
}
