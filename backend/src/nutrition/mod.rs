//! Target computation. Deterministic arithmetic, never an LLM.
//!
//! PRD §6 is explicit about the split: the LLM estimates *what you ate*; it does
//! not compute *what you should eat*. The formulas here are offline, auditable
//! and testable against published reference values.

pub mod targets;

pub use targets::{
    compute_targets, deficit_warning, rate_warning, RateWarning, TargetInputs, Targets,
};
