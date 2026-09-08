use crate::{DomainError, ProblemCode};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Name(String);

impl Name {
    pub const MAX_CHARS: usize = 200;

    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into().trim().to_owned();
        if value.is_empty() {
            return Err(DomainError::EmptyValue { field: "name" });
        }
        let actual = value.chars().count();
        if actual > Self::MAX_CHARS {
            return Err(DomainError::ValueTooLong {
                field: "name",
                max: Self::MAX_CHARS,
                actual,
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ExternalCode(String);

impl ExternalCode {
    pub const MAX_CHARS: usize = 100;

    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into().trim().to_owned();
        if value.is_empty() {
            return Err(DomainError::EmptyValue {
                field: "external_code",
            });
        }
        let actual = value.chars().count();
        if actual > Self::MAX_CHARS {
            return Err(DomainError::ValueTooLong {
                field: "external_code",
                max: Self::MAX_CHARS,
                actual,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(DomainError::InvalidCode {
                field: "external_code",
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExternalCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct LocalDate {
    year: u16,
    month: u8,
    day: u8,
}

impl LocalDate {
    pub fn new(year: u16, month: u8, day: u8) -> Result<Self, DomainError> {
        let max_day =
            days_in_month(year, month).ok_or(DomainError::InvalidDate { year, month, day })?;
        if day == 0 || day > max_day {
            return Err(DomainError::InvalidDate { year, month, day });
        }
        Ok(Self { year, month, day })
    }

    #[must_use]
    pub const fn year(self) -> u16 {
        self.year
    }

    #[must_use]
    pub const fn month(self) -> u8 {
        self.month
    }

    #[must_use]
    pub const fn day(self) -> u8 {
        self.day
    }
}

const fn days_in_month(year: u16, month: u8) -> Option<u8> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if is_leap_year(year) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

const fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

impl fmt::Display for LocalDate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02}",
            self.year, self.month, self.day
        )
    }
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub struct Revision(u64);

impl Revision {
    pub const INITIAL: Self = Self(0);

    #[must_use]
    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn ensure(self, expected: Self) -> Result<(), DomainError> {
        if self == expected {
            Ok(())
        } else {
            Err(DomainError::RevisionConflict {
                expected,
                actual: self,
            })
        }
    }

    pub fn next(self) -> Result<Self, DomainError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(DomainError::RevisionOverflow)
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

macro_rules! positive_integer {
    ($name:ident, $inner:ty, $field:literal) => {
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(transparent)]
        pub struct $name($inner);

        impl $name {
            pub fn new(value: $inner) -> Result<Self, DomainError> {
                if value == 0 {
                    Err(DomainError::ZeroValue { field: $field })
                } else {
                    Ok(Self(value))
                }
            }

            #[must_use]
            pub const fn get(self) -> $inner {
                self.0
            }
        }
    };
}

positive_integer!(Capacity, u16, "capacity");
positive_integer!(MeetingDuration, u8, "meeting_duration");
positive_integer!(WeeklyPeriods, u16, "weekly_periods");
positive_integer!(PeriodIndex, u16, "period_index");
positive_integer!(WeekIndex, u16, "week_index");

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ObjectiveTier(u8);

impl ObjectiveTier {
    pub const HIGHEST: u8 = 1;
    pub const LOWEST: u8 = 32;

    pub fn new(priority: u8) -> Result<Self, DomainError> {
        if !(Self::HIGHEST..=Self::LOWEST).contains(&priority) {
            return Err(DomainError::ValueOutOfRange {
                field: "objective_tier",
                min: u64::from(Self::HIGHEST),
                max: u64::from(Self::LOWEST),
                actual: u64::from(priority),
            });
        }
        Ok(Self(priority))
    }

    #[must_use]
    pub const fn priority(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SolverSeed(u64);

impl SolverSeed {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ClassSizeRange {
    min: u16,
    target: u16,
    max: Capacity,
}

impl ClassSizeRange {
    pub fn new(min: u16, target: u16, max: Capacity) -> Result<Self, DomainError> {
        if min > target || target > max.get() {
            return Err(DomainError::ValueOutOfRange {
                field: "class_size_target",
                min: u64::from(min),
                max: u64::from(max.get()),
                actual: u64::from(target),
            });
        }
        Ok(Self { min, target, max })
    }

    #[must_use]
    pub const fn min(self) -> u16 {
        self.min
    }

    #[must_use]
    pub const fn target(self) -> u16 {
        self.target
    }

    #[must_use]
    pub const fn max(self) -> Capacity {
        self.max
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DomainProblem {
    code: ProblemCode,
    entity: Option<String>,
    field: Option<String>,
    parameters: Vec<(String, String)>,
}

impl DomainProblem {
    #[must_use]
    pub fn new(code: ProblemCode) -> Self {
        Self {
            code,
            entity: None,
            field: None,
            parameters: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_entity(mut self, entity: impl Into<String>) -> Self {
        self.entity = Some(entity.into());
        self
    }

    #[must_use]
    pub fn with_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }

    #[must_use]
    pub fn with_parameter(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.parameters.push((key.into(), value.into()));
        self
    }

    #[must_use]
    pub const fn code(&self) -> ProblemCode {
        self.code
    }

    #[must_use]
    pub fn entity(&self) -> Option<&str> {
        self.entity.as_deref()
    }

    #[must_use]
    pub fn field(&self) -> Option<&str> {
        self.field.as_deref()
    }

    #[must_use]
    pub fn parameters(&self) -> &[(String, String)] {
        &self.parameters
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_gregorian_dates() {
        assert!(LocalDate::new(2024, 2, 29).is_ok());
        assert_eq!(
            LocalDate::new(2023, 2, 29).unwrap_err().code(),
            ProblemCode::DomainInvalidDate
        );
    }

    #[test]
    fn optimistic_revision_conflicts_are_structured() {
        let actual = Revision::from_u64(9);
        let error = actual.ensure(Revision::from_u64(8)).unwrap_err();
        assert_eq!(error.code(), ProblemCode::DomainRevisionConflict);
    }
}
