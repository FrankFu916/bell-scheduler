use crate::{
    AdministrativeClassId, ConstraintId, CoursePlanId, DomainError, MeetingDemandId, Name,
    ObjectiveTier, PreferenceId, RoomId, StudentId, SubjectId, TeacherId, TeachingSectionId,
    TimeslotId,
};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "tier")]
pub enum ConstraintLevel {
    Hard,
    Quality(ObjectiveTier),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum ResourceRef {
    Student(StudentId),
    Teacher(TeacherId),
    Room(RoomId),
    AdministrativeClass(AdministrativeClassId),
    TeachingSection(TeachingSectionId),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TimeslotSet(BTreeSet<TimeslotId>);

impl TimeslotSet {
    pub fn new(values: impl IntoIterator<Item = TimeslotId>) -> Result<Self, DomainError> {
        let values: Vec<_> = values.into_iter().collect();
        if values.is_empty() {
            return Err(DomainError::EmptyCollection { field: "timeslots" });
        }
        let set: BTreeSet<_> = values.iter().copied().collect();
        if set.len() != values.len() {
            let mut seen = BTreeSet::new();
            let duplicate = values
                .into_iter()
                .find(|value| !seen.insert(*value))
                .expect("length mismatch guarantees duplicate");
            return Err(DomainError::DuplicateValue {
                field: "timeslots",
                value: duplicate.to_string(),
            });
        }
        Ok(Self(set))
    }

    #[must_use]
    pub const fn as_set(&self) -> &BTreeSet<TimeslotId> {
        &self.0
    }
}

impl<'de> Deserialize<'de> for TimeslotSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<TimeslotId>::deserialize(deserializer)
            .and_then(|values| Self::new(values).map_err(D::Error::custom))
    }
}

/// User-authored constraints only. Intrinsic collision/capacity invariants remain hard in
/// validation and cannot be disabled by omitting an item from this catalogue.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ConstraintRule {
    ResourceUnavailable {
        resource: ResourceRef,
        timeslots: TimeslotSet,
    },
    RequiredStart {
        demand_id: MeetingDemandId,
        timeslot_id: TimeslotId,
    },
    ProhibitedStarts {
        demand_id: MeetingDemandId,
        timeslots: TimeslotSet,
    },
    RequiredRoom {
        demand_id: MeetingDemandId,
        room_id: RoomId,
    },
    RequiredTeacher {
        demand_id: MeetingDemandId,
        teacher_id: TeacherId,
    },
    MinimumDayGap {
        course_plan_id: CoursePlanId,
        days: u8,
    },
    MaximumPeriodsPerDay {
        course_plan_id: CoursePlanId,
        periods: u16,
    },
    MustNotCrossBreak {
        course_plan_id: CoursePlanId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Constraint {
    id: ConstraintId,
    name: Name,
    level: ConstraintLevel,
    rule: ConstraintRule,
}

impl Constraint {
    pub fn new(
        id: ConstraintId,
        name: Name,
        level: ConstraintLevel,
        rule: ConstraintRule,
    ) -> Result<Self, DomainError> {
        match &rule {
            ConstraintRule::MinimumDayGap { days, .. } if *days > 6 => {
                return Err(DomainError::ValueOutOfRange {
                    field: "constraint.minimum_day_gap.days",
                    min: 0,
                    max: 6,
                    actual: u64::from(*days),
                });
            }
            ConstraintRule::MaximumPeriodsPerDay { periods: 0, .. } => {
                return Err(DomainError::ZeroValue {
                    field: "constraint.maximum_periods_per_day.periods",
                });
            }
            _ => {}
        }
        Ok(Self {
            id,
            name,
            level,
            rule,
        })
    }

    #[must_use]
    pub const fn id(&self) -> ConstraintId {
        self.id
    }
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }
    #[must_use]
    pub const fn level(&self) -> ConstraintLevel {
        self.level
    }
    #[must_use]
    pub const fn rule(&self) -> &ConstraintRule {
        &self.rule
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum PreferenceTarget {
    Teacher(TeacherId),
    Subject(SubjectId),
    Room(RoomId),
    AdministrativeClass(AdministrativeClassId),
    TeachingSection(TeachingSectionId),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferenceDirection {
    Prefer,
    Avoid,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Preference {
    id: PreferenceId,
    name: Name,
    target: PreferenceTarget,
    timeslots: TimeslotSet,
    direction: PreferenceDirection,
    objective_tier: ObjectiveTier,
}

impl Preference {
    #[must_use]
    pub const fn new(
        id: PreferenceId,
        name: Name,
        target: PreferenceTarget,
        timeslots: TimeslotSet,
        direction: PreferenceDirection,
        objective_tier: ObjectiveTier,
    ) -> Self {
        Self {
            id,
            name,
            target,
            timeslots,
            direction,
            objective_tier,
        }
    }

    #[must_use]
    pub const fn id(&self) -> PreferenceId {
        self.id
    }
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }
    #[must_use]
    pub const fn target(&self) -> PreferenceTarget {
        self.target
    }
    #[must_use]
    pub const fn timeslots(&self) -> &TimeslotSet {
        &self.timeslots
    }
    #[must_use]
    pub const fn direction(&self) -> PreferenceDirection {
        self.direction
    }
    #[must_use]
    pub const fn objective_tier(&self) -> ObjectiveTier {
        self.objective_tier
    }
}
