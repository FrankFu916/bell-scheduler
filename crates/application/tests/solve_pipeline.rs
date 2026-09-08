use std::collections::BTreeSet;
use std::time::Duration;

use class_schedule_application::{
    SolveApplicationError, SolveContext, SolveExecution, SolveOptions, execute_solve, prepare_solve,
};
use class_schedule_domain::{
    AdministrativeClassId, BuildingId, CourseOfferingId, CoursePlanId, Day, MeetingDemandId,
    RoomId, StudentId, SubjectId, TeacherId, TimeslotId,
};
use class_schedule_scheduling::{
    Activity, ActivityIndex, Assignment, DenseBitSet, DenseRoom, DenseTeacher, DenseTimeslot,
    RoomIndex, RoomRequirement, SchedulingProblemDraft, SchedulingProblemSnapshot, TeacherIndex,
    TeacherRequirement, TimeslotIndex,
};
use solver_client::{CancellationToken, SidecarSpec, SolverClient, SolverRunStatus};
use solver_contract::{SolveMode, ValidateContract, solver_envelope};
use uuid::Uuid;

fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}

#[allow(clippy::too_many_lines)]
fn problem() -> SchedulingProblemSnapshot {
    let students = vec![StudentId::from_uuid(id(1))];
    let building = BuildingId::from_uuid(id(2));
    let timeslots = vec![
        DenseTimeslot {
            stable_id: TimeslotId::from_uuid(id(10)),
            day: Day::Monday,
            period_index: 1,
            instructional_block: 1,
            next_consecutive: None,
        },
        DenseTimeslot {
            stable_id: TimeslotId::from_uuid(id(11)),
            day: Day::Monday,
            period_index: 2,
            instructional_block: 1,
            next_consecutive: None,
        },
    ];
    let teachers = vec![
        DenseTeacher {
            stable_id: TeacherId::from_uuid(id(20)),
            available: DenseBitSet::full(2),
        },
        DenseTeacher {
            stable_id: TeacherId::from_uuid(id(21)),
            available: DenseBitSet::from_indices(2, [1]).unwrap(),
        },
    ];
    let rooms = vec![
        DenseRoom {
            stable_id: RoomId::from_uuid(id(30)),
            building_id: building,
            capacity: 1,
            features: BTreeSet::new(),
            available: DenseBitSet::full(2),
        },
        DenseRoom {
            stable_id: RoomId::from_uuid(id(31)),
            building_id: building,
            capacity: 1,
            features: BTreeSet::new(),
            available: DenseBitSet::from_indices(2, [1]).unwrap(),
        },
    ];
    let subject = SubjectId::from_uuid(id(40));
    let plan = CoursePlanId::from_uuid(id(41));
    let class = AdministrativeClassId::from_uuid(id(42));
    let audience = DenseBitSet::from_indices(1, [0]).unwrap();
    let activities = vec![
        Activity {
            stable_id: MeetingDemandId::from_uuid(id(50)),
            course_offering_id: CourseOfferingId::from_uuid(id(60)),
            subject_id: subject,
            course_plan_id: plan,
            teaching_section_id: None,
            administrative_class_id: Some(class),
            duration_periods: 1,
            may_cross_breaks: false,
            allowed_starts: vec![TimeslotIndex(0), TimeslotIndex(1)],
            audience: audience.clone(),
            teacher: TeacherRequirement::Fixed {
                teacher: TeacherIndex(0),
            },
            room: RoomRequirement::Fixed { room: RoomIndex(0) },
            required_capacity: 1,
            required_room_features: BTreeSet::new(),
        },
        Activity {
            stable_id: MeetingDemandId::from_uuid(id(51)),
            course_offering_id: CourseOfferingId::from_uuid(id(61)),
            subject_id: subject,
            course_plan_id: plan,
            teaching_section_id: None,
            administrative_class_id: Some(class),
            duration_periods: 1,
            may_cross_breaks: false,
            allowed_starts: vec![TimeslotIndex(0), TimeslotIndex(1)],
            audience,
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
        meeting_patterns: Vec::new(),
        locks: Vec::new(),
    }
    .try_into()
    .unwrap()
}

fn context() -> SolveContext {
    SolveContext {
        project_id: "project-test".to_owned(),
        project_revision: 3,
        scenario_id: "scenario-a".to_owned(),
        scenario_revision: 2,
        request_id: "request-application-test".to_owned(),
    }
}

fn options() -> SolveOptions {
    SolveOptions::reproducible(42, Duration::from_secs(3))
}

fn incumbent() -> Vec<Assignment> {
    vec![
        Assignment {
            activity: ActivityIndex(0),
            start: TimeslotIndex(0),
            room: RoomIndex(0),
            teacher: TeacherIndex(0),
        },
        Assignment {
            activity: ActivityIndex(1),
            start: TimeslotIndex(1),
            room: RoomIndex(1),
            teacher: TeacherIndex(1),
        },
    ]
}

fn client(mode: &str) -> SolverClient {
    SolverClient::new(SidecarSpec::new(env!("CARGO_BIN_EXE_application-test-worker")).arg(mode))
}

#[test]
fn adapter_carries_resource_availability_and_valid_contract() {
    let prepared = prepare_solve(&problem(), &context(), &options()).unwrap();
    prepared.envelope.validate_contract().unwrap();
    let Some(solver_envelope::Payload::SolveRequest(request)) = prepared.envelope.payload else {
        panic!("expected request payload");
    };
    let protocol_problem = request.problem.unwrap();
    assert_eq!(protocol_problem.teachers[1].available_timeslot_ids, vec![2]);
    assert_eq!(protocol_problem.rooms[1].available_timeslot_ids, vec![2]);
    assert_eq!(protocol_problem.snapshot_hash, prepared.snapshot_hash);
    assert_eq!(protocol_problem.meeting_patterns.len(), 2);
    let conflicts = protocol_problem.student_conflicts.as_ref().unwrap();
    assert!(conflicts.edges.is_empty());
    assert_eq!(conflicts.cliques.len(), 1);
    assert_eq!(conflicts.cliques[0].activity_ids, vec![1, 2]);
    assert_ne!(
        protocol_problem.activities[0].teacher_binding_id,
        protocol_problem.activities[1].teacher_binding_id
    );
}

#[test]
fn meetings_of_one_offering_share_teacher_and_pattern_bindings() {
    let original = problem();
    let mut activities = original.activities().to_vec();
    activities[1].course_offering_id = activities[0].course_offering_id;
    activities[1].teacher = activities[0].teacher.clone();
    let same_offering: SchedulingProblemSnapshot = SchedulingProblemDraft {
        schema_version: original.schema_version(),
        students: original.students().to_vec(),
        teachers: original.teachers().to_vec(),
        rooms: original.rooms().to_vec(),
        timeslots: original.timeslots().to_vec(),
        activities,
        meeting_patterns: Vec::new(),
        locks: Vec::new(),
    }
    .try_into()
    .unwrap();
    let prepared = prepare_solve(&same_offering, &context(), &options()).unwrap();
    let Some(solver_envelope::Payload::SolveRequest(request)) = prepared.envelope.payload else {
        panic!("expected request payload");
    };
    let protocol_problem = request.problem.unwrap();
    assert_eq!(protocol_problem.meeting_patterns.len(), 1);
    assert_eq!(
        protocol_problem.activities[0].teacher_binding_id,
        protocol_problem.activities[1].teacher_binding_id
    );
    assert_eq!(
        protocol_problem.activities[0].meeting_pattern_id,
        protocol_problem.activities[1].meeting_pattern_id
    );
}

#[test]
fn repair_serializes_a_complete_independently_valid_incumbent() {
    let mut repair = options();
    repair.mode = SolveMode::Repair;
    repair.incumbent_assignments = incumbent();
    let prepared = prepare_solve(&problem(), &context(), &repair).unwrap();
    let Some(solver_envelope::Payload::SolveRequest(request)) = prepared.envelope.payload else {
        panic!("expected request payload");
    };
    let protocol_problem = request.problem.unwrap();
    assert_eq!(protocol_problem.incumbent_assignments.len(), 2);
    assert_eq!(protocol_problem.incumbent_assignments[0].activity_id, 1);
    assert_eq!(
        protocol_problem.incumbent_assignments[1].start_timeslot_id,
        2
    );
}

#[test]
fn improve_and_repair_reject_missing_or_hard_invalid_incumbents() {
    let mut missing = options();
    missing.mode = SolveMode::Improve;
    let error = prepare_solve(&problem(), &context(), &missing).unwrap_err();
    assert_eq!(error.code(), "APPLICATION_INCUMBENT_REQUIRED_FOR_MODE");

    let mut invalid = options();
    invalid.mode = SolveMode::Repair;
    invalid.incumbent_assignments = incumbent();
    invalid.incumbent_assignments[1].start = TimeslotIndex(0);
    let error = prepare_solve(&problem(), &context(), &invalid).unwrap_err();
    assert_eq!(error.code(), "APPLICATION_INVALID_INCUMBENT");
}

#[test]
fn generate_rejects_an_incumbent_to_prevent_mode_state_confusion() {
    let mut generate = options();
    generate.incumbent_assignments = incumbent();
    let error = prepare_solve(&problem(), &context(), &generate).unwrap_err();
    assert_eq!(error.code(), "APPLICATION_INCUMBENT_NOT_ALLOWED_FOR_MODE");
}

#[test]
fn valid_worker_output_passes_independent_validation() {
    let result = execute_solve(
        &problem(),
        &context(),
        &options(),
        &client("valid"),
        &CancellationToken::new(),
    )
    .unwrap();
    let SolveExecution::Completed(completed) = result else {
        panic!("precheck unexpectedly failed");
    };
    assert_eq!(completed.status, SolverRunStatus::Feasible);
    assert!(completed.independent_validation.unwrap().is_valid());
    assert!(completed.output_hash.is_some());
}

#[test]
fn invalid_worker_output_is_never_returned_as_success() {
    let error = execute_solve(
        &problem(),
        &context(),
        &options(),
        &client("invalid-conflict"),
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        SolveApplicationError::InvalidSolverOutput { .. }
    ));
    assert_eq!(error.code(), "INTERNAL_ERROR_INVALID_SOLVER_OUTPUT");
}

#[test]
fn failed_precheck_does_not_launch_worker() {
    let original = problem();
    let mut activities = original.activities().to_vec();
    activities[0].allowed_starts.clear();
    let invalid: SchedulingProblemSnapshot = SchedulingProblemDraft {
        schema_version: original.schema_version(),
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
    let nonexistent_client = SolverClient::new(SidecarSpec::new("/definitely/not/a/worker"));
    let result = execute_solve(
        &invalid,
        &context(),
        &options(),
        &nonexistent_client,
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(matches!(result, SolveExecution::PrecheckFailed { .. }));
}
