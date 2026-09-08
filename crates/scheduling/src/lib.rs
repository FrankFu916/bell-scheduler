#![forbid(unsafe_code)]

//! Solver-independent semantic scheduling snapshot and compact audience representation.

mod bitset;
mod model;

pub use bitset::DenseBitSet;
pub use model::*;
use thiserror::Error;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum SchedulingError {
    #[error("snapshot schema version must be positive")]
    ZeroSchemaVersion,
    #[error("bit index {index} is outside set length {len}")]
    BitIndexOutOfRange { index: usize, len: usize },
    #[error("{field}[{index}] has bitset length {actual}; expected {expected}")]
    BitSetLength {
        field: &'static str,
        index: usize,
        expected: usize,
        actual: usize,
    },
    #[error("{field} compact index {index} is outside length {len}")]
    IndexOutOfRange {
        field: &'static str,
        index: u32,
        len: usize,
    },
    #[error("{field}[{index}] must be greater than zero")]
    ZeroValue { field: &'static str, index: usize },
    #[error("{field}[{index}] must contain at least one item")]
    EmptyCollection { field: &'static str, index: usize },
    #[error("{field} contains duplicate compact index {index}")]
    DuplicateIndex { field: &'static str, index: u32 },
    #[error("{field} contains duplicate stable id {value}")]
    DuplicateStableId { field: &'static str, value: String },
    #[error("timeslot {index} has an invalid consecutive link")]
    InvalidConsecutiveLink { index: usize },
    #[error("meeting starting at {start} with duration {duration} crosses a break")]
    DurationCrossesBreak { start: u32, duration: u8 },
    #[error("{field}[{index}] has candidate {value} in both disjoint sets")]
    OverlappingCandidates {
        field: &'static str,
        index: usize,
        value: u32,
    },
    #[error(
        "meeting pattern {pattern_index} and activity {activity_index} use different course plans"
    )]
    MismatchedCoursePlan {
        pattern_index: usize,
        activity_index: u32,
    },
    #[error(
        "activities for offering at index {index} disagree on audience, plan, subject, or teacher policy"
    )]
    InconsistentOffering { index: usize },
    #[error(
        "meeting pattern {pattern_index} and activity {activity_index} use different course offerings"
    )]
    MismatchedCourseOffering {
        pattern_index: usize,
        activity_index: u32,
    },
}

impl SchedulingError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ZeroSchemaVersion => "SCHEDULING_ZERO_SCHEMA_VERSION",
            Self::BitIndexOutOfRange { .. } => "SCHEDULING_BIT_INDEX_OUT_OF_RANGE",
            Self::BitSetLength { .. } => "SCHEDULING_BITSET_LENGTH_MISMATCH",
            Self::IndexOutOfRange { .. } => "SCHEDULING_INDEX_OUT_OF_RANGE",
            Self::ZeroValue { .. } => "SCHEDULING_ZERO_VALUE",
            Self::EmptyCollection { .. } => "SCHEDULING_EMPTY_COLLECTION",
            Self::DuplicateIndex { .. } => "SCHEDULING_DUPLICATE_INDEX",
            Self::DuplicateStableId { .. } => "SCHEDULING_DUPLICATE_STABLE_ID",
            Self::InvalidConsecutiveLink { .. } => "SCHEDULING_INVALID_CONSECUTIVE_LINK",
            Self::DurationCrossesBreak { .. } => "SCHEDULING_DURATION_CROSSES_BREAK",
            Self::OverlappingCandidates { .. } => "SCHEDULING_OVERLAPPING_CANDIDATES",
            Self::MismatchedCoursePlan { .. } => "SCHEDULING_MISMATCHED_COURSE_PLAN",
            Self::InconsistentOffering { .. } => "SCHEDULING_INCONSISTENT_COURSE_OFFERING",
            Self::MismatchedCourseOffering { .. } => "SCHEDULING_MISMATCHED_COURSE_OFFERING",
        }
    }
}
