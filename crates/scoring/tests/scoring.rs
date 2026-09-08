use std::cmp::Ordering;
use std::collections::BTreeSet;

use class_schedule_domain::{
    AdministrativeClassId, BuildingId, CourseOfferingId, CoursePlanId, Day, MeetingDemandId,
    RoomId, StudentId, SubjectId, TeacherId, TimeslotId,
};
use class_schedule_scheduling::{
    Activity, ActivityIndex, Assignment, DenseBitSet, DenseRoom, DenseTeacher, DenseTimeslot,
    MeetingPatternRule, RoomIndex, RoomRequirement, SchedulingProblemDraft,
    SchedulingProblemSnapshot, TeacherIndex, TeacherRequirement, TimeslotIndex,
};
use class_schedule_scoring::{
    MetricKind, ObjectivePlan, ObjectiveVector, ScoringContext, ScoringError, TierScore, evaluate,
};
use uuid::Uuid;

fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}

#[allow(clippy::too_many_lines)]
fn fixture() -> SchedulingProblemSnapshot {
    let plan = CoursePlanId::from_uuid(id(30));
    let offering = CourseOfferingId::from_uuid(id(29));
    let subject = SubjectId::from_uuid(id(31));
    let admin = AdministrativeClassId::from_uuid(id(32));
    let activities = (0..2)
        .map(|index| Activity {
            stable_id: MeetingDemandId::from_uuid(id(40 + index)),
            course_offering_id: offering,
            subject_id: subject,
            course_plan_id: plan,
            teaching_section_id: None,
            administrative_class_id: Some(admin),
            duration_periods: 1,
            may_cross_breaks: false,
            allowed_starts: vec![TimeslotIndex(0), TimeslotIndex(1), TimeslotIndex(2)],
            audience: DenseBitSet::full(1),
            teacher: TeacherRequirement::Fixed {
                teacher: TeacherIndex(0),
            },
            room: RoomRequirement::Fixed {
                room: RoomIndex(u32::try_from(index).unwrap()),
            },
            required_capacity: 1,
            required_room_features: BTreeSet::new(),
        })
        .collect();
    SchedulingProblemDraft {
        schema_version: 1,
        students: vec![StudentId::from_uuid(id(1))],
        teachers: vec![
            DenseTeacher {
                stable_id: TeacherId::from_uuid(id(2)),
                available: DenseBitSet::full(3),
            },
            DenseTeacher {
                stable_id: TeacherId::from_uuid(id(3)),
                available: DenseBitSet::full(3),
            },
        ],
        rooms: vec![
            DenseRoom {
                stable_id: RoomId::from_uuid(id(4)),
                building_id: BuildingId::from_uuid(id(5)),
                capacity: 1,
                features: BTreeSet::new(),
                available: DenseBitSet::full(3),
            },
            DenseRoom {
                stable_id: RoomId::from_uuid(id(6)),
                building_id: BuildingId::from_uuid(id(7)),
                capacity: 1,
                features: BTreeSet::new(),
                available: DenseBitSet::full(3),
            },
        ],
        timeslots: vec![
            DenseTimeslot {
                stable_id: TimeslotId::from_uuid(id(10)),
                day: Day::Monday,
                period_index: 1,
                instructional_block: 1,
                next_consecutive: Some(TimeslotIndex(1)),
            },
            DenseTimeslot {
                stable_id: TimeslotId::from_uuid(id(11)),
                day: Day::Monday,
                period_index: 2,
                instructional_block: 1,
                next_consecutive: None,
            },
            DenseTimeslot {
                stable_id: TimeslotId::from_uuid(id(12)),
                day: Day::Tuesday,
                period_index: 1,
                instructional_block: 1,
                next_consecutive: None,
            },
        ],
        activities,
        meeting_patterns: vec![MeetingPatternRule {
            course_offering_id: offering,
            course_plan_id: plan,
            activities: vec![ActivityIndex(0), ActivityIndex(1)],
            minimum_gap_days: 0,
            maximum_periods_per_day: 2,
        }],
        locks: vec![],
    }
    .try_into()
    .unwrap()
}

fn assignments(second_start: u32) -> Vec<Assignment> {
    vec![
        Assignment {
            activity: ActivityIndex(0),
            start: TimeslotIndex(0),
            room: RoomIndex(0),
            teacher: TeacherIndex(0),
        },
        Assignment {
            activity: ActivityIndex(1),
            start: TimeslotIndex(second_start),
            room: RoomIndex(1),
            teacher: TeacherIndex(0),
        },
    ]
}

#[test]
fn score_vectors_compare_tiers_lexicographically() {
    let left = ObjectiveVector {
        tiers: vec![
            TierScore {
                id: "first".into(),
                priority: 1,
                value: 1,
                metrics: vec![],
            },
            TierScore {
                id: "second".into(),
                priority: 2,
                value: 1_000_000,
                metrics: vec![],
            },
        ],
    };
    let right = ObjectiveVector {
        tiers: vec![
            TierScore {
                id: "first".into(),
                priority: 1,
                value: 2,
                metrics: vec![],
            },
            TierScore {
                id: "second".into(),
                priority: 2,
                value: 0,
                metrics: vec![],
            },
        ],
    };
    assert_eq!(left.lexicographic_cmp(&right), Ordering::Less);
}

#[test]
fn balanced_score_penalizes_same_day_course_concentration() {
    let problem = fixture();
    let context = ScoringContext::neutral(&problem);
    let spread = evaluate(
        &problem,
        &assignments(2),
        &ObjectivePlan::balanced_default(),
        &context,
    )
    .unwrap();
    let concentrated = evaluate(
        &problem,
        &assignments(1),
        &ObjectivePlan::balanced_default(),
        &context,
    )
    .unwrap();
    assert_eq!(spread.lexicographic_cmp(&concentrated), Ordering::Less);
}

#[test]
fn repair_changes_are_a_separate_first_tier() {
    let problem = fixture();
    let baseline = assignments(2);
    let mut context = ScoringContext::neutral(&problem);
    context.baseline = baseline
        .iter()
        .map(|assignment| (assignment.activity, *assignment))
        .collect();

    let unchanged = evaluate(
        &problem,
        &baseline,
        &ObjectivePlan::repair_default(),
        &context,
    )
    .unwrap();
    let changed = evaluate(
        &problem,
        &assignments(1),
        &ObjectivePlan::repair_default(),
        &context,
    )
    .unwrap();
    assert_eq!(
        unchanged.tiers[0].metrics[0].kind,
        MetricKind::RepairChanges
    );
    assert_eq!(unchanged.tiers[0].value, 0);
    assert_eq!(changed.tiers[0].value, 1);
    assert_eq!(unchanged.lexicographic_cmp(&changed), Ordering::Less);
}

#[test]
fn invalid_assignments_never_receive_a_quality_score() {
    let problem = fixture();
    let error = evaluate(
        &problem,
        &assignments(0),
        &ObjectivePlan::balanced_default(),
        &ScoringContext::neutral(&problem),
    )
    .unwrap_err();
    assert!(matches!(error, ScoringError::HardInvalid(_)));
}
