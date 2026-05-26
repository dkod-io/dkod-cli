//! Data model and on-disk I/O for `~/.dkod/config.toml` — the seamless
//! capture wizard's persistent state.
//!
//! Design notes:
//!
//! * The TOML schema is versioned via `schema_version` (currently `1`). To
//!   protect users from silent data loss when an older CLI reads a file
//!   written by a newer CLI, every struct in the tree carries an `extra`
//!   carrier (`BTreeMap<String, toml::Value>` flattened via `serde`) that
//!   captures unknown keys and round-trips them back out on save.
//!
//! * `save()` is atomic: it writes a `.tmp` sibling and renames it over
//!   the target. A crash between write and rename leaves the previous
//!   config intact; a successful save leaves no `.tmp` behind.
//!
//! * `fingerprint()` is the SHA-256 of arbitrary bytes, hex-encoded and
//!   prefixed with `"sha256:"`. The wizard uses it to detect drift on
//!   installed hook files.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Current on-disk schema version. Bump only when introducing a
/// breaking change that requires a migration step.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Top-level shape of `~/.dkod/config.toml`.
///
/// `Eq` is intentionally omitted: the `extra` carriers hold `toml::Value`s
/// which can include floats, so only `PartialEq` is sound. Tests use
/// `assert_eq!`, which only requires `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DkodConfig {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub scope: ScopeConfig,
    #[serde(default)]
    pub vault: VaultConfig,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentState>,
    /// Unknown keys captured during deserialization so they survive a
    /// load/save round-trip on a CLI that doesn't yet understand them.
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

fn default_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

impl Default for DkodConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            scope: ScopeConfig::default(),
            vault: VaultConfig::default(),
            agents: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScopeConfig {
    #[serde(default)]
    pub default: Scope,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    #[default]
    User,
    PerRepo,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VaultConfig {
    #[serde(default)]
    pub path: PathBuf,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentState {
    #[serde(default)]
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent: Option<Consent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_at: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Consent {
    Yes,
    No,
    Never,
    Ask,
    SkippedNoninteractive,
}

impl DkodConfig {
    /// Default path for the wizard's config: `~/.dkod/config.toml`.
    /// Falls back to `$HOME` if `dirs::home_dir()` returns `None`.
    pub fn default_path() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .context("cannot locate home directory (HOME unset and dirs::home_dir() is None)")?;
        Ok(home.join(".dkod").join("config.toml"))
    }

    /// Load from `path`, or return `Self::default()` when the file is
    /// missing. Parse errors are surfaced — a corrupt config should not
    /// be silently overwritten.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let body =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let cfg: Self =
            toml::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
        Ok(cfg)
    }

    /// Atomically write to `path`. The serialized TOML is first staged
    /// in a `.tmp` sibling, then renamed over the target. Parent
    /// directories are created on demand.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create parent dir {}", parent.display()))?;
            }
        }

        let body = toml::to_string_pretty(self).context("serialize DkodConfig to TOML")?;

        let tmp = tmp_sibling(path);
        std::fs::write(&tmp, body)
            .with_context(|| format!("write staging file {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }
}

/// Build the `.tmp` sibling path used by `save()`. Appends `.tmp` to the
/// full filename so we don't collide with `path.with_extension("tmp")`
/// on paths that lack an extension.
fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(name),
        _ => PathBuf::from(name),
    }
}

/// SHA-256 of `bytes`, hex-encoded (lowercase) with a `"sha256:"`
/// prefix. Used to detect drift on installed hook files.
pub fn fingerprint(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity("sha256:".len() + 64);
    out.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        // Lowercase hex; the test asserts on this.
        write!(&mut out, "{byte:02x}").expect("writing to String");
    }
    out
}
