use crate::{
    ConstraintId, DomainError, LockId, MeetingDemandId, MeetingDuration, Name, PreferenceId,
    Revision, RoomId, ScenarioId, ScheduledMeetingId, SchoolProjectId, TeacherId, TimeslotId,
    TimetableId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct MeetingAssignment {
    start: TimeslotId,
    duration: MeetingDuration,
    room_id: RoomId,
    teacher_id: TeacherId,
}

impl MeetingAssignment {
    #[must_use]
    pub const fn new(
        start: TimeslotId,
        duration: MeetingDuration,
        room_id: RoomId,
        teacher_id: TeacherId,
    ) -> Self {
        Self {
            start,
            duration,
            room_id,
            teacher_id,
        }
    }

    #[must_use]
    pub const fn start(self) -> TimeslotId {
        self.start
    }
    #[must_use]
    pub const fn duration(self) -> MeetingDuration {
        self.duration
    }
    #[must_use]
    pub const fn room_id(self) -> RoomId {
        self.room_id
    }
    #[must_use]
    pub const fn teacher_id(self) -> TeacherId {
        self.teacher_id
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ScheduledMeeting {
    id: ScheduledMeetingId,
    demand_id: MeetingDemandId,
    assignment: MeetingAssignment,
}

impl ScheduledMeeting {
    #[must_use]
    pub const fn new(
        id: ScheduledMeetingId,
        demand_id: MeetingDemandId,
        assignment: MeetingAssignment,
    ) -> Self {
        Self {
            id,
            demand_id,
            assignment,
        }
    }

    #[must_use]
    pub const fn id(self) -> ScheduledMeetingId {
        self.id
    }
    #[must_use]
    pub const fn demand_id(self) -> MeetingDemandId {
        self.demand_id
    }
    #[must_use]
    pub const fn assignment(self) -> MeetingAssignment {
        self.assignment
    }
}

/// A lock captures the exact assignment, avoiding ambiguity if a timetable is revised later.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Lock {
    id: LockId,
    scheduled_meeting_id: ScheduledMeetingId,
    assignment: MeetingAssignment,
}

impl Lock {
    #[must_use]
    pub const fn from_meeting(id: LockId, meeting: ScheduledMeeting) -> Self {
        Self {
            id,
            scheduled_meeting_id: meeting.id(),
            assignment: meeting.assignment(),
        }
    }

    #[must_use]
    pub const fn id(self) -> LockId {
        self.id
    }
    #[must_use]
    pub const fn scheduled_meeting_id(self) -> ScheduledMeetingId {
        self.scheduled_meeting_id
    }
    #[must_use]
    pub const fn assignment(self) -> MeetingAssignment {
        self.assignment
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum ScenarioBase {
    Empty,
    Timetable(TimetableId),
    Scenario(ScenarioId),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Scenario {
    id: ScenarioId,
    project_id: SchoolProjectId,
    name: Name,
    base: ScenarioBase,
    revision: Revision,
    constraint_ids: Vec<ConstraintId>,
    preference_ids: Vec<PreferenceId>,
    lock_ids: Vec<LockId>,
}

impl Scenario {
    pub fn new(
        id: ScenarioId,
        project_id: SchoolProjectId,
        name: Name,
        base: ScenarioBase,
    ) -> Result<Self, DomainError> {
        if base == ScenarioBase::Scenario(id) {
            return Err(DomainError::InvalidReference {
                field: "scenario.base",
                target: "different scenario",
                value: id.to_string(),
            });
        }
        Ok(Self {
            id,
            project_id,
            name,
            base,
            revision: Revision::INITIAL,
            constraint_ids: Vec::new(),
            preference_ids: Vec::new(),
            lock_ids: Vec::new(),
        })
    }

    pub fn replace_configuration(
        &mut self,
        expected_revision: Revision,
        constraint_ids: Vec<ConstraintId>,
        preference_ids: Vec<PreferenceId>,
        lock_ids: Vec<LockId>,
    ) -> Result<(), DomainError> {
        self.revision.ensure(expected_revision)?;
        require_unique(&constraint_ids, "scenario.constraint_ids")?;
        require_unique(&preference_ids, "scenario.preference_ids")?;
        require_unique(&lock_ids, "scenario.lock_ids")?;
        self.constraint_ids = constraint_ids;
        self.preference_ids = preference_ids;
        self.lock_ids = lock_ids;
        self.revision = self.revision.next()?;
        Ok(())
    }

    #[must_use]
    pub const fn id(&self) -> ScenarioId {
        self.id
    }
    #[must_use]
    pub const fn project_id(&self) -> SchoolProjectId {
        self.project_id
    }
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }
    #[must_use]
    pub const fn base(&self) -> ScenarioBase {
        self.base
    }
    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }
    #[must_use]
    pub fn constraint_ids(&self) -> &[ConstraintId] {
        &self.constraint_ids
    }
    #[must_use]
    pub fn preference_ids(&self) -> &[PreferenceId] {
        &self.preference_ids
    }
    #[must_use]
    pub fn lock_ids(&self) -> &[LockId] {
        &self.lock_ids
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Timetable {
    id: TimetableId,
    scenario_id: ScenarioId,
    revision: Revision,
    meetings: Vec<ScheduledMeeting>,
}

impl Timetable {
    pub fn new(
        id: TimetableId,
        scenario_id: ScenarioId,
        meetings: Vec<ScheduledMeeting>,
    ) -> Result<Self, DomainError> {
        require_unique_by(
            &meetings,
            |meeting| meeting.id().to_string(),
            "timetable.scheduled_meeting_ids",
        )?;
        require_unique_by(
            &meetings,
            |meeting| meeting.demand_id().to_string(),
            "timetable.meeting_demand_ids",
        )?;
        Ok(Self {
            id,
            scenario_id,
            revision: Revision::INITIAL,
            meetings,
        })
    }

    #[must_use]
    pub const fn id(&self) -> TimetableId {
        self.id
    }
    #[must_use]
    pub const fn scenario_id(&self) -> ScenarioId {
        self.scenario_id
    }
    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }
    #[must_use]
    pub fn meetings(&self) -> &[ScheduledMeeting] {
        &self.meetings
    }

    pub fn replace_meeting(
        &mut self,
        expected_revision: Revision,
        replacement: ScheduledMeeting,
    ) -> Result<(), DomainError> {
        self.revision.ensure(expected_revision)?;
        let Some(position) = self
            .meetings
            .iter()
            .position(|meeting| meeting.id() == replacement.id())
        else {
            return Err(DomainError::InvalidReference {
                field: "replacement.id",
                target: "scheduled meeting",
                value: replacement.id().to_string(),
            });
        };
        if self.meetings.iter().enumerate().any(|(index, meeting)| {
            index != position && meeting.demand_id() == replacement.demand_id()
        }) {
            return Err(DomainError::DuplicateValue {
                field: "timetable.meeting_demand_ids",
                value: replacement.demand_id().to_string(),
            });
        }
        self.meetings[position] = replacement;
        self.revision = self.revision.next()?;
        Ok(())
    }
}

fn require_unique<T>(values: &[T], field: &'static str) -> Result<(), DomainError>
where
    T: Copy + Ord + ToString,
{
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(*value) {
            return Err(DomainError::DuplicateValue {
                field,
                value: value.to_string(),
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
    use crate::ProblemCode;

    fn meeting(demand_id: MeetingDemandId) -> ScheduledMeeting {
        ScheduledMeeting::new(
            ScheduledMeetingId::new_v4(),
            demand_id,
            MeetingAssignment::new(
                TimeslotId::new_v4(),
                MeetingDuration::new(1).unwrap(),
                RoomId::new_v4(),
                TeacherId::new_v4(),
            ),
        )
    }

    #[test]
    fn timetable_rejects_two_assignments_for_one_demand() {
        let demand_id = MeetingDemandId::new_v4();
        let result = Timetable::new(
            TimetableId::new_v4(),
            ScenarioId::new_v4(),
            vec![meeting(demand_id), meeting(demand_id)],
        );
        assert_eq!(
            result.unwrap_err().code(),
            ProblemCode::DomainDuplicateValue
        );
    }
}
