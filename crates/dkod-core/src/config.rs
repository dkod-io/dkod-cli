use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub redact: RedactConfig,
    pub drift: DriftConfig,
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
        }
    }
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
}
