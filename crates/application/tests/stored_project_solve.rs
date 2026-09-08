use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use class_schedule_application::{
    AutoSectioningPolicy, AutoSectioningSolveStatus, CalendarDefinition, CsvImportAuditOptions,
    CsvImportMode, ImportCommandError, ImportCommitCommand, ImportCommitIntent,
    ImportedProjectSolve, SectioningProfile, SolveExecution, SolveOptions,
    StoredProjectSolveCommand, StoredProjectSolveError, StoredProjectSolveMode, commit_csv_import,
    execute_stored_project_solve, load_imported_project, prepare_stored_project_solve,
};
use class_schedule_application::{
    PreparedSolveArtifact, execute_durable_stored_project_solve, list_project_solve_artifacts,
    load_solve_artifact, save_prepared_solve_artifact,
};
use class_schedule_domain::SchoolProjectId;
use class_schedule_import::{CsvSource, DatasetKind};
use class_schedule_persistence::{PersistenceError, ProjectDocument, SqliteStore};
use class_schedule_scheduling::{
    ActivityIndex, Assignment, RoomIndex, TeacherIndex, TimeslotIndex,
};
use prost::Message;
use rusqlite::Connection;
use solver_client::{CancellationToken, SidecarSpec, SolverClient, SolverRunStatus};
use solver_contract::SolveMode;

type CsvFiles = BTreeMap<DatasetKind, Vec<u8>>;

// Exact three-choice CSV input, one teacher and room, and single-period meetings. The existing
// framed test worker can return independently valid sequential assignments for this fixture.
fn files(unsectioned: bool) -> CsvFiles {
    let mut files = [
        (DatasetKind::Students, "student_code,name,administrative_class_code\nS1,Student 1,AC1\nS2,Student 2,AC1\n"),
        (DatasetKind::AdministrativeClasses, "administrative_class_code,name,grade_code,home_room_code\nAC1,Class 1,G12,R1\n"),
        (DatasetKind::StudentSubjectChoices, "student_code,subject_code\nS1,physics\nS1,chemistry\nS1,biology\nS2,physics\nS2,chemistry\nS2,biology\n"),
        (DatasetKind::Teachers, "teacher_code,name\nT1,Teacher 1\nT2,Teacher 2\n"),
        (DatasetKind::Rooms, "room_code,name,building_code,capacity,features\nR1,Room 1,B1,2,lab\nR2,Room 2,B1,2,lab\n"),
        (DatasetKind::CoursePlans, concat!(
            "course_plan_code,name,grade_code,subject_code,audience_kind,weekly_periods,meeting_pattern,min_days_between,max_periods_per_day,may_cross_breaks,required_room_features,teacher_assignment,teacher_codes,room_policy,room_candidates,preferred_rooms,fallback_rooms\n",
            "P1,Physics,G12,physics,teaching_section,1,1,0,1,false,lab,fixed,T1,section_fixed,R1,,\n",
            "P2,Chemistry,G12,chemistry,teaching_section,1,1,0,1,false,lab,fixed,T1,section_fixed,R1,,\n",
            "P3,Biology,G12,biology,teaching_section,1,1,0,1,false,lab,fixed,T1,section_fixed,R1,,\n",
        )),
    ].into_iter().map(|(kind, csv)| (kind, csv.as_bytes().to_vec())).collect::<CsvFiles>();
    if !unsectioned {
        files.insert(DatasetKind::TeachingSections, concat!(
            "section_code,name,grade_code,subject_code,min_size,target_size,max_size,room_policy,room_candidates,preferred_rooms,fallback_rooms,teacher_assignment,teacher_codes\n",
            "PHY,Physics,G12,physics,1,2,2,section_fixed,R1,,,fixed,T1\n",
            "CHEM,Chemistry,G12,chemistry,1,2,2,section_fixed,R1,,,fixed,T1\n",
            "BIO,Biology,G12,biology,1,2,2,section_fixed,R1,,,fixed,T1\n",
        ).as_bytes().to_vec());
        files.insert(
            DatasetKind::SectionEnrollments,
            concat!(
                "section_code,student_code\nPHY,S1\nPHY,S2\nCHEM,S1\nCHEM,S2\nBIO,S1\nBIO,S2\n",
            )
            .as_bytes()
            .to_vec(),
        );
    }
    files
}

fn sources(files: &CsvFiles) -> impl Iterator<Item = CsvSource<'_>> {
    files
        .iter()
        .map(|(&kind, bytes)| CsvSource::new(kind, bytes))
}

fn policy() -> AutoSectioningPolicy {
    AutoSectioningPolicy::new(1, 1, 2, 99, SectioningProfile::Balanced, 2).unwrap()
}

fn create(store: &mut SqliteStore, unsectioned: bool) -> StoredProjectSolveCommand {
    let project_id = SchoolProjectId::new_v4();
    let command = ImportCommitCommand {
        project_id,
        display_name: "Saved original project".to_owned(),
        intent: ImportCommitIntent::Create,
        options: CsvImportAuditOptions {
            project_stable_key: "saved-project-fixture".to_owned(),
            calendar: CalendarDefinition::weekday_with_break(4, 2).unwrap(),
            exact_subject_choices: 3,
            mode: if unsectioned {
                CsvImportMode::Unsectioned(policy())
            } else {
                CsvImportMode::ExistingSections
            },
        },
    };
    commit_csv_import(store, &command, sources(&files(unsectioned))).unwrap();
    StoredProjectSolveCommand {
        project_id,
        expected_revision: 0,
        mode: if unsectioned {
            StoredProjectSolveMode::AutoSectioning(policy())
        } else {
            StoredProjectSolveMode::ExistingSections
        },
    }
}

fn client(mode: &str) -> SolverClient {
    SolverClient::new(SidecarSpec::new(env!("CARGO_BIN_EXE_application-test-worker")).arg(mode))
}

fn options() -> SolveOptions {
    SolveOptions::reproducible(99, Duration::from_secs(1))
}

fn assert_no_run_writes(path: &Path, expected_revisions: u64) {
    let connection = Connection::open(path).unwrap();
    let counts: (u64, u64, u64) = connection.query_row(
        "SELECT (SELECT count(*) FROM projects), (SELECT count(*) FROM project_revisions), (SELECT count(*) FROM solver_runs)",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(counts, (1, expected_revisions, 0));
}

fn stored(store: &SqliteStore, command: &StoredProjectSolveCommand) -> ProjectDocument {
    store.load_project(&command.project_id.to_string()).unwrap()
}

fn durable_run(
    store: &SqliteStore,
    command: &StoredProjectSolveCommand,
    worker: &str,
) -> PreparedSolveArtifact {
    execute_durable_stored_project_solve(
        prepare_stored_project_solve(store, command).unwrap(),
        &options(),
        &client(worker),
        &CancellationToken::new(),
    )
    .unwrap()
}

fn mutate_artifact(path: &Path, run_id: &str, mutation: impl FnOnce(&mut serde_json::Value)) {
    let connection = Connection::open(path).unwrap();
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT payload FROM solve_artifacts WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    mutation(&mut payload);
    let bytes = serde_json::to_vec(&payload).unwrap();
    connection
        .execute(
            "UPDATE solve_artifacts SET payload = ?1, payload_hash = ?2 WHERE run_id = ?3",
            rusqlite::params![bytes, blake3::hash(&bytes).as_bytes(), run_id],
        )
        .unwrap();
}

#[test]
fn durable_input_a_survives_reopen_with_real_provenance_and_historical_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("durable.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let command = create(&mut store, false);
    let before = stored(&store, &command);
    let artifact = durable_run(&store, &command, "cancel-after-feasible");
    assert_eq!(artifact.status_code(), "Feasible");
    let receipt = save_prepared_solve_artifact(&mut store, &artifact).unwrap();
    assert_eq!(
        save_prepared_solve_artifact(&mut store, &artifact)
            .unwrap_err()
            .code(),
        "PERSISTENCE_SOLVE_RUN_ALREADY_EXISTS"
    );
    let mut replacement = before.clone();
    replacement.display_name = "Current name after original run".to_owned();
    let mut changed: serde_json::Value = serde_json::from_slice(&replacement.payload).unwrap();
    changed["import_batch"]["students"][0]["name"] = serde_json::json!("Changed in later revision");
    replacement.payload = serde_json::to_vec(&changed).unwrap();
    replacement.revision = 1;
    store.replace_project(0, &replacement).unwrap();
    drop(store);
    let reopened = SqliteStore::open(&path).unwrap();
    let loaded = load_solve_artifact(&reopened, &receipt.run_id).unwrap();
    assert_eq!(loaded.receipt, receipt);
    assert_eq!(loaded.receipt.source.revision, 0);
    assert_eq!(loaded.display_name, before.display_name);
    assert_eq!(loaded.options(), &options());
    assert!(loaded.failure.is_none());
    assert_eq!(loaded.attempts.len(), 1);
    assert_eq!(loaded.attempts[0].validation_code, "Passed");
    assert_eq!(loaded.attempts[0].worker_count, 1);
    assert_eq!(loaded.attempts[0].time_limit_ms, 1000);
    assert!(loaded.attempts[0].output_hash.is_some());
    assert_eq!(loaded.source_document().import_batch.students().len(), 2);
    assert_eq!(
        loaded.source_document().import_batch.students()[0].name,
        "Student 1"
    );
    assert_ne!(
        stored(&reopened, &command).payload_hash(),
        before.payload_hash()
    );
    let ImportedProjectSolve::Existing(result) = &loaded.result().unwrap().result else {
        panic!("Input A");
    };
    let SolveExecution::Completed(completed) = &result.execution else {
        panic!("completed");
    };
    assert_eq!(completed.assignments.len(), 3);
    assert!(
        completed
            .independent_validation
            .as_ref()
            .unwrap()
            .is_valid()
    );
    assert_eq!(stored(&reopened, &command).revision, 1);
    let connection = Connection::open(&path).unwrap();
    let counts: (u64, u64, u64) = connection.query_row("SELECT (SELECT count(*) FROM project_revisions), (SELECT count(*) FROM solver_runs), (SELECT count(*) FROM solve_artifacts)", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
    assert_eq!(counts, (2, 1, 1));
}

#[test]
fn durable_input_b_rebuilds_selected_sectioning_without_materializing_source() {
    let mut store = SqliteStore::in_memory().unwrap();
    let mut command = create(&mut store, true);
    let mut explicit = policy();
    explicit.candidate_count = 1;
    command.mode = StoredProjectSolveMode::AutoSectioning(explicit);
    let before = stored(&store, &command);
    let artifact = durable_run(&store, &command, "cancel-after-feasible");
    let receipt = save_prepared_solve_artifact(&mut store, &artifact).unwrap();
    let loaded = load_solve_artifact(&store, &receipt.run_id).unwrap();
    let ImportedProjectSolve::AutoSectioned(result) = &loaded.result().unwrap().result else {
        panic!("Input B");
    };
    assert_eq!(result.status, AutoSectioningSolveStatus::SelectedFeasible);
    assert_eq!(result.selected_attempt_index, Some(0));
    assert!(result.selected_attempt().unwrap().quality.is_some());
    assert_eq!(stored(&store, &command), before);
    assert!(loaded.source_document().generated_sectioning.is_none());
    assert!(
        loaded
            .source_document()
            .import_batch
            .teaching_sections()
            .is_empty()
    );
}

#[test]
fn durable_cancellation_retains_prior_feasible_attempt_without_selecting_it() {
    let mut store = SqliteStore::in_memory().unwrap();
    let command = create(&mut store, true);
    let artifact = durable_run(&store, &command, "cancel-after-feasible");
    let receipt = save_prepared_solve_artifact(&mut store, &artifact).unwrap();
    let loaded = load_solve_artifact(&store, &receipt.run_id).unwrap();
    assert_eq!(loaded.receipt.status_code, "Cancelled");
    assert_eq!(loaded.attempts.len(), 2);
    assert_eq!(loaded.attempts[0].status_code, "Feasible");
    assert_eq!(loaded.attempts[1].status_code, "Cancelled");
    let ImportedProjectSolve::AutoSectioned(result) = &loaded.result().unwrap().result else {
        panic!("Input B");
    };
    assert!(result.selected_attempt().is_none());
    assert!(result.attempts[0].quality.is_some());
}

#[test]
fn durable_timeout_unknown_and_infeasible_remain_distinct_on_reload() {
    let mut store = SqliteStore::in_memory().unwrap();
    let command = create(&mut store, false);
    for (worker, status) in [
        ("timeout", "Timeout"),
        ("unknown", "Unknown"),
        ("proven-infeasible", "ProvenInfeasible"),
    ] {
        let artifact = durable_run(&store, &command, worker);
        let receipt = save_prepared_solve_artifact(&mut store, &artifact).unwrap();
        let loaded = load_solve_artifact(&store, &receipt.run_id).unwrap();
        assert_eq!(loaded.receipt.status_code, status);
        assert!(loaded.failure.is_none());
        assert_eq!(loaded.attempts[0].validation_code, "NotApplicable");
    }
}

#[test]
fn durable_failures_keep_stable_codes_without_fabricated_response_or_provenance() {
    let mut store = SqliteStore::in_memory().unwrap();
    let command = create(&mut store, false);
    for (worker, code) in [
        ("worker-crash", "SOLVER_CLIENT_WORKER_EXITED"),
        (
            "protocol-wrong-request",
            "SOLVER_PROTOCOL_REQUEST_ID_MISMATCH",
        ),
        ("invalid-conflict", "INTERNAL_ERROR_INVALID_SOLVER_OUTPUT"),
    ] {
        let artifact = durable_run(&store, &command, worker);
        assert!(artifact.result().is_none());
        assert_eq!(artifact.status_code(), "InternalError");
        assert_eq!(artifact.failure().unwrap().code, code);
        let receipt = save_prepared_solve_artifact(&mut store, &artifact).unwrap();
        let loaded = load_solve_artifact(&store, &receipt.run_id).unwrap();
        assert_eq!(loaded.failure.unwrap().code, code);
        assert!(loaded.result.is_none());
        assert!(loaded.attempts.is_empty());
    }
}

#[test]
fn durable_failure_after_completed_candidate_keeps_its_actual_attempt() {
    let mut store = SqliteStore::in_memory().unwrap();
    let command = create(&mut store, true);
    let artifact = durable_run(&store, &command, "feasible-then-crash");
    let receipt = save_prepared_solve_artifact(&mut store, &artifact).unwrap();
    let loaded = load_solve_artifact(&store, &receipt.run_id).unwrap();
    assert!(loaded.result.is_none());
    assert_eq!(loaded.failure.unwrap().code, "SOLVER_CLIENT_WORKER_EXITED");
    assert_eq!(loaded.attempts.len(), 1);
    assert_eq!(loaded.attempts[0].status_code, "Feasible");
    assert_eq!(loaded.attempts[0].validation_code, "Passed");
}

#[test]
fn durable_pre_cancel_without_worker_has_no_invented_effective_parameters() {
    let mut store = SqliteStore::in_memory().unwrap();
    let command = create(&mut store, false);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let artifact = execute_durable_stored_project_solve(
        prepare_stored_project_solve(&store, &command).unwrap(),
        &options(),
        &SolverClient::new(SidecarSpec::new("nonexistent-private-worker")),
        &cancellation,
    )
    .unwrap();
    let receipt = save_prepared_solve_artifact(&mut store, &artifact).unwrap();
    let loaded = load_solve_artifact(&store, &receipt.run_id).unwrap();
    assert_eq!(loaded.receipt.status_code, "Cancelled");
    assert!(loaded.attempts.is_empty());
    assert!(loaded.failure.is_none());
}

#[test]
fn durable_rehashed_illegal_assignment_is_rejected_by_independent_validator() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tampered.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let command = create(&mut store, false);
    let artifact = durable_run(&store, &command, "cancel-after-feasible");
    let receipt = save_prepared_solve_artifact(&mut store, &artifact).unwrap();
    mutate_artifact(&path, &receipt.run_id, |payload| {
        let bytes: Vec<u8> =
            serde_json::from_value(payload["attempts"][0]["response"].clone()).unwrap();
        let mut response = solver_contract::SolveResponse::decode(bytes.as_slice()).unwrap();
        response.assignments[1].start_timeslot_id = response.assignments[0].start_timeslot_id;
        payload["attempts"][0]["response"] =
            serde_json::to_value(response.encode_to_vec()).unwrap();
    });
    assert_eq!(
        load_solve_artifact(&store, &receipt.run_id)
            .unwrap_err()
            .code(),
        "APPLICATION_SOLVE_ARTIFACT_INVALID_OUTPUT"
    );
}

#[test]
fn durable_rehashed_snapshot_request_and_semantic_version_tampering_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tampered.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let command = create(&mut store, false);
    for mutation in 0..3 {
        let artifact = durable_run(&store, &command, "cancel-after-feasible");
        let receipt = save_prepared_solve_artifact(&mut store, &artifact).unwrap();
        mutate_artifact(&path, &receipt.run_id, |payload| match mutation {
            0 => payload["attempts"][0]["snapshot_hash"][0] = serde_json::json!(255),
            1 => {
                let bytes: Vec<u8> =
                    serde_json::from_value(payload["attempts"][0]["request"].clone()).unwrap();
                let mut request =
                    solver_contract::SolverEnvelope::decode(bytes.as_slice()).unwrap();
                let Some(solver_contract::solver_envelope::Payload::SolveRequest(request_body)) =
                    &mut request.payload
                else {
                    panic!("request");
                };
                request_body.problem.as_mut().unwrap().activities[0].duration_periods = 2;
                payload["attempts"][0]["request"] =
                    serde_json::to_value(request.encode_to_vec()).unwrap();
            }
            _ => payload["semantics_version"] = serde_json::json!("unsupported.v2"),
        });
        let expected = [
            "APPLICATION_SOLVE_ARTIFACT_SNAPSHOT",
            "APPLICATION_SOLVE_ARTIFACT_REQUEST",
            "APPLICATION_SOLVE_ARTIFACT_UNSUPPORTED_VERSION",
        ];
        assert_eq!(
            load_solve_artifact(&store, &receipt.run_id)
                .unwrap_err()
                .code(),
            expected[mutation]
        );
    }
}

#[test]
fn durable_rehashed_cancellation_cannot_select_prior_feasible_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tampered.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let command = create(&mut store, true);
    let artifact = durable_run(&store, &command, "cancel-after-feasible");
    let receipt = save_prepared_solve_artifact(&mut store, &artifact).unwrap();
    mutate_artifact(&path, &receipt.run_id, |payload| {
        payload["terminal"]["selected_attempt_index"] = serde_json::json!(0);
    });
    assert_eq!(
        load_solve_artifact(&store, &receipt.run_id)
            .unwrap_err()
            .code(),
        "APPLICATION_SOLVE_ARTIFACT_SELECTION"
    );
}

#[test]
fn durable_metadata_pagination_is_bounded_and_payload_open_remains_separate() {
    let mut store = SqliteStore::in_memory().unwrap();
    let command = create(&mut store, false);
    for _ in 0..3 {
        let artifact = durable_run(&store, &command, "unknown");
        save_prepared_solve_artifact(&mut store, &artifact).unwrap();
    }
    let first = list_project_solve_artifacts(&store, command.project_id, 2, 0).unwrap();
    assert_eq!(first.runs.len(), 2);
    assert!(first.has_more);
    assert_eq!(first.next_offset, Some(2));
    let second = list_project_solve_artifacts(&store, command.project_id, 2, 2).unwrap();
    assert_eq!(second.runs.len(), 1);
    assert!(!second.has_more);
    assert_eq!(second.next_offset, None);
    assert!(list_project_solve_artifacts(&store, command.project_id, 0, 0).is_err());
    assert!(list_project_solve_artifacts(&store, command.project_id, 101, 0).is_err());
    assert!(list_project_solve_artifacts(&store, command.project_id, 2, u32::MAX).is_err());
}

#[test]
fn durable_invalid_execution_parameters_fail_before_worker_and_write() {
    let mut store = SqliteStore::in_memory().unwrap();
    let command = create(&mut store, false);
    for (mode, seed, workers, expected) in [
        (
            SolveMode::Improve,
            99,
            1,
            "APPLICATION_STORED_PROJECT_SOLVE_MODE_UNSUPPORTED",
        ),
        (
            SolveMode::Generate,
            u64::MAX,
            1,
            "APPLICATION_SOLVE_ARTIFACT_PARAMETER_RANGE",
        ),
        (
            SolveMode::Generate,
            99,
            2,
            "APPLICATION_REPRODUCIBLE_REQUIRES_ONE_WORKER",
        ),
    ] {
        let mut settings = options();
        settings.mode = mode;
        settings.seed = seed;
        settings.worker_count = workers;
        let error = execute_durable_stored_project_solve(
            prepare_stored_project_solve(&store, &command).unwrap(),
            &settings,
            &SolverClient::new(SidecarSpec::new("nonexistent-private-worker")),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(error.code(), expected);
    }
    let mut invalid_objective = options();
    invalid_objective.objective_tiers[0].tier_id.clear();
    let error = execute_durable_stored_project_solve(
        prepare_stored_project_solve(&store, &command).unwrap(),
        &invalid_objective,
        &SolverClient::new(SidecarSpec::new("nonexistent-private-worker")),
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert_eq!(error.code(), "APPLICATION_SOLVER_CLIENT");
    assert!(
        list_project_solve_artifacts(&store, command.project_id, 100, 0)
            .unwrap()
            .runs
            .is_empty()
    );
}

#[test]
fn durable_rehashed_response_parameters_output_hash_and_sql_provenance_are_checked() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tampered.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let command = create(&mut store, false);
    for mutation in 0..3 {
        let artifact = durable_run(&store, &command, "cancel-after-feasible");
        let receipt = save_prepared_solve_artifact(&mut store, &artifact).unwrap();
        if mutation == 2 {
            Connection::open(&path).unwrap().execute("UPDATE solver_runs SET validation_code = 'NotApplicable' WHERE request_id = ?1", [&receipt.run_id]).unwrap();
        } else {
            mutate_artifact(&path, &receipt.run_id, |payload| {
                if mutation == 0 {
                    let bytes: Vec<u8> =
                        serde_json::from_value(payload["attempts"][0]["response"].clone()).unwrap();
                    let mut response =
                        solver_contract::SolveResponse::decode(bytes.as_slice()).unwrap();
                    response.effective_parameters.as_mut().unwrap().seed += 1;
                    response.statistics.as_mut().unwrap().seed += 1;
                    payload["attempts"][0]["response"] =
                        serde_json::to_value(response.encode_to_vec()).unwrap();
                } else {
                    let original = payload["attempts"][0]["output_hash"][0].as_u64().unwrap();
                    payload["attempts"][0]["output_hash"][0] =
                        serde_json::json!((original + 1) % 256);
                }
            });
        }
        let expected = [
            "APPLICATION_SOLVE_ARTIFACT_RESPONSE_IDENTITY",
            "APPLICATION_SOLVE_ARTIFACT_VALIDATION",
            "APPLICATION_SOLVE_ARTIFACT_PROVENANCE",
        ];
        assert_eq!(
            load_solve_artifact(&store, &receipt.run_id)
                .unwrap_err()
                .code(),
            expected[mutation]
        );
    }
}

#[test]
fn saved_input_a_runs_existing_pipeline_and_keeps_revision_payload_and_protocol_scope() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let command = create(&mut store, false);
    let before = stored(&store, &command);
    let prepared = prepare_stored_project_solve(&store, &command).unwrap();
    let receipt = prepared.receipt().clone();
    assert_eq!(prepared.display_name(), before.display_name);
    assert_eq!(prepared.mode(), command.mode);
    assert_eq!(
        receipt.payload_hash,
        before.payload_hash().to_hex().to_string()
    );
    let output = execute_stored_project_solve(
        prepared,
        &options(),
        &client("cancel-after-feasible"),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(output.receipt, receipt);
    assert_eq!(output.display_name, before.display_name);
    assert_eq!(output.context.project_id, command.project_id.to_string());
    assert_eq!(output.context.project_revision, 0);
    assert_eq!(output.context.scenario_id, "project-import");
    assert_eq!(output.context.scenario_revision, 0);
    assert!(!output.context.request_id.is_empty());
    let ImportedProjectSolve::Existing(result) = output.result else {
        panic!("Input A");
    };
    let SolveExecution::Completed(completed) = result.execution else {
        panic!("worker completed");
    };
    assert_eq!(completed.status, SolverRunStatus::Feasible);
    assert_eq!(completed.assignments.len(), 3);
    assert!(completed.independent_validation.unwrap().is_valid());
    assert!(result.quality.is_some());
    assert_eq!(stored(&store, &command), before);
    assert_no_run_writes(&path, 1);
}

#[test]
fn saved_input_b_budget_exhaustion_preserves_raw_choices_and_does_not_claim_global_infeasibility() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let command = create(&mut store, true);
    let before = stored(&store, &command);
    let prepared = prepare_stored_project_solve(&store, &command).unwrap();
    let output = execute_stored_project_solve(
        prepared,
        &options(),
        &client("proven-infeasible"),
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(output.receipt.sectioning_required);
    let ImportedProjectSolve::AutoSectioned(result) = output.result else {
        panic!("Input B");
    };
    assert_eq!(
        result.status,
        AutoSectioningSolveStatus::CandidateBudgetExhausted
    );
    assert!(result.selected_attempt().is_none());
    assert_eq!(result.attempts.len(), 2);
    assert!(
        result
            .attempts
            .iter()
            .all(|attempt| attempt.solver_status() == Some(SolverRunStatus::ProvenInfeasible))
    );
    assert_eq!(stored(&store, &command), before);
    let loaded = load_imported_project(&store, command.project_id).unwrap();
    assert!(loaded.document.generated_sectioning.is_none());
    assert!(loaded.document.import_batch.teaching_sections().is_empty());
    assert_eq!(
        loaded.document.import_batch.student_subject_choices().len(),
        6
    );
    assert_no_run_writes(&path, 1);
}

#[test]
fn saved_input_b_selected_candidate_uses_explicit_run_policy_without_materializing_source() {
    let mut store = SqliteStore::in_memory().unwrap();
    let mut command = create(&mut store, true);
    let before = stored(&store, &command);
    // Import audited target=1 with two candidates. The saved document keeps raw choices;
    // this run explicitly asks for target=2 and one candidate instead.
    command.mode = StoredProjectSolveMode::AutoSectioning(
        AutoSectioningPolicy::new(1, 2, 2, 101, SectioningProfile::Balanced, 1).unwrap(),
    );
    let prepared = prepare_stored_project_solve(&store, &command).unwrap();
    let output = execute_stored_project_solve(
        prepared,
        &options(),
        &client("cancel-after-feasible"),
        &CancellationToken::new(),
    )
    .unwrap();
    let ImportedProjectSolve::AutoSectioned(result) = output.result else {
        panic!("Input B");
    };
    assert_eq!(result.status, AutoSectioningSolveStatus::SelectedFeasible);
    assert_eq!(result.attempts.len(), 1);
    let selected = result.selected_attempt().unwrap();
    assert_eq!(selected.sectioning.generated_sections().len(), 3);
    assert_eq!(selected.sectioning.generated_enrollments().len(), 6);
    assert!(selected.quality.is_some());
    let SolveExecution::Completed(completed) = &selected.execution else {
        panic!("worker completed");
    };
    assert!(
        completed
            .independent_validation
            .as_ref()
            .unwrap()
            .is_valid()
    );
    assert_eq!(stored(&store, &command), before);
    let loaded = load_imported_project(&store, command.project_id).unwrap();
    assert!(loaded.document.generated_sectioning.is_none());
    assert!(loaded.receipt.sectioning_required);
}

#[test]
fn missing_project_is_rejected_during_preparation() {
    let store = SqliteStore::in_memory().unwrap();
    let error = prepare_stored_project_solve(
        &store,
        &StoredProjectSolveCommand {
            project_id: SchoolProjectId::new_v4(),
            expected_revision: 0,
            mode: StoredProjectSolveMode::ExistingSections,
        },
    )
    .unwrap_err();
    assert_eq!(error.code(), "PERSISTENCE_PROJECT_NOT_FOUND");
}

#[test]
fn stale_revision_returns_existing_conflict_before_mode_checks() {
    let mut store = SqliteStore::in_memory().unwrap();
    let mut command = create(&mut store, false);
    let mut current = stored(&store, &command);
    current.revision = 1;
    store.replace_project(0, &current).unwrap();
    command.mode = StoredProjectSolveMode::AutoSectioning(policy());
    let error = prepare_stored_project_solve(&store, &command).unwrap_err();
    assert_eq!(error.code(), "PERSISTENCE_REVISION_CONFLICT");
    assert!(matches!(
        error,
        StoredProjectSolveError::Import(ImportCommandError::Persistence(
            PersistenceError::RevisionConflict {
                expected_revision: 0,
                actual_revision: 1,
                ..
            }
        ))
    ));
    assert_eq!(stored(&store, &command).revision, 1);
}

#[test]
fn both_input_mode_mismatches_are_explicit_and_do_not_modify_the_project() {
    for unsectioned in [false, true] {
        let mut store = SqliteStore::in_memory().unwrap();
        let mut command = create(&mut store, unsectioned);
        let before = stored(&store, &command);
        command.mode = if unsectioned {
            StoredProjectSolveMode::ExistingSections
        } else {
            StoredProjectSolveMode::AutoSectioning(policy())
        };
        let error = prepare_stored_project_solve(&store, &command).unwrap_err();
        assert_eq!(error.code(), "APPLICATION_STORED_PROJECT_MODE_MISMATCH");
        assert!(
            matches!(error, StoredProjectSolveError::ModeMismatch { requested, sectioning_required } if requested == command.mode && sectioning_required == unsectioned)
        );
        assert_eq!(stored(&store, &command), before);
    }
}

#[test]
fn public_sectioning_policy_fields_are_revalidated_before_execution() {
    let mut store = SqliteStore::in_memory().unwrap();
    let mut command = create(&mut store, true);
    for invalid in [
        AutoSectioningPolicy {
            minimum_size: 0,
            ..policy()
        },
        AutoSectioningPolicy {
            candidate_count: 0,
            ..policy()
        },
    ] {
        command.mode = StoredProjectSolveMode::AutoSectioning(invalid);
        let error = prepare_stored_project_solve(&store, &command).unwrap_err();
        assert!(matches!(
            error,
            StoredProjectSolveError::Import(ImportCommandError::Sectioning(_))
        ));
    }
    assert_eq!(stored(&store, &command).revision, 0);
}

#[test]
fn prepare_revalidates_persisted_content_even_when_revision_is_stale() {
    let mut store = SqliteStore::in_memory().unwrap();
    let command = create(&mut store, false);
    let mut invalid = stored(&store, &command);
    invalid.revision = 1;
    invalid.payload = b"{}".to_vec();
    store.replace_project(0, &invalid).unwrap();
    let error = prepare_stored_project_solve(&store, &command).unwrap_err();
    assert!(matches!(
        error,
        StoredProjectSolveError::Import(ImportCommandError::Document(_))
    ));
    assert_eq!(stored(&store, &command).revision, 1);
}

#[test]
fn another_connection_replacing_project_cannot_change_the_prepared_run() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let mut command = create(&mut store, false);
    let mut first_revision = stored(&store, &command);
    first_revision.revision = 1;
    store.replace_project(0, &first_revision).unwrap();
    command.expected_revision = 1;
    let old_loaded = load_imported_project(&store, command.project_id).unwrap();
    let expected_problem = serde_json::to_vec(&old_loaded.compiled.unwrap().problem).unwrap();
    let prepared = prepare_stored_project_solve(&store, &command).unwrap();
    let mut writer = SqliteStore::open(&path).unwrap();
    let mut changed = stored(&writer, &command);
    changed.revision = 2;
    let mut document = old_loaded.document;
    document.project_stable_key = "a-different-import-identity".to_owned();
    changed.payload = document.to_json_bytes().unwrap();
    changed.display_name = "Later project revision".to_owned();
    writer.replace_project(1, &changed).unwrap();
    let latest = load_imported_project(&writer, command.project_id).unwrap();
    assert_ne!(latest.receipt.payload_hash, prepared.receipt().payload_hash);
    assert_ne!(
        serde_json::to_vec(&latest.compiled.unwrap().problem).unwrap(),
        expected_problem
    );
    drop(writer);
    drop(store);

    let output = execute_stored_project_solve(
        prepared,
        &options(),
        &client("cancel-after-feasible"),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(output.receipt, old_loaded.receipt);
    assert_eq!(output.display_name, "Saved original project");
    assert_eq!(output.context.project_revision, 1);
    assert_eq!(output.context.scenario_revision, 1);
    let ImportedProjectSolve::Existing(result) = output.result else {
        panic!("Input A");
    };
    assert_eq!(
        serde_json::to_vec(&result.compiled.problem).unwrap(),
        expected_problem
    );
    let SolveExecution::Completed(completed) = result.execution else {
        panic!("worker completed");
    };
    assert_eq!(
        completed.snapshot_hash,
        *blake3::hash(&expected_problem).as_bytes()
    );
    assert!(completed.independent_validation.unwrap().is_valid());
    let reopened = SqliteStore::open(&path).unwrap();
    assert_eq!(stored(&reopened, &command).revision, 2);
    assert_no_run_writes(&path, 3);
}

#[test]
fn saved_project_bad_worker_output_cannot_bypass_independent_validation() {
    let mut store = SqliteStore::in_memory().unwrap();
    let command = create(&mut store, false);
    let before = stored(&store, &command);
    let prepared = prepare_stored_project_solve(&store, &command).unwrap();
    let error = execute_stored_project_solve(
        prepared,
        &options(),
        &client("invalid-conflict"),
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert_eq!(error.code(), "INTERNAL_ERROR_INVALID_SOLVER_OUTPUT");
    assert_eq!(stored(&store, &command), before);
}

#[test]
fn saved_input_b_cancellation_after_feasible_attempt_never_selects_the_prior_candidate() {
    let mut store = SqliteStore::in_memory().unwrap();
    let command = create(&mut store, true);
    let before = stored(&store, &command);
    let prepared = prepare_stored_project_solve(&store, &command).unwrap();
    let output = execute_stored_project_solve(
        prepared,
        &options(),
        &client("cancel-after-feasible"),
        &CancellationToken::new(),
    )
    .unwrap();
    let ImportedProjectSolve::AutoSectioned(result) = output.result else {
        panic!("Input B");
    };
    assert_eq!(result.status, AutoSectioningSolveStatus::Cancelled);
    assert!(result.selected_attempt().is_none());
    assert_eq!(result.attempts.len(), 2);
    assert_eq!(
        result.attempts[0].solver_status(),
        Some(SolverRunStatus::Feasible)
    );
    assert!(result.attempts[0].quality.is_some());
    assert_eq!(
        result.attempts[1].solver_status(),
        Some(SolverRunStatus::Cancelled)
    );
    assert_eq!(stored(&store, &command), before);
}

#[test]
fn unsupported_solve_modes_are_rejected_without_starting_a_worker() {
    let mut store = SqliteStore::in_memory().unwrap();
    let command = create(&mut store, false);
    let unavailable = SolverClient::new(SidecarSpec::new("/does-not-exist/saved-project-worker"));
    for mode in [
        SolveMode::Unspecified,
        SolveMode::Diagnose,
        SolveMode::Improve,
        SolveMode::Repair,
    ] {
        let prepared = prepare_stored_project_solve(&store, &command).unwrap();
        let error = execute_stored_project_solve(
            prepared,
            &SolveOptions { mode, ..options() },
            &unavailable,
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(
            error.code(),
            "APPLICATION_STORED_PROJECT_SOLVE_MODE_UNSUPPORTED"
        );
        assert!(
            matches!(error, StoredProjectSolveError::UnsupportedSolveMode { mode: actual } if actual == mode)
        );
    }
}

#[test]
fn saved_generate_keeps_existing_incumbent_rejection_before_starting_a_worker() {
    let mut store = SqliteStore::in_memory().unwrap();
    let command = create(&mut store, false);
    let prepared = prepare_stored_project_solve(&store, &command).unwrap();
    let unavailable = SolverClient::new(SidecarSpec::new("/does-not-exist/saved-project-worker"));
    let mut options = options();
    options.incumbent_assignments = vec![Assignment {
        activity: ActivityIndex(0),
        start: TimeslotIndex(0),
        room: RoomIndex(0),
        teacher: TeacherIndex(0),
    }];
    let error =
        execute_stored_project_solve(prepared, &options, &unavailable, &CancellationToken::new())
            .unwrap_err();
    assert_eq!(error.code(), "APPLICATION_INCUMBENT_NOT_ALLOWED_FOR_MODE");
}

#[test]
fn cancellation_before_execution_preserves_saved_inputs_and_starts_no_worker() {
    for unsectioned in [false, true] {
        let mut store = SqliteStore::in_memory().unwrap();
        let command = create(&mut store, unsectioned);
        let before = stored(&store, &command);
        let prepared = prepare_stored_project_solve(&store, &command).unwrap();
        let unavailable =
            SolverClient::new(SidecarSpec::new("/does-not-exist/saved-project-worker"));
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let output =
            execute_stored_project_solve(prepared, &options(), &unavailable, &cancellation)
                .unwrap();
        match output.result {
            ImportedProjectSolve::Existing(result) => {
                let SolveExecution::Completed(completed) = result.execution else {
                    panic!("cancelled execution");
                };
                assert_eq!(completed.status, SolverRunStatus::Cancelled);
                assert!(completed.assignments.is_empty());
                assert!(completed.independent_validation.is_none());
                assert!(result.quality.is_none());
            }
            ImportedProjectSolve::AutoSectioned(result) => {
                assert_eq!(result.status, AutoSectioningSolveStatus::Cancelled);
                assert!(result.selected_attempt().is_none());
                assert!(
                    result
                        .attempts
                        .iter()
                        .all(|attempt| attempt.quality.is_none())
                );
            }
        }
        assert_eq!(stored(&store, &command), before);
    }
}
