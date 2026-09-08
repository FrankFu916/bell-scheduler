use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use uuid::Uuid;

macro_rules! typed_uuid_ids {
    ($($name:ident),+ $(,)?) => {
        $(
            #[doc = concat!("Stable internal identifier for `", stringify!($name), "`.")]
            #[derive(
                Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
            )]
            #[serde(transparent)]
            pub struct $name(Uuid);

            impl $name {
                #[must_use]
                pub fn new_v4() -> Self {
                    Self(Uuid::new_v4())
                }

                #[must_use]
                pub const fn from_uuid(value: Uuid) -> Self {
                    Self(value)
                }

                #[must_use]
                pub const fn as_uuid(self) -> Uuid {
                    self.0
                }
            }

            impl From<Uuid> for $name {
                fn from(value: Uuid) -> Self {
                    Self::from_uuid(value)
                }
            }

            impl From<$name> for Uuid {
                fn from(value: $name) -> Self {
                    value.as_uuid()
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    self.0.fmt(formatter)
                }
            }

            impl FromStr for $name {
                type Err = uuid::Error;

                fn from_str(value: &str) -> Result<Self, Self::Err> {
                    Uuid::parse_str(value).map(Self)
                }
            }
        )+
    };
}

typed_uuid_ids!(
    SchoolProjectId,
    AcademicTermId,
    CalendarId,
    WeekPatternId,
    PeriodId,
    TimeslotId,
    GradeId,
    StudentId,
    TeacherId,
    BuildingId,
    RoomId,
    AdministrativeClassId,
    SubjectId,
    CoursePlanId,
    CourseOfferingId,
    TeachingSectionId,
    SectionEnrollmentId,
    MeetingDemandId,
    ScheduledMeetingId,
    ConstraintId,
    PreferenceId,
    LockId,
    ScenarioId,
    TimetableId,
    SolverRunId,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_id_round_trips_as_uuid_text() {
        let raw = Uuid::parse_str("018f6eb7-73a5-7c32-97c8-b1b4f7f06501").unwrap();
        let id = StudentId::from_uuid(raw);

        assert_eq!(id.to_string().parse::<StudentId>().unwrap(), id);
        assert_eq!(id.as_uuid(), raw);
    }
}
