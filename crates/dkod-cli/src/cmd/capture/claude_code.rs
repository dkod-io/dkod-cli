//! `dkod capture claude-code` (long-lived server) +
//! `dkod capture-hook` (per-event hook handler).
//!
//! See `docs/research/claude-code-capture-protocol.md` for the wire spec
//! and `crates/dkod-core/src/capture/claude_code.rs` for the protocol /
//! parser / async server this module wraps.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dkod_core::capture::claude_code::{run_server, FinishedSession, WireEvent};

/// Sentinel field added to every hook entry dkod installs into
/// `.claude/settings.local.json`. Allows surgical removal without
/// clobbering the user's other hooks.
const DKOD_SENTINEL_KEY: &str = "_dkod";

/// Hook events we listen for. The order is significant for the install /
/// uninstall round-trip: keep stable so diffs against
/// `.claude/settings.local.json` stay clean.
const HOOK_EVENTS: &[(&str, u32)] = &[
    ("SessionStart", 1),
    ("UserPromptSubmit", 1),
    ("PreToolUse", 1),
    ("PostToolUse", 1),
    ("PostToolUseFailure", 1),
    ("PreCompact", 1),
    ("Stop", 1),
    ("SessionEnd", 2),
];

/// Compute the 12-hex-char repo hash from a canonical repo root path.
fn compute_repo_hash(repo_root: &Path) -> String {
    let bytes = repo_root.to_string_lossy();
    let mut hasher = Sha256::new();
    hasher.update(bytes.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex.chars().take(12).collect()
}

/// Resolve the per-repo socket path. macOS uses `$TMPDIR`, Linux uses
/// `$XDG_RUNTIME_DIR/dkod/`. Other OSes are refused at the call site.
fn resolve_socket_path(repo_hash: &str) -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let base = std::env::var_os("TMPDIR").unwrap_or_else(|| "/tmp".into());
        Ok(PathBuf::from(base).join(format!("dkod-{repo_hash}.sock")))
    }
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| "/tmp/dkod".into());
        let dir = PathBuf::from(base).join("dkod");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create socket dir {}", dir.display()))?;
        Ok(dir.join(format!("{repo_hash}.sock")))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = repo_hash;
        Err(anyhow!(
            "dkod capture claude-code is only supported on macOS and Linux in V1"
        ))
    }
}

/// Resolve `~/.local/share/dkod/captures/<repo_hash>.json`.
fn heartbeat_path(repo_hash: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set; cannot locate heartbeat dir"))?;
    let dir = home.join(".local/share/dkod/captures");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create heartbeat dir {}", dir.display()))?;
    Ok(dir.join(format!("{repo_hash}.json")))
}

/// Resolve `~/.claude/settings.json`.
fn user_claude_settings_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude/settings.json"))
}

/// Returns `true` if the user's global Claude settings disable hooks.
fn global_hooks_disabled() -> Result<bool> {
    let path = match user_claude_settings_path() {
        Some(p) => p,
        None => return Ok(false),
    };
    if !path.exists() {
        return Ok(false);
    }
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let v: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return Ok(false), // malformed settings — let claude itself complain
    };
    Ok(v.get("disableAllHooks")
        .and_then(|x| x.as_bool())
        .unwrap_or(false))
}

/// Try to connect to the socket; returns `true` if a peer accepted
/// (server is alive), `false` if the socket is dead or absent (safe to
/// re-bind).
fn another_server_is_running(socket_path: &Path) -> bool {
    if !socket_path.exists() {
        return false;
    }
    StdUnixStream::connect(socket_path).is_ok()
}

/// Build the JSON value that goes under `hooks[<Event>]` for one event.
fn dkod_hook_entry_for_event(repo_hash: &str, event: &str, timeout: u32) -> Value {
    serde_json::json!({
        "matcher": "*",
        DKOD_SENTINEL_KEY: true,
        "hooks": [{
            "type": "command",
            "command": format!("dkod capture-hook {repo_hash} {event}"),
            "timeout": timeout,
        }]
    })
}

/// Merge dkod hook entries into `.claude/settings.local.json`, preserving
/// all other content. Removes any prior dkod entries (matched by
/// `_dkod: true`) before re-inserting current ones.
fn install_hooks(repo_root: &Path, repo_hash: &str) -> Result<()> {
    let path = repo_root.join(".claude/settings.local.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut root: Value = if path.exists() {
        let body =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        if body.trim().is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&body).context("parse .claude/settings.local.json")?
        }
    } else {
        Value::Object(serde_json::Map::new())
    };

    // Ensure `hooks` exists as an object.
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow!(".claude/settings.local.json is not a JSON object"))?;
    let hooks = root_obj
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| anyhow!(".claude/settings.local.json: hooks must be an object"))?;

    for &(event, timeout) in HOOK_EVENTS {
        let arr = hooks_obj
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let arr = arr
            .as_array_mut()
            .ok_or_else(|| anyhow!("hooks.{event} must be an array"))?;
        // Drop any prior dkod-installed entries.
        arr.retain(|e| {
            !e.get(DKOD_SENTINEL_KEY)
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        });
        arr.push(dkod_hook_entry_for_event(repo_hash, event, timeout));
    }

    let serialized = serde_json::to_string_pretty(&root).context("serialize settings.local")?;
    std::fs::write(&path, serialized).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Remove only the dkod-installed hook entries from
/// `.claude/settings.local.json`. Preserves everything else. If the file
/// doesn't exist or has no dkod entries, no-op.
fn uninstall_hooks(repo_root: &Path) -> Result<()> {
    let path = repo_root.join(".claude/settings.local.json");
    if !path.exists() {
        return Ok(());
    }
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(_) => return Ok(()),
    };
    if body.trim().is_empty() {
        return Ok(());
    }
    let mut root: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };

    let mut changed = false;
    if let Some(obj) = root.as_object_mut() {
        if let Some(Value::Object(hooks)) = obj.get_mut("hooks") {
            for arr in hooks.values_mut() {
                if let Some(arr) = arr.as_array_mut() {
                    let before = arr.len();
                    arr.retain(|e| {
                        !e.get(DKOD_SENTINEL_KEY)
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    });
                    if arr.len() != before {
                        changed = true;
                    }
                }
            }
            // Strip empty event arrays we leave behind, so we don't grow
            // the file with noise on every restart.
            hooks.retain(|_, v| match v {
                Value::Array(a) => !a.is_empty(),
                _ => true,
            });
            if hooks.is_empty() {
                obj.remove("hooks");
                changed = true;
            }
        }
    }
    if changed {
        let serialized = serde_json::to_string_pretty(&root).context("serialize settings.local")?;
        std::fs::write(&path, serialized).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

/// Format a SystemTime as a basic RFC3339 UTC string with millisecond
/// resolution. Avoids pulling in `chrono`.
fn rfc3339_utc(t: SystemTime) -> String {
    let dur = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let millis = dur.subsec_millis();
    // Civil date from epoch seconds (UTC). Algorithm from Howard Hinnant.
    let z = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400) as u32;
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let h = secs_of_day / 3600;
    let mi = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}

/// Truncate a JSON value's serialised form to ~4 KiB. If serialisation
/// fits, return as-is; otherwise replace with a JSON string carrying the
/// truncated UTF-8 prefix.
fn truncate_tool_input(input: Value, limit: usize) -> Value {
    let s = match serde_json::to_string(&input) {
        Ok(s) => s,
        Err(_) => return Value::Null,
    };
    if s.len() <= limit {
        return input;
    }
    // Find a UTF-8 char boundary at or below `limit`.
    let mut end = limit;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    Value::String(format!("[truncated {} bytes] {}", s.len(), &s[..end]))
}

/// Best-effort append a brief error line to `/tmp/dkod-hook-<repo_hash>.log`.
/// Never fails — used only on hook-side error paths.
fn log_hook_error(repo_hash: &str, msg: &str) {
    let path = format!("/tmp/dkod-hook-{repo_hash}.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{} {}", rfc3339_utc(SystemTime::now()), msg);
    }
}

// -------------------------------------------------------------------------
// Server side: `dkod capture claude-code`.
// -------------------------------------------------------------------------

/// Implementation of `dkod capture claude-code`.
///
/// `_args` is currently unused (V1 has no flags). Kept in the signature so
/// future flags can land without changing main.rs.
pub fn run_server_command(cwd: &Path, _args: Vec<String>) -> Result<()> {
    // 1. Must be a git repo.
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    // 2. Canonicalise the repo root (so different cwds inside the same repo
    // produce the same hash). Falls back to the literal cwd if canonicalize
    // fails (shouldn't happen if `gix::open` succeeded, but cheap safety).
    let repo_root = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let repo_hash = compute_repo_hash(&repo_root);

    // 3. Resolve socket path (V1: macOS / Linux only).
    let socket_path = resolve_socket_path(&repo_hash)?;

    // 4. Stale-socket check.
    if another_server_is_running(&socket_path) {
        let hb = heartbeat_path(&repo_hash)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<heartbeat dir unknown>".to_string());
        return Err(anyhow!(
            "dkod capture claude-code is already running for this repo (PID file at {hb})"
        ));
    }
    // Either no peer or stale socket — remove and proceed.
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    // 5. Refuse if the user has globally disabled hooks.
    if global_hooks_disabled()? {
        return Err(anyhow!(
            "dkod cannot capture: ~/.claude/settings.json has disableAllHooks=true; \
             remove that flag (or scope it to specific events) before running \
             dkod capture claude-code."
        ));
    }

    // 6. Install hooks into the repo's .claude/settings.local.json.
    install_hooks(&repo_root, &repo_hash)
        .with_context(|| "install dkod hooks into .claude/settings.local.json")?;

    // 7. Heartbeat file.
    let heartbeat = heartbeat_path(&repo_hash)?;
    let started_at = rfc3339_utc(SystemTime::now());
    let heartbeat_body = serde_json::json!({
        "pid": std::process::id(),
        "socket_path": socket_path.display().to_string(),
        "started_at": started_at,
        "repo_root": repo_root.display().to_string(),
    });
    std::fs::write(&heartbeat, serde_json::to_string_pretty(&heartbeat_body)?)
        .with_context(|| format!("write heartbeat {}", heartbeat.display()))?;

    // Print starting message.
    println!(
        "dkod: capturing Claude Code sessions in {} (Ctrl-C to stop)",
        repo_root.display()
    );

    // 8 & 10. Build a tokio runtime, install signal handlers, run the
    // server. We exit the runtime once a signal arrives, then perform
    // synchronous cleanup before returning.
    let cfg = super::super::load_config(&repo_root)?;
    let repo_root_for_cb = repo_root.clone();
    let on_finished = move |fs: FinishedSession| {
        if let Err(e) = handle_finished_session(&repo_root_for_cb, &cfg, fs) {
            eprintln!("dkod: claude-code: failed to flush session: {e:#}");
        }
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    let socket_path_for_server = socket_path.clone();
    rt.block_on(async move {
        let server = run_server(
            &socket_path_for_server,
            Duration::from_secs(60),
            on_finished,
        );
        tokio::select! {
            res = server => {
                if let Err(e) = res {
                    eprintln!("dkod: claude-code: server error: {e:#}");
                }
            }
            _ = wait_for_shutdown_signal() => {
                eprintln!("dkod: claude-code: shutting down");
            }
        }
    });

    // 8a/8b/8c: cleanup. Best-effort — log on error but always proceed.
    if let Err(e) = uninstall_hooks(&repo_root) {
        eprintln!("dkod: claude-code: failed to uninstall hooks: {e:#}");
    }
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
    let _ = std::fs::remove_file(&heartbeat);

    Ok(())
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("dkod: claude-code: cannot install SIGINT handler: {e}");
            return;
        }
    };
    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("dkod: claude-code: cannot install SIGTERM handler: {e}");
            return;
        }
    };
    tokio::select! {
        _ = sigint.recv() => {}
        _ = sigterm.recv() => {}
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() {
    // Server is unix-only; this path is unreachable but kept for type
    // completeness if anyone ever ports the binary to other OSes.
    std::future::pending::<()>().await;
}

/// Translate a `FinishedSession` callback into a redacted, written
/// `Session` blob. Called from inside the tokio runtime via the
/// `run_server` callback.
fn handle_finished_session(
    repo_root: &Path,
    cfg: &dkod_core::config::Config,
    fs: FinishedSession,
) -> Result<()> {
    if fs.transcript_path.as_os_str().is_empty() {
        // Orphan with no announced transcript path — nothing to flush.
        eprintln!(
            "dkod: claude-code: session {} ended with no transcript path; skipping",
            fs.session_id
        );
        return Ok(());
    }
    let mut session = dkod_core::capture::claude_code::parse_transcript(&fs.transcript_path)
        .with_context(|| format!("parse transcript {}", fs.transcript_path.display()))?;
    dkod_core::redact::redact_session(&mut session, &cfg.redact);
    let n = session.messages.len();
    let id = session.id.clone();
    dkod_core::store::write_session(repo_root, &session).context("write session")?;
    eprintln!("dkod: captured Claude Code session {id} ({n} messages) -> dkod show {id}");
    Ok(())
}

// -------------------------------------------------------------------------
// Hook side: `dkod capture-hook <repo_hash> <event_name>`.
// -------------------------------------------------------------------------

/// Implementation of `dkod capture-hook`.
///
/// Always returns `Ok(())` — even on internal errors — so the binary
/// exits 0 and never breaks Claude Code.
pub fn hook_command(repo_hash: &str, event_name: &str) -> Result<()> {
    // Reject any repo_hash that doesn't match the format we control
    // (12 lowercase hex chars). The hash is interpolated into filenames
    // (`/tmp/dkod-<hash>.sock`, `/tmp/dkod-hook-<hash>.log`) so a malformed
    // value coming from a tampered settings.local.json could escape the
    // intended path. Validate up front; on failure, exit 0 silently
    // (never break Claude Code) but skip every filesystem touch.
    if !is_valid_repo_hash(repo_hash) {
        return Ok(());
    }
    if let Err(e) = hook_inner(repo_hash, event_name) {
        log_hook_error(repo_hash, &format!("hook error ({event_name}): {e:#}"));
    }
    Ok(())
}

/// `repo_hash` must be exactly 12 lowercase hex chars (the prefix of a
/// SHA-256 digest). Anything else is rejected — it's almost certainly a
/// stale or tampered settings.local.json entry, and we'd rather no-op
/// than write to a path the user didn't expect.
fn is_valid_repo_hash(s: &str) -> bool {
    s.len() == 12
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn hook_inner(repo_hash: &str, event_name: &str) -> Result<()> {
    // 1. Read stdin as JSON. Empty input is allowed for tests / robustness.
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    let input: Value = if buf.trim().is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(&buf).context("parse hook input JSON from stdin")?
    };

    // 2. Build a wire event from the hook event name + input.
    let event = match build_wire_event(event_name, &input) {
        Some(e) => e,
        None => {
            log_hook_error(repo_hash, &format!("unknown hook event: {event_name}"));
            return Ok(());
        }
    };

    // 3. Resolve socket and connect. UnixStream::connect blocks only as
    // long as the kernel takes to negotiate the local socket (microseconds
    // when the server is alive; instant ECONNREFUSED / ENOENT otherwise),
    // so we don't need an explicit connect timeout.
    let socket_path = resolve_socket_path(repo_hash)?;
    let mut stream = StdUnixStream::connect(&socket_path)
        .with_context(|| format!("connect {}", socket_path.display()))?;
    stream.set_write_timeout(Some(Duration::from_secs(1))).ok();

    // 4. Write NDJSON line + flush.
    let mut line = serde_json::to_string(&event).context("serialise wire event")?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .context("write wire event")?;
    stream.flush().ok();
    drop(stream);
    Ok(())
}

/// Map a hook event name + parsed hook input → `WireEvent`. Returns
/// `None` for events we don't translate (e.g. `NotARealEvent`); the
/// caller logs and exits 0.
fn build_wire_event(event_name: &str, input: &Value) -> Option<WireEvent> {
    let session_id = input
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let cwd = input
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let transcript_path = input
        .get("transcript_path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let ts = rfc3339_utc(SystemTime::now());

    Some(match event_name {
        "SessionStart" => WireEvent::SessionStart {
            v: 1,
            session_id,
            ts,
            cwd,
            transcript_path,
            model: input
                .get("model")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            agent_type: input
                .get("agent_type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            source: input
                .get("source")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        },
        "UserPromptSubmit" => WireEvent::PromptSubmitted {
            v: 1,
            session_id,
            ts,
            cwd,
            prompt: input
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            permission_mode: input
                .get("permission_mode")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        },
        "PreToolUse" => WireEvent::ToolStart {
            v: 1,
            session_id,
            ts,
            cwd,
            tool_name: input
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            tool_input: truncate_tool_input(
                input.get("tool_input").cloned().unwrap_or(Value::Null),
                4096,
            ),
            tool_use_id: input
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        "PostToolUse" => WireEvent::ToolEnd {
            v: 1,
            session_id,
            ts,
            cwd,
            tool_name: input
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            tool_use_id: input
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            status: "success".to_string(),
            duration_ms: input
                .get("duration_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            error: None,
        },
        "PostToolUseFailure" => WireEvent::ToolEnd {
            v: 1,
            session_id,
            ts,
            cwd,
            tool_name: input
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            tool_use_id: input
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            status: "failure".to_string(),
            duration_ms: input
                .get("duration_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            error: input
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        },
        "PreCompact" => WireEvent::PreCompact {
            v: 1,
            session_id,
            ts,
            cwd,
            trigger: input
                .get("trigger")
                .and_then(|v| v.as_str())
                .unwrap_or("manual")
                .to_string(),
        },
        "Stop" => WireEvent::TurnStop {
            v: 1,
            session_id,
            ts,
            cwd,
        },
        "SessionEnd" => WireEvent::SessionEnd {
            v: 1,
            session_id,
            ts,
            cwd,
            reason: input
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            transcript_path,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_hash_is_12_hex_chars() {
        let h = compute_repo_hash(Path::new("/Users/test/repo"));
        assert_eq!(h.len(), 12);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn repo_hash_is_stable() {
        let a = compute_repo_hash(Path::new("/Users/test/repo"));
        let b = compute_repo_hash(Path::new("/Users/test/repo"));
        assert_eq!(a, b);
    }

    #[test]
    fn repo_hash_differs_per_path() {
        let a = compute_repo_hash(Path::new("/Users/test/repo"));
        let b = compute_repo_hash(Path::new("/Users/test/other"));
        assert_ne!(a, b);
    }

    #[test]
    fn build_wire_event_session_start() {
        let input = serde_json::json!({
            "session_id": "abc",
            "cwd": "/x",
            "transcript_path": "/x/t.jsonl",
            "source": "startup",
        });
        let e = build_wire_event("SessionStart", &input).unwrap();
        match e {
            WireEvent::SessionStart {
                session_id,
                cwd,
                transcript_path,
                source,
                ..
            } => {
                assert_eq!(session_id, "abc");
                assert_eq!(cwd, "/x");
                assert_eq!(transcript_path, "/x/t.jsonl");
                assert_eq!(source, Some("startup".to_string()));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn build_wire_event_unknown_event_returns_none() {
        let input = serde_json::json!({});
        assert!(build_wire_event("NotARealEvent", &input).is_none());
    }

    #[test]
    fn repo_hash_validator_accepts_12_lowercase_hex() {
        assert!(is_valid_repo_hash("deadbeefcafe"));
        assert!(is_valid_repo_hash("000000000000"));
        assert!(is_valid_repo_hash("abcdef012345"));
    }

    #[test]
    fn repo_hash_validator_rejects_bad_input() {
        assert!(!is_valid_repo_hash(""));
        assert!(!is_valid_repo_hash("short"));
        assert!(!is_valid_repo_hash("toolongtoolongtoolong"));
        // Path-traversal attempt:
        assert!(!is_valid_repo_hash("../etc/pwd"));
        // Uppercase rejected (we always emit lowercase):
        assert!(!is_valid_repo_hash("DEADBEEFCAFE"));
        // Mixed case rejected:
        assert!(!is_valid_repo_hash("DeadBeefCafe"));
        // Non-hex chars:
        assert!(!is_valid_repo_hash("zzzzzzzzzzzz"));
    }

    #[test]
    fn build_wire_event_post_tool_failure_carries_error() {
        let input = serde_json::json!({
            "session_id": "s",
            "cwd": "/x",
            "tool_name": "Edit",
            "tool_use_id": "tu1",
            "error": "permission denied",
        });
        let e = build_wire_event("PostToolUseFailure", &input).unwrap();
        match e {
            WireEvent::ToolEnd { status, error, .. } => {
                assert_eq!(status, "failure");
                assert_eq!(error, Some("permission denied".to_string()));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn truncate_tool_input_keeps_small_values() {
        let v = serde_json::json!({"file_path": "x.rs"});
        let out = truncate_tool_input(v.clone(), 4096);
        assert_eq!(out, v);
    }

    #[test]
    fn truncate_tool_input_truncates_large_values() {
        let big = "a".repeat(10_000);
        let v = serde_json::json!({"file_path": "x.rs", "content": big});
        let out = truncate_tool_input(v, 4096);
        // After truncation, the result is a JSON string with the [truncated …] prefix.
        match out {
            Value::String(s) => {
                assert!(s.starts_with("[truncated "));
                assert!(s.len() < 5000);
            }
            other => panic!("expected truncated string, got {other:?}"),
        }
    }

    #[test]
    fn install_and_uninstall_hooks_round_trip_preserves_user_entries() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        // User has a pre-existing custom hook on `Stop` that dkod must not touch.
        let pre = serde_json::json!({
            "hooks": {
                "Stop": [{
                    "matcher": "*",
                    "hooks": [{"type": "command", "command": "echo user-hook"}]
                }]
            },
            "otherSetting": 42
        });
        std::fs::write(
            claude.join("settings.local.json"),
            serde_json::to_string_pretty(&pre).unwrap(),
        )
        .unwrap();

        install_hooks(tmp.path(), "deadbeefcafe").unwrap();
        let after_install: Value = serde_json::from_str(
            &std::fs::read_to_string(claude.join("settings.local.json")).unwrap(),
        )
        .unwrap();
        // Stop array has both: the user's hook + the dkod hook.
        let stop = after_install
            .pointer("/hooks/Stop")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(
            after_install.get("otherSetting").unwrap(),
            &serde_json::json!(42)
        );

        // Re-install does NOT duplicate.
        install_hooks(tmp.path(), "deadbeefcafe").unwrap();
        let after_reinstall: Value = serde_json::from_str(
            &std::fs::read_to_string(claude.join("settings.local.json")).unwrap(),
        )
        .unwrap();
        let stop = after_reinstall
            .pointer("/hooks/Stop")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(
            stop.len(),
            2,
            "re-install should not duplicate dkod entries"
        );

        // Uninstall removes only the dkod entries.
        uninstall_hooks(tmp.path()).unwrap();
        let after_uninstall: Value = serde_json::from_str(
            &std::fs::read_to_string(claude.join("settings.local.json")).unwrap(),
        )
        .unwrap();
        let stop = after_uninstall
            .pointer("/hooks/Stop")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(stop.len(), 1);
        let cmd = stop[0]
            .pointer("/hooks/0/command")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(cmd, "echo user-hook");
        // Other dkod-installed events (which the user didn't have) get
        // their now-empty arrays pruned.
        assert!(
            after_uninstall.pointer("/hooks/SessionStart").is_none(),
            "empty event arrays should be pruned"
        );
        // User's other settings preserved.
        assert_eq!(
            after_uninstall.get("otherSetting").unwrap(),
            &serde_json::json!(42)
        );
    }

    #[tokio::test]
    async fn server_round_trip_writes_session_blob() {
        // End-to-end inside the module: spin up `run_server` against a
        // temp socket, send SessionStart + SessionEnd via NDJSON, and
        // verify a Session was written to a tempdir git repo via
        // store::write_session.
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        // Tempdir repo.
        let repo = tempfile::TempDir::new().unwrap();
        let _ = std::process::Command::new("git")
            .arg("init")
            .arg(repo.path())
            .output()
            .unwrap();

        // Build a synthetic transcript on disk.
        let transcript = repo.path().join("transcript.jsonl");
        std::fs::write(
            &transcript,
            serde_json::to_string(&serde_json::json!({
                "type": "user",
                "message": {"content": "hello"}
            }))
            .unwrap()
                + "\n",
        )
        .unwrap();

        // Tempdir socket.
        let sock_dir = tempfile::TempDir::new().unwrap();
        let socket_path = sock_dir.path().join("server.sock");

        let written = Arc::new(AtomicUsize::new(0));
        let written_cb = written.clone();
        let repo_path = repo.path().to_path_buf();

        let socket_path_for_server = socket_path.clone();
        let server = tokio::spawn(async move {
            let _ = run_server(
                &socket_path_for_server,
                Duration::from_secs(60),
                move |fs: FinishedSession| {
                    let mut session =
                        dkod_core::capture::claude_code::parse_transcript(&fs.transcript_path)
                            .expect("parse");
                    let cfg = dkod_core::config::Config::default();
                    dkod_core::redact::redact_session(&mut session, &cfg.redact);
                    dkod_core::store::write_session(&repo_path, &session).expect("write");
                    written_cb.fetch_add(1, Ordering::SeqCst);
                },
            )
            .await;
        });

        for _ in 0..50 {
            if socket_path.exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(socket_path.exists(), "server didn't bind");

        // Send SessionStart pointing at our transcript.
        let start = WireEvent::SessionStart {
            v: 1,
            session_id: "sid-rt".into(),
            ts: "2026-05-03T12:00:00.000Z".into(),
            cwd: repo.path().display().to_string(),
            transcript_path: transcript.display().to_string(),
            model: None,
            agent_type: None,
            source: Some("startup".into()),
        };
        let end = WireEvent::SessionEnd {
            v: 1,
            session_id: "sid-rt".into(),
            ts: "2026-05-03T12:01:00.000Z".into(),
            cwd: repo.path().display().to_string(),
            reason: "logout".into(),
            transcript_path: transcript.display().to_string(),
        };

        let mut client = UnixStream::connect(&socket_path).await.unwrap();
        let mut buf = serde_json::to_string(&start).unwrap();
        buf.push('\n');
        client.write_all(buf.as_bytes()).await.unwrap();
        let mut buf = serde_json::to_string(&end).unwrap();
        buf.push('\n');
        client.write_all(buf.as_bytes()).await.unwrap();
        client.flush().await.unwrap();
        client.shutdown().await.unwrap();
        drop(client);

        for _ in 0..200 {
            if written.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        if written.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(written.load(Ordering::SeqCst), 1);

        // Verify a session exists in the repo.
        let sessions = dkod_core::store::list_sessions(repo.path()).unwrap();
        assert_eq!(sessions.len(), 1);

        server.abort();
    }
}
