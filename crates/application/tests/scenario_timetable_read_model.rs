use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;
use std::time::Duration;

use class_schedule_application::{
    AdoptRunCommand, AutoSectioningPolicy, CalendarDefinition, CloneScenarioCommand,
    CsvImportAuditOptions, CsvImportMode, ImportCommitCommand, ImportCommitIntent, ScenarioReceipt,
    ScenarioTimetableQueryError, SectioningProfile, SolveOptions, StoredProjectSolveCommand,
    StoredProjectSolveMode, TimetableAudience, TimetableFilter, TimetableQuery, TimetableView,
    commit_csv_import, commit_prepared_scenario_creation, execute_durable_stored_project_solve,
    load_imported_project, load_scenario, prepare_adopt_run, prepare_clone_scenario,
    prepare_stored_project_solve, query_saved_timetable, query_saved_timetable_entities,
    query_scenario_timetable, query_scenario_timetable_entities, save_prepared_solve_artifact,
};
use class_schedule_domain::{Revision, ScenarioId, SchoolProjectId, StudentId};
use class_schedule_import::{CsvSource, DatasetKind};
use class_schedule_persistence::SqliteStore;
use rusqlite::{Connection, params};
use solver_client::{CancellationToken, SidecarSpec, SolverClient};

const ZERO: Revision = Revision::from_u64(0);

fn files(unsectioned: bool) -> BTreeMap<DatasetKind, Vec<u8>> {
    let mut files: BTreeMap<_, _> = [
        (DatasetKind::Students, "student_code,name,administrative_class_code\nS1,Student one,AC1\nS2,Student two,AC1\nS3,Student three,AC2\nS4,Student four,AC2\n"),
        (DatasetKind::AdministrativeClasses, "administrative_class_code,name,grade_code,home_room_code\nAC1,Class one,G12,R1\nAC2,Class two,G12,R1\n"),
        (DatasetKind::Teachers, "teacher_code,name\nT1,Teacher one\nT2,Unused teacher\n"),
        (DatasetKind::Rooms, "room_code,name,building_code,capacity,features\nR1,Room one,B1,4,lab\nR2,Unused room,B1,4,lab\n"),
        (DatasetKind::CoursePlans, concat!(
            "course_plan_code,name,grade_code,subject_code,audience_kind,weekly_periods,meeting_pattern,min_days_between,max_periods_per_day,may_cross_breaks,required_room_features,teacher_assignment,teacher_codes,room_policy,room_candidates,preferred_rooms,fallback_rooms\n",
            "P0,Chinese,G12,chinese,administrative_class,1,1,0,1,false,,fixed,T1,admin_home_room,,,\n",
            "P1,Physics,G12,physics,teaching_section,1,1,0,1,false,lab,fixed,T1,section_fixed,R1,,\n",
            "P2,Chemistry,G12,chemistry,teaching_section,1,1,0,1,false,lab,fixed,T1,section_fixed,R1,,\n",
            "P3,Biology,G12,biology,teaching_section,2,2,0,2,false,lab,fixed,T1,section_fixed,R1,,\n",
        )),
    ].into_iter().map(|(kind, value)| (kind, value.as_bytes().to_vec())).collect();
    let mut choices = "student_code,subject_code\n".to_owned();
    for student in ["S1", "S2", "S3", "S4"] {
        for subject in ["physics", "chemistry", "biology"] {
            writeln!(choices, "{student},{subject}").unwrap();
        }
    }
    files.insert(DatasetKind::StudentSubjectChoices, choices.into_bytes());
    if !unsectioned {
        files.insert(DatasetKind::TeachingSections, concat!(
            "section_code,name,grade_code,subject_code,min_size,target_size,max_size,room_policy,room_candidates,preferred_rooms,fallback_rooms,teacher_assignment,teacher_codes\n",
            "PHY1,Physics one,G12,physics,1,2,4,section_fixed,R1,,,fixed,T1\n",
            "PHY2,Physics two,G12,physics,1,2,4,section_fixed,R1,,,fixed,T1\n",
            "CHEM,Chemistry,G12,chemistry,1,4,4,section_fixed,R1,,,fixed,T1\n",
            "BIO,Biology,G12,biology,1,4,4,section_fixed,R1,,,fixed,T1\n",
        ).as_bytes().to_vec());
        files.insert(
            DatasetKind::SectionEnrollments,
            concat!(
                "section_code,student_code\nPHY1,S1\nPHY1,S3\nPHY2,S2\nPHY2,S4\n",
                "CHEM,S1\nCHEM,S2\nCHEM,S3\nCHEM,S4\nBIO,S1\nBIO,S2\nBIO,S3\nBIO,S4\n",
            )
            .as_bytes()
            .to_vec(),
        );
    }
    files
}

fn saved_run(
    store: &mut SqliteStore,
    unsectioned: bool,
    worker_mode: &str,
    candidates: u8,
) -> (SchoolProjectId, String) {
    let project_id = SchoolProjectId::new_v4();
    let policy =
        AutoSectioningPolicy::new(1, 2, 4, 99, SectioningProfile::Balanced, candidates).unwrap();
    let files = files(unsectioned);
    let command = ImportCommitCommand {
        project_id,
        display_name: "Read model fixture".to_owned(),
        intent: ImportCommitIntent::Create,
        options: CsvImportAuditOptions {
            project_stable_key: "read-model-fixture".to_owned(),
            calendar: CalendarDefinition::weekday_with_break(4, 2).unwrap(),
            exact_subject_choices: 3,
            mode: if unsectioned {
                CsvImportMode::Unsectioned(policy)
            } else {
                CsvImportMode::ExistingSections
            },
        },
    };
    commit_csv_import(
        store,
        &command,
        files
            .iter()
            .map(|(&kind, bytes)| CsvSource::new(kind, bytes)),
    )
    .unwrap();
    let command = StoredProjectSolveCommand {
        project_id,
        expected_revision: 0,
        mode: if unsectioned {
            StoredProjectSolveMode::AutoSectioning(policy)
        } else {
            StoredProjectSolveMode::ExistingSections
        },
    };
    let prepared = prepare_stored_project_solve(store, &command).unwrap();
    let client = SolverClient::new(
        SidecarSpec::new(env!("CARGO_BIN_EXE_application-test-worker")).arg(worker_mode),
    );
    let artifact = execute_durable_stored_project_solve(
        prepared,
        &SolveOptions::reproducible(99, Duration::from_secs(10)),
        &client,
        &CancellationToken::new(),
    )
    .unwrap();
    let receipt = save_prepared_solve_artifact(store, &artifact).unwrap();
    (project_id, receipt.run_id)
}

fn saved_scenario(store: &mut SqliteStore, unsectioned: bool) -> ScenarioReceipt {
    let (project_id, run_id) = saved_run(store, unsectioned, "cancel-after-feasible", 1);
    let prepared = prepare_adopt_run(
        store,
        &AdoptRunCommand {
            project_id,
            expected_source_revision: 0,
            run_id: run_id.parse().unwrap(),
            scenario_id: ScenarioId::new_v4(),
            display_name: "采用方案".to_owned(),
        },
    )
    .unwrap();
    commit_prepared_scenario_creation(store, prepared).unwrap()
}

fn copy_scenario(store: &mut SqliteStore, parent: &ScenarioReceipt) -> ScenarioReceipt {
    let prepared = prepare_clone_scenario(
        store,
        &CloneScenarioCommand {
            project_id: parent.project_id,
            expected_source_revision: parent.source_project_revision,
            parent_scenario_id: parent.scenario_id,
            expected_scenario_revision: parent.scenario_revision,
            expected_timetable_revision: parent.timetable_revision,
            scenario_id: ScenarioId::new_v4(),
            display_name: "独立副本".to_owned(),
        },
    )
    .unwrap();
    commit_prepared_scenario_creation(store, prepared).unwrap()
}

fn filter(
    store: &SqliteStore,
    scenario: ScenarioId,
    view: TimetableView,
    code: &str,
) -> TimetableFilter {
    query_scenario_timetable_entities(store, scenario, ZERO, ZERO, view, 0, 100)
        .unwrap()
        .entities
        .into_iter()
        .find(|entity| entity.code == code)
        .unwrap()
        .filter
}

fn query(filter: TimetableFilter) -> TimetableQuery {
    TimetableQuery {
        filter,
        offset: 0,
        limit: 100,
    }
}

#[test]
fn scenario_seven_views_select_exact_entities_including_unused_resources() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("views.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = saved_scenario(&mut store, false);
    drop(store);
    let store = SqliteStore::open_readonly(&path).unwrap();
    for (view, code, expected) in [
        (TimetableView::AdministrativeClass, "AC1", 5),
        (TimetableView::TeachingSection, "PHY1", 1),
        (TimetableView::Teacher, "T1", 6),
        (TimetableView::Room, "R1", 6),
        (TimetableView::Student, "S1", 4),
        (TimetableView::Subject, "physics", 2),
        (TimetableView::Grade, "G12", 6),
        (TimetableView::Teacher, "T2", 0),
        (TimetableView::Room, "R2", 0),
    ] {
        let page = query_scenario_timetable(
            &store,
            receipt.scenario_id,
            ZERO,
            ZERO,
            &query(filter(&store, receipt.scenario_id, view, code)),
        )
        .unwrap();
        assert_eq!(page.total_rows, expected, "{view:?}");
        assert_eq!(page.calendar.len(), 20);
        assert_eq!(page.receipt, receipt);
        assert_eq!(page.scenario_display_name, "采用方案");
        assert!(page.source_is_current);
        assert_eq!(page.schema_version, 1);
    }
}

#[test]
fn scenario_administrative_class_and_student_follow_actual_walking_enrollment() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("enrollment.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = saved_scenario(&mut store, false);
    drop(store);
    let store = SqliteStore::open_readonly(&path).unwrap();
    let class = query_scenario_timetable(
        &store,
        receipt.scenario_id,
        ZERO,
        ZERO,
        &query(filter(
            &store,
            receipt.scenario_id,
            TimetableView::AdministrativeClass,
            "AC1",
        )),
    )
    .unwrap();
    let audiences: BTreeSet<_> = class
        .rows
        .iter()
        .map(|row| match &row.audience {
            TimetableAudience::AdministrativeClass(entity) => entity.code.as_str(),
            TimetableAudience::TeachingSection(entity) => entity.code.as_str(),
        })
        .collect();
    assert_eq!(
        audiences,
        BTreeSet::from(["AC1", "BIO", "CHEM", "PHY1", "PHY2"])
    );
    let student = query_scenario_timetable(
        &store,
        receipt.scenario_id,
        ZERO,
        ZERO,
        &query(filter(
            &store,
            receipt.scenario_id,
            TimetableView::Student,
            "S1",
        )),
    )
    .unwrap();
    assert!(student.rows.iter().any(|row| matches!(&row.audience,
        TimetableAudience::TeachingSection(section) if section.code == "PHY1")));
    assert!(!student.rows.iter().any(|row| matches!(&row.audience,
        TimetableAudience::TeachingSection(section) if section.code == "PHY2")));
    let json = serde_json::to_string(&class.rows).unwrap();
    for name in [
        "Student one",
        "Student two",
        "Student three",
        "Student four",
        "Unused teacher",
    ] {
        assert!(
            !json.contains(name),
            "unrequested person must not appear: {name}"
        );
    }
}

#[test]
fn scenario_grid_expands_double_periods_and_preserves_empty_cells() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("calendar.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = saved_scenario(&mut store, false);
    drop(store);
    let store = SqliteStore::open_readonly(&path).unwrap();
    let grade = query_scenario_timetable(
        &store,
        receipt.scenario_id,
        ZERO,
        ZERO,
        &query(filter(
            &store,
            receipt.scenario_id,
            TimetableView::Grade,
            "G12",
        )),
    )
    .unwrap();
    let double = grade
        .rows
        .iter()
        .find(|row| row.duration_periods == 2)
        .unwrap();
    assert_eq!(double.occupied_timeslot_indices.len(), 2);
    for index in &double.occupied_timeslot_indices {
        let cell = &grade.calendar[*index as usize];
        assert_eq!(cell.day, double.day);
        assert!(cell.page_activity_ids.contains(&double.activity_id));
        assert_eq!(cell.occupied_count, 1);
    }
    assert!(grade.calendar.iter().any(|cell| cell.occupied_count == 0));
    assert_eq!(
        grade
            .calendar
            .iter()
            .map(|cell| cell.occupied_count)
            .sum::<u32>(),
        7
    );
}

#[test]
fn a_and_b_copies_keep_their_own_identity_and_all_views_survive_readonly_reopen() {
    let directory = tempfile::tempdir().unwrap();
    for unsectioned in [false, true] {
        let path = directory
            .path()
            .join(format!("copies-{unsectioned}.sqlite3"));
        let mut store = SqliteStore::open(&path).unwrap();
        let parent = saved_scenario(&mut store, unsectioned);
        let child = copy_scenario(&mut store, &parent);
        let before = load_imported_project(&store, parent.project_id).unwrap();
        drop(store);
        let store = SqliteStore::open_readonly(&path).unwrap();
        assert_ne!(parent.scenario_id, child.scenario_id);
        assert_ne!(parent.timetable_id, child.timetable_id);
        for view in [
            TimetableView::AdministrativeClass,
            TimetableView::TeachingSection,
            TimetableView::Teacher,
            TimetableView::Room,
            TimetableView::Student,
            TimetableView::Subject,
            TimetableView::Grade,
        ] {
            let entities = query_scenario_timetable_entities(
                &store,
                parent.scenario_id,
                ZERO,
                ZERO,
                view,
                0,
                100,
            )
            .unwrap();
            let copied = query_scenario_timetable_entities(
                &store,
                child.scenario_id,
                ZERO,
                ZERO,
                view,
                0,
                100,
            )
            .unwrap();
            let original = query_saved_timetable_entities(
                &store,
                &parent.origin_run_id.to_string(),
                view,
                0,
                100,
            )
            .unwrap();
            assert_eq!(entities.entities, copied.entities);
            assert_eq!(entities.entities, original.entities);
            assert_eq!(copied.receipt, child);
            assert_eq!(copied.scenario_display_name, "独立副本");
            for entity in entities.entities {
                let query = query(entity.filter);
                let page = query_scenario_timetable(&store, parent.scenario_id, ZERO, ZERO, &query)
                    .unwrap();
                let copied =
                    query_scenario_timetable(&store, child.scenario_id, ZERO, ZERO, &query)
                        .unwrap();
                let original =
                    query_saved_timetable(&store, &parent.origin_run_id.to_string(), &query)
                        .unwrap();
                assert_eq!(page.rows, copied.rows);
                assert_eq!(page.rows, original.rows);
                assert_eq!(page.calendar, copied.calendar);
                assert_eq!(page.quality, copied.quality);
                assert_eq!(copied.receipt, child);
                assert!(copied.source_is_current);
            }
        }
        let loaded = load_scenario(&store, child.scenario_id).unwrap();
        assert_eq!(loaded.materialized_sectioning().is_some(), unsectioned);
        assert_eq!(loaded.lineage().unwrap().scenario_id, parent.scenario_id);
        let after = load_imported_project(&store, parent.project_id).unwrap();
        assert_eq!(before.receipt, after.receipt);
        assert_eq!(before.document, after.document);
        assert!(after.document.generated_sectioning.is_none());
        if unsectioned {
            assert!(after.document.import_batch.teaching_sections().is_empty());
            assert!(loaded.materialized_sectioning().unwrap().sections.len() > 1);
        }
    }
}

#[test]
fn scenario_pages_preserve_hidden_occupancy_and_enforce_bounds() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("pages.sqlite3")).unwrap();
    let receipt = saved_scenario(&mut store, false);
    let selected = filter(&store, receipt.scenario_id, TimetableView::Grade, "G12");
    let first = query_scenario_timetable(
        &store,
        receipt.scenario_id,
        ZERO,
        ZERO,
        &TimetableQuery {
            filter: selected,
            offset: 0,
            limit: 1,
        },
    )
    .unwrap();
    assert_eq!(first.total_rows, 6);
    assert_eq!(first.rows.len(), 1);
    assert_eq!(first.next_offset, Some(1));
    assert!(
        first
            .calendar
            .iter()
            .any(|cell| cell.occupied_count > 0 && cell.page_activity_ids.is_empty())
    );
    let last = query_scenario_timetable(
        &store,
        receipt.scenario_id,
        ZERO,
        ZERO,
        &TimetableQuery {
            filter: selected,
            offset: 5,
            limit: 1,
        },
    )
    .unwrap();
    assert!(!last.has_more);
    assert_eq!(last.next_offset, None);
    assert_ne!(first.rows[0].activity_id, last.rows[0].activity_id);
    let students = query_scenario_timetable_entities(
        &store,
        receipt.scenario_id,
        ZERO,
        ZERO,
        TimetableView::Student,
        1,
        2,
    )
    .unwrap();
    assert_eq!(
        students
            .entities
            .iter()
            .map(|entity| entity.code.as_str())
            .collect::<Vec<_>>(),
        ["S2", "S3"]
    );
    assert_eq!(students.total_entities, 4);
    assert_eq!(students.next_offset, Some(3));
    for (offset, limit) in [(0, 0), (0, 101), (u32::MAX, 1)] {
        let missing = ScenarioId::new_v4();
        assert_eq!(
            query_scenario_timetable(
                &store,
                missing,
                ZERO,
                ZERO,
                &TimetableQuery {
                    filter: selected,
                    offset,
                    limit
                }
            )
            .unwrap_err()
            .code(),
            "APPLICATION_TIMETABLE_INVALID_PAGE"
        );
        assert_eq!(
            query_scenario_timetable_entities(
                &store,
                missing,
                ZERO,
                ZERO,
                TimetableView::Student,
                offset,
                limit
            )
            .unwrap_err()
            .code(),
            "APPLICATION_TIMETABLE_INVALID_PAGE"
        );
    }
}

#[test]
fn readonly_queries_never_modify_any_persisted_table() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("readonly.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let parent = saved_scenario(&mut store, true);
    let child = copy_scenario(&mut store, &parent);
    drop(store);
    let observer = Connection::open(&path).unwrap();
    for table in [
        "projects",
        "project_revisions",
        "solver_runs",
        "solve_artifacts",
        "solve_artifact_attempts",
        "scenarios",
        "scenario_revisions",
        "timetable_revisions",
    ] {
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            observer.execute_batch(&format!("CREATE TRIGGER deny_{table}_{operation} BEFORE {operation} ON {table} BEGIN SELECT RAISE(ABORT, 'read query attempted write'); END;")).unwrap();
        }
    }
    let before: u64 = observer
        .query_row("PRAGMA data_version", [], |row| row.get(0))
        .unwrap();
    let store = SqliteStore::open_readonly(&path).unwrap();
    for receipt in [&parent, &child] {
        for view in [
            TimetableView::AdministrativeClass,
            TimetableView::TeachingSection,
            TimetableView::Teacher,
            TimetableView::Room,
            TimetableView::Student,
            TimetableView::Subject,
            TimetableView::Grade,
        ] {
            let entities = query_scenario_timetable_entities(
                &store,
                receipt.scenario_id,
                ZERO,
                ZERO,
                view,
                0,
                100,
            )
            .unwrap();
            for entity in entities.entities {
                query_scenario_timetable(
                    &store,
                    receipt.scenario_id,
                    ZERO,
                    ZERO,
                    &query(entity.filter),
                )
                .unwrap();
            }
        }
    }
    let after: u64 = observer
        .query_row("PRAGMA data_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(before, after);
}

#[test]
fn historical_source_is_readable_without_rebasing_rows_or_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = saved_scenario(&mut store, false);
    let selected = filter(&store, receipt.scenario_id, TimetableView::Grade, "G12");
    let before =
        query_scenario_timetable(&store, receipt.scenario_id, ZERO, ZERO, &query(selected))
            .unwrap();
    let mut source = store.load_project(&receipt.project_id.to_string()).unwrap();
    source.revision = 1;
    source.display_name = "Replaced source".to_owned();
    store.replace_project(0, &source).unwrap();
    drop(store);
    let store = SqliteStore::open_readonly(&path).unwrap();
    let page = query_scenario_timetable(&store, receipt.scenario_id, ZERO, ZERO, &query(selected))
        .unwrap();
    assert!(!page.source_is_current);
    assert_eq!(page.receipt, receipt);
    assert_eq!(page.receipt.source_project_revision, 0);
    assert_eq!(page.rows, before.rows);
    assert_eq!(page.calendar, before.calendar);
    let entities = query_scenario_timetable_entities(
        &store,
        receipt.scenario_id,
        ZERO,
        ZERO,
        TimetableView::Grade,
        0,
        100,
    )
    .unwrap();
    assert!(!entities.source_is_current);
    assert_eq!(entities.receipt, receipt);
    assert_eq!(
        store
            .load_project(&receipt.project_id.to_string())
            .unwrap()
            .revision,
        1
    );
}

#[test]
fn both_revisions_are_exact_and_unknown_entities_never_widen_selection() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("conflicts.sqlite3")).unwrap();
    let receipt = saved_scenario(&mut store, false);
    let selected = filter(&store, receipt.scenario_id, TimetableView::Grade, "G12");
    let large = Revision::from_u64(9_007_199_254_740_993);
    let error =
        query_scenario_timetable(&store, receipt.scenario_id, large, ZERO, &query(selected))
            .unwrap_err();
    assert_eq!(error.code(), "APPLICATION_SCENARIO_REVISION_CONFLICT");
    assert!(
        matches!(error, ScenarioTimetableQueryError::ScenarioRevisionConflict { expected, actual }
        if expected == large && actual == ZERO)
    );
    let error =
        query_scenario_timetable(&store, receipt.scenario_id, ZERO, large, &query(selected))
            .unwrap_err();
    assert_eq!(error.code(), "APPLICATION_TIMETABLE_REVISION_CONFLICT");
    assert!(
        matches!(error, ScenarioTimetableQueryError::TimetableRevisionConflict { expected, actual }
        if expected == large && actual == ZERO)
    );
    for (scenario, timetable, expected_code) in [
        (large, ZERO, "APPLICATION_SCENARIO_REVISION_CONFLICT"),
        (ZERO, large, "APPLICATION_TIMETABLE_REVISION_CONFLICT"),
    ] {
        assert_eq!(
            query_scenario_timetable_entities(
                &store,
                receipt.scenario_id,
                scenario,
                timetable,
                TimetableView::Grade,
                0,
                100
            )
            .unwrap_err()
            .code(),
            expected_code
        );
    }
    assert_eq!(
        query_scenario_timetable(
            &store,
            receipt.scenario_id,
            ZERO,
            ZERO,
            &query(TimetableFilter::Student(StudentId::new_v4()))
        )
        .unwrap_err()
        .code(),
        "APPLICATION_TIMETABLE_ENTITY_NOT_FOUND"
    );
    assert_eq!(
        query_scenario_timetable_entities(
            &store,
            ScenarioId::new_v4(),
            ZERO,
            ZERO,
            TimetableView::Grade,
            0,
            100
        )
        .unwrap_err()
        .code(),
        "PERSISTENCE_SCENARIO_NOT_FOUND"
    );
}

fn corrupt_timetable(path: &Path, scenario_id: ScenarioId, rehash: bool) {
    let connection = Connection::open(path).unwrap();
    let payload: Vec<u8> = connection
        .query_row(
            "SELECT payload FROM timetable_revisions WHERE scenario_id = ?1",
            [scenario_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let mut json: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    json["meetings"][1]["assignment"]["start"] = json["meetings"][0]["assignment"]["start"].clone();
    let payload = serde_json::to_vec(&json).unwrap();
    if rehash {
        connection.execute("UPDATE timetable_revisions SET payload = ?1, payload_hash = ?2 WHERE scenario_id = ?3",
            params![payload, blake3::hash(&payload).as_bytes(), scenario_id.to_string()]).unwrap();
    } else {
        connection
            .execute(
                "UPDATE timetable_revisions SET payload = ?1 WHERE scenario_id = ?2",
                params![payload, scenario_id.to_string()],
            )
            .unwrap();
    }
}

#[test]
fn corrupt_own_timetable_fails_even_with_valid_origin_or_stale_caller_but_copy_stays_independent() {
    let directory = tempfile::tempdir().unwrap();
    for rehash in [false, true] {
        let path = directory.path().join(format!("corrupt-{rehash}.sqlite3"));
        let mut store = SqliteStore::open(&path).unwrap();
        let parent = saved_scenario(&mut store, false);
        let child = copy_scenario(&mut store, &parent);
        let selected = filter(&store, child.scenario_id, TimetableView::Grade, "G12");
        let copy_before =
            query_scenario_timetable(&store, child.scenario_id, ZERO, ZERO, &query(selected))
                .unwrap();
        drop(store);
        corrupt_timetable(&path, parent.scenario_id, rehash);
        let store = SqliteStore::open_readonly(&path).unwrap();
        let expected_code = if rehash {
            "APPLICATION_SCENARIO_HARD_VALIDATION_FAILED"
        } else {
            "PERSISTENCE_TIMETABLE_HASH_MISMATCH"
        };
        for revision in [ZERO, Revision::from_u64(1)] {
            assert_eq!(
                query_scenario_timetable(
                    &store,
                    parent.scenario_id,
                    revision,
                    ZERO,
                    &query(selected)
                )
                .unwrap_err()
                .code(),
                expected_code
            );
            assert_eq!(
                query_scenario_timetable_entities(
                    &store,
                    parent.scenario_id,
                    ZERO,
                    revision,
                    TimetableView::Grade,
                    0,
                    100
                )
                .unwrap_err()
                .code(),
                expected_code
            );
        }
        assert_eq!(
            query_saved_timetable(&store, &parent.origin_run_id.to_string(), &query(selected))
                .unwrap()
                .total_rows,
            6
        );
        let copied =
            query_scenario_timetable(&store, child.scenario_id, ZERO, ZERO, &query(selected))
                .unwrap();
        assert_eq!(copied, copy_before);
    }
}
