use solver_contract::{
    Activity, AdminHomeRoom, CalendarTimeslot, ContractError, EngineVersion, FixedTeacher,
    MeetingAssignment, MeetingPattern, PROTOCOL_VERSION, Room, RoomPolicy, SNAPSHOT_SCHEMA_VERSION,
    SchedulingProblemSnapshot, SectionRoomBinding, SolveMode, SolveRequest, SolverEnvelope,
    SolverParameters, SolverProfile, SolverStatistics, SolverStatus, Teacher, ValidateContract,
    framing::{
        DEFAULT_MAX_FRAME_LEN, FrameError, decode_frame, encode_frame, read_frame, write_frame,
    },
    room_policy, solver_envelope, teacher_assignment_policy,
};

fn engine_version() -> EngineVersion {
    EngineVersion {
        engine_name: "or-tools-cp-sat".into(),
        engine_version: "locked-test-version".into(),
        adapter_version: "scheduler-adapter-v1".into(),
        build_revision: "test-build".into(),
    }
}

fn parameters() -> SolverParameters {
    SolverParameters {
        seed: 42,
        mode: SolveMode::Generate as i32,
        profile: SolverProfile::Balanced as i32,
        reproducible: true,
        time_limit_millis: 30_000,
        worker_count: 1,
        collect_diagnostics: true,
        memory_limit_bytes: 512 * 1024 * 1024,
        relative_gap_limit_ppm: 0,
    }
}

fn snapshot() -> SchedulingProblemSnapshot {
    SchedulingProblemSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        snapshot_hash: vec![0x5a; 32],
        project_id: "project-01".into(),
        project_revision: 7,
        scenario_id: "scenario-a".into(),
        scenario_revision: 3,
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
            capacity: 45,
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

fn request_envelope() -> SolverEnvelope {
    SolverEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-123".into(),
        payload: Some(solver_envelope::Payload::SolveRequest(SolveRequest {
            required_engine_version: Some(engine_version()),
            parameters: Some(parameters()),
            problem: Some(snapshot()),
        })),
    }
}

#[test]
fn request_round_trips_through_binary_frame() {
    let expected = request_envelope();
    expected.validate_contract().unwrap();

    let encoded = encode_frame(&expected, DEFAULT_MAX_FRAME_LEN).unwrap();
    let decoded: SolverEnvelope = decode_frame(&encoded, DEFAULT_MAX_FRAME_LEN).unwrap();

    assert_eq!(decoded, expected);
    decoded.validate_contract().unwrap();
}

#[test]
fn stream_codec_round_trips_without_transport_noise() {
    let expected = request_envelope();
    let mut wire = Vec::new();
    write_frame(&mut wire, &expected, DEFAULT_MAX_FRAME_LEN).unwrap();

    let decoded: SolverEnvelope = read_frame(&mut wire.as_slice(), DEFAULT_MAX_FRAME_LEN).unwrap();
    assert_eq!(decoded, expected);
}

#[test]
fn unknown_enum_survives_decode_and_is_rejected_by_contract_validation() {
    let mut envelope = request_envelope();
    let Some(solver_envelope::Payload::SolveRequest(request)) = envelope.payload.as_mut() else {
        panic!("test fixture is a request");
    };
    request.parameters.as_mut().unwrap().mode = 77_777;

    let encoded = encode_frame(&envelope, DEFAULT_MAX_FRAME_LEN).unwrap();
    let decoded: SolverEnvelope = decode_frame(&encoded, DEFAULT_MAX_FRAME_LEN).unwrap();
    assert_eq!(
        decoded.validate_contract(),
        Err(ContractError::UnknownEnum {
            field: "parameters.mode",
            value: 77_777,
        })
    );
}

#[test]
fn unsupported_protocol_version_is_rejected_before_payload_use() {
    let mut envelope = request_envelope();
    envelope.protocol_version = PROTOCOL_VERSION + 1;
    assert_eq!(
        envelope.validate_contract(),
        Err(ContractError::UnsupportedProtocolVersion {
            expected: PROTOCOL_VERSION,
            actual: PROTOCOL_VERSION + 1,
        })
    );
}

#[test]
fn truncated_header_and_payload_are_distinct_errors() {
    let encoded = encode_frame(&request_envelope(), DEFAULT_MAX_FRAME_LEN).unwrap();
    assert!(matches!(
        decode_frame::<SolverEnvelope>(&encoded[..3], DEFAULT_MAX_FRAME_LEN),
        Err(FrameError::TruncatedHeader { actual: 3 })
    ));
    assert!(matches!(
        decode_frame::<SolverEnvelope>(&encoded[..encoded.len() - 1], DEFAULT_MAX_FRAME_LEN),
        Err(FrameError::TruncatedPayload { .. })
    ));
}

#[test]
fn oversized_frames_are_rejected_before_allocation_or_decode() {
    let declared = 1_025_u32;
    let header_only = declared.to_be_bytes();
    assert!(matches!(
        decode_frame::<SolverEnvelope>(&header_only, 1_024),
        Err(FrameError::FrameTooLarge {
            declared: 1_025,
            maximum: 1_024,
        })
    ));

    assert!(matches!(
        encode_frame(&request_envelope(), 1),
        Err(FrameError::FrameTooLarge { maximum: 1, .. })
    ));
}

#[test]
fn solver_status_discriminants_remain_distinct_and_stable() {
    assert_eq!(SolverStatus::Optimal as i32, 1);
    assert_eq!(SolverStatus::Feasible as i32, 2);
    assert_eq!(SolverStatus::ProvenInfeasible as i32, 3);
    assert_eq!(SolverStatus::Timeout as i32, 4);
    assert_eq!(SolverStatus::Unknown as i32, 5);
    assert_eq!(SolverStatus::Cancelled as i32, 6);
    assert_eq!(SolverStatus::InvalidInput as i32, 7);
    assert_eq!(SolverStatus::InvalidModel as i32, 8);
    assert_eq!(SolverStatus::InternalError as i32, 9);
}

#[test]
fn non_success_status_cannot_smuggle_a_timetable() {
    let response = solver_contract::SolveResponse {
        engine_version: Some(engine_version()),
        status: SolverStatus::Timeout as i32,
        status_detail_code: "SOLVER.TIME_LIMIT_REACHED".into(),
        objective: None,
        statistics: Some(SolverStatistics {
            wall_time_millis: 30_000,
            deterministic_time: 1.0,
            conflicts: 0,
            branches: 0,
            propagations: 0,
            peak_memory_bytes: 0,
            worker_count: 1,
            seed: 42,
        }),
        assignments: vec![MeetingAssignment {
            activity_id: 1,
            start_timeslot_id: 1,
            room_id: 1,
            teacher_id: 1,
            duration_periods: 1,
        }],
        section_room_assignments: vec![],
        diagnostic_groups: vec![],
        output_hash: vec![],
        effective_parameters: Some(parameters()),
        input_snapshot_hash: vec![0x5a; 32],
    };

    assert_eq!(
        response.validate_contract(),
        Err(ContractError::AssignmentsForNonSuccessStatus)
    );
}

#[test]
fn descriptor_set_is_generated_for_cross_language_compatibility_checks() {
    assert!(!solver_contract::FILE_DESCRIPTOR_SET.is_empty());
}
