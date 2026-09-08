use std::thread;
use std::time::Duration;

use solver_client::{
    CancellationToken, SidecarSpec, SolverClient, SolverClientError, SolverRunStatus,
    WorkerProtocolError,
};
use solver_contract::{
    Activity, AdminHomeRoom, CalendarTimeslot, EngineVersion, FixedTeacher, MeetingPattern, Room,
    RoomPolicy, SNAPSHOT_SCHEMA_VERSION, SchedulingProblemSnapshot, SectionRoomBinding, SolveMode,
    SolveRequest, SolverEnvelope, SolverParameters, SolverProfile, Teacher, room_policy,
    solver_envelope, teacher_assignment_policy,
};

fn client(mode: &str) -> SolverClient {
    SolverClient::new(SidecarSpec::new(env!("CARGO_BIN_EXE_solver-client-test-worker")).arg(mode))
}

fn request() -> SolverEnvelope {
    SolverEnvelope {
        protocol_version: solver_contract::PROTOCOL_VERSION,
        request_id: "client-test-request".into(),
        payload: Some(solver_envelope::Payload::SolveRequest(SolveRequest {
            required_engine_version: Some(EngineVersion {
                engine_name: "test-engine".into(),
                engine_version: "1.0.0".into(),
                adapter_version: "1.0.0".into(),
                build_revision: "fixture".into(),
            }),
            parameters: Some(SolverParameters {
                seed: 91,
                mode: SolveMode::Generate as i32,
                profile: SolverProfile::Balanced as i32,
                reproducible: true,
                time_limit_millis: 5_000,
                worker_count: 1,
                collect_diagnostics: true,
                memory_limit_bytes: 0,
                relative_gap_limit_ppm: 0,
            }),
            problem: Some(snapshot()),
        })),
    }
}

fn snapshot() -> SchedulingProblemSnapshot {
    SchedulingProblemSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        snapshot_hash: vec![7; 32],
        project_id: "project".into(),
        project_revision: 1,
        scenario_id: "base".into(),
        scenario_revision: 1,
        week_patterns: vec![],
        timeslots: vec![CalendarTimeslot {
            timeslot_id: 1,
            week_pattern_id: 1,
            day_index: 1,
            period_index: 1,
            next_consecutive_timeslot_id: None,
        }],
        rooms: vec![Room {
            room_id: 1,
            capacity: 50,
            feature_ids: vec![],
            building_id: 1,
            available_timeslot_ids: vec![1],
        }],
        activities: vec![Activity {
            activity_id: 1,
            meeting_demand_id: 1,
            subject_id: 1,
            section_id: Some(1),
            administrative_class_id: None,
            duration_periods: 1,
            allowed_start_timeslot_ids: vec![1],
            teacher_policy: Some(solver_contract::TeacherAssignmentPolicy {
                policy: Some(teacher_assignment_policy::Policy::FixedTeacher(
                    FixedTeacher { teacher_id: 1 },
                )),
            }),
            section_room_binding_id: 1,
            meeting_pattern_id: 1,
            constraint_group_ids: vec![],
            teacher_binding_id: 1,
        }],
        student_conflicts: None,
        resource_conflicts: vec![],
        section_room_bindings: vec![SectionRoomBinding {
            binding_id: 1,
            section_id: Some(1),
            policy: Some(RoomPolicy {
                policy: Some(room_policy::Policy::AdminHomeRoom(AdminHomeRoom {
                    room_id: 1,
                })),
            }),
            required_capacity: 1,
            required_feature_ids: vec![],
        }],
        meeting_patterns: vec![MeetingPattern {
            meeting_pattern_id: 1,
            activity_ids: vec![1],
            duration_periods: vec![1],
            minimum_gap_days: 0,
            maximum_periods_per_day: 1,
            forbid_cross_break: true,
        }],
        locks: vec![],
        incumbent_assignments: vec![],
        constraint_groups: vec![],
        objective_tiers: vec![],
        teachers: vec![Teacher {
            teacher_id: 1,
            available_timeslot_ids: vec![1],
        }],
    }
}

#[test]
fn successful_worker_returns_strictly_mapped_status() {
    let outcome = client("success")
        .solve(&request(), &CancellationToken::new())
        .unwrap();

    assert_eq!(outcome.status, SolverRunStatus::Feasible);
    assert_eq!(outcome.response.unwrap().assignments.len(), 1);
    assert!(outcome.process.exit_success);
}

#[test]
fn managed_worker_can_clear_inherited_environment_while_preserving_explicit_values() {
    let spec = SidecarSpec::new(env!("CARGO_BIN_EXE_solver-client-test-worker"))
        .arg("environment-cleared")
        .clear_environment()
        .env("WORKER_EXPLICIT_TEST_VALUE", "explicit");
    let outcome = SolverClient::new(spec)
        .solve(&request(), &CancellationToken::new())
        .unwrap();
    assert_eq!(outcome.status, SolverRunStatus::Feasible);
    assert!(outcome.process.exit_success);
}

#[test]
fn otherwise_valid_assignments_with_wrong_response_identity_are_protocol_errors() {
    for (mode, code) in [
        ("wrong-request-id", "SOLVER_PROTOCOL_REQUEST_ID_MISMATCH"),
        (
            "wrong-snapshot-hash",
            "SOLVER_PROTOCOL_SNAPSHOT_HASH_MISMATCH",
        ),
        (
            "wrong-engine-name",
            "SOLVER_PROTOCOL_ENGINE_VERSION_MISMATCH",
        ),
        (
            "wrong-engine-version",
            "SOLVER_PROTOCOL_ENGINE_VERSION_MISMATCH",
        ),
        (
            "wrong-adapter-version",
            "SOLVER_PROTOCOL_ENGINE_VERSION_MISMATCH",
        ),
    ] {
        let error = client(mode)
            .solve(&request(), &CancellationToken::new())
            .unwrap_err();
        let SolverClientError::Protocol { source, process } = error else {
            panic!("{mode} must remain a protocol failure: {error:?}");
        };
        assert_eq!(source.code(), code, "{mode}");
        assert!(process.exit_success, "fixture completed normally: {mode}");
    }
}

#[test]
fn engine_build_revision_is_preserved_as_provenance_not_a_version_requirement() {
    let outcome = client("different-build-revision")
        .solve(&request(), &CancellationToken::new())
        .unwrap();
    assert_eq!(outcome.status, SolverRunStatus::Feasible);
    assert_eq!(
        outcome
            .response
            .unwrap()
            .engine_version
            .unwrap()
            .build_revision,
        "another-build"
    );
}

#[test]
fn wall_clock_timeout_terminates_worker_and_returns_timeout() {
    let outcome = client("timeout")
        .timeout(Duration::from_millis(80))
        .solve(&request(), &CancellationToken::new())
        .unwrap();

    assert_eq!(outcome.status, SolverRunStatus::Timeout);
    assert!(outcome.response.is_none());
    assert!(outcome.process.elapsed < Duration::from_secs(3));
}

#[test]
fn explicit_cancellation_terminates_worker_and_returns_cancelled() {
    let cancellation = CancellationToken::new();
    let trigger = cancellation.clone();
    let cancellation_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(60));
        trigger.cancel();
    });

    let outcome = client("cancel")
        .timeout(Duration::from_secs(5))
        .solve(&request(), &cancellation)
        .unwrap();
    cancellation_thread.join().unwrap();

    assert_eq!(outcome.status, SolverRunStatus::Cancelled);
    assert!(outcome.response.is_none());
    assert!(outcome.process.elapsed < Duration::from_secs(3));
}

#[test]
fn malformed_stdout_is_a_protocol_error() {
    let error = client("malformed")
        .solve(&request(), &CancellationToken::new())
        .unwrap_err();
    assert!(matches!(
        error,
        SolverClientError::Protocol {
            source: WorkerProtocolError::Frame(_),
            ..
        }
    ));
}

#[test]
fn nonzero_exit_is_not_misreported_as_a_protocol_error() {
    let error = client("nonzero")
        .solve(&request(), &CancellationToken::new())
        .unwrap_err();
    assert!(matches!(
        error,
        SolverClientError::WorkerExited {
            exit_code: Some(23),
            ..
        }
    ));
}

#[test]
fn trailing_stdout_after_valid_frame_is_rejected() {
    let error = client("trailing")
        .solve(&request(), &CancellationToken::new())
        .unwrap_err();
    assert!(matches!(
        error,
        SolverClientError::Protocol {
            source: WorkerProtocolError::TrailingStdout,
            ..
        }
    ));
}

#[test]
fn worker_reported_internal_error_is_distinct() {
    let error = client("internal")
        .solve(&request(), &CancellationToken::new())
        .unwrap_err();
    assert!(matches!(
        error,
        SolverClientError::WorkerInternalError { .. }
    ));
}

#[test]
fn stderr_is_drained_but_capture_is_bounded() {
    let outcome = client("stderr")
        .maximum_stderr_bytes(32)
        .solve(&request(), &CancellationToken::new())
        .unwrap();

    assert_eq!(outcome.process.stderr.captured_bytes, 32);
    assert_eq!(outcome.process.stderr.total_bytes, 8 * 1024);
    assert!(outcome.process.stderr.truncated);
}
