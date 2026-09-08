use class_schedule_application::{
    AdoptRunCommand, AutoSectioningPolicy, CalendarDefinition, CloneScenarioCommand,
    CsvImportAuditOptions, CsvImportMode, ImportCommitCommand, ImportCommitIntent, ScenarioReceipt,
    SectioningProfile, SolveOptions, StoredProjectSolveCommand, StoredProjectSolveMode,
    commit_csv_import, commit_prepared_scenario_creation, execute_durable_stored_project_solve,
    load_imported_project, load_scenario, load_solve_artifact, prepare_adopt_run,
    prepare_clone_scenario, prepare_stored_project_solve, save_prepared_solve_artifact,
};
use class_schedule_domain::{ScenarioId, SchoolProjectId};
use class_schedule_import::{CsvSource, DatasetKind};
use class_schedule_persistence::SqliteStore;
use rusqlite::{Connection, params};
use solver_client::{CancellationToken, SidecarSpec, SolverClient};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;
type CsvFiles = BTreeMap<DatasetKind, Vec<u8>>;
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

fn saved_run(
    store: &mut SqliteStore,
    unsectioned: bool,
    worker: &str,
    candidates: u8,
) -> AdoptRunCommand {
    let mut command = create(store, unsectioned);
    if unsectioned {
        command.mode = StoredProjectSolveMode::AutoSectioning(AutoSectioningPolicy {
            candidate_count: candidates,
            ..policy()
        });
    }
    let artifact = execute_durable_stored_project_solve(
        prepare_stored_project_solve(store, &command).unwrap(),
        &options(),
        &client(worker),
        &CancellationToken::new(),
    )
    .unwrap();
    let receipt = save_prepared_solve_artifact(store, &artifact).unwrap();
    AdoptRunCommand {
        project_id: command.project_id,
        expected_source_revision: 0,
        run_id: receipt.run_id.parse().unwrap(),
        scenario_id: ScenarioId::new_v4(),
        display_name: "方案 A".to_owned(),
    }
}
fn adopt(store: &mut SqliteStore, command: &AdoptRunCommand) -> ScenarioReceipt {
    let prepared = prepare_adopt_run(store, command).unwrap();
    commit_prepared_scenario_creation(store, prepared).unwrap()
}
fn clone_command(receipt: &ScenarioReceipt) -> CloneScenarioCommand {
    CloneScenarioCommand {
        project_id: receipt.project_id,
        expected_source_revision: receipt.source_project_revision,
        parent_scenario_id: receipt.scenario_id,
        expected_scenario_revision: receipt.scenario_revision,
        expected_timetable_revision: receipt.timetable_revision,
        scenario_id: ScenarioId::new_v4(),
        display_name: "方案 B".to_owned(),
    }
}
fn scenario_count(path: &Path) -> u64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT count(*) FROM scenarios", [], |row| row.get(0))
        .unwrap()
}
fn mutate_payload(
    path: &Path,
    scenario_id: ScenarioId,
    timetable: bool,
    mutate: impl FnOnce(&mut serde_json::Value),
) {
    let connection = Connection::open(path).unwrap();
    let table = if timetable {
        "timetable_revisions"
    } else {
        "scenario_revisions"
    };
    let payload: Vec<u8> = connection
        .query_row(
            &format!("SELECT payload FROM {table} WHERE scenario_id = ?1"),
            [scenario_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let mut json: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    mutate(&mut json);
    let payload = serde_json::to_vec(&json).unwrap();
    connection
        .execute(
            &format!("UPDATE {table} SET payload = ?1, payload_hash = ?2 WHERE scenario_id = ?3"),
            params![
                payload,
                blake3::hash(&payload).as_bytes(),
                scenario_id.to_string()
            ],
        )
        .unwrap();
}
#[test]
fn a_and_b_adoption_survive_reopen_without_mutating_source_or_run() {
    let directory = tempfile::tempdir().unwrap();
    for unsectioned in [false, true] {
        let path = directory.path().join(format!("{unsectioned}.sqlite3"));
        let mut store = SqliteStore::open(&path).unwrap();
        let command = saved_run(&mut store, unsectioned, "cancel-after-feasible", 1);
        let source_before = load_imported_project(&store, command.project_id).unwrap();
        let run_before = load_solve_artifact(&store, &command.run_id.to_string())
            .unwrap()
            .receipt;
        let prepared = prepare_adopt_run(&store, &command).unwrap();
        assert_eq!(scenario_count(&path), 0);
        let receipt = commit_prepared_scenario_creation(&mut store, prepared).unwrap();
        drop(store);
        let store = SqliteStore::open(&path).unwrap();
        let loaded = load_scenario(&store, command.scenario_id).unwrap();
        assert_eq!(loaded.receipt(), &receipt);
        assert!(loaded.source_is_current());
        assert_eq!(loaded.receipt().scenario_revision, 0);
        assert_eq!(loaded.receipt().timetable_revision, 0);
        assert_eq!(loaded.assignments().len(), if unsectioned { 6 } else { 3 });
        assert!(loaded.lineage().is_none());
        assert_eq!(loaded.materialized_sectioning().is_some(), unsectioned);
        let source_after = load_imported_project(&store, command.project_id).unwrap();
        assert_eq!(source_before.receipt, source_after.receipt);
        assert_eq!(source_before.document, source_after.document);
        assert!(source_after.document.generated_sectioning.is_none());
        assert_eq!(
            load_solve_artifact(&store, &command.run_id.to_string())
                .unwrap()
                .receipt,
            run_before
        );
    }
}
#[test]
fn clone_has_independent_ids_and_exact_immutable_parent_lineage() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("clone.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let command = saved_run(&mut store, true, "cancel-after-feasible", 1);
    let parent = adopt(&mut store, &command);
    let prepared = prepare_clone_scenario(&store, &clone_command(&parent)).unwrap();
    assert_eq!(scenario_count(&path), 1);
    let child = commit_prepared_scenario_creation(&mut store, prepared).unwrap();
    let parent_loaded = load_scenario(&store, parent.scenario_id).unwrap();
    let child_loaded = load_scenario(&store, child.scenario_id).unwrap();
    assert_ne!(parent.scenario_id, child.scenario_id);
    assert_ne!(parent.timetable_id, child.timetable_id);
    assert_eq!(parent.origin_run_id, child.origin_run_id);
    assert_eq!(parent_loaded.assignments(), child_loaded.assignments());
    assert_eq!(
        parent_loaded.materialized_sectioning(),
        child_loaded.materialized_sectioning()
    );
    assert!(
        parent_loaded
            .timetable()
            .meetings()
            .iter()
            .zip(child_loaded.timetable().meetings())
            .all(|(left, right)| left.id() != right.id()
                && left.demand_id() == right.demand_id()
                && left.assignment() == right.assignment())
    );
    let lineage = child_loaded.lineage().unwrap();
    assert_eq!(lineage.scenario_id, parent.scenario_id);
    assert_eq!(lineage.timetable_id, parent.timetable_id);
    assert_eq!(
        blake3::Hash::from(lineage.scenario_payload_hash)
            .to_hex()
            .to_string(),
        parent.scenario_payload_hash
    );
    assert_eq!(
        blake3::Hash::from(lineage.timetable_payload_hash)
            .to_hex()
            .to_string(),
        parent.timetable_payload_hash
    );
    mutate_payload(&path, parent.scenario_id, false, |json| {
        json["semantics_version"] = "unsupported".into();
    });
    assert!(load_scenario(&store, parent.scenario_id).is_err());
    drop(store);
    let store = SqliteStore::open(path).unwrap();
    assert_eq!(
        load_scenario(&store, child.scenario_id).unwrap().receipt(),
        &child
    );
}
#[test]
fn source_replacement_allows_historical_open_but_rejects_adoption_and_copy() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("historical.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let command = saved_run(&mut store, false, "cancel-after-feasible", 1);
    let parent = adopt(&mut store, &command);
    let another = AdoptRunCommand {
        scenario_id: ScenarioId::new_v4(),
        ..command.clone()
    };
    let prepared = prepare_adopt_run(&store, &another).unwrap();
    let mut current = store.load_project(&command.project_id.to_string()).unwrap();
    current.revision = 1;
    store.replace_project(0, &current).unwrap();
    assert_eq!(
        commit_prepared_scenario_creation(&mut store, prepared)
            .unwrap_err()
            .code(),
        "PERSISTENCE_REVISION_CONFLICT"
    );
    assert_eq!(
        prepare_adopt_run(&store, &another).unwrap_err().code(),
        "PERSISTENCE_REVISION_CONFLICT"
    );
    assert_eq!(
        prepare_clone_scenario(&store, &clone_command(&parent))
            .unwrap_err()
            .code(),
        "PERSISTENCE_REVISION_CONFLICT"
    );
    let loaded = load_scenario(&store, parent.scenario_id).unwrap();
    assert!(!loaded.source_is_current());
    assert_eq!(loaded.receipt().source_project_revision, 0);
    assert_eq!(scenario_count(&path), 1);
}
#[test]
fn only_successful_selected_terminal_runs_can_be_adopted() {
    let directory = tempfile::tempdir().unwrap();
    for (index, (unsectioned, mode, candidates)) in [
        (false, "unknown", 1),
        (false, "timeout", 1),
        (false, "proven-infeasible", 1),
        (false, "worker-crash", 1),
        (true, "cancel-after-feasible", 2),
    ]
    .into_iter()
    .enumerate()
    {
        let path = directory.path().join(format!("rejected-{index}.sqlite3"));
        let mut store = SqliteStore::open(&path).unwrap();
        let command = saved_run(&mut store, unsectioned, mode, candidates);
        assert_eq!(
            prepare_adopt_run(&store, &command).unwrap_err().code(),
            "APPLICATION_SCENARIO_RUN_NOT_ADOPTABLE"
        );
        assert_eq!(scenario_count(&path), 0);
    }
}
#[test]
fn rehashed_illegal_assignments_are_rejected_by_independent_hard_validation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("illegal.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let command = saved_run(&mut store, false, "cancel-after-feasible", 1);
    let receipt = adopt(&mut store, &command);
    mutate_payload(&path, receipt.scenario_id, true, |json| {
        json["meetings"][1]["assignment"]["start"] =
            json["meetings"][0]["assignment"]["start"].clone();
    });
    assert_eq!(
        load_scenario(&store, receipt.scenario_id)
            .unwrap_err()
            .code(),
        "APPLICATION_SCENARIO_HARD_VALIDATION_FAILED"
    );
}
#[test]
fn rehashed_snapshot_materialization_and_schema_do_not_bypass_replay() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("replay.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let base = saved_run(&mut store, true, "cancel-after-feasible", 1);
    for case in 0..3 {
        let command = AdoptRunCommand {
            scenario_id: ScenarioId::new_v4(),
            ..base.clone()
        };
        let receipt = adopt(&mut store, &command);
        mutate_payload(&path, receipt.scenario_id, case == 0, |json| match case {
            0 => {
                let byte = json["snapshot_hash"][0].as_u64().unwrap();
                json["snapshot_hash"][0] = ((byte + 1) % 256).into();
            }
            1 => json["materialized_sectioning"]["enrollments"] = serde_json::json!([]),
            _ => json["semantics_version"] = "future-version".into(),
        });
        let expected = [
            "APPLICATION_SCENARIO_SNAPSHOT_MISMATCH",
            "APPLICATION_SCENARIO_ORIGIN_MISMATCH",
            "APPLICATION_SCENARIO_UNSUPPORTED_SCHEMA",
        ][case];
        assert_eq!(
            load_scenario(&store, receipt.scenario_id)
                .unwrap_err()
                .code(),
            expected
        );
    }
}
#[test]
fn duplicate_and_invalid_commands_never_create_partial_scenarios() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("commands.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let command = saved_run(&mut store, false, "cancel-after-feasible", 1);
    let receipt = adopt(&mut store, &command);
    let duplicate = prepare_adopt_run(&store, &command).unwrap();
    assert_eq!(
        commit_prepared_scenario_creation(&mut store, duplicate)
            .unwrap_err()
            .code(),
        "PERSISTENCE_SCENARIO_ALREADY_EXISTS"
    );
    let unrelated = AdoptRunCommand {
        project_id: SchoolProjectId::new_v4(),
        ..command.clone()
    };
    assert_eq!(
        prepare_adopt_run(&store, &unrelated).unwrap_err().code(),
        "APPLICATION_SCENARIO_RUN_SOURCE_MISMATCH"
    );
    let mut copy = clone_command(&receipt);
    copy.expected_scenario_revision = 1;
    assert_eq!(
        prepare_clone_scenario(&store, &copy).unwrap_err().code(),
        "APPLICATION_SCENARIO_REVISION_CONFLICT"
    );
    copy.expected_scenario_revision = 0;
    copy.expected_timetable_revision = 1;
    assert_eq!(
        prepare_clone_scenario(&store, &copy).unwrap_err().code(),
        "APPLICATION_TIMETABLE_REVISION_CONFLICT"
    );
    copy.expected_timetable_revision = 0;
    copy.scenario_id = receipt.scenario_id;
    assert_eq!(
        prepare_clone_scenario(&store, &copy).unwrap_err().code(),
        "APPLICATION_SCENARIO_SELF_COPY"
    );
    let invalid_name = AdoptRunCommand {
        display_name: " ".to_owned(),
        ..command
    };
    assert_eq!(
        prepare_adopt_run(&store, &invalid_name).unwrap_err().code(),
        "APPLICATION_SCENARIO_INVALID_NAME"
    );
    assert_eq!(scenario_count(&path), 1);
}
#[test]
fn prepared_copy_rechecks_actual_parent_hash_before_any_new_rows() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("parent-race.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let command = saved_run(&mut store, false, "cancel-after-feasible", 1);
    let parent = adopt(&mut store, &command);
    let prepared = prepare_clone_scenario(&store, &clone_command(&parent)).unwrap();
    mutate_payload(&path, parent.scenario_id, false, |json| {
        json["display_name"] = "Changed parent".into();
    });
    assert_eq!(
        commit_prepared_scenario_creation(&mut store, prepared)
            .unwrap_err()
            .code(),
        "PERSISTENCE_SCENARIO_HASH_MISMATCH"
    );
    assert_eq!(scenario_count(&path), 1);
}

#[test]
fn legal_but_different_timetable_and_changed_selected_index_are_not_the_adopted_run() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("different-output.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let base = saved_run(&mut store, false, "cancel-after-feasible", 1);
    let receipt = adopt(&mut store, &base);
    let loaded = load_scenario(&store, receipt.scenario_id).unwrap();
    let different_start = loaded.compiled().problem.timeslots()[5].stable_id;
    mutate_payload(&path, receipt.scenario_id, true, |json| {
        json["meetings"][0]["assignment"]["start"] = serde_json::to_value(different_start).unwrap();
    });
    assert_eq!(
        load_scenario(&store, receipt.scenario_id)
            .unwrap_err()
            .code(),
        "APPLICATION_SCENARIO_ASSIGNMENT_MISMATCH"
    );
    let base = saved_run(&mut store, true, "cancel-after-feasible", 1);
    let receipt = adopt(&mut store, &base);
    mutate_payload(&path, receipt.scenario_id, false, |json| {
        json["selected_attempt_index"] = 1.into();
    });
    assert_eq!(
        load_scenario(&store, receipt.scenario_id)
            .unwrap_err()
            .code(),
        "APPLICATION_SCENARIO_ORIGIN_MISMATCH"
    );
}
