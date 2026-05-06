//! Per-agent capture adapters.
//!
//! Each submodule converts an agent's native transcript format into the
//! shared `Session` / `Message` model. Adapters are pure (no redaction,
//! no git writes); the caller composes redaction and storage.

pub mod ansi;
pub mod claude_code;
pub mod codex;
pub mod copilot_cli;
pub mod cursor;
pub mod gemini_cli;
pub mod opencode;
pub(crate) mod timestamp;
pub(crate) mod worktree_diff;
