//! Tests for `cmd::setup::state` — the seamless capture wizard's data
//! model and on-disk I/O for `~/.dkod/config.toml`.
//!
//! Each test exercises one contract from the wizard plan (Task 1.1):
//! 1. Missing-file default returns `schema_version = 1`.
//! 2. Round-trip equality through save/load.
//! 3. Atomic save leaves no `.tmp` sibling behind on success.
//! 4. Fingerprint determinism.
//! 5. Fingerprint string format.
//! 6. Unknown TOML keys survive a load/save/load round-trip so future
//!    schema versions don't silently drop user data.

use dkod_cli::cmd::setup::state::{
    fingerprint, AgentState, Consent, DkodConfig, Scope, ScopeConfig, VaultConfig,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tempfile::TempDir;

fn sample_config(vault_path: PathBuf) -> DkodConfig {
    let mut agents = BTreeMap::new();
    agents.insert(
        "claude-code".to_string(),
        AgentState {
            installed: true,
            scope: Some(Scope::User),
            hook_path: Some(PathBuf::from("/home/u/.claude/hooks/dkod.sh")),
            installed_version: Some("0.1.1".to_string()),
            fingerprint: Some("sha256:deadbeef".to_string()),
            last_check: Some("2026-05-25T12:00:00Z".to_string()),
            consent: Some(Consent::Yes),
            detected_at: Some("2026-05-25T11:59:00Z".to_string()),
            extra: Default::default(),
        },
    );
    DkodConfig {
        schema_version: 1,
        scope: ScopeConfig {
            default: Scope::User,
            extra: Default::default(),
        },
        vault: VaultConfig {
            path: vault_path,
            extra: Default::default(),
        },
        agents,
        extra: Default::default(),
    }
}

#[test]
fn load_or_default_returns_default_when_missing() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("config.toml");
    assert!(!path.exists());

    let cfg = DkodConfig::load_or_default(&path).unwrap();
    assert_eq!(cfg.schema_version, 1);
    assert!(cfg.agents.is_empty());
}

#[test]
fn save_then_load_round_trips() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("config.toml");
    let original = sample_config(tmp.path().join("vault"));

    original.save(&path).unwrap();
    let reloaded = DkodConfig::load_or_default(&path).unwrap();

    assert_eq!(original, reloaded);
}

#[test]
fn save_leaves_no_tmp_file_behind() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("nested/config.toml");
    let cfg = sample_config(tmp.path().join("vault"));

    cfg.save(&path).unwrap();
    assert!(path.exists(), "target file must exist after save");

    // Atomic save: the `.tmp` sibling must be renamed away, never
    // left lying around for the next process to trip over.
    let tmp_sibling = path.with_extension("toml.tmp");
    assert!(
        !tmp_sibling.exists(),
        "leftover tmp file at {tmp_sibling:?} — save was not atomic"
    );

    // Belt-and-braces: scan the parent dir for any *.tmp leftover.
    for entry in std::fs::read_dir(path.parent().unwrap()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(
            !name.ends_with(".tmp"),
            "unexpected leftover tmp file: {name}"
        );
    }
}

#[test]
fn fingerprint_is_deterministic() {
    let bytes = b"hello dkod";
    let a = fingerprint(bytes);
    let b = fingerprint(bytes);
    assert_eq!(a, b);
}

#[test]
fn fingerprint_format_is_sha256_prefix_plus_64_hex() {
    let fp = fingerprint(b"anything");
    assert!(fp.starts_with("sha256:"), "got {fp:?}");
    let hex = &fp["sha256:".len()..];
    assert_eq!(hex.len(), 64, "expected 64 hex chars, got {}", hex.len());
    assert!(
        hex.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "expected lowercase hex, got {hex:?}"
    );
}

#[test]
fn unknown_toml_keys_round_trip() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("config.toml");

    let original = r#"
schema_version = 1
future_top_level = "keep me"

[scope]
default = "user"
future_field = "x"

[vault]
path = "/tmp/vault"
future_vault_field = 42

[agents.claude-code]
installed = true
future_agent_field = "y"
"#;
    std::fs::write(&path, original).unwrap();

    let cfg = DkodConfig::load_or_default(&path).unwrap();
    cfg.save(&path).unwrap();

    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        body.contains("future_top_level"),
        "missing top-level unknown key:\n{body}"
    );
    assert!(
        body.contains("future_field"),
        "missing scope unknown key:\n{body}"
    );
    assert!(
        body.contains("future_vault_field"),
        "missing vault unknown key:\n{body}"
    );
    assert!(
        body.contains("future_agent_field"),
        "missing agent unknown key:\n{body}"
    );

    // And the parsed-back value should also still carry them.
    let cfg2 = DkodConfig::load_or_default(&path).unwrap();
    assert!(cfg2.extra.contains_key("future_top_level"));
    assert!(cfg2.scope.extra.contains_key("future_field"));
    assert!(cfg2.vault.extra.contains_key("future_vault_field"));
    assert!(cfg2
        .agents
        .get("claude-code")
        .unwrap()
        .extra
        .contains_key("future_agent_field"));
}
