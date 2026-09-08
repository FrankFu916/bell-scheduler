#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! Deterministic student-sectioning for elective teaching sections.
//!
//! This crate deliberately stops at a verified decomposition boundary. It assigns
//! each student's selected subjects to concrete teaching sections and emits coarse
//! resource recommendations; it does not construct timetable variables or claim
//! that its feasibility proxy proves timetable feasibility.

mod diagnostics;
mod generate;
mod hash;
mod model;
mod objective;
mod validate;

pub use diagnostics::*;
pub use generate::generate_candidates;
pub use model::*;
pub use validate::validate_candidate;

/// Exact algorithm identifier recorded in every sectioning provenance record.
pub const SECTIONING_ALGORITHM_VERSION: &str = "sectioning.decomposed.v2";
