//! ANSI escape-sequence stripping shared by the capture adapters.
//!
//! Tool outputs frequently arrive colorized when the running terminal had
//! `ls`/`eza`/etc. aliased to colorize. We strip CSI (`ESC [ ... letter`)
//! and the minimal OSC subset (`ESC ] ... BEL`) at parse time so blob
//! storage stays clean and downstream indexers don't need their own copy
//! of this regex.
//!
//! Model output (User/Assistant/Reasoning) is intentionally NOT stripped
//! here — if the model ever emits ANSI on purpose, that's content.

use regex::Regex;
use std::sync::OnceLock;

/// Combined matcher for:
///
/// - CSI: `ESC [ <params> <final byte>` where params are `[0-9;?]*` and
///   the final byte is any ASCII letter.
/// - OSC: `ESC ] <anything-but-BEL> BEL`.
fn ansi_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Two alternations under one regex so we only do one pass.
        // `\x1b\[[0-9;?]*[a-zA-Z]` for CSI, `\x1b\][^\x07]*\x07` for OSC.
        Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b\][^\x07]*\x07")
            .expect("static ANSI regex must compile")
    })
}

/// Remove CSI and OSC escape sequences from `s`. Idempotent.
pub fn strip_ansi(s: &str) -> String {
    // Fast path: nothing to do.
    if !s.contains('\x1b') {
        return s.to_string();
    }
    ansi_re().replace_all(s, "").into_owned()
}

/// Recursively strip ANSI from any string leaves of a JSON value.
/// Used to scrub `Message::Tool::input` fields in case an agent ever
/// pasted colorized text into a tool's arguments.
pub fn strip_ansi_in_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            if s.contains('\x1b') {
                *s = strip_ansi(s);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                strip_ansi_in_json(v);
            }
        }
        serde_json::Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                strip_ansi_in_json(v);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_and_osc() {
        let input = "\x1b[1;33mhello\x1b[0m \x1b]0;title\x07world";
        assert_eq!(strip_ansi(input), "hello world");
    }

    #[test]
    fn strip_is_idempotent() {
        let input = "\x1b[31mred\x1b[0m";
        let once = strip_ansi(input);
        let twice = strip_ansi(&once);
        assert_eq!(once, "red");
        assert_eq!(once, twice);
    }

    #[test]
    fn passthrough_when_no_escape() {
        assert_eq!(strip_ansi("plain text"), "plain text");
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn handles_csi_with_question_mark_params() {
        // `ESC [ ? 25 l` (hide cursor) — DEC private mode.
        assert_eq!(strip_ansi("\x1b[?25lhi\x1b[?25h"), "hi");
    }

    #[test]
    fn json_string_leaves_get_scrubbed() {
        let mut v = serde_json::json!({
            "command": ["bash", "-c", "ls \x1b[31mfoo\x1b[0m"],
            "nested": {"label": "\x1b[1mbold\x1b[0m"},
            "n": 42,
        });
        strip_ansi_in_json(&mut v);
        assert_eq!(v["command"][2], serde_json::json!("ls foo"));
        assert_eq!(v["nested"]["label"], serde_json::json!("bold"));
        assert_eq!(v["n"], serde_json::json!(42));
    }
}
