#![forbid(unsafe_code)]

//! Independent hard-constraint validation and static feasibility checks.
//!
//! This crate never calls a solver. A solver-produced timetable is publishable only when
//! [`validate_assignments`] returns a report with no hard problems.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use class_schedule_domain::Day;
use class_schedule_scheduling::{
    ActivityIndex, Assignment, RoomIndex, SchedulingProblemSnapshot, TeacherIndex, TimeslotIndex,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[non_exhaustive]
pub enum HardProblemCode {
    AssignmentMissing,
    AssignmentDuplicate,
    AssignmentUnknownActivity,
    StartNotAllowed,
    DurationInvalid,
    TeacherNotAllowed,
    TeacherUnavailable,
    OfferingTeacherMismatch,
    RoomNotAllowed,
    RoomUnavailable,
    RoomCapacityInsufficient,
    RoomFeatureMissing,
    StudentConflict,
    TeacherConflict,
    RoomConflict,
    SectionFixedRoomMismatch,
    LockedAssignmentChanged,
    MeetingMinimumGapDays,
    MeetingMaximumPeriodsPerDay,
    MeetingDemandNoLegalStart,
    ActivityNoEligibleTeacher,
    ActivityNoEligibleRoom,
    ActivityNoFeasibleResourceCombination,
    TeacherRequiredLoadExceedsAvailability,
    StudentRequiredLoadExceedsCalendar,
    SectionHasNoCommonFixedRoom,
    FeatureRoomSupplyInsufficient,
    FixedAssignmentsConflict,
}

impl HardProblemCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AssignmentMissing => "VALIDATION_ASSIGNMENT_MISSING",
            Self::AssignmentDuplicate => "VALIDATION_ASSIGNMENT_DUPLICATE",
            Self::AssignmentUnknownActivity => "VALIDATION_ASSIGNMENT_UNKNOWN_ACTIVITY",
            Self::StartNotAllowed => "VALIDATION_START_NOT_ALLOWED",
            Self::DurationInvalid => "VALIDATION_DURATION_INVALID",
            Self::TeacherNotAllowed => "VALIDATION_TEACHER_NOT_ALLOWED",
            Self::TeacherUnavailable => "VALIDATION_TEACHER_UNAVAILABLE",
            Self::OfferingTeacherMismatch => "VALIDATION_OFFERING_TEACHER_MISMATCH",
            Self::RoomNotAllowed => "VALIDATION_ROOM_NOT_ALLOWED",
            Self::RoomUnavailable => "VALIDATION_ROOM_UNAVAILABLE",
            Self::RoomCapacityInsufficient => "VALIDATION_ROOM_CAPACITY_INSUFFICIENT",
            Self::RoomFeatureMissing => "VALIDATION_ROOM_FEATURE_MISSING",
            Self::StudentConflict => "VALIDATION_STUDENT_CONFLICT",
            Self::TeacherConflict => "VALIDATION_TEACHER_CONFLICT",
            Self::RoomConflict => "VALIDATION_ROOM_CONFLICT",
            Self::SectionFixedRoomMismatch => "VALIDATION_SECTION_FIXED_ROOM_MISMATCH",
            Self::LockedAssignmentChanged => "VALIDATION_LOCKED_ASSIGNMENT_CHANGED",
            Self::MeetingMinimumGapDays => "VALIDATION_MEETING_MINIMUM_GAP_DAYS",
            Self::MeetingMaximumPeriodsPerDay => "VALIDATION_MEETING_MAXIMUM_PERIODS_PER_DAY",
            Self::MeetingDemandNoLegalStart => "PRECHECK_MEETING_DEMAND_NO_LEGAL_START",
            Self::ActivityNoEligibleTeacher => "PRECHECK_ACTIVITY_NO_ELIGIBLE_TEACHER",
            Self::ActivityNoEligibleRoom => "PRECHECK_ACTIVITY_NO_ELIGIBLE_ROOM",
            Self::ActivityNoFeasibleResourceCombination => {
                "PRECHECK_ACTIVITY_NO_FEASIBLE_RESOURCE_COMBINATION"
            }
            Self::TeacherRequiredLoadExceedsAvailability => {
                "PRECHECK_TEACHER_REQUIRED_LOAD_EXCEEDS_AVAILABILITY"
            }
            Self::StudentRequiredLoadExceedsCalendar => {
                "PRECHECK_STUDENT_REQUIRED_LOAD_EXCEEDS_CALENDAR"
            }
            Self::SectionHasNoCommonFixedRoom => "PRECHECK_SECTION_HAS_NO_COMMON_FIXED_ROOM",
            Self::FeatureRoomSupplyInsufficient => "PRECHECK_FEATURE_ROOM_SUPPLY_INSUFFICIENT",
            Self::FixedAssignmentsConflict => "PRECHECK_FIXED_ASSIGNMENTS_CONFLICT",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HardProblem {
    pub code: HardProblemCode,
    pub activities: Vec<ActivityIndex>,
    pub entity_indices: BTreeMap<String, u32>,
    pub parameters: BTreeMap<String, String>,
}

impl HardProblem {
    fn for_activity(code: HardProblemCode, activity: ActivityIndex) -> Self {
        Self {
            code,
            activities: vec![activity],
            entity_indices: BTreeMap::new(),
            parameters: BTreeMap::new(),
        }
    }

    fn for_pair(code: HardProblemCode, left: ActivityIndex, right: ActivityIndex) -> Self {
        Self {
            code,
            activities: vec![left, right],
            entity_indices: BTreeMap::new(),
            parameters: BTreeMap::new(),
        }
    }

    fn entity(mut self, kind: &str, index: u32) -> Self {
        self.entity_indices.insert(kind.to_owned(), index);
        self
    }

    fn parameter(mut self, key: &str, value: &impl ToString) -> Self {
        self.parameters.insert(key.to_owned(), value.to_string());
        self
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ValidationReport {
    pub hard_problems: Vec<HardProblem>,
}

impl ValidationReport {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.hard_problems.is_empty()
    }

    #[must_use]
    pub fn contains(&self, code: HardProblemCode) -> bool {
        self.hard_problems
            .iter()
            .any(|problem| problem.code == code)
    }

    fn normalize(&mut self) {
        self.hard_problems.sort_by(|left, right| {
            (
                left.code.as_str(),
                &left.activities,
                &left.entity_indices,
                &left.parameters,
            )
                .cmp(&(
                    right.code.as_str(),
                    &right.activities,
                    &right.entity_indices,
                    &right.parameters,
                ))
        });
        self.hard_problems.dedup();
    }
}

/// Validates every hard rule represented by the semantic snapshot, independently of CP-SAT.
#[must_use]
// The single audit entry point intentionally keeps the validation phases in execution order.
#[allow(clippy::too_many_lines)]
pub fn validate_assignments(
    problem: &SchedulingProblemSnapshot,
    assignments: &[Assignment],
) -> ValidationReport {
    let mut report = ValidationReport::default();
    let mut by_activity = vec![None; problem.activities().len()];

    for assignment in assignments {
        let Some(slot) = by_activity.get_mut(assignment.activity.as_usize()) else {
            report.hard_problems.push(HardProblem::for_activity(
                HardProblemCode::AssignmentUnknownActivity,
                assignment.activity,
            ));
            continue;
        };
        if slot.replace(*assignment).is_some() {
            report.hard_problems.push(HardProblem::for_activity(
                HardProblemCode::AssignmentDuplicate,
                assignment.activity,
            ));
        }
    }
    for (index, assignment) in by_activity.iter().enumerate() {
        if assignment.is_none() {
            report.hard_problems.push(HardProblem::for_activity(
                HardProblemCode::AssignmentMissing,
                ActivityIndex(compact_u32(index)),
            ));
        }
    }

    let mut occupied_by_activity: Vec<Option<Vec<TimeslotIndex>>> =
        vec![None; problem.activities().len()];
    let mut teacher_occupancy: HashMap<(TeacherIndex, TimeslotIndex), ActivityIndex> =
        HashMap::new();
    let mut room_occupancy: HashMap<(RoomIndex, TimeslotIndex), ActivityIndex> = HashMap::new();
    let mut fixed_section_rooms = HashMap::new();
    let mut offering_teachers = HashMap::new();

    for (index, assignment) in by_activity.iter().enumerate() {
        let Some(assignment) = assignment else {
            continue;
        };
        let activity_index = ActivityIndex(compact_u32(index));
        let activity = &problem.activities()[index];
        if !activity.allowed_starts.contains(&assignment.start) {
            report.hard_problems.push(
                HardProblem::for_activity(HardProblemCode::StartNotAllowed, activity_index)
                    .entity("timeslot", assignment.start.0),
            );
        }
        let occupied = match problem.occupied_slots(activity_index, assignment.start) {
            Ok(value) => value,
            Err(error) => {
                report.hard_problems.push(
                    HardProblem::for_activity(HardProblemCode::DurationInvalid, activity_index)
                        .parameter("reason", &error.code()),
                );
                continue;
            }
        };
        occupied_by_activity[index] = Some(occupied.clone());

        if !activity.teacher.candidates().contains(&assignment.teacher) {
            report.hard_problems.push(
                HardProblem::for_activity(HardProblemCode::TeacherNotAllowed, activity_index)
                    .entity("teacher", assignment.teacher.0),
            );
        }
        match problem.teachers().get(assignment.teacher.as_usize()) {
            Some(teacher) => {
                if occupied
                    .iter()
                    .any(|slot| !teacher.available.contains(slot.as_usize()))
                {
                    report.hard_problems.push(
                        HardProblem::for_activity(
                            HardProblemCode::TeacherUnavailable,
                            activity_index,
                        )
                        .entity("teacher", assignment.teacher.0),
                    );
                }
            }
            None => report.hard_problems.push(
                HardProblem::for_activity(HardProblemCode::TeacherNotAllowed, activity_index)
                    .entity("teacher", assignment.teacher.0),
            ),
        }
        match offering_teachers.insert(
            activity.course_offering_id,
            (assignment.teacher, activity_index),
        ) {
            Some((previous_teacher, previous_activity))
                if previous_teacher != assignment.teacher =>
            {
                report.hard_problems.push(
                    HardProblem::for_pair(
                        HardProblemCode::OfferingTeacherMismatch,
                        previous_activity,
                        activity_index,
                    )
                    .entity("teacher", assignment.teacher.0)
                    .parameter("previous_teacher", &previous_teacher.0),
                );
            }
            _ => {}
        }

        let room_candidates = activity.room.candidates();
        if !room_candidates.contains(&assignment.room) {
            report.hard_problems.push(
                HardProblem::for_activity(HardProblemCode::RoomNotAllowed, activity_index)
                    .entity("room", assignment.room.0),
            );
        }
        match problem.rooms().get(assignment.room.as_usize()) {
            Some(room) => {
                let audience_size = activity
                    .audience
                    .count()
                    .max(activity.required_capacity as usize);
                if usize::from(room.capacity) < audience_size {
                    report.hard_problems.push(
                        HardProblem::for_activity(
                            HardProblemCode::RoomCapacityInsufficient,
                            activity_index,
                        )
                        .entity("room", assignment.room.0)
                        .parameter("required_capacity", &audience_size)
                        .parameter("actual_capacity", &room.capacity),
                    );
                }
                for feature in activity.required_room_features.difference(&room.features) {
                    report.hard_problems.push(
                        HardProblem::for_activity(
                            HardProblemCode::RoomFeatureMissing,
                            activity_index,
                        )
                        .entity("room", assignment.room.0)
                        .parameter("feature", feature),
                    );
                }
                if occupied
                    .iter()
                    .any(|slot| !room.available.contains(slot.as_usize()))
                {
                    report.hard_problems.push(
                        HardProblem::for_activity(HardProblemCode::RoomUnavailable, activity_index)
                            .entity("room", assignment.room.0),
                    );
                }
            }
            None => report.hard_problems.push(
                HardProblem::for_activity(HardProblemCode::RoomNotAllowed, activity_index)
                    .entity("room", assignment.room.0),
            ),
        }

        if let Some(section_id) = activity.room.fixed_section() {
            match fixed_section_rooms.insert(section_id, assignment.room) {
                Some(previous) if previous != assignment.room => {
                    report.hard_problems.push(
                        HardProblem::for_activity(
                            HardProblemCode::SectionFixedRoomMismatch,
                            activity_index,
                        )
                        .entity("room", assignment.room.0)
                        .parameter("previous_room", &previous.0),
                    );
                }
                _ => {}
            }
        }

        for slot in occupied {
            if let Some(other) =
                teacher_occupancy.insert((assignment.teacher, slot), activity_index)
            {
                report.hard_problems.push(
                    HardProblem::for_pair(HardProblemCode::TeacherConflict, other, activity_index)
                        .entity("teacher", assignment.teacher.0)
                        .entity("timeslot", slot.0),
                );
            }
            if let Some(other) = room_occupancy.insert((assignment.room, slot), activity_index) {
                report.hard_problems.push(
                    HardProblem::for_pair(HardProblemCode::RoomConflict, other, activity_index)
                        .entity("room", assignment.room.0)
                        .entity("timeslot", slot.0),
                );
            }
        }
    }

    validate_student_conflicts(problem, &by_activity, &occupied_by_activity, &mut report);
    validate_locks(problem, &by_activity, &mut report);
    validate_meeting_patterns(problem, &by_activity, &mut report);
    report.normalize();
    report
}

fn validate_student_conflicts(
    problem: &SchedulingProblemSnapshot,
    assignments: &[Option<Assignment>],
    occupied: &[Option<Vec<TimeslotIndex>>],
    report: &mut ValidationReport,
) {
    for left in 0..problem.activities().len() {
        for right in (left + 1)..problem.activities().len() {
            if !problem.activities()[left]
                .audience
                .intersects(&problem.activities()[right].audience)
            {
                continue;
            }
            let (Some(_), Some(_), Some(left_slots), Some(right_slots)) = (
                assignments[left],
                assignments[right],
                &occupied[left],
                &occupied[right],
            ) else {
                continue;
            };
            if slots_overlap(left_slots, right_slots) {
                report.hard_problems.push(HardProblem::for_pair(
                    HardProblemCode::StudentConflict,
                    ActivityIndex(compact_u32(left)),
                    ActivityIndex(compact_u32(right)),
                ));
            }
        }
    }
}

fn validate_locks(
    problem: &SchedulingProblemSnapshot,
    assignments: &[Option<Assignment>],
    report: &mut ValidationReport,
) {
    for lock in problem.locks() {
        if assignments
            .get(lock.assignment.activity.as_usize())
            .copied()
            .flatten()
            != Some(lock.assignment)
        {
            report.hard_problems.push(HardProblem::for_activity(
                HardProblemCode::LockedAssignmentChanged,
                lock.assignment.activity,
            ));
        }
    }
}

fn validate_meeting_patterns(
    problem: &SchedulingProblemSnapshot,
    assignments: &[Option<Assignment>],
    report: &mut ValidationReport,
) {
    for pattern in problem.meeting_patterns() {
        let mut assigned_days = Vec::new();
        let mut daily_periods: BTreeMap<Day, u16> = BTreeMap::new();
        for activity_index in &pattern.activities {
            let Some(assignment) = assignments[activity_index.as_usize()] else {
                continue;
            };
            let Some(slot) = problem.timeslots().get(assignment.start.as_usize()) else {
                continue;
            };
            assigned_days.push((day_number(slot.day), *activity_index));
            *daily_periods.entry(slot.day).or_default() +=
                u16::from(problem.activities()[activity_index.as_usize()].duration_periods);
        }
        assigned_days.sort_unstable_by_key(|(day, _)| *day);
        for pair in assigned_days.windows(2) {
            if pair[1].0 - pair[0].0 < u16::from(pattern.minimum_gap_days) {
                report.hard_problems.push(HardProblem::for_pair(
                    HardProblemCode::MeetingMinimumGapDays,
                    pair[0].1,
                    pair[1].1,
                ));
            }
        }
        for (day, periods) in daily_periods {
            if periods > u16::from(pattern.maximum_periods_per_day) {
                let mut problem_item = HardProblem {
                    code: HardProblemCode::MeetingMaximumPeriodsPerDay,
                    activities: pattern.activities.clone(),
                    entity_indices: BTreeMap::new(),
                    parameters: BTreeMap::new(),
                };
                problem_item
                    .parameters
                    .insert("day".to_owned(), format!("{day:?}"));
                problem_item
                    .parameters
                    .insert("periods".to_owned(), periods.to_string());
                problem_item.parameters.insert(
                    "maximum".to_owned(),
                    pattern.maximum_periods_per_day.to_string(),
                );
                report.hard_problems.push(problem_item);
            }
        }
    }
}

/// Finds necessary contradictions before starting a native solver worker.
#[must_use]
pub fn static_feasibility_check(problem: &SchedulingProblemSnapshot) -> ValidationReport {
    let mut report = ValidationReport::default();
    check_activity_domains(problem, &mut report);
    check_fixed_teacher_load(problem, &mut report);
    check_student_load(problem, &mut report);
    check_fixed_section_rooms(problem, &mut report);
    check_feature_supply(problem, &mut report);
    check_locked_conflicts(problem, &mut report);
    report.normalize();
    report
}

fn check_activity_domains(problem: &SchedulingProblemSnapshot, report: &mut ValidationReport) {
    for (index, activity) in problem.activities().iter().enumerate() {
        let activity_index = ActivityIndex(compact_u32(index));
        if activity.allowed_starts.is_empty() {
            report.hard_problems.push(HardProblem::for_activity(
                HardProblemCode::MeetingDemandNoLegalStart,
                activity_index,
            ));
        }
        if activity.teacher.candidates().is_empty() {
            report.hard_problems.push(HardProblem::for_activity(
                HardProblemCode::ActivityNoEligibleTeacher,
                activity_index,
            ));
        }
        let eligible_rooms: Vec<_> = activity
            .room
            .candidates()
            .into_iter()
            .filter(|room_index| {
                problem
                    .rooms()
                    .get(room_index.as_usize())
                    .is_some_and(|room| {
                        usize::from(room.capacity)
                            >= activity
                                .audience
                                .count()
                                .max(activity.required_capacity as usize)
                            && activity.required_room_features.is_subset(&room.features)
                    })
            })
            .collect();
        if eligible_rooms.is_empty() {
            report.hard_problems.push(HardProblem::for_activity(
                HardProblemCode::ActivityNoEligibleRoom,
                activity_index,
            ));
        }
        if !activity.allowed_starts.is_empty()
            && !activity.teacher.candidates().is_empty()
            && !eligible_rooms.is_empty()
            && !has_resource_combination(problem, activity_index, &eligible_rooms)
        {
            report.hard_problems.push(HardProblem::for_activity(
                HardProblemCode::ActivityNoFeasibleResourceCombination,
                activity_index,
            ));
        }
    }
}

fn has_resource_combination(
    problem: &SchedulingProblemSnapshot,
    activity_index: ActivityIndex,
    eligible_rooms: &[RoomIndex],
) -> bool {
    let activity = &problem.activities()[activity_index.as_usize()];
    activity.allowed_starts.iter().any(|start| {
        let Ok(slots) = problem.occupied_slots(activity_index, *start) else {
            return false;
        };
        let has_teacher = activity.teacher.candidates().iter().any(|teacher_index| {
            problem
                .teachers()
                .get(teacher_index.as_usize())
                .is_some_and(|teacher| {
                    slots
                        .iter()
                        .all(|slot| teacher.available.contains(slot.as_usize()))
                })
        });
        let has_room = eligible_rooms.iter().any(|room_index| {
            problem
                .rooms()
                .get(room_index.as_usize())
                .is_some_and(|room| {
                    slots
                        .iter()
                        .all(|slot| room.available.contains(slot.as_usize()))
                })
        });
        has_teacher && has_room
    })
}

fn check_fixed_teacher_load(problem: &SchedulingProblemSnapshot, report: &mut ValidationReport) {
    let mut required = vec![0_u64; problem.teachers().len()];
    let mut activities = vec![Vec::new(); problem.teachers().len()];
    for (index, activity) in problem.activities().iter().enumerate() {
        if let class_schedule_scheduling::TeacherRequirement::Fixed { teacher } = activity.teacher {
            required[teacher.as_usize()] += u64::from(activity.duration_periods);
            activities[teacher.as_usize()].push(ActivityIndex(compact_u32(index)));
        }
    }
    for (index, teacher) in problem.teachers().iter().enumerate() {
        let available = teacher.available.count() as u64;
        if required[index] > available {
            report.hard_problems.push(HardProblem {
                code: HardProblemCode::TeacherRequiredLoadExceedsAvailability,
                activities: activities[index].clone(),
                entity_indices: BTreeMap::from([("teacher".to_owned(), compact_u32(index))]),
                parameters: BTreeMap::from([
                    ("required_periods".to_owned(), required[index].to_string()),
                    ("available_periods".to_owned(), available.to_string()),
                ]),
            });
        }
    }
}

fn check_student_load(problem: &SchedulingProblemSnapshot, report: &mut ValidationReport) {
    let mut required = vec![0_u64; problem.students().len()];
    let mut activities = vec![Vec::new(); problem.students().len()];
    for (index, activity) in problem.activities().iter().enumerate() {
        for student in activity.audience.indices() {
            required[student] += u64::from(activity.duration_periods);
            activities[student].push(ActivityIndex(compact_u32(index)));
        }
    }
    let available = problem.timeslots().len() as u64;
    for (student, load) in required.into_iter().enumerate() {
        if load > available {
            report.hard_problems.push(HardProblem {
                code: HardProblemCode::StudentRequiredLoadExceedsCalendar,
                activities: activities[student].clone(),
                entity_indices: BTreeMap::from([("student".to_owned(), compact_u32(student))]),
                parameters: BTreeMap::from([
                    ("required_periods".to_owned(), load.to_string()),
                    ("available_periods".to_owned(), available.to_string()),
                ]),
            });
        }
    }
}

fn check_fixed_section_rooms(problem: &SchedulingProblemSnapshot, report: &mut ValidationReport) {
    let mut candidates: HashMap<_, (BTreeSet<RoomIndex>, Vec<ActivityIndex>)> = HashMap::new();
    for (index, activity) in problem.activities().iter().enumerate() {
        let Some(section) = activity.room.fixed_section() else {
            continue;
        };
        let current: BTreeSet<_> = activity.room.candidates().into_iter().collect();
        candidates
            .entry(section)
            .and_modify(|(intersection, activities)| {
                intersection.retain(|room| current.contains(room));
                activities.push(ActivityIndex(compact_u32(index)));
            })
            .or_insert_with(|| (current, vec![ActivityIndex(compact_u32(index))]));
    }
    for (_section, (rooms, activities)) in candidates {
        if rooms.is_empty() {
            report.hard_problems.push(HardProblem {
                code: HardProblemCode::SectionHasNoCommonFixedRoom,
                activities,
                entity_indices: BTreeMap::new(),
                parameters: BTreeMap::new(),
            });
        }
    }
}

fn check_feature_supply(problem: &SchedulingProblemSnapshot, report: &mut ValidationReport) {
    let mut demand: BTreeMap<&str, (u64, Vec<ActivityIndex>)> = BTreeMap::new();
    for (index, activity) in problem.activities().iter().enumerate() {
        for feature in &activity.required_room_features {
            let entry = demand.entry(feature).or_default();
            entry.0 += u64::from(activity.duration_periods);
            entry.1.push(ActivityIndex(compact_u32(index)));
        }
    }
    for (feature, (required, activities)) in demand {
        let available: u64 = problem
            .rooms()
            .iter()
            .filter(|room| room.features.contains(feature))
            .map(|room| room.available.count() as u64)
            .sum();
        if required > available {
            report.hard_problems.push(HardProblem {
                code: HardProblemCode::FeatureRoomSupplyInsufficient,
                activities,
                entity_indices: BTreeMap::new(),
                parameters: BTreeMap::from([
                    ("feature".to_owned(), feature.to_owned()),
                    ("required_periods".to_owned(), required.to_string()),
                    ("available_room_periods".to_owned(), available.to_string()),
                ]),
            });
        }
    }
}

fn check_locked_conflicts(problem: &SchedulingProblemSnapshot, report: &mut ValidationReport) {
    for left in 0..problem.locks().len() {
        for right in (left + 1)..problem.locks().len() {
            let left_assignment = problem.locks()[left].assignment;
            let right_assignment = problem.locks()[right].assignment;
            let Ok(left_slots) =
                problem.occupied_slots(left_assignment.activity, left_assignment.start)
            else {
                continue;
            };
            let Ok(right_slots) =
                problem.occupied_slots(right_assignment.activity, right_assignment.start)
            else {
                continue;
            };
            if !slots_overlap(&left_slots, &right_slots) {
                continue;
            }
            let left_activity = &problem.activities()[left_assignment.activity.as_usize()];
            let right_activity = &problem.activities()[right_assignment.activity.as_usize()];
            if left_assignment.teacher == right_assignment.teacher
                || left_assignment.room == right_assignment.room
                || left_activity.audience.intersects(&right_activity.audience)
            {
                report.hard_problems.push(HardProblem::for_pair(
                    HardProblemCode::FixedAssignmentsConflict,
                    left_assignment.activity,
                    right_assignment.activity,
                ));
            }
        }
    }
}

fn slots_overlap(left: &[TimeslotIndex], right: &[TimeslotIndex]) -> bool {
    left.iter().any(|slot| right.contains(slot))
}

fn compact_u32(index: usize) -> u32 {
    u32::try_from(index).expect("a materialized scheduling snapshot cannot exceed u32 indices")
}

const fn day_number(day: Day) -> u16 {
    match day {
        Day::Monday => 1,
        Day::Tuesday => 2,
        Day::Wednesday => 3,
        Day::Thursday => 4,
        Day::Friday => 5,
        Day::Saturday => 6,
        Day::Sunday => 7,
    }
}
