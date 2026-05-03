//! Per-agent capture adapters.
//!
//! Each submodule converts an agent's native transcript format into the
//! shared `Session` / `Message` model. Adapters are pure (no redaction,
//! no git writes); the caller composes redaction and storage.

pub mod codex;
