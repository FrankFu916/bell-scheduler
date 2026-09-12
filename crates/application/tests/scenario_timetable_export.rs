use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use calamine::{Data, Reader, Xlsx, open_workbook};
use class_schedule_application::{
    AdoptRunCommand, AutoSectioningPolicy, CalendarDefinition, CloneScenarioCommand,
    CsvImportAuditOptions, CsvImportMode, ImportCommitCommand, ImportCommitIntent,
    PreparedScenarioTimetableExport, ScenarioReceipt, ScenarioTimetableExportCommand,
    ScenarioTimetableExportFormat as ExportFormat, SectioningProfile, SolveOptions,
    StoredProjectSolveCommand, StoredProjectSolveMode, TimetableFilter, TimetableQuery,
    TimetableView, commit_csv_import, commit_prepared_scenario_creation,
    execute_durable_stored_project_solve, prepare_adopt_run, prepare_clone_scenario,
    prepare_scenario_timetable_export, prepare_stored_project_solve,
    publish_scenario_timetable_export, query_scenario_timetable, query_scenario_timetable_entities,
    save_prepared_solve_artifact,
};
use class_schedule_domain::{Revision, ScenarioId, SchoolProjectId};
use class_schedule_import::{CsvSource, DatasetKind};
use class_schedule_persistence::SqliteStore;
use solver_client::{CancellationToken, SidecarSpec, SolverClient};

const ZERO: Revision = Revision::INITIAL;

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
    files: &BTreeMap<DatasetKind, Vec<u8>>,
) -> (SchoolProjectId, String) {
    let project_id = SchoolProjectId::new_v4();
    let policy =
        AutoSectioningPolicy::new(1, 2, 4, 99, SectioningProfile::Balanced, candidates).unwrap();

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
    saved_scenario_files(store, unsectioned, &files(unsectioned))
}

fn saved_scenario_files(
    store: &mut SqliteStore,
    unsectioned: bool,
    files: &BTreeMap<DatasetKind, Vec<u8>>,
) -> ScenarioReceipt {
    saved_scenario_files_mode(store, unsectioned, files, "cancel-after-feasible")
}

fn saved_scenario_files_mode(
    store: &mut SqliteStore,
    unsectioned: bool,
    files: &BTreeMap<DatasetKind, Vec<u8>>,
    mode: &str,
) -> ScenarioReceipt {
    let (project_id, run_id) = saved_run(store, unsectioned, mode, 1, files);
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

fn prepare(
    store: &SqliteStore,
    receipt: &ScenarioReceipt,
    view: TimetableView,
    code: &str,
    format: ExportFormat,
) -> PreparedScenarioTimetableExport {
    prepare_scenario_timetable_export(
        store,
        &ScenarioTimetableExportCommand {
            scenario_id: receipt.scenario_id,
            expected_scenario_revision: ZERO,
            expected_timetable_revision: ZERO,
            filter: filter(store, receipt.scenario_id, view, code),
            format,
        },
    )
    .unwrap()
}

fn csv_rows(path: &Path) -> Vec<BTreeMap<String, String>> {
    let mut reader = csv::Reader::from_path(path).unwrap();
    let headers = reader.headers().unwrap().clone();
    reader
        .records()
        .map(|record| {
            headers
                .iter()
                .zip(record.unwrap().iter())
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect()
        })
        .collect()
}

#[test]
fn a_and_b_export_the_selected_students_real_meetings_and_independent_copy_identity() {
    let directory = tempfile::tempdir().unwrap();
    for unsectioned in [false, true] {
        let path = directory
            .path()
            .join(format!("source-{unsectioned}.sqlite3"));
        let mut store = SqliteStore::open(&path).unwrap();
        let parent = saved_scenario(&mut store, unsectioned);
        let child = copy_scenario(&mut store, &parent);
        drop(store);
        let before = fs::read(&path).unwrap();
        let store = SqliteStore::open_readonly(&path).unwrap();
        let selected = filter(&store, child.scenario_id, TimetableView::Student, "S1");
        let expected =
            query_scenario_timetable(&store, child.scenario_id, ZERO, ZERO, &query(selected))
                .unwrap();
        for format in [ExportFormat::Csv, ExportFormat::Xlsx] {
            let prepared = prepare(&store, &child, TimetableView::Student, "S1", format);
            let destination = directory
                .path()
                .join(format!("student-{unsectioned}.{}", format.extension()));
            assert!(!destination.exists());
            let published = publish_scenario_timetable_export(&prepared, &destination).unwrap();
            let bytes = fs::read(&published.path).unwrap();
            assert_eq!(published.metadata.receipt, child);
            assert_ne!(published.metadata.receipt.timetable_id, parent.timetable_id);
            assert_eq!(published.metadata.meeting_count, expected.total_rows);
            assert_eq!(published.metadata.byte_length, bytes.len() as u64);
            assert_eq!(
                published.metadata.payload_hash,
                blake3::hash(&bytes).to_hex().to_string()
            );
            let actual = if format == ExportFormat::Csv {
                assert!(bytes.starts_with(&[0xef, 0xbb, 0xbf]));
                csv_rows(&published.path)
                    .into_iter()
                    .map(|row| {
                        assert_eq!(row["scenario_id"], child.scenario_id.to_string());
                        assert_eq!(row["timetable_id"], child.timetable_id.to_string());
                        row["activity_id"].clone()
                    })
                    .collect::<BTreeSet<_>>()
            } else {
                let mut workbook: Xlsx<_> = open_workbook(&published.path).unwrap();
                assert_eq!(workbook.sheet_names(), ["来源说明", "周课表", "课程清单"]);
                let detail = workbook.worksheet_range("课程清单").unwrap();
                assert_eq!(detail.height(), expected.rows.len() + 1);
                detail
                    .rows()
                    .skip(1)
                    .map(|row| {
                        assert!(row.iter().all(|cell| matches!(cell, Data::String(_))));
                        row[13].to_string()
                    })
                    .collect()
            };
            assert_eq!(
                actual,
                expected
                    .rows
                    .iter()
                    .map(|row| row.activity_id.to_string())
                    .collect()
            );
        }
        drop(store);
        assert_eq!(before, fs::read(path).unwrap());
    }
}

#[test]
fn literal_formulas_chinese_quotes_and_leading_zero_codes_survive_export_safely() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("text.sqlite3")).unwrap();
    let mut files = files(false);
    let dangerous = "=SUM(1,2) \"中文\"";
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(["teacher_code", "name"]).unwrap();
    writer.write_record(["T1", dangerous]).unwrap();
    writer.write_record(["T2", "Unused teacher"]).unwrap();
    files.insert(DatasetKind::Teachers, writer.into_inner().unwrap());
    for bytes in files.values_mut() {
        *bytes = String::from_utf8(bytes.clone())
            .unwrap()
            .replace("S1", "0012")
            .into_bytes();
    }
    let receipt = saved_scenario_files(&mut store, false, &files);
    for format in [ExportFormat::Csv, ExportFormat::Xlsx] {
        let prepared = prepare(&store, &receipt, TimetableView::Student, "0012", format);
        let path = directory
            .path()
            .join(format!("literal.{}", format.extension()));
        publish_scenario_timetable_export(&prepared, &path).unwrap();
        if format == ExportFormat::Csv {
            for row in csv_rows(&path) {
                assert_eq!(row["entity_code"], "0012");
                assert_eq!(row["teacher_name"], format!("'{dangerous}"));
            }
        } else {
            let mut workbook: Xlsx<_> = open_workbook(&path).unwrap();
            let detail = workbook.worksheet_range("课程清单").unwrap();
            assert_eq!(
                detail.get_value((1, 9)),
                Some(&Data::String(dangerous.to_owned()))
            );
            for sheet in workbook.sheet_names() {
                assert!(
                    workbook
                        .worksheet_formula(&sheet)
                        .unwrap()
                        .rows()
                        .flatten()
                        .all(String::is_empty)
                );
            }
            let mut zip = zip::ZipArchive::new(fs::File::open(&path).unwrap()).unwrap();
            for index in 0..zip.len() {
                let mut file = zip.by_index(index).unwrap();
                if Path::new(file.name())
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"))
                {
                    let mut xml = String::new();
                    file.read_to_string(&mut xml).unwrap();
                    assert!(!xml.contains("<f>") && !xml.contains("<f "));
                }
            }
        }
    }
}

fn parallel_files() -> BTreeMap<DatasetKind, Vec<u8>> {
    let mut files = files(false);
    let mut students = "student_code,name,administrative_class_code\n".to_owned();
    let mut classes = "administrative_class_code,name,grade_code,home_room_code\n".to_owned();
    let mut teachers = "teacher_code,name\n".to_owned();
    let mut rooms = "room_code,name,building_code,capacity,features\n".to_owned();
    let mut choices = "student_code,subject_code\n".to_owned();
    let mut enrollments = "section_code,student_code\n".to_owned();
    for index in 1..=26 {
        writeln!(students, "S{index},Student {index},AC{index}").unwrap();
        writeln!(classes, "AC{index},Class {index},G12,R{index}").unwrap();
        writeln!(teachers, "T{index},Teacher {index}").unwrap();
        writeln!(rooms, "R{index},Room {index},B1,26,lab").unwrap();
        for (subject, section) in [
            ("physics", "PHY"),
            ("chemistry", "CHEM"),
            ("biology", "BIO"),
        ] {
            writeln!(choices, "S{index},{subject}").unwrap();
            writeln!(enrollments, "{section},S{index}").unwrap();
        }
    }
    for (kind, value) in [
        (DatasetKind::Students, students),
        (DatasetKind::AdministrativeClasses, classes),
        (DatasetKind::Teachers, teachers),
        (DatasetKind::Rooms, rooms),
        (DatasetKind::StudentSubjectChoices, choices),
        (DatasetKind::SectionEnrollments, enrollments),
    ] {
        files.insert(kind, value.into_bytes());
    }
    let mut sections = "section_code,name,grade_code,subject_code,min_size,target_size,max_size,room_policy,room_candidates,preferred_rooms,fallback_rooms,teacher_assignment,teacher_codes\n".to_owned();
    for (code, subject) in [
        ("PHY", "physics"),
        ("CHEM", "chemistry"),
        ("BIO", "biology"),
    ] {
        writeln!(
            sections,
            "{code},{subject},G12,{subject},26,26,26,section_fixed,R1,,,fixed,T1"
        )
        .unwrap();
    }
    files.insert(DatasetKind::TeachingSections, sections.into_bytes());
    let candidates = (1..=26)
        .map(|index| format!("T{index}"))
        .collect::<Vec<_>>()
        .join(";");
    let plans = String::from_utf8(files[&DatasetKind::CoursePlans].clone()).unwrap().replace(
        "P0,Chinese,G12,chinese,administrative_class,1,1,0,1,false,,fixed,T1,",
        &format!("P0,Chinese,G12,chinese,administrative_class,4,1;1;1;1,0,4,false,,candidates,{candidates},"));
    files.insert(DatasetKind::CoursePlans, plans.into_bytes());
    files
}

#[test]
fn export_includes_more_than_one_query_page_and_parallel_grid_lessons_have_separate_cells() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("parallel.sqlite3")).unwrap();
    let receipt =
        saved_scenario_files_mode(&mut store, false, &parallel_files(), "parallel-export");
    let selected = filter(&store, receipt.scenario_id, TimetableView::Grade, "G12");
    let first = query_scenario_timetable(&store, receipt.scenario_id, ZERO, ZERO, &query(selected))
        .unwrap();
    assert_eq!(first.rows.len(), 100);
    assert_eq!(first.total_rows, 107);
    assert!(first.calendar.iter().any(|cell| cell.occupied_count > 1));
    let mut expected = first.rows;
    let second = query_scenario_timetable(
        &store,
        receipt.scenario_id,
        ZERO,
        ZERO,
        &TimetableQuery {
            filter: selected,
            offset: 100,
            limit: 100,
        },
    )
    .unwrap();
    expected.extend(second.rows);
    let occupied: usize = expected
        .iter()
        .map(|row| usize::from(row.duration_periods))
        .sum();
    for format in [ExportFormat::Csv, ExportFormat::Xlsx] {
        let prepared = prepare(&store, &receipt, TimetableView::Grade, "G12", format);
        assert_eq!(prepared.metadata().meeting_count, 107);
        let path = directory
            .path()
            .join(format!("complete.{}", format.extension()));
        publish_scenario_timetable_export(&prepared, &path).unwrap();
        if format == ExportFormat::Csv {
            assert_eq!(csv_rows(&path).len(), 107);
        } else {
            let mut workbook: Xlsx<_> = open_workbook(&path).unwrap();
            assert_eq!(workbook.worksheet_range("课程清单").unwrap().height(), 108);
            let grid = workbook.worksheet_range("周课表").unwrap();
            let lessons = grid
                .rows()
                .skip(4)
                .flat_map(|row| row.iter().skip(1))
                .map(ToString::to_string)
                .filter(|text| !text.is_empty() && text != "无课程")
                .collect::<Vec<_>>();
            assert_eq!(lessons.len(), occupied);
            assert!(lessons.iter().all(|text| !text.contains("\n\n")));
            assert!(lessons.iter().any(|text| text.contains(" · 续")));
            assert!(grid.height() > 8);
        }
    }
}

#[test]
fn historical_sources_export_without_rebasing_and_wrong_revisions_fail_before_destination() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = saved_scenario(&mut store, false);
    let selected = filter(&store, receipt.scenario_id, TimetableView::Grade, "G12");
    let frozen = prepare(
        &store,
        &receipt,
        TimetableView::Grade,
        "G12",
        ExportFormat::Csv,
    );
    let mut project = store.load_project(&receipt.project_id.to_string()).unwrap();
    project.revision = 1;
    store.replace_project(0, &project).unwrap();
    let historical = prepare(
        &store,
        &receipt,
        TimetableView::Grade,
        "G12",
        ExportFormat::Xlsx,
    );
    assert!(!historical.metadata().source_is_current);
    assert_eq!(historical.metadata().receipt.source_project_revision, 0);
    let published =
        publish_scenario_timetable_export(&frozen, &directory.path().join("frozen.csv")).unwrap();
    assert!(published.metadata.source_is_current);
    assert_eq!(published.metadata.receipt, receipt);
    for (scenario, timetable, expected) in [
        (1, 0, "APPLICATION_SCENARIO_REVISION_CONFLICT"),
        (0, 1, "APPLICATION_TIMETABLE_REVISION_CONFLICT"),
    ] {
        let error = prepare_scenario_timetable_export(
            &store,
            &ScenarioTimetableExportCommand {
                scenario_id: receipt.scenario_id,
                expected_scenario_revision: Revision::from_u64(scenario),
                expected_timetable_revision: Revision::from_u64(timetable),
                filter: selected,
                format: ExportFormat::Csv,
            },
        )
        .unwrap_err();
        assert_eq!(error.code(), expected);
    }
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
}

#[test]
fn empty_object_exports_headers_and_an_empty_grid_without_invented_meetings() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("empty.sqlite3")).unwrap();
    let receipt = saved_scenario(&mut store, false);
    for format in [ExportFormat::Csv, ExportFormat::Xlsx] {
        let prepared = prepare(&store, &receipt, TimetableView::Teacher, "T2", format);
        assert_eq!(prepared.metadata().meeting_count, 0);
        let path = directory
            .path()
            .join(format!("empty.{}", format.extension()));
        publish_scenario_timetable_export(&prepared, &path).unwrap();
        if format == ExportFormat::Csv {
            assert!(csv_rows(&path).is_empty());
        } else {
            let mut workbook: Xlsx<_> = open_workbook(&path).unwrap();
            assert_eq!(workbook.worksheet_range("课程清单").unwrap().height(), 1);
            assert_eq!(
                workbook
                    .worksheet_range("周课表")
                    .unwrap()
                    .rows()
                    .skip(4)
                    .flat_map(|row| row.iter().skip(1))
                    .filter(|cell| matches!(cell, Data::String(value) if value == "无课程"))
                    .count(),
                20
            );
        }
    }
}

#[test]
fn existing_targets_bad_paths_and_competing_publishers_never_overwrite_files() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("publish.sqlite3")).unwrap();
    let receipt = saved_scenario(&mut store, false);
    let prepared = Arc::new(prepare(
        &store,
        &receipt,
        TimetableView::Grade,
        "G12",
        ExportFormat::Csv,
    ));
    let existing = directory.path().join("existing.csv");
    fs::write(&existing, b"user existing content").unwrap();
    assert_eq!(
        publish_scenario_timetable_export(&prepared, &existing)
            .unwrap_err()
            .code(),
        "APPLICATION_TIMETABLE_EXPORT_TARGET_EXISTS"
    );
    assert_eq!(fs::read(&existing).unwrap(), b"user existing content");
    for name in [
        "CON.csv",
        "LPT1.csv",
        "bad?.csv",
        ".hidden.csv",
        "bad.csv.",
        "bad.csv ",
    ] {
        let path = directory.path().join(name);
        assert_eq!(
            publish_scenario_timetable_export(&prepared, &path)
                .unwrap_err()
                .code(),
            "APPLICATION_TIMETABLE_EXPORT_PATH_INVALID"
        );
        assert!(!path.exists());
    }
    assert_eq!(
        publish_scenario_timetable_export(&prepared, &directory.path().join("wrong.xlsx"))
            .unwrap_err()
            .code(),
        "APPLICATION_TIMETABLE_EXPORT_EXTENSION_MISMATCH"
    );
    assert_eq!(
        publish_scenario_timetable_export(&prepared, &directory.path().join("missing/new.csv"))
            .unwrap_err()
            .code(),
        "APPLICATION_TIMETABLE_EXPORT_PARENT_UNAVAILABLE"
    );
    assert!(!directory.path().join("missing").exists());
    let barrier = Arc::new(Barrier::new(2));
    let target = directory.path().join("race.csv");
    let workers = (0..2)
        .map(|_| {
            let prepared = Arc::clone(&prepared);
            let barrier = Arc::clone(&barrier);
            let path = target.clone();
            std::thread::spawn(move || {
                barrier.wait();
                publish_scenario_timetable_export(&prepared, &path)
            })
        })
        .collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results.into_iter().find_map(Result::err).unwrap().code(),
        "APPLICATION_TIMETABLE_EXPORT_TARGET_EXISTS"
    );
    assert_eq!(csv_rows(&target).len(), 6);
    assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".bell-export-")
    }));
}

#[cfg(unix)]
#[test]
fn target_symlinks_are_refused_and_never_followed() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("symlink.sqlite3")).unwrap();
    let receipt = saved_scenario(&mut store, false);
    let prepared = prepare(
        &store,
        &receipt,
        TimetableView::Grade,
        "G12",
        ExportFormat::Csv,
    );
    let original = directory.path().join("original.csv");
    fs::write(&original, b"original").unwrap();
    let link = directory.path().join("link.csv");
    std::os::unix::fs::symlink(&original, &link).unwrap();
    assert_eq!(
        publish_scenario_timetable_export(&prepared, &link)
            .unwrap_err()
            .code(),
        "APPLICATION_TIMETABLE_EXPORT_TARGET_EXISTS"
    );
    assert_eq!(fs::read(original).unwrap(), b"original");
}

#[test]
fn workbook_refuses_unprintable_text_without_creating_or_overwriting_a_file() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("layout.sqlite3")).unwrap();
    let mut files = files(false);
    files.insert(
        DatasetKind::Teachers,
        format!(
            "teacher_code,name\nT1,{}\nT2,Unused teacher\n",
            "教师".repeat(500)
        )
        .into_bytes(),
    );
    let receipt = saved_scenario_files(&mut store, false, &files);
    let destination = directory.path().join("unchanged.xlsx");
    fs::write(&destination, b"existing workbook bytes").unwrap();
    let error = prepare_scenario_timetable_export(
        &store,
        &ScenarioTimetableExportCommand {
            scenario_id: receipt.scenario_id,
            expected_scenario_revision: ZERO,
            expected_timetable_revision: ZERO,
            filter: filter(&store, receipt.scenario_id, TimetableView::Grade, "G12"),
            format: ExportFormat::Xlsx,
        },
    )
    .unwrap_err();
    assert_eq!(
        error.code(),
        "APPLICATION_TIMETABLE_EXPORT_PRINT_LAYOUT_LIMIT"
    );
    assert_eq!(fs::read(&destination).unwrap(), b"existing workbook bytes");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
}
