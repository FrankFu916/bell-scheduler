use std::collections::{BTreeMap, BTreeSet};

use class_schedule_domain::{
    AdministrativeClassId, BuildingId, CourseOfferingId, CoursePlanId, Day, MeetingDemandId,
    RoomId, StudentId, SubjectId, TeacherId, TeachingSectionId, TimeslotId,
};
use serde::{Deserialize, Serialize};

use crate::{DenseBitSet, SchedulingError};

macro_rules! dense_index {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u32);

        impl $name {
            #[must_use]
            pub const fn as_usize(self) -> usize {
                self.0 as usize
            }
        }
    };
}

dense_index!(ActivityIndex);
dense_index!(RoomIndex);
dense_index!(StudentIndex);
dense_index!(TeacherIndex);
dense_index!(TimeslotIndex);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DenseTimeslot {
    pub stable_id: TimeslotId,
    pub day: Day,
    pub period_index: u16,
    pub instructional_block: u8,
    pub next_consecutive: Option<TimeslotIndex>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DenseTeacher {
    pub stable_id: TeacherId,
    /// Slots in which the teacher may teach.
    pub available: DenseBitSet,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DenseRoom {
    pub stable_id: RoomId,
    pub building_id: BuildingId,
    pub capacity: u16,
    pub features: BTreeSet<String>,
    /// Slots in which the room may be used.
    pub available: DenseBitSet,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeacherRequirement {
    Fixed { teacher: TeacherIndex },
    Candidates { teachers: Vec<TeacherIndex> },
}

impl TeacherRequirement {
    pub fn candidates(&self) -> &[TeacherIndex] {
        match self {
            Self::Fixed { teacher } => std::slice::from_ref(teacher),
            Self::Candidates { teachers } => teachers,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoomRequirement {
    AdminHomeRoom {
        room: RoomIndex,
    },
    Fixed {
        room: RoomIndex,
    },
    SectionFixed {
        section_id: TeachingSectionId,
        candidate_rooms: Vec<RoomIndex>,
    },
    PreferredFixed {
        section_id: TeachingSectionId,
        preferred_rooms: Vec<RoomIndex>,
        fallback_rooms: Vec<RoomIndex>,
    },
    Flexible {
        candidate_rooms: Vec<RoomIndex>,
    },
}

impl RoomRequirement {
    pub fn candidates(&self) -> Vec<RoomIndex> {
        match self {
            Self::AdminHomeRoom { room } | Self::Fixed { room } => vec![*room],
            Self::SectionFixed {
                candidate_rooms, ..
            }
            | Self::Flexible { candidate_rooms } => candidate_rooms.clone(),
            Self::PreferredFixed {
                preferred_rooms,
                fallback_rooms,
                ..
            } => preferred_rooms
                .iter()
                .chain(fallback_rooms)
                .copied()
                .collect(),
        }
    }

    #[must_use]
    pub const fn fixed_section(&self) -> Option<TeachingSectionId> {
        match self {
            Self::SectionFixed { section_id, .. } | Self::PreferredFixed { section_id, .. } => {
                Some(*section_id)
            }
            Self::AdminHomeRoom { .. } | Self::Fixed { .. } | Self::Flexible { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Activity {
    pub stable_id: MeetingDemandId,
    pub course_offering_id: CourseOfferingId,
    pub subject_id: SubjectId,
    pub course_plan_id: CoursePlanId,
    pub teaching_section_id: Option<TeachingSectionId>,
    pub administrative_class_id: Option<AdministrativeClassId>,
    pub duration_periods: u8,
    pub may_cross_breaks: bool,
    pub allowed_starts: Vec<TimeslotIndex>,
    pub audience: DenseBitSet,
    pub teacher: TeacherRequirement,
    pub room: RoomRequirement,
    pub required_capacity: u16,
    pub required_room_features: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Assignment {
    pub activity: ActivityIndex,
    pub start: TimeslotIndex,
    pub room: RoomIndex,
    pub teacher: TeacherIndex,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LockedAssignment {
    pub assignment: Assignment,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MeetingPatternRule {
    pub course_offering_id: CourseOfferingId,
    pub course_plan_id: CoursePlanId,
    pub activities: Vec<ActivityIndex>,
    pub minimum_gap_days: u8,
    pub maximum_periods_per_day: u8,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SchedulingProblemDraft {
    pub schema_version: u32,
    pub students: Vec<StudentId>,
    pub teachers: Vec<DenseTeacher>,
    pub rooms: Vec<DenseRoom>,
    pub timeslots: Vec<DenseTimeslot>,
    pub activities: Vec<Activity>,
    pub meeting_patterns: Vec<MeetingPatternRule>,
    pub locks: Vec<LockedAssignment>,
}

/// Canonical, structurally validated semantic input shared by scheduling, validation and adapters.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SchedulingProblemSnapshot(SchedulingProblemDraft);

impl TryFrom<SchedulingProblemDraft> for SchedulingProblemSnapshot {
    type Error = SchedulingError;

    fn try_from(draft: SchedulingProblemDraft) -> Result<Self, Self::Error> {
        validate_structure(&draft)?;
        Ok(Self(draft))
    }
}

impl SchedulingProblemSnapshot {
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.0.schema_version
    }
    #[must_use]
    pub fn students(&self) -> &[StudentId] {
        &self.0.students
    }
    #[must_use]
    pub fn teachers(&self) -> &[DenseTeacher] {
        &self.0.teachers
    }
    #[must_use]
    pub fn rooms(&self) -> &[DenseRoom] {
        &self.0.rooms
    }
    #[must_use]
    pub fn timeslots(&self) -> &[DenseTimeslot] {
        &self.0.timeslots
    }
    #[must_use]
    pub fn activities(&self) -> &[Activity] {
        &self.0.activities
    }
    #[must_use]
    pub fn meeting_patterns(&self) -> &[MeetingPatternRule] {
        &self.0.meeting_patterns
    }
    #[must_use]
    pub fn locks(&self) -> &[LockedAssignment] {
        &self.0.locks
    }

    /// Expands an activity start into its consecutive occupied timeslots.
    ///
    /// # Errors
    ///
    /// Returns a structured error for an unknown activity/start or a duration crossing a break.
    pub fn occupied_slots(
        &self,
        activity: ActivityIndex,
        start: TimeslotIndex,
    ) -> Result<Vec<TimeslotIndex>, SchedulingError> {
        let activity_value =
            self.activities()
                .get(activity.as_usize())
                .ok_or(SchedulingError::IndexOutOfRange {
                    field: "assignment.activity",
                    index: activity.0,
                    len: self.activities().len(),
                })?;
        expand_duration(
            &self.0.timeslots,
            start,
            activity_value.duration_periods,
            activity_value.may_cross_breaks,
        )
    }

    #[must_use]
    pub fn student_conflict_edges(&self) -> Vec<(ActivityIndex, ActivityIndex)> {
        let mut edges = Vec::new();
        for left in 0..self.activities().len() {
            for right in (left + 1)..self.activities().len() {
                if self.activities()[left]
                    .audience
                    .intersects(&self.activities()[right].audience)
                {
                    edges.push((
                        ActivityIndex(compact_u32(left)),
                        ActivityIndex(compact_u32(right)),
                    ));
                }
            }
        }
        edges
    }
}

// Keeping the complete structural gate together makes the construction contract auditable.
#[allow(clippy::too_many_lines)]
fn validate_structure(draft: &SchedulingProblemDraft) -> Result<(), SchedulingError> {
    if draft.schema_version == 0 {
        return Err(SchedulingError::ZeroSchemaVersion);
    }
    require_unique(&draft.students, "students.stable_id")?;
    require_unique_by(&draft.teachers, |item| item.stable_id, "teachers.stable_id")?;
    require_unique_by(&draft.rooms, |item| item.stable_id, "rooms.stable_id")?;
    require_unique_by(
        &draft.timeslots,
        |item| item.stable_id,
        "timeslots.stable_id",
    )?;
    require_unique_by(
        &draft.activities,
        |item| item.stable_id,
        "activities.stable_id",
    )?;

    let slot_count = draft.timeslots.len();
    for (index, slot) in draft.timeslots.iter().enumerate() {
        if slot.period_index == 0 {
            return Err(SchedulingError::ZeroValue {
                field: "timeslot.period_index",
                index,
            });
        }
        if let Some(next) = slot.next_consecutive {
            let next_slot =
                draft
                    .timeslots
                    .get(next.as_usize())
                    .ok_or(SchedulingError::IndexOutOfRange {
                        field: "timeslot.next_consecutive",
                        index: next.0,
                        len: slot_count,
                    })?;
            if next_slot.day != slot.day || next_slot.period_index != slot.period_index + 1 {
                return Err(SchedulingError::InvalidConsecutiveLink { index });
            }
        }
    }
    for (index, teacher) in draft.teachers.iter().enumerate() {
        ensure_bitset_len(&teacher.available, slot_count, "teacher.available", index)?;
    }
    for (index, room) in draft.rooms.iter().enumerate() {
        ensure_bitset_len(&room.available, slot_count, "room.available", index)?;
        if room.capacity == 0 {
            return Err(SchedulingError::ZeroValue {
                field: "room.capacity",
                index,
            });
        }
    }
    for (activity_index, activity) in draft.activities.iter().enumerate() {
        if activity.duration_periods == 0 {
            return Err(SchedulingError::ZeroValue {
                field: "activity.duration_periods",
                index: activity_index,
            });
        }
        if activity.audience.is_empty() {
            return Err(SchedulingError::EmptyCollection {
                field: "activity.audience",
                index: activity_index,
            });
        }
        ensure_bitset_len(
            &activity.audience,
            draft.students.len(),
            "activity.audience",
            activity_index,
        )?;
        validate_indices_allow_empty(
            &activity.allowed_starts,
            slot_count,
            "activity.allowed_starts",
            activity_index,
        )?;
        for start in &activity.allowed_starts {
            expand_duration(
                &draft.timeslots,
                *start,
                activity.duration_periods,
                activity.may_cross_breaks,
            )?;
        }
        validate_indices_allow_empty(
            activity.teacher.candidates(),
            draft.teachers.len(),
            "activity.teacher_candidates",
            activity_index,
        )?;
        let room_candidates = activity.room.candidates();
        validate_indices_allow_empty(
            &room_candidates,
            draft.rooms.len(),
            "activity.room_candidates",
            activity_index,
        )?;
        if let RoomRequirement::PreferredFixed {
            preferred_rooms,
            fallback_rooms,
            ..
        } = &activity.room
        {
            let preferred: BTreeSet<_> = preferred_rooms.iter().collect();
            if let Some(overlap) = fallback_rooms.iter().find(|room| preferred.contains(room)) {
                return Err(SchedulingError::OverlappingCandidates {
                    field: "activity.preferred_and_fallback_rooms",
                    index: activity_index,
                    value: overlap.0,
                });
            }
        }
    }

    let mut offerings = BTreeMap::new();
    for (index, activity) in draft.activities.iter().enumerate() {
        let signature = (
            activity.course_plan_id,
            activity.subject_id,
            activity.teaching_section_id,
            activity.administrative_class_id,
            &activity.teacher,
        );
        if let Some(previous) = offerings.insert(activity.course_offering_id, signature)
            && previous != signature
        {
            return Err(SchedulingError::InconsistentOffering { index });
        }
    }

    let mut covered_activities = BTreeSet::new();
    let mut covered_offerings = BTreeSet::new();
    for (pattern_index, pattern) in draft.meeting_patterns.iter().enumerate() {
        if !covered_offerings.insert(pattern.course_offering_id) {
            return Err(SchedulingError::DuplicateStableId {
                field: "meeting_patterns.course_offering_id",
                value: pattern.course_offering_id.to_string(),
            });
        }
        validate_indices(
            &pattern.activities,
            draft.activities.len(),
            "meeting_pattern.activities",
            pattern_index,
        )?;
        if pattern.maximum_periods_per_day == 0 {
            return Err(SchedulingError::ZeroValue {
                field: "meeting_pattern.maximum_periods_per_day",
                index: pattern_index,
            });
        }
        for activity in &pattern.activities {
            if !covered_activities.insert(*activity) {
                return Err(SchedulingError::DuplicateIndex {
                    field: "meeting_pattern.activity_membership",
                    index: activity.0,
                });
            }
            if draft.activities[activity.as_usize()].course_plan_id != pattern.course_plan_id {
                return Err(SchedulingError::MismatchedCoursePlan {
                    pattern_index,
                    activity_index: activity.0,
                });
            }
            if draft.activities[activity.as_usize()].course_offering_id
                != pattern.course_offering_id
            {
                return Err(SchedulingError::MismatchedCourseOffering {
                    pattern_index,
                    activity_index: activity.0,
                });
            }
        }
    }
    let mut locked = BTreeMap::new();
    for lock in &draft.locks {
        let assignment = lock.assignment;
        validate_index(
            assignment.activity.0,
            draft.activities.len(),
            "lock.activity",
        )?;
        validate_index(assignment.start.0, slot_count, "lock.start")?;
        validate_index(assignment.room.0, draft.rooms.len(), "lock.room")?;
        validate_index(assignment.teacher.0, draft.teachers.len(), "lock.teacher")?;
        if locked.insert(assignment.activity, assignment).is_some() {
            return Err(SchedulingError::DuplicateIndex {
                field: "locks.activity",
                index: assignment.activity.0,
            });
        }
    }
    Ok(())
}

fn expand_duration(
    timeslots: &[DenseTimeslot],
    start: TimeslotIndex,
    duration: u8,
    may_cross_breaks: bool,
) -> Result<Vec<TimeslotIndex>, SchedulingError> {
    let mut result = Vec::with_capacity(duration as usize);
    let mut current = start;
    for offset in 0..duration {
        let slot = timeslots
            .get(current.as_usize())
            .ok_or(SchedulingError::IndexOutOfRange {
                field: "activity.allowed_start",
                index: current.0,
                len: timeslots.len(),
            })?;
        result.push(current);
        if offset + 1 < duration {
            let next = slot
                .next_consecutive
                .ok_or(SchedulingError::DurationCrossesBreak {
                    start: start.0,
                    duration,
                })?;
            if !may_cross_breaks
                && timeslots[next.as_usize()].instructional_block != slot.instructional_block
            {
                return Err(SchedulingError::DurationCrossesBreak {
                    start: start.0,
                    duration,
                });
            }
            current = next;
        }
    }
    Ok(result)
}

fn validate_indices<T>(
    values: &[T],
    len: usize,
    field: &'static str,
    owner_index: usize,
) -> Result<(), SchedulingError>
where
    T: Copy + Ord + IntoIndex,
{
    if values.is_empty() {
        return Err(SchedulingError::EmptyCollection {
            field,
            index: owner_index,
        });
    }
    let mut seen = BTreeSet::new();
    for value in values {
        let index = value.index();
        validate_index(index, len, field)?;
        if !seen.insert(index) {
            return Err(SchedulingError::DuplicateIndex { field, index });
        }
    }
    Ok(())
}

fn validate_indices_allow_empty<T>(
    values: &[T],
    len: usize,
    field: &'static str,
    _owner_index: usize,
) -> Result<(), SchedulingError>
where
    T: Copy + Ord + IntoIndex,
{
    let mut seen = BTreeSet::new();
    for value in values {
        let index = value.index();
        validate_index(index, len, field)?;
        if !seen.insert(index) {
            return Err(SchedulingError::DuplicateIndex { field, index });
        }
    }
    Ok(())
}

trait IntoIndex {
    fn index(self) -> u32;
}

macro_rules! impl_into_index {
    ($($name:ident),+ $(,)?) => {$(
        impl IntoIndex for $name {
            fn index(self) -> u32 { self.0 }
        }
    )+};
}
impl_into_index!(ActivityIndex, RoomIndex, TeacherIndex, TimeslotIndex);

fn validate_index(index: u32, len: usize, field: &'static str) -> Result<(), SchedulingError> {
    if index as usize >= len {
        Err(SchedulingError::IndexOutOfRange { field, index, len })
    } else {
        Ok(())
    }
}

fn ensure_bitset_len(
    bitset: &DenseBitSet,
    expected: usize,
    field: &'static str,
    index: usize,
) -> Result<(), SchedulingError> {
    if bitset.len() == expected {
        Ok(())
    } else {
        Err(SchedulingError::BitSetLength {
            field,
            index,
            expected,
            actual: bitset.len(),
        })
    }
}

fn require_unique<T: Ord>(values: &[T], field: &'static str) -> Result<(), SchedulingError> {
    let mut seen = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        if !seen.insert(value) {
            return Err(SchedulingError::DuplicateIndex {
                field,
                index: compact_u32(index),
            });
        }
    }
    Ok(())
}

fn compact_u32(index: usize) -> u32 {
    u32::try_from(index).expect("a materialized scheduling snapshot cannot exceed u32 indices")
}

fn require_unique_by<T, K: Ord + std::fmt::Display>(
    values: &[T],
    key: impl Fn(&T) -> K,
    field: &'static str,
) -> Result<(), SchedulingError> {
    let mut seen = BTreeSet::new();
    for value in values {
        let key = key(value);
        if !seen.insert(key) {
            return Err(SchedulingError::DuplicateStableId {
                field,
                value: seen
                    .last()
                    .expect("just inserted or found a duplicate")
                    .to_string(),
            });
        }
    }
    Ok(())
}
