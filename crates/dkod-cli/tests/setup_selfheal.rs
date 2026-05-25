//! Tests for `cmd::setup::selfheal` — fast drift detection.

use dkod_cli::cmd::setup::selfheal::{ensure_setup_current, SelfHealResult};
use dkod_cli::cmd::setup::state::{AgentState, DkodConfig};
use std::collections::BTreeMap;
use std::time::Instant;
use tempfile::TempDir;

fn empty_home() -> TempDir {
    TempDir::new().unwrap()
}

fn touch(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, "{}").unwrap();
}

#[test]
fn empty_config_and_empty_home_reports_never_installed() {
    let home = empty_home();
    let config = DkodConfig::default();
    assert_eq!(
        ensure_setup_current(&config, home.path()),
        SelfHealResult::NeverInstalled
    );
}

#[test]
fn new_agent_on_disk_but_not_tracked_reports_drift() {
    let home = empty_home();
    touch(&home.path().join(".cursor/hooks.json"));

    let config = DkodConfig::default();
    let result = ensure_setup_current(&config, home.path());
    match result {
        SelfHealResult::DriftDetected(agents) => {
            assert_eq!(agents, vec!["cursor".to_string()]);
        }
        other => panic!("expected DriftDetected, got {other:?}"),
    }
}

#[test]
fn tracked_agent_with_present_path_is_clean() {
    let home = empty_home();
    touch(&home.path().join(".claude/settings.json"));

    let mut config = DkodConfig::default();
    let mut agents = BTreeMap::new();
    agents.insert(
        "claude-code".to_string(),
        AgentState {
            installed: true,
            scope: None,
            hook_path: None,
            installed_version: None,
            fingerprint: None,
            last_check: None,
            consent: None,
            detected_at: None,
            extra: Default::default(),
        },
    );
    config.agents = agents;

    assert_eq!(
        ensure_setup_current(&config, home.path()),
        SelfHealResult::Clean
    );
}

#[test]
fn tracked_agent_with_missing_path_reports_drift() {
    let home = empty_home();
    // No config files touched — agent appears uninstalled.

    let mut config = DkodConfig::default();
    let mut agents = BTreeMap::new();
    agents.insert(
        "claude-code".to_string(),
        AgentState {
            installed: true,
            scope: None,
            hook_path: None,
            installed_version: None,
            fingerprint: None,
            last_check: None,
            consent: None,
            detected_at: None,
            extra: Default::default(),
        },
    );
    config.agents = agents;

    let result = ensure_setup_current(&config, home.path());
    match result {
        SelfHealResult::DriftDetected(names) => {
            assert!(names.contains(&"claude-code".to_string()));
        }
        other => panic!("expected drift for uninstalled tracked agent, got {other:?}"),
    }
}

#[test]
fn drift_list_is_sorted_and_deduped() {
    let home = empty_home();
    touch(&home.path().join(".cursor/hooks.json"));
    touch(&home.path().join(".claude/settings.json"));
    touch(&home.path().join(".gemini/settings.json"));

    let config = DkodConfig::default();
    let result = ensure_setup_current(&config, home.path());
    match result {
        SelfHealResult::DriftDetected(names) => {
            assert_eq!(
                names,
                vec![
                    "claude-code".to_string(),
                    "cursor".to_string(),
                    "gemini-cli".to_string()
                ]
            );
        }
        other => panic!("expected DriftDetected with 3 agents, got {other:?}"),
    }
}

#[test]
fn ensure_setup_current_is_fast_on_warm_cache() {
    // Microbenchmark gate — 1000 calls against a populated config + tmp
    // home must complete in well under 50ms. This is the function we'll
    // call on the hot path of every `dkod` invocation; if it ever creeps
    // toward shelling out or parsing, this test should catch it.
    let home = empty_home();
    touch(&home.path().join(".claude/settings.json"));
    touch(&home.path().join(".cursor/hooks.json"));

    let mut config = DkodConfig::default();
    let mut agents = BTreeMap::new();
    for name in ["claude-code", "cursor"] {
        agents.insert(
            name.to_string(),
            AgentState {
                installed: true,
                scope: None,
                hook_path: None,
                installed_version: None,
                fingerprint: None,
                last_check: None,
                consent: None,
                detected_at: None,
                extra: Default::default(),
            },
        );
    }
    config.agents = agents;

    let start = Instant::now();
    for _ in 0..1000 {
        let _ = ensure_setup_current(&config, home.path());
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 50,
        "1000 calls took {elapsed:?}; selfheal must stay sub-50µs per call on warm cache"
    );
}
