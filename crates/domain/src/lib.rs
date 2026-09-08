#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! Core scheduling domain for Chinese senior-high-school 3+3 timetabling.
//!
//! This crate deliberately contains no persistence, transport, UI, or solver-engine
//! types. Cross-field invariants are established by constructors; protocol and
//! persistence layers must map their DTOs through those constructors.

pub mod calendar;
pub mod course;
pub mod error;
pub mod id;
pub mod project;
pub mod resources;
pub mod rules;
pub mod solver;
pub mod timetable;
pub mod value;

pub use calendar::*;
pub use course::*;
pub use error::{DomainError, ProblemCode};
pub use id::*;
pub use project::*;
pub use resources::*;
pub use rules::*;
pub use solver::*;
pub use timetable::*;
pub use value::*;
