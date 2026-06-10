//! Fail-open hook-command template + self-heal rewriter (issue #24).
//!
//! Background: a stale `dkod` on PATH (e.g. an old cargo-installed binary
//! whose clap rejects `--agent`) exits 2 from every hook invocation.
//! Claude Code treats a PreToolUse hook exiting 2 as BLOCK and a Stop
//! hook exiting 2 as forced-continue — every session on the machine is
//! bricked until manual intervention. Capture hooks never intend to
//! block, so exit-2-as-block must be impossible by construction.
//!
//! Every hook command the wizard (and `dkod init`) writes is therefore
//! wrapped as:
//!
//! ```text
//! command -v dkod >/dev/null 2>&1 && <inner> || true
//! ```
//!
//! which guarantees exit 0 whether dkod is missing, stale, or crashing.
//!
//! [`heal_command`] is the self-heal half: it recognises old-form (bare)
//! `dkod capture-hook ...` commands — both the wizard's `--agent/--event`
//! form and the legacy per-repo positional form — and rewrites them into
//! the fail-open template. A bricked user just runs the new binary's
//! `dkod setup` and every broken entry is repaired in place.

/// Shell prefix that makes the hook a no-op when dkod isn't on PATH.
const GUARD_PREFIX: &str = "command -v dkod >/dev/null 2>&1 && ";

/// Shell suffix that swallows any non-zero exit from a present-but-broken
/// dkod binary.
const FAIL_OPEN_SUFFIX: &str = " || true";

/// Wrap a raw hook command in the fail-open template. The result exits 0
/// in every case: dkod missing (guard short-circuits), dkod stale or
/// crashing (`|| true` swallows the failure), dkod healthy (inner runs).
pub fn fail_open_hook_command(inner: &str) -> String {
    format!("{GUARD_PREFIX}{inner}{FAIL_OPEN_SUFFIX}")
}

/// `true` if `cmd` is already in the fail-open form produced by
/// [`fail_open_hook_command`].
pub fn is_fail_open(cmd: &str) -> bool {
    let cmd = cmd.trim();
    cmd.starts_with(GUARD_PREFIX) && cmd.ends_with(FAIL_OPEN_SUFFIX.trim_start())
}

/// `true` if `cmd` invokes `dkod capture-hook` (any form, wrapped or not).
pub fn is_dkod_capture_hook_command(cmd: &str) -> bool {
    cmd.contains("dkod capture-hook")
}

/// If `cmd` is an old-form (non-fail-open) `dkod capture-hook` invocation,
/// return its fail-open rewrite. Returns `None` when the command is
/// already fail-open or isn't a dkod capture hook at all.
///
/// Recognised old forms (both written by pre-0.2.1 releases):
/// * wizard / user-scope: `dkod capture-hook --agent <name> --event <Event>`
/// * legacy per-repo:     `dkod capture-hook <repo_hash> <Event>`
///
/// Only commands that are *strictly* a dkod capture-hook invocation —
/// `dkod capture-hook` followed by plain `[A-Za-z0-9_-]` arguments, no
/// shell metacharacters — are rewritten. Anything fancier (pipes,
/// subshells, chained commands) is the user's own construction; we leave
/// it untouched rather than wrap a compound command we don't understand.
pub fn heal_command(cmd: &str) -> Option<String> {
    let trimmed = cmd.trim();
    if is_fail_open(trimmed) || !is_bare_capture_hook_invocation(trimmed) {
        return None;
    }
    Some(fail_open_hook_command(trimmed))
}

/// `true` when `cmd` is exactly `dkod capture-hook <args...>` with every
/// argument made of shell-safe chars (`[A-Za-z0-9_-]` plus the leading
/// `--` of flag names). This is the full alphabet pre-0.2.1 releases ever
/// emitted, so it matches every command we are responsible for healing
/// while refusing to rewrap anything containing shell metacharacters.
fn is_bare_capture_hook_invocation(cmd: &str) -> bool {
    let Some(rest) = cmd.strip_prefix("dkod capture-hook ") else {
        return false;
    };
    !rest.trim().is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_'))
}

/// Walk a Claude-style `hooks` settings object (event name → array of
/// matcher entries, each carrying a `hooks: [{type, command, ...}]` list)
/// and rewrite every old-form dkod capture-hook command to fail-open,
/// in place. Non-dkod commands and already-healed entries are untouched.
/// Returns `true` if anything changed.
pub fn heal_hooks_object(hooks_obj: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    let mut changed = false;
    for arr in hooks_obj.values_mut() {
        let Some(entries) = arr.as_array_mut() else {
            continue;
        };
        for entry in entries {
            let Some(inner) = entry.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
                continue;
            };
            for hook in inner {
                let Some(obj) = hook.as_object_mut() else {
                    continue;
                };
                let Some(cmd) = obj.get("command").and_then(|c| c.as_str()) else {
                    continue;
                };
                if let Some(healed) = heal_command(cmd) {
                    obj.insert("command".into(), serde_json::Value::String(healed));
                    changed = true;
                }
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_round_trip() {
        let inner = "dkod capture-hook --agent claude-code --event Stop";
        let wrapped = fail_open_hook_command(inner);
        assert!(is_fail_open(&wrapped));
        assert!(is_dkod_capture_hook_command(&wrapped));
        // Healing an already-healed command is a no-op.
        assert_eq!(heal_command(&wrapped), None);
    }

    #[test]
    fn heals_both_legacy_forms() {
        for old in [
            "dkod capture-hook --agent claude-code --event PreToolUse",
            "dkod capture-hook deadbeefcafe Stop",
        ] {
            let healed = heal_command(old).expect("should heal");
            assert!(is_fail_open(&healed));
            assert!(healed.contains(old));
        }
    }

    #[test]
    fn leaves_foreign_commands_alone() {
        assert_eq!(heal_command("echo hi"), None);
        assert!(!is_fail_open("echo hi || true")); // no guard prefix
    }

    #[test]
    fn refuses_to_wrap_compound_commands() {
        // Commands carrying shell metacharacters are not ours to rewrap —
        // wrapping a compound command changes its meaning.
        for cmd in [
            "dkod capture-hook --agent claude-code --event Stop; rm -rf /tmp/x",
            "dkod capture-hook a b | tee /tmp/log",
            "dkod capture-hook $(whoami) Stop",
            "dkod capture-hook ",
        ] {
            assert_eq!(heal_command(cmd), None, "must not heal: {cmd}");
        }
    }

    #[test]
    fn heal_hooks_object_rewrites_in_place() {
        let mut root = serde_json::json!({
            "Stop": [
                {"hooks": [{"type": "command", "command": "dkod capture-hook --agent claude-code --event Stop"}]},
                {"hooks": [{"type": "command", "command": "echo user-hook"}]}
            ]
        });
        let obj = root.as_object_mut().unwrap();
        assert!(heal_hooks_object(obj));
        let cmd = root
            .pointer("/Stop/0/hooks/0/command")
            .and_then(|c| c.as_str())
            .unwrap();
        assert!(is_fail_open(cmd));
        let user = root
            .pointer("/Stop/1/hooks/0/command")
            .and_then(|c| c.as_str())
            .unwrap();
        assert_eq!(user, "echo user-hook");
        // Second pass: nothing left to heal.
        let obj = root.as_object_mut().unwrap();
        assert!(!heal_hooks_object(obj));
    }
}
