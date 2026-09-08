use std::collections::BTreeSet;

use class_schedule_domain::{
    AdministrativeClassId, BuildingId, CourseOfferingId, CoursePlanId, Day, MeetingDemandId,
    RoomId, StudentId, SubjectId, TeacherId, TimeslotId,
};
use class_schedule_scheduling::{
    Activity, ActivityIndex, Assignment, DenseBitSet, DenseRoom, DenseTeacher, DenseTimeslot,
    RoomIndex, RoomRequirement, SchedulingProblemDraft, SchedulingProblemSnapshot, TeacherIndex,
    TeacherRequirement, TimeslotIndex,
};
use class_schedule_validation::{HardProblemCode, static_feasibility_check, validate_assignments};
use uuid::Uuid;

fn uuid(value: u128) -> Uuid {
    Uuid::from_u128(value)
}

#[allow(clippy::too_many_lines)]
fn fixture() -> SchedulingProblemSnapshot {
    let students = vec![StudentId::from_uuid(uuid(1)), StudentId::from_uuid(uuid(2))];
    let building = BuildingId::from_uuid(uuid(3));
    let timeslots = vec![
        DenseTimeslot {
            stable_id: TimeslotId::from_uuid(uuid(10)),
            day: Day::Monday,
            period_index: 1,
            instructional_block: 1,
            next_consecutive: Some(TimeslotIndex(1)),
        },
        DenseTimeslot {
            stable_id: TimeslotId::from_uuid(uuid(11)),
            day: Day::Monday,
            period_index: 2,
            instructional_block: 1,
            next_consecutive: None,
        },
        DenseTimeslot {
            stable_id: TimeslotId::from_uuid(uuid(12)),
            day: Day::Tuesday,
            period_index: 1,
            instructional_block: 1,
            next_consecutive: None,
        },
    ];
    let teachers = vec![
        DenseTeacher {
            stable_id: TeacherId::from_uuid(uuid(20)),
            available: DenseBitSet::full(3),
        },
        DenseTeacher {
            stable_id: TeacherId::from_uuid(uuid(21)),
            available: DenseBitSet::full(3),
        },
    ];
    let rooms = vec![
        DenseRoom {
            stable_id: RoomId::from_uuid(uuid(30)),
            building_id: building,
            capacity: 2,
            features: BTreeSet::new(),
            available: DenseBitSet::full(3),
        },
        DenseRoom {
            stable_id: RoomId::from_uuid(uuid(31)),
            building_id: building,
            capacity: 2,
            features: BTreeSet::new(),
            available: DenseBitSet::full(3),
        },
    ];
    let subject = SubjectId::from_uuid(uuid(40));
    let plan = CoursePlanId::from_uuid(uuid(41));
    let admin_class = AdministrativeClassId::from_uuid(uuid(42));
    let activities = vec![
        Activity {
            stable_id: MeetingDemandId::from_uuid(uuid(50)),
            course_offering_id: CourseOfferingId::from_uuid(uuid(60)),
            subject_id: subject,
            course_plan_id: plan,
            teaching_section_id: None,
            administrative_class_id: Some(admin_class),
            duration_periods: 1,
            may_cross_breaks: false,
            allowed_starts: vec![TimeslotIndex(0), TimeslotIndex(2)],
            audience: DenseBitSet::from_indices(2, [0, 1]).unwrap(),
            teacher: TeacherRequirement::Fixed {
                teacher: TeacherIndex(0),
            },
            room: RoomRequirement::AdminHomeRoom { room: RoomIndex(0) },
            required_capacity: 2,
            required_room_features: BTreeSet::new(),
        },
        Activity {
            stable_id: MeetingDemandId::from_uuid(uuid(51)),
            course_offering_id: CourseOfferingId::from_uuid(uuid(61)),
            subject_id: subject,
            course_plan_id: plan,
            teaching_section_id: None,
            administrative_class_id: Some(admin_class),
            duration_periods: 1,
            may_cross_breaks: false,
            allowed_starts: vec![TimeslotIndex(0), TimeslotIndex(2)],
            audience: DenseBitSet::from_indices(2, [1]).unwrap(),
            teacher: TeacherRequirement::Fixed {
                teacher: TeacherIndex(1),
            },
            room: RoomRequirement::Fixed { room: RoomIndex(1) },
            required_capacity: 1,
            required_room_features: BTreeSet::new(),
        },
    ];
    SchedulingProblemDraft {
        schema_version: 1,
        students,
        teachers,
        rooms,
        timeslots,
        activities,
        meeting_patterns: vec![],
        locks: vec![],
    }
    .try_into()
    .unwrap()
}

#[test]
fn accepts_a_valid_assignment_set() {
    let problem = fixture();
    let assignments = [
        Assignment {
            activity: ActivityIndex(0),
            start: TimeslotIndex(0),
            room: RoomIndex(0),
            teacher: TeacherIndex(0),
        },
        Assignment {
            activity: ActivityIndex(1),
            start: TimeslotIndex(2),
            room: RoomIndex(1),
            teacher: TeacherIndex(1),
        },
    ];
    assert!(validate_assignments(&problem, &assignments).is_valid());
    assert!(static_feasibility_check(&problem).is_valid());
}

#[test]
fn catches_student_conflict_even_with_different_classes_and_resources() {
    let problem = fixture();
    let assignments = [
        Assignment {
            activity: ActivityIndex(0),
            start: TimeslotIndex(0),
            room: RoomIndex(0),
            teacher: TeacherIndex(0),
        },
        Assignment {
            activity: ActivityIndex(1),
            start: TimeslotIndex(0),
            room: RoomIndex(1),
            teacher: TeacherIndex(1),
        },
    ];
    let report = validate_assignments(&problem, &assignments);
    assert!(report.contains(HardProblemCode::StudentConflict));
}

#[test]
fn distinguishes_missing_duplicate_and_illegal_start() {
    let problem = fixture();
    let assignment = Assignment {
        activity: ActivityIndex(0),
        start: TimeslotIndex(1),
        room: RoomIndex(0),
        teacher: TeacherIndex(0),
    };
    let report = validate_assignments(&problem, &[assignment, assignment]);
    assert!(report.contains(HardProblemCode::AssignmentDuplicate));
    assert!(report.contains(HardProblemCode::AssignmentMissing));
    assert!(report.contains(HardProblemCode::StartNotAllowed));
}

#[test]
fn precheck_reports_empty_legal_start_domain() {
    let original = fixture();
    let mut activities = original.activities().to_vec();
    activities[0].allowed_starts.clear();
    let problem: SchedulingProblemSnapshot = SchedulingProblemDraft {
        schema_version: 1,
        students: original.students().to_vec(),
        teachers: original.teachers().to_vec(),
        rooms: original.rooms().to_vec(),
        timeslots: original.timeslots().to_vec(),
        activities,
        meeting_patterns: original.meeting_patterns().to_vec(),
        locks: original.locks().to_vec(),
    }
    .try_into()
    .unwrap();
    assert!(
        static_feasibility_check(&problem).contains(HardProblemCode::MeetingDemandNoLegalStart)
    );
}

#[test]
fn candidate_teacher_is_fixed_for_every_meeting_of_an_offering() {
    let original = fixture();
    let mut activities = original.activities().to_vec();
    activities[1].course_offering_id = activities[0].course_offering_id;
    activities[0].teacher = TeacherRequirement::Candidates {
        teachers: vec![TeacherIndex(0), TeacherIndex(1)],
    };
    activities[1].teacher = activities[0].teacher.clone();
    let problem: SchedulingProblemSnapshot = SchedulingProblemDraft {
        schema_version: 1,
        students: original.students().to_vec(),
        teachers: original.teachers().to_vec(),
        rooms: original.rooms().to_vec(),
        timeslots: original.timeslots().to_vec(),
        activities,
        meeting_patterns: vec![],
        locks: vec![],
    }
    .try_into()
    .unwrap();
    let assignments = [
        Assignment {
            activity: ActivityIndex(0),
            start: TimeslotIndex(0),
            room: RoomIndex(0),
            teacher: TeacherIndex(0),
        },
        Assignment {
            activity: ActivityIndex(1),
            start: TimeslotIndex(2),
            room: RoomIndex(1),
            teacher: TeacherIndex(1),
        },
    ];
    assert!(
        validate_assignments(&problem, &assignments)
            .contains(HardProblemCode::OfferingTeacherMismatch)
    );
}
