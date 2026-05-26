//! Generic `dkod capture-hook --agent <name> --event <event>` handler.
//!
//! The seamless capture wizard installs hooks (or PATH shims) that point
//! every supported agent at this single entry point. The handler:
//!
//! 1. Reads the agent's JSON payload from stdin (best-effort — empty is OK).
//! 2. Picks a destination via [`route_session`].
//! 3. Buffers the raw payload to `<dest>/.dkod/pending/<uuid>.json`.
//!
//! The actual transcript parsing / redaction / `refs/dkod/*` write happens
//! later (the orchestrator drains `pending/` into the store). Buffering up
//! front means a hook firing while dkod is partially installed — or while
//! the per-repo capture server hasn't been auto-spawned yet — never loses
//! events.
//!
//! Failure policy: the public entry point ALWAYS returns `Ok(())` so the
//! process exits 0. A misbehaving hook that breaks the user's agent is the
//! one thing we cannot ship. Any internal error is logged to
//! `/tmp/dkod-hook-<agent>.log` and swallowed.

use anyhow::{anyhow, Context, Result};
use dkod_core::capture::route::{route_session, RouteConfig, RouteDestination};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Sentinel filename that disables auto-init for a specific repo. The
/// wizard's per-repo opt-out drops this at `<repo>/.dkod/no-auto-init`.
const AUTO_INIT_OPT_OUT: &str = ".dkod/no-auto-init";

/// Public entry point used by `main.rs`. Always returns `Ok(())` so the
/// hook process exits 0 — never break the user's agent because dkod had
/// a bad day.
pub fn route_and_buffer(agent: &str, event: &str) -> Result<()> {
    if let Err(e) = inner(agent, event) {
        log_error(agent, &format!("hook ({event}): {e:#}"));
    }
    Ok(())
}

fn inner(agent: &str, event: &str) -> Result<()> {
    if !is_valid_agent_name(agent) {
        return Err(anyhow!("invalid agent name: {agent:?}"));
    }
    if !is_valid_event_name(event) {
        return Err(anyhow!("invalid event name: {event:?}"));
    }

    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    let payload_cwd = parse_cwd_from_payload(&buf);
    let cwd = payload_cwd
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| anyhow!("no cwd in payload and current_dir() failed"))?;

    let vault = vault_path()?;
    let cfg = RouteConfig {
        // The hook itself can't tell whether the wizard ran in user-scope
        // mode. The orchestrator may eventually pass scope through env;
        // for now assume user-scope (the common case) and let the
        // per-repo opt-out sentinel cover the override.
        user_scope: true,
        vault_path: &vault,
        auto_init_disabled_sentinel: AUTO_INIT_OPT_OUT,
    };
    let dest = route_session(&cwd, &cfg);
    write_pending(&dest, agent, event, buf.as_bytes())?;
    Ok(())
}

fn parse_cwd_from_payload(buf: &str) -> Option<PathBuf> {
    if buf.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(buf).ok()?;
    v.get("cwd")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// `~/.dkod/vault` by default, overridable via `DKOD_VAULT_PATH` (used by
/// tests to keep the writes hermetic — the wizard does not yet expose a
/// user-facing override).
fn vault_path() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("DKOD_VAULT_PATH") {
        return Ok(PathBuf::from(v));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set; cannot locate vault"))?;
    Ok(home.join(".dkod/vault"))
}

/// Write the payload bytes verbatim into `<dest>/.dkod/pending/<uuid>.json`.
///
/// Wraps the payload in a small envelope so the orchestrator knows which
/// agent + event the bytes belong to without having to sniff. The orphan
/// `AutoInitThenRepo` case writes alongside `Repo` — `dkod init` will
/// pick up these pending files on the next dispatch.
fn write_pending(dest: &RouteDestination, agent: &str, event: &str, payload: &[u8]) -> Result<()> {
    let root = match dest {
        RouteDestination::Repo(p) => p.clone(),
        RouteDestination::AutoInitThenRepo(p) => p.clone(),
        RouteDestination::Vault(p) => p.clone(),
    };
    let pending_dir = root.join(".dkod").join("pending");
    std::fs::create_dir_all(&pending_dir)
        .with_context(|| format!("create pending dir {}", pending_dir.display()))?;

    let envelope = serde_json::json!({
        "schema": 1,
        "agent": agent,
        "event": event,
        "received_at": now_iso8601(),
        "payload_b64": base64_encode(payload),
    });

    let id = uuid_v4();
    let final_path = pending_dir.join(format!("{id}.json"));
    let tmp_path = pending_dir.join(format!("{id}.json.tmp"));
    let bytes = serde_json::to_vec_pretty(&envelope).context("serialize hook envelope")?;
    std::fs::write(&tmp_path, &bytes).with_context(|| format!("write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename {} -> {}", tmp_path.display(), final_path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Input validation — defence in depth. The hook command line is interpolated
// into the user's hook config; rejecting nonsense lowers blast radius if a
// stale config carries garbage values.
// ---------------------------------------------------------------------------

fn is_valid_agent_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn is_valid_event_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

// ---------------------------------------------------------------------------
// Tiny helpers — kept inline so the hook stays dependency-light. Replacing
// these with crates is fine later but not part of Wave 3.2's scope.
// ---------------------------------------------------------------------------

fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Coarse-grained timestamp; the orchestrator can refine if needed.
    format!("unix:{secs}")
}

/// Pseudo-random v4-style UUID. Not cryptographic — only needs to avoid
/// collisions within a single user's hook stream, which is dominated by
/// process PID + monotonic counter.
fn uuid_v4() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id() as u64;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{pid:08x}-{nanos:016x}-{seq:08x}")
}

/// Minimal RFC 4648 base64 (no padding shortcut, std-alphabet). Used for
/// the envelope's `payload_b64` field; pulling in `base64` for one call
/// site is overkill.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let b0 = input[i];
        let b1 = input[i + 1];
        let b2 = input[i + 2];
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        out.push(ALPHABET[(b2 & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let b0 = input[i];
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[((b0 & 0x03) << 4) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let b0 = input[i];
        let b1 = input[i + 1];
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(ALPHABET[((b1 & 0x0f) << 2) as usize] as char);
        out.push('=');
    }
    out
}

/// Append an error line to a per-user hook log. We deliberately write under
/// `$HOME/.local/share/dkod/` (mode 0700 when we create it) instead of `/tmp`
/// — error messages may include path context that should not be readable by
/// other users on a shared host. Falls back to `/tmp` only when `HOME` is
/// unset, and uses 0600 perms on the file itself.
fn log_error(agent: &str, msg: &str) {
    let path = log_path_for(agent);
    if let Some(parent) = path.parent() {
        // Create the log directory with 0700 in a single syscall via
        // DirBuilder. The naive create_dir_all + set_permissions sequence
        // has a TOCTOU window where the dir briefly exists with the
        // process umask before we tighten it — small but real on a shared
        // host. Silently fail if creation doesn't work; the user is the
        // only consumer of this log and breaking a hook is worse than
        // missing a log line.
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            let _ = builder.create(parent);
        }
        #[cfg(not(unix))]
        {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_CREAT + mode 0600 on the file itself so a freshly-created log
        // isn't world-readable even if the parent dir somehow ends up loose.
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(&path) {
        let _ = writeln!(f, "{} {}", now_iso8601(), msg);
    }
}

fn log_path_for(agent: &str) -> std::path::PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = std::path::PathBuf::from(home);
        p.push(".local/share/dkod");
        p.push(format!("hook-{agent}.log"));
        return p;
    }
    std::path::PathBuf::from(format!("/tmp/dkod-hook-{agent}.log"))
}

/// Visible for tests so they can call the routing function without going
/// through stdin / `current_dir()`.
#[doc(hidden)]
pub fn _route_and_buffer_for_test(
    agent: &str,
    event: &str,
    cwd: &Path,
    vault: &Path,
    payload: &[u8],
) -> Result<RouteDestination> {
    if !is_valid_agent_name(agent) {
        return Err(anyhow!("invalid agent name: {agent:?}"));
    }
    if !is_valid_event_name(event) {
        return Err(anyhow!("invalid event name: {event:?}"));
    }
    let cfg = RouteConfig {
        user_scope: true,
        vault_path: vault,
        auto_init_disabled_sentinel: AUTO_INIT_OPT_OUT,
    };
    let dest = route_session(cwd, &cfg);
    write_pending(&dest, agent, event, payload)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_name_validator_accepts_supported_agents() {
        for a in [
            "claude-code",
            "codex",
            "copilot-cli",
            "cursor",
            "factory-ai",
            "gemini-cli",
            "opencode",
        ] {
            assert!(is_valid_agent_name(a), "should accept {a}");
        }
    }

    #[test]
    fn agent_name_validator_rejects_bad_input() {
        assert!(!is_valid_agent_name(""));
        assert!(!is_valid_agent_name("Claude-Code"));
        assert!(!is_valid_agent_name("agent name"));
        assert!(!is_valid_agent_name("../etc"));
        assert!(!is_valid_agent_name(&"a".repeat(64)));
    }

    #[test]
    fn event_name_validator_accepts_common_events() {
        for e in [
            "SessionStart",
            "SessionEnd",
            "PreToolUse",
            "Stop",
            "session_start",
        ] {
            assert!(is_valid_event_name(e), "should accept {e}");
        }
    }

    #[test]
    fn event_name_validator_rejects_bad_input() {
        assert!(!is_valid_event_name(""));
        assert!(!is_valid_event_name("Pre-ToolUse")); // dash not allowed
        assert!(!is_valid_event_name("session start"));
        assert!(!is_valid_event_name(&"e".repeat(128)));
    }

    #[test]
    fn base64_encodes_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn uuid_values_are_unique_within_a_run() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(uuid_v4()));
        }
    }

    #[test]
    fn parse_cwd_from_payload_extracts_string() {
        let p = parse_cwd_from_payload(r#"{"cwd": "/home/u/repo"}"#);
        assert_eq!(p, Some(PathBuf::from("/home/u/repo")));
    }

    #[test]
    fn parse_cwd_from_payload_handles_missing_field() {
        assert_eq!(parse_cwd_from_payload(r#"{"foo": 1}"#), None);
    }

    #[test]
    fn parse_cwd_from_payload_handles_empty_and_garbage() {
        assert_eq!(parse_cwd_from_payload(""), None);
        assert_eq!(parse_cwd_from_payload("not json"), None);
    }
}
