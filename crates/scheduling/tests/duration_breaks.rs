use std::collections::BTreeSet;

use class_schedule_domain::{
    BuildingId, CourseOfferingId, CoursePlanId, Day, MeetingDemandId, RoomId, StudentId, SubjectId,
    TeacherId, TimeslotId,
};
use class_schedule_scheduling::{
    Activity, DenseBitSet, DenseRoom, DenseTeacher, DenseTimeslot, RoomIndex, RoomRequirement,
    SchedulingError, SchedulingProblemDraft, SchedulingProblemSnapshot, TeacherIndex,
    TeacherRequirement, TimeslotIndex,
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
