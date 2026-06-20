use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub redact: RedactConfig,
    pub drift: DriftConfig,
    pub storage: StorageConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RedactConfig {
    pub enabled: bool,
    pub patterns: Vec<String>,
    pub custom: Vec<String>,
}

impl Default for RedactConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            patterns: vec![
                "builtin:aws".into(),
                "builtin:github_token".into(),
                "builtin:openai_key".into(),
                "builtin:stripe".into(),
                "builtin:env_assignment".into(),
                "builtin:entropy".into(),
            ],
            custom: vec![],
        }
    }
}

/// Configuration for `dkod drift` (intent-vs-output drift detection). All
/// fields default so the feature works with zero config; `enabled = false`
/// makes `dkod drift` report every session clean.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DriftConfig {
    pub enabled: bool,
    /// Globs (forward-slash, `**`/`*`) for paths whose modification is always
    /// worth flagging, regardless of the prompt.
    pub sensitive_paths: Vec<String>,
    /// A prompt at or below this many chars counts as "small-sounding".
    pub small_ask_max_chars: usize,
    /// Substrings that mark a prompt as small-sounding regardless of length.
    pub small_ask_keywords: Vec<String>,
    /// At or above this many touched files counts as a "large change".
    pub large_change_files: usize,
    /// At or above this many changed lines (when diff stats are available)
    /// counts as a "large change".
    pub large_change_lines: usize,
    /// Keywords (matched as whole words; multi-word entries as substrings)
    /// that mark the prompt as a dependency ask. When any is present, lockfile
    /// churn is treated as the mechanical consequence of the ask: paths
    /// matching `lockfile_paths` are exempt from the sensitive-path tripwire,
    /// and paths matching `lockfile_paths` or `manifest_paths` are exempt from
    /// the unmentioned-file rule.
    pub dep_ask_keywords: Vec<String>,
    /// Globs identifying lockfiles — the subset of `sensitive_paths` that a
    /// dependency ask legitimately rewrites. Consulted only by the dep-ask
    /// suppression; non-lockfile sensitive paths always stay armed.
    pub lockfile_paths: Vec<String>,
    /// Globs identifying dependency manifests (the human-edited half of a
    /// manifest+lockfile pair). Under a dependency ask these are exempt from
    /// the unmentioned-file rule.
    pub manifest_paths: Vec<String>,
    /// Substrings that mark a prompt as broad-scoped ("rename X everywhere").
    /// A broad-scoped prompt is never treated as a small ask by the magnitude
    /// rule, regardless of length or `small_ask_keywords`.
    pub broad_scope_phrases: Vec<String>,
}

impl Default for DriftConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sensitive_paths: vec![
                ".github/workflows/**",
                "**/Dockerfile",
                "**/*.pem",
                "**/*.key",
                "**/.env*",
                "**/secrets*",
                "**/Cargo.lock",
                "**/package-lock.json",
                "**/yarn.lock",
                "**/poetry.lock",
                "**/go.sum",
                "**/pnpm-lock.yaml",
                "**/migrations/**",
                "**/auth*",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            small_ask_max_chars: 140,
            small_ask_keywords: vec![
                "typo",
                "rename",
                "comment",
                "bump",
                "tweak",
                "format",
                "lint",
                "whitespace",
                "one-liner",
                "small fix",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            large_change_files: 5,
            large_change_lines: 150,
            dep_ask_keywords: vec![
                "dependency",
                "dependencies",
                "dep",
                "deps",
                "upgrade",
                "update",
                "bump",
                "add",
                "install",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            lockfile_paths: vec![
                "**/Cargo.lock",
                "**/package-lock.json",
                "**/yarn.lock",
                "**/poetry.lock",
                "**/go.sum",
                "**/pnpm-lock.yaml",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            manifest_paths: vec![
                "**/package.json",
                "**/Cargo.toml",
                "**/pyproject.toml",
                "**/go.mod",
                "**/Gemfile",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            broad_scope_phrases: vec![
                "everywhere",
                "across the codebase",
                "all files",
                "throughout",
                "codebase-wide",
                "every ",
                "clean up",
                "refactor",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        }
    }
}

/// Storage-format coordination, committed in `.dkod/config.toml` (§12 of the
/// storage-v2 design). Old CLIs ignore unknown keys (serde default behavior).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// `Some("v2")` once `dkod reindex` (or a new init) has run; `None` means
    /// a v1-era repo. Informational — readers always use the fallback chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Dual-write toggle: when true, every index write also writes the legacy
    /// `refs/dkod/{sessions,commits,patchid}/*` refs so old CLIs keep reading.
    pub write_legacy_refs: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            format: None,
            write_legacy_refs: true,
        }
    }
}

/// Best-effort `[storage]` load for `dkod-core` writers (`store.rs` cannot
/// take a `Config` parameter without breaking public signatures). Missing or
/// unparseable `.dkod/config.toml` → defaults; never errors.
pub fn load_storage_config(repo_path: &std::path::Path) -> StorageConfig {
    let path = repo_path.join(".dkod/config.toml");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return StorageConfig::default();
    };
    toml::from_str::<Config>(&body)
        .map(|c| c.storage)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let toml = r#"
            [redact]
            enabled = true
            patterns = ["builtin:aws"]
        "#;
        let c: Config = toml::from_str(toml).unwrap();
        assert!(c.redact.enabled);
        assert_eq!(c.redact.patterns, vec!["builtin:aws"]);
    }

    #[test]
    fn defaults_redaction_to_on_with_full_builtin_set() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.redact.enabled);
        assert!(c.redact.patterns.contains(&"builtin:aws".to_string()));
        assert!(c
            .redact
            .patterns
            .contains(&"builtin:github_token".to_string()));
        assert!(c
            .redact
            .patterns
            .contains(&"builtin:openai_key".to_string()));
        assert!(c.redact.patterns.contains(&"builtin:stripe".to_string()));
        assert!(c
            .redact
            .patterns
            .contains(&"builtin:env_assignment".to_string()));
        assert!(c.redact.patterns.contains(&"builtin:entropy".to_string()));
    }

    #[test]
    fn defaults_drift_enabled_with_sensitive_paths_and_thresholds() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.drift.enabled);
        assert!(c
            .drift
            .sensitive_paths
            .iter()
            .any(|p| p == ".github/workflows/**"));
        assert!(c.drift.sensitive_paths.iter().any(|p| p == "**/auth*"));
        assert_eq!(c.drift.small_ask_max_chars, 140);
        assert_eq!(c.drift.large_change_files, 5);
        assert_eq!(c.drift.large_change_lines, 150);
    }

    #[test]
    fn defaults_drift_suppression_knobs() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.drift.dep_ask_keywords.iter().any(|k| k == "dependency"));
        assert!(c.drift.dep_ask_keywords.iter().any(|k| k == "bump"));
        assert!(c.drift.lockfile_paths.iter().any(|p| p == "**/Cargo.lock"));
        assert!(c
            .drift
            .lockfile_paths
            .iter()
            .any(|p| p == "**/pnpm-lock.yaml"));
        assert!(c
            .drift
            .manifest_paths
            .iter()
            .any(|p| p == "**/package.json"));
        assert!(c
            .drift
            .broad_scope_phrases
            .iter()
            .any(|p| p == "everywhere"));
    }

    #[test]
    fn drift_suppression_knobs_parse_from_toml() {
        let toml = r#"
            [drift]
            dep_ask_keywords = ["vendored"]
            lockfile_paths = ["**/flake.lock"]
            manifest_paths = ["**/flake.nix"]
            broad_scope_phrases = ["repo-wide"]
        "#;
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.drift.dep_ask_keywords, vec!["vendored"]);
        assert_eq!(c.drift.lockfile_paths, vec!["**/flake.lock"]);
        assert_eq!(c.drift.manifest_paths, vec!["**/flake.nix"]);
        assert_eq!(c.drift.broad_scope_phrases, vec!["repo-wide"]);
    }

    #[test]
    fn drift_section_overrides_parse() {
        let toml = r#"
            [drift]
            enabled = false
            large_change_files = 9
            small_ask_keywords = ["typo", "nit"]
        "#;
        let c: Config = toml::from_str(toml).unwrap();
        assert!(!c.drift.enabled);
        assert_eq!(c.drift.large_change_files, 9);
        assert_eq!(c.drift.small_ask_keywords, vec!["typo", "nit"]);
        assert_eq!(c.drift.small_ask_max_chars, 140); // unspecified → default
    }

    #[test]
    fn defaults_storage_to_dual_write_on_with_no_format() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.storage.write_legacy_refs);
        assert_eq!(c.storage.format, None);
    }

    #[test]
    fn storage_section_overrides_parse() {
        let toml = r#"
            [storage]
            format = "v2"
            write_legacy_refs = false
        "#;
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.storage.format.as_deref(), Some("v2"));
        assert!(!c.storage.write_legacy_refs);
    }

    #[test]
    fn load_storage_config_defaults_when_file_missing_or_bad() {
        let tmp = tempfile::TempDir::new().unwrap();
        let s = load_storage_config(tmp.path());
        assert!(s.write_legacy_refs); // missing file → default
        std::fs::create_dir_all(tmp.path().join(".dkod")).unwrap();
        std::fs::write(tmp.path().join(".dkod/config.toml"), "not [valid toml").unwrap();
        let s = load_storage_config(tmp.path());
        assert!(s.write_legacy_refs); // unparseable file → default, never an error
    }
}
