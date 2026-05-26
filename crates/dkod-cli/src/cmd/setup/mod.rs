//! Seamless-capture-wizard scaffolding.
//!
//! This module hosts the data model and helpers that drive `dkod setup`'s
//! per-agent install flow. All submodules are declared up front so they
//! can cross-reference each other freely:
//!
//! * [`state`] — on-disk config + per-agent state types (Wave 1).
//! * [`vault`] — orphan-session vault init (Wave 1).
//! * [`consent`] — TTY-aware Yes/No/Never prompter (Wave 1).
//! * [`selfheal`] — sub-50µs drift detection on every dkod call (Wave 1).
//! * [`agents`] — per-agent installer trait + implementations (Wave 2).
//!
//! The `dkod setup` CLI subcommand and the orchestrator that wires these
//! together land in Wave 4.

pub mod agents;
pub mod consent;
pub mod orchestrator;
pub mod selfheal;
pub mod state;
pub mod vault;
