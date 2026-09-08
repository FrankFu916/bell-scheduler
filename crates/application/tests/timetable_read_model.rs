use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::time::Duration;

use class_schedule_application::{
    AutoSectioningPolicy, CalendarDefinition, CsvImportAuditOptions, CsvImportMode,
    ImportCommitCommand, ImportCommitIntent, SectioningProfile, SolveOptions,
    StoredProjectSolveCommand, StoredProjectSolveMode, TimetableAudience, TimetableFilter,
    TimetableQuery, TimetableView, commit_csv_import, execute_durable_stored_project_solve,
    load_imported_project, prepare_stored_project_solve, query_saved_timetable,
    query_saved_timetable_entities, save_prepared_solve_artifact,
};
use class_schedule_domain::{SchoolProjectId, StudentId};
use class_schedule_import::{CsvSource, DatasetKind};
use class_schedule_persistence::SqliteStore;
use rusqlite::Connection;
use solver_client::{CancellationToken, SidecarSpec, SolverClient};

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

fn filter(store: &SqliteStore, run_id: &str, view: TimetableView, code: &str) -> TimetableFilter {
    query_saved_timetable_entities(store, run_id, view, 0, 100)
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
fn administrative_class_includes_walking_classes_and_student_excludes_unrelated_enrollment() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("read-model.sqlite3")).unwrap();
    let (_, run_id) = saved_run(&mut store, false, "cancel-after-feasible", 1);
    let class = query_saved_timetable(
        &store,
        &run_id,
        &query(filter(
            &store,
            &run_id,
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
    let student = query_saved_timetable(
        &store,
        &run_id,
        &query(filter(&store, &run_id, TimetableView::Student, "S1")),
    )
    .unwrap();
    assert_eq!(student.total_rows, 4);
    assert!(student.rows.iter().any(|row| matches!(&row.audience, TimetableAudience::TeachingSection(section) if section.code == "PHY1")));
    assert!(!student.rows.iter().any(|row| matches!(&row.audience, TimetableAudience::TeachingSection(section) if section.code == "PHY2")));
    let json = serde_json::to_string(&class).unwrap();
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
    assert!(!class.adopted);
    assert_eq!(class.project_revision, "0");
}

#[test]
fn seven_views_have_exact_filters_and_calendar_expands_duration_without_losing_empty_cells() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("read-model.sqlite3")).unwrap();
    let (_, run_id) = saved_run(&mut store, false, "cancel-after-feasible", 1);
    for (view, code, expected) in [
        (TimetableView::AdministrativeClass, "AC1", 5),
        (TimetableView::TeachingSection, "PHY1", 1),
        (TimetableView::Teacher, "T1", 6),
        (TimetableView::Room, "R1", 6),
        (TimetableView::Student, "S1", 4),
        (TimetableView::Subject, "physics", 2),
        (TimetableView::Grade, "G12", 6),
        (TimetableView::Teacher, "T2", 0),
    ] {
        let page =
            query_saved_timetable(&store, &run_id, &query(filter(&store, &run_id, view, code)))
                .unwrap();
        assert_eq!(page.total_rows, expected, "{view:?}");
        assert_eq!(page.calendar.len(), 20);
    }
    let page = query_saved_timetable(
        &store,
        &run_id,
        &query(filter(&store, &run_id, TimetableView::Grade, "G12")),
    )
    .unwrap();
    let double = page
        .rows
        .iter()
        .find(|row| row.duration_periods == 2)
        .unwrap();
    assert_eq!(double.occupied_timeslot_indices.len(), 2);
    for index in &double.occupied_timeslot_indices {
        let cell = &page.calendar[*index as usize];
        assert_eq!(cell.day, double.day);
        assert!(cell.page_activity_ids.contains(&double.activity_id));
        assert_eq!(cell.occupied_count, 1);
    }
    assert!(page.calendar.iter().any(|cell| cell.occupied_count == 0));
    assert_eq!(
        page.calendar
            .iter()
            .map(|cell| cell.occupied_count)
            .sum::<u32>(),
        7
    );
}

#[test]
fn row_and_entity_pagination_never_turn_hidden_activities_into_empty_slots() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("read-model.sqlite3")).unwrap();
    let (_, run_id) = saved_run(&mut store, false, "cancel-after-feasible", 1);
    let selected = filter(&store, &run_id, TimetableView::Grade, "G12");
    let first = query_saved_timetable(
        &store,
        &run_id,
        &TimetableQuery {
            filter: selected,
            offset: 0,
            limit: 1,
        },
    )
    .unwrap();
    assert_eq!(first.rows.len(), 1);
    assert_eq!(first.total_rows, 6);
    assert_eq!(first.next_offset, Some(1));
    assert!(
        first
            .calendar
            .iter()
            .any(|cell| cell.occupied_count > 0 && cell.page_activity_ids.is_empty())
    );
    let last = query_saved_timetable(
        &store,
        &run_id,
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
    let students =
        query_saved_timetable_entities(&store, &run_id, TimetableView::Student, 1, 2).unwrap();
    assert_eq!(
        students
            .entities
            .iter()
            .map(|row| row.code.as_str())
            .collect::<Vec<_>>(),
        ["S2", "S3"]
    );
    assert_eq!(students.total_entities, 4);
    assert_eq!(students.next_offset, Some(3));
}

#[test]
fn unknown_or_missing_filter_and_invalid_pagination_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("read-model.sqlite3")).unwrap();
    let (_, run_id) = saved_run(&mut store, false, "cancel-after-feasible", 1);
    let missing = TimetableFilter::Student(StudentId::new_v4());
    assert_eq!(
        query_saved_timetable(&store, &run_id, &query(missing))
            .unwrap_err()
            .code(),
        "APPLICATION_TIMETABLE_ENTITY_NOT_FOUND"
    );
    for (offset, limit) in [(0, 0), (0, 101), (u32::MAX, 1)] {
        assert_eq!(
            query_saved_timetable(
                &store,
                "unused",
                &TimetableQuery {
                    filter: missing,
                    offset,
                    limit
                }
            )
            .unwrap_err()
            .code(),
            "APPLICATION_TIMETABLE_INVALID_PAGE"
        );
    }
    assert!(
        serde_json::from_str::<TimetableQuery>(
            r#"{"filter":{"view":"student"},"offset":0,"limit":10}"#
        )
        .is_err()
    );
}

#[test]
fn input_b_projects_selected_run_sections_without_materializing_source_import() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("read-model.sqlite3")).unwrap();
    let (project_id, run_id) = saved_run(&mut store, true, "cancel-after-feasible", 1);
    let sections =
        query_saved_timetable_entities(&store, &run_id, TimetableView::TeachingSection, 0, 100)
            .unwrap();
    assert!(!sections.entities.is_empty());
    let page = query_saved_timetable(&store, &run_id, &query(sections.entities[0].filter)).unwrap();
    assert_eq!(page.selected_attempt_index, Some(0));
    assert!(!page.adopted);
    assert!(
        page.rows
            .iter()
            .all(|row| matches!(row.audience, TimetableAudience::TeachingSection(_)))
    );
    let source = load_imported_project(&store, project_id).unwrap();
    assert_eq!(source.receipt.revision, 0);
    assert!(source.document.import_batch.teaching_sections().is_empty());
    assert!(source.document.generated_sectioning.is_none());
}

#[test]
fn non_success_cancelled_and_corrupt_artifacts_never_return_a_timetable() {
    let directory = tempfile::tempdir().unwrap();
    for (index, mode) in ["unknown", "timeout", "proven-infeasible", "worker-crash"]
        .into_iter()
        .enumerate()
    {
        let mut store =
            SqliteStore::open(directory.path().join(format!("failure-{index}.sqlite3"))).unwrap();
        let (_, run_id) = saved_run(&mut store, false, mode, 1);
        assert_eq!(
            query_saved_timetable_entities(&store, &run_id, TimetableView::Grade, 0, 10)
                .unwrap_err()
                .code(),
            "APPLICATION_TIMETABLE_UNAVAILABLE"
        );
    }
    let path = directory.path().join("cancelled.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let (_, run_id) = saved_run(&mut store, true, "cancel-after-feasible", 2);
    assert_eq!(
        query_saved_timetable_entities(&store, &run_id, TimetableView::Grade, 0, 10)
            .unwrap_err()
            .code(),
        "APPLICATION_TIMETABLE_UNAVAILABLE"
    );
    Connection::open(path)
        .unwrap()
        .execute(
            "UPDATE solve_artifacts SET payload = x'7b7d' WHERE run_id = ?1",
            [&run_id],
        )
        .unwrap();
    assert_eq!(
        query_saved_timetable_entities(&store, &run_id, TimetableView::Grade, 0, 10)
            .unwrap_err()
            .code(),
        "PERSISTENCE_SOLVE_ARTIFACT_HASH_MISMATCH"
    );
}
