use crate::{
    AcademicTermId, CalendarId, DomainError, LocalDate, Name, PeriodId, PeriodIndex,
    SchoolProjectId, TimeslotId, WeekIndex, WeekPatternId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AcademicTerm {
    id: AcademicTermId,
    project_id: SchoolProjectId,
    name: Name,
    starts_on: LocalDate,
    ends_on: LocalDate,
}

impl AcademicTerm {
    pub fn new(
        id: AcademicTermId,
        project_id: SchoolProjectId,
        name: Name,
        starts_on: LocalDate,
        ends_on: LocalDate,
    ) -> Result<Self, DomainError> {
        if starts_on > ends_on {
            return Err(DomainError::InvalidDateRange {
                start_field: "academic_term.starts_on",
                end_field: "academic_term.ends_on",
            });
        }
        Ok(Self {
            id,
            project_id,
            name,
            starts_on,
            ends_on,
        })
    }

    #[must_use]
    pub const fn id(&self) -> AcademicTermId {
        self.id
    }

    #[must_use]
    pub const fn project_id(&self) -> SchoolProjectId {
        self.project_id
    }

    #[must_use]
    pub fn name(&self) -> &Name {
        &self.name
    }

    #[must_use]
    pub const fn starts_on(&self) -> LocalDate {
        self.starts_on
    }

    #[must_use]
    pub const fn ends_on(&self) -> LocalDate {
        self.ends_on
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Day {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WeekPattern {
    id: WeekPatternId,
    name: Name,
    weeks: BTreeSet<WeekIndex>,
}

impl WeekPattern {
    pub fn new(
        id: WeekPatternId,
        name: Name,
        weeks: impl IntoIterator<Item = WeekIndex>,
    ) -> Result<Self, DomainError> {
        let weeks: Vec<_> = weeks.into_iter().collect();
        if weeks.is_empty() {
            return Err(DomainError::EmptyCollection {
                field: "week_pattern.weeks",
            });
        }
        let unique: BTreeSet<_> = weeks.iter().copied().collect();
        if unique.len() != weeks.len() {
            let duplicate = first_duplicate(weeks).expect("length mismatch guarantees duplicate");
            return Err(DomainError::DuplicateValue {
                field: "week_pattern.weeks",
                value: duplicate.get().to_string(),
            });
        }
        Ok(Self {
            id,
            name,
            weeks: unique,
        })
    }

    #[must_use]
    pub const fn id(&self) -> WeekPatternId {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &Name {
        &self.name
    }

    #[must_use]
    pub const fn weeks(&self) -> &BTreeSet<WeekIndex> {
        &self.weeks
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Period {
    id: PeriodId,
    index: PeriodIndex,
    label: Name,
    /// Consecutive periods may form one meeting only while their block is equal.
    instructional_block: u8,
}

impl Period {
    #[must_use]
    pub const fn new(
        id: PeriodId,
        index: PeriodIndex,
        label: Name,
        instructional_block: u8,
    ) -> Self {
        Self {
            id,
            index,
            label,
            instructional_block,
        }
    }

    #[must_use]
    pub const fn id(&self) -> PeriodId {
        self.id
    }

    #[must_use]
    pub const fn index(&self) -> PeriodIndex {
        self.index
    }

    #[must_use]
    pub const fn label(&self) -> &Name {
        &self.label
    }

    #[must_use]
    pub const fn instructional_block(&self) -> u8 {
        self.instructional_block
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Timeslot {
    id: TimeslotId,
    day: Day,
    period_id: PeriodId,
}

impl Timeslot {
    #[must_use]
    pub const fn new(id: TimeslotId, day: Day, period_id: PeriodId) -> Self {
        Self { id, day, period_id }
    }

    #[must_use]
    pub const fn id(self) -> TimeslotId {
        self.id
    }

    #[must_use]
    pub const fn day(self) -> Day {
        self.day
    }

    #[must_use]
    pub const fn period_id(self) -> PeriodId {
        self.period_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Calendar {
    id: CalendarId,
    term_id: AcademicTermId,
    instructional_days: BTreeSet<Day>,
    periods: Vec<Period>,
    week_patterns: Vec<WeekPattern>,
    timeslots: Vec<Timeslot>,
}

impl Calendar {
    pub fn new(
        id: CalendarId,
        term_id: AcademicTermId,
        instructional_days: impl IntoIterator<Item = Day>,
        periods: Vec<Period>,
        week_patterns: Vec<WeekPattern>,
        timeslots: Vec<Timeslot>,
    ) -> Result<Self, DomainError> {
        let day_values: Vec<_> = instructional_days.into_iter().collect();
        let days: BTreeSet<_> = day_values.iter().copied().collect();
        require_nonempty(&day_values, "calendar.instructional_days")?;
        require_unique(&day_values, "calendar.instructional_days")?;
        require_nonempty(&periods, "calendar.periods")?;
        require_nonempty(&week_patterns, "calendar.week_patterns")?;
        require_nonempty(&timeslots, "calendar.timeslots")?;

        require_unique_by(
            &periods,
            |period| period.id().to_string(),
            "calendar.period_ids",
        )?;
        require_unique_by(
            &periods,
            |period| period.index().get().to_string(),
            "calendar.period_indices",
        )?;
        require_unique_by(
            &week_patterns,
            |pattern| pattern.id().to_string(),
            "calendar.week_pattern_ids",
        )?;
        require_unique_by(
            &timeslots,
            |timeslot| timeslot.id().to_string(),
            "calendar.timeslot_ids",
        )?;
        require_unique_by(
            &timeslots,
            |timeslot| format!("{:?}:{}", timeslot.day(), timeslot.period_id()),
            "calendar.day_period_pairs",
        )?;

        let period_ids: BTreeSet<_> = periods.iter().map(Period::id).collect();
        for timeslot in &timeslots {
            if !days.contains(&timeslot.day()) {
                return Err(DomainError::InvalidReference {
                    field: "timeslot.day",
                    target: "instructional day",
                    value: format!("{:?}", timeslot.day()),
                });
            }
            if !period_ids.contains(&timeslot.period_id()) {
                return Err(DomainError::InvalidReference {
                    field: "timeslot.period_id",
                    target: "period",
                    value: timeslot.period_id().to_string(),
                });
            }
        }

        Ok(Self {
            id,
            term_id,
            instructional_days: days,
            periods,
            week_patterns,
            timeslots,
        })
    }

    #[must_use]
    pub const fn id(&self) -> CalendarId {
        self.id
    }

    #[must_use]
    pub const fn term_id(&self) -> AcademicTermId {
        self.term_id
    }

    #[must_use]
    pub const fn instructional_days(&self) -> &BTreeSet<Day> {
        &self.instructional_days
    }

    #[must_use]
    pub fn periods(&self) -> &[Period] {
        &self.periods
    }

    #[must_use]
    pub fn week_patterns(&self) -> &[WeekPattern] {
        &self.week_patterns
    }

    #[must_use]
    pub fn timeslots(&self) -> &[Timeslot] {
        &self.timeslots
    }
}

fn first_duplicate<T: Ord + Copy>(values: Vec<T>) -> Option<T> {
    let mut seen = BTreeSet::new();
    values.into_iter().find(|value| !seen.insert(*value))
}

fn require_nonempty<T>(values: &[T], field: &'static str) -> Result<(), DomainError> {
    if values.is_empty() {
        Err(DomainError::EmptyCollection { field })
    } else {
        Ok(())
    }
}

fn require_unique<T: Ord + std::fmt::Debug>(
    values: &[T],
    field: &'static str,
) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(DomainError::DuplicateValue {
                field,
                value: format!("{value:?}"),
            });
        }
    }
    Ok(())
}

fn require_unique_by<T>(
    values: &[T],
    key: impl Fn(&T) -> String,
    field: &'static str,
) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for value in values {
        let key = key(value);
        if !seen.insert(key.clone()) {
            return Err(DomainError::DuplicateValue { field, value: key });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn academic_term_rejects_reversed_dates() {
        let result = AcademicTerm::new(
            AcademicTermId::new_v4(),
            SchoolProjectId::new_v4(),
            Name::new("2026 秋季").unwrap(),
            LocalDate::new(2027, 1, 1).unwrap(),
            LocalDate::new(2026, 9, 1).unwrap(),
        );
        assert!(matches!(result, Err(DomainError::InvalidDateRange { .. })));
    }
}
