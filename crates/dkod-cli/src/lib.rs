//! Library surface for `dkod-cli`.
//!
//! The binary in `src/main.rs` is the user-facing entry point; this `lib.rs`
//! exists so integration tests under `tests/` can reach internals (notably
//! the seamless-capture-wizard modules under `cmd::setup`) without going
//! through the CLI process boundary.

pub mod cmd;
