use crate::Revision;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

/// Stable, localizable machine codes. Natural-language error text is never the API contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum ProblemCode {
    DomainEmptyValue,
    DomainValueTooLong,
    DomainInvalidCode,
    DomainInvalidDate,
    DomainInvalidDateRange,
    DomainZeroValue,
    DomainValueOutOfRange,
    DomainEmptyCollection,
    DomainDuplicateValue,
    DomainOverlappingCollections,
    DomainInconsistentTotal,
    DomainInvalidReference,
    DomainRevisionConflict,
    DomainRevisionOverflow,
    DomainInvalidStateTransition,
}

impl ProblemCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DomainEmptyValue => "DOMAIN_EMPTY_VALUE",
            Self::DomainValueTooLong => "DOMAIN_VALUE_TOO_LONG",
            Self::DomainInvalidCode => "DOMAIN_INVALID_CODE",
            Self::DomainInvalidDate => "DOMAIN_INVALID_DATE",
            Self::DomainInvalidDateRange => "DOMAIN_INVALID_DATE_RANGE",
            Self::DomainZeroValue => "DOMAIN_ZERO_VALUE",
            Self::DomainValueOutOfRange => "DOMAIN_VALUE_OUT_OF_RANGE",
            Self::DomainEmptyCollection => "DOMAIN_EMPTY_COLLECTION",
            Self::DomainDuplicateValue => "DOMAIN_DUPLICATE_VALUE",
            Self::DomainOverlappingCollections => "DOMAIN_OVERLAPPING_COLLECTIONS",
            Self::DomainInconsistentTotal => "DOMAIN_INCONSISTENT_TOTAL",
            Self::DomainInvalidReference => "DOMAIN_INVALID_REFERENCE",
            Self::DomainRevisionConflict => "DOMAIN_REVISION_CONFLICT",
            Self::DomainRevisionOverflow => "DOMAIN_REVISION_OVERFLOW",
            Self::DomainInvalidStateTransition => "DOMAIN_INVALID_STATE_TRANSITION",
        }
    }
}

impl fmt::Display for ProblemCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Structured failures raised while establishing domain invariants.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum DomainError {
    #[error("{field} must not be empty")]
    EmptyValue { field: &'static str },

    #[error("{field} exceeds its maximum length of {max} characters (actual {actual})")]
    ValueTooLong {
        field: &'static str,
        max: usize,
        actual: usize,
    },

    #[error("{field} contains unsupported control characters")]
    InvalidCode { field: &'static str },

    #[error("invalid calendar date {year:04}-{month:02}-{day:02}")]
    InvalidDate { year: u16, month: u8, day: u8 },

    #[error("{start_field} must not be after {end_field}")]
    InvalidDateRange {
        start_field: &'static str,
        end_field: &'static str,
    },

    #[error("{field} must be greater than zero")]
    ZeroValue { field: &'static str },

    #[error("{field} must be between {min} and {max}, got {actual}")]
    ValueOutOfRange {
        field: &'static str,
        min: u64,
        max: u64,
        actual: u64,
    },

    #[error("{field} must contain at least one value")]
    EmptyCollection { field: &'static str },

    #[error("{field} contains duplicate value {value}")]
    DuplicateValue { field: &'static str, value: String },

    #[error("{first} and {second} overlap at {value}")]
    OverlappingCollections {
        first: &'static str,
        second: &'static str,
        value: String,
    },

    #[error("{field} total must be {expected}, got {actual}")]
    InconsistentTotal {
        field: &'static str,
        expected: u64,
        actual: u64,
    },

    #[error("{field} references missing {target} {value}")]
    InvalidReference {
        field: &'static str,
        target: &'static str,
        value: String,
    },

    #[error("revision conflict: expected {expected}, current revision is {actual}")]
    RevisionConflict {
        expected: Revision,
        actual: Revision,
    },

    #[error("revision cannot be incremented beyond u64::MAX")]
    RevisionOverflow,

    #[error("invalid {entity} state transition from {from} to {to}")]
    InvalidStateTransition {
        entity: &'static str,
        from: &'static str,
        to: &'static str,
    },
}

impl DomainError {
    #[must_use]
    pub const fn code(&self) -> ProblemCode {
        match self {
            Self::EmptyValue { .. } => ProblemCode::DomainEmptyValue,
            Self::ValueTooLong { .. } => ProblemCode::DomainValueTooLong,
            Self::InvalidCode { .. } => ProblemCode::DomainInvalidCode,
            Self::InvalidDate { .. } => ProblemCode::DomainInvalidDate,
            Self::InvalidDateRange { .. } => ProblemCode::DomainInvalidDateRange,
            Self::ZeroValue { .. } => ProblemCode::DomainZeroValue,
            Self::ValueOutOfRange { .. } => ProblemCode::DomainValueOutOfRange,
            Self::EmptyCollection { .. } => ProblemCode::DomainEmptyCollection,
            Self::DuplicateValue { .. } => ProblemCode::DomainDuplicateValue,
            Self::OverlappingCollections { .. } => ProblemCode::DomainOverlappingCollections,
            Self::InconsistentTotal { .. } => ProblemCode::DomainInconsistentTotal,
            Self::InvalidReference { .. } => ProblemCode::DomainInvalidReference,
            Self::RevisionConflict { .. } => ProblemCode::DomainRevisionConflict,
            Self::RevisionOverflow => ProblemCode::DomainRevisionOverflow,
            Self::InvalidStateTransition { .. } => ProblemCode::DomainInvalidStateTransition,
        }
    }
}
