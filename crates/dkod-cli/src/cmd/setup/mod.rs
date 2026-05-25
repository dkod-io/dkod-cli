//! Seamless-capture-wizard scaffolding.
//!
//! This module hosts the data model and helpers that drive `dkod setup`'s
//! per-agent install flow. Task 1.1 only stands up the state module —
//! orchestration, per-agent installers, and the CLI subcommand wiring land
//! in later waves.

pub mod consent;
pub mod selfheal;
pub mod state;
pub mod vault;
