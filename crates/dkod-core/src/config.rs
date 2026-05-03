use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub redact: RedactConfig,
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
}
