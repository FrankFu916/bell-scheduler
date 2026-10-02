use std::collections::BTreeSet;

use class_schedule_domain::{
    BuildingId, CourseOfferingId, CoursePlanId, Day, MeetingDemandId, RoomId, StudentId, SubjectId,
    TeacherId, TimeslotId,
};
use class_schedule_scheduling::{
    Activity, ActivityIndex, Assignment, DenseBitSet, DenseRoom, DenseTeacher, DenseTimeslot,
    LockedAssignment, RoomIndex, RoomRequirement, SchedulingError, SchedulingProblemDraft,
    SchedulingProblemSnapshot, TeacherIndex, TeacherRequirement, TimeslotIndex,
};
use uuid::Uuid;

fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}

fn draft(may_cross_breaks: bool) -> SchedulingProblemDraft {
    SchedulingProblemDraft {
        schema_version: 1,
        students: vec![StudentId::from_uuid(id(1))],
        teachers: vec![DenseTeacher {
            stable_id: TeacherId::from_uuid(id(2)),
            available: DenseBitSet::full(2),
        }],
        rooms: vec![DenseRoom {
            stable_id: RoomId::from_uuid(id(3)),
            building_id: BuildingId::from_uuid(id(4)),
            capacity: 1,
            features: BTreeSet::new(),
            available: DenseBitSet::full(2),
        }],
        timeslots: vec![
            DenseTimeslot {
                stable_id: TimeslotId::from_uuid(id(5)),
                day: Day::Monday,
                period_index: 4,
                instructional_block: 1,
                next_consecutive: Some(TimeslotIndex(1)),
            },
            DenseTimeslot {
                stable_id: TimeslotId::from_uuid(id(6)),
                day: Day::Monday,
                period_index: 5,
                instructional_block: 2,
                next_consecutive: None,
            },
        ],
        activities: vec![Activity {
            stable_id: MeetingDemandId::from_uuid(id(7)),
            course_offering_id: CourseOfferingId::from_uuid(id(70)),
            subject_id: SubjectId::from_uuid(id(8)),
            course_plan_id: CoursePlanId::from_uuid(id(9)),
            teaching_section_id: None,
            administrative_class_id: None,
            duration_periods: 2,
            may_cross_breaks,
            allowed_starts: vec![TimeslotIndex(0)],
            audience: DenseBitSet::full(1),
            teacher: TeacherRequirement::Fixed {
                teacher: TeacherIndex(0),
            },
            room: RoomRequirement::Fixed { room: RoomIndex(0) },
            required_capacity: 1,
            required_room_features: BTreeSet::new(),
        }],
        meeting_patterns: vec![],
        locks: vec![],
    }
}

#[test]
fn rejects_a_multi_period_start_that_crosses_a_break_by_default() {
    assert_eq!(
        SchedulingProblemSnapshot::try_from(draft(false)).unwrap_err(),
        SchedulingError::DurationCrossesBreak {
            start: 0,
            duration: 2,
        }
    );
}

#[test]
fn invalid_last_period_link_returns_an_error_without_integer_overflow() {
    let mut draft = draft(true);
    draft.timeslots[0].period_index = u16::MAX;
    assert_eq!(
        SchedulingProblemSnapshot::try_from(draft).unwrap_err(),
        SchedulingError::InvalidConsecutiveLink { index: 0 }
    );
}

#[test]
fn permits_crossing_a_break_only_when_the_activity_explicitly_allows_it() {
    let snapshot = SchedulingProblemSnapshot::try_from(draft(true)).unwrap();
    assert_eq!(
        snapshot
            .occupied_slots(
                class_schedule_scheduling::ActivityIndex(0),
                TimeslotIndex(0),
            )
            .unwrap(),
        vec![TimeslotIndex(0), TimeslotIndex(1)]
    );
}

fn lock(activity: u32) -> LockedAssignment {
    LockedAssignment {
        assignment: Assignment {
            activity: ActivityIndex(activity),
            start: TimeslotIndex(0),
            room: RoomIndex(0),
            teacher: TeacherIndex(0),
        },
    }
}

#[test]
fn added_locks_keep_the_source_and_have_a_canonical_input_order() {
    let mut draft = draft(true);
    for value in [10, 11] {
        let mut activity = draft.activities[0].clone();
        activity.stable_id = MeetingDemandId::from_uuid(id(value));
        draft.activities.push(activity);
    }
    draft.locks.push(lock(0));
    let original = SchedulingProblemSnapshot::try_from(draft).unwrap();
    assert_eq!(original.with_additional_locks(&[]).unwrap(), original);
    let extended = original.with_additional_locks(&[lock(2), lock(1)]).unwrap();
    assert_eq!(
        extended,
        original.with_additional_locks(&[lock(1), lock(2)]).unwrap()
    );
    assert_eq!(extended.locks(), &[lock(0), lock(1), lock(2)]);
    assert_eq!(original.locks(), &[lock(0)]);
    for invalid in [vec![lock(0)], vec![lock(1), lock(1)]] {
        assert!(matches!(
            original.with_additional_locks(&invalid),
            Err(SchedulingError::DuplicateIndex {
                field: "locks.activity",
                ..
            })
        ));
        assert_eq!(original.locks(), &[lock(0)]);
    }
}

#[test]
fn added_locks_reject_out_of_range_assignment_indices_without_changing_the_source() {
    let original = SchedulingProblemSnapshot::try_from(draft(true)).unwrap();
    let before = original.clone();
    let valid = lock(0);
    let invalid = [
        LockedAssignment {
            assignment: Assignment {
                activity: ActivityIndex(1),
                ..valid.assignment
            },
        },
        LockedAssignment {
            assignment: Assignment {
                start: TimeslotIndex(2),
                ..valid.assignment
            },
        },
        LockedAssignment {
            assignment: Assignment {
                room: RoomIndex(1),
                ..valid.assignment
            },
        },
        LockedAssignment {
            assignment: Assignment {
                teacher: TeacherIndex(1),
                ..valid.assignment
            },
        },
    ];
    for value in invalid {
        assert!(matches!(
            original.with_additional_locks(&[value]),
            Err(SchedulingError::IndexOutOfRange { .. })
        ));
        assert_eq!(original, before);
    }
}
