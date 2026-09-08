use class_schedule_application::{
    AutoSectioningPolicy, CalendarDefinition, CsvImportAuditOptions, CsvImportMode,
    ImportCommandError, ImportCommitCommand, ImportCommitIntent, MAXIMUM_TABULAR_IMPORT_BYTES,
    SectioningProfile, WorkbookImportSource, audit_csv_import, audit_tabular_import,
    commit_prepared_import, load_imported_project, prepare_tabular_import_commit,
};
use class_schedule_domain::SchoolProjectId;
use class_schedule_import::{CsvSource, DatasetKind, WorkbookSheetMapping};
use class_schedule_persistence::SqliteStore;

#[path = "support/workbook_fixture.rs"]
mod workbook_fixture;

fn files() -> Vec<(DatasetKind, Vec<u8>)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/small");
    DatasetKind::ALL
        .into_iter()
        .map(|dataset| {
            (
                dataset,
                std::fs::read(root.join(format!("{}.csv", dataset.as_str()))).unwrap(),
            )
        })
        .collect()
}

fn options() -> CsvImportAuditOptions {
    CsvImportAuditOptions {
        project_stable_key: "tabular-small".to_owned(),
        calendar: CalendarDefinition::weekday_with_break(8, 4).unwrap(),
        exact_subject_choices: 3,
        mode: CsvImportMode::ExistingSections,
    }
}

fn workbook(files: &[(DatasetKind, Vec<u8>)]) -> Vec<u8> {
    workbook_fixture::from_csv(
        &files
            .iter()
            .map(|(kind, bytes)| (kind.as_str(), bytes.as_slice()))
            .collect::<Vec<_>>(),
    )
}

#[test]
fn mixed_csv_and_mapped_workbook_share_one_audit_and_atomic_commit() {
    let files = files();
    let teachers = files
        .iter()
        .find(|(kind, _)| *kind == DatasetKind::Teachers)
        .unwrap();
    let workbook = workbook_fixture::from_csv(&[("教师数据", teachers.1.as_slice())]);
    let mappings = [WorkbookSheetMapping {
        sheet_name: "教师数据".to_owned(),
        dataset: DatasetKind::Teachers,
    }];
    let workbooks = [WorkbookImportSource {
        bytes: &workbook,
        mappings: &mappings,
    }];
    let csv = files
        .iter()
        .filter(|(kind, _)| *kind != DatasetKind::Teachers)
        .map(|(kind, bytes)| CsvSource::new(*kind, bytes))
        .collect::<Vec<_>>();
    let options = options();
    let expected = audit_csv_import(
        files
            .iter()
            .map(|(kind, bytes)| CsvSource::new(*kind, bytes)),
        &options,
    )
    .unwrap();
    let actual = audit_tabular_import(csv.iter().copied(), workbooks, &options).unwrap();
    assert_eq!(
        serde_json::to_value(&actual.batch).unwrap(),
        serde_json::to_value(&expected.batch).unwrap()
    );
    assert_eq!(
        actual.candidates[0].compiled.problem,
        expected.candidates[0].compiled.problem
    );
    let command = ImportCommitCommand {
        project_id: SchoolProjectId::new_v4(),
        display_name: "Mixed format fixture".to_owned(),
        intent: ImportCommitIntent::Create,
        options,
    };
    let prepared = prepare_tabular_import_commit(&command, csv, workbooks).unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("projects.sqlite3");
    let receipt = commit_prepared_import(&mut SqliteStore::open(&path).unwrap(), prepared).unwrap();
    let loaded =
        load_imported_project(&SqliteStore::open(&path).unwrap(), command.project_id).unwrap();
    assert_eq!(receipt, loaded.receipt);
    assert_eq!(
        loaded.compiled.unwrap().problem,
        expected.candidates[0].compiled.problem
    );
}

#[test]
fn workbook_input_b_preserves_raw_choices_without_formal_sections() {
    let mut files = files();
    files.retain(|(kind, _)| {
        !matches!(
            kind,
            DatasetKind::TeachingSections
                | DatasetKind::SectionEnrollments
                | DatasetKind::CourseOfferings
                | DatasetKind::FixedActivities
        )
    });
    let workbook = workbook(&files);
    let mut options = options();
    options.mode = CsvImportMode::Unsectioned(
        AutoSectioningPolicy::new(10, 12, 16, 7, SectioningProfile::Balanced, 2).unwrap(),
    );
    let command = ImportCommitCommand {
        project_id: SchoolProjectId::new_v4(),
        display_name: "XLSX Input B".to_owned(),
        intent: ImportCommitIntent::Create,
        options,
    };
    let prepared = prepare_tabular_import_commit(
        &command,
        [],
        [WorkbookImportSource {
            bytes: &workbook,
            mappings: &[],
        }],
    )
    .unwrap();
    let mut store = SqliteStore::in_memory().unwrap();
    let receipt = commit_prepared_import(&mut store, prepared).unwrap();
    assert!(receipt.sectioning_required);
    let loaded = load_imported_project(&store, command.project_id).unwrap();
    assert!(loaded.compiled.is_none());
    assert!(loaded.document.generated_sectioning.is_none());
    assert!(loaded.document.import_batch.teaching_sections().is_empty());
    assert_eq!(
        loaded.document.import_batch.student_subject_choices().len(),
        72
    );
}

#[test]
fn malformed_workbook_and_cross_format_duplicates_fail_before_compilation() {
    let files = files();
    let csv = files
        .iter()
        .map(|(kind, bytes)| CsvSource::new(*kind, bytes))
        .collect::<Vec<_>>();
    let bad = audit_tabular_import(
        csv.iter().copied(),
        [WorkbookImportSource {
            bytes: b"CANARY bad ZIP",
            mappings: &[],
        }],
        &options(),
    )
    .unwrap_err();
    assert_eq!(bad.code(), "APPLICATION_WORKBOOK_REJECTED");
    let teachers = files
        .iter()
        .find(|(kind, _)| *kind == DatasetKind::Teachers)
        .unwrap();
    let workbook = workbook_fixture::from_csv(&[("teachers", teachers.1.as_slice())]);
    let duplicate = audit_tabular_import(
        csv,
        [WorkbookImportSource {
            bytes: &workbook,
            mappings: &[],
        }],
        &options(),
    )
    .unwrap_err();
    let ImportCommandError::Import(failure) = duplicate else {
        panic!("strict importer must reject duplicates")
    };
    assert!(
        failure
            .problems()
            .iter()
            .any(|problem| problem.code().as_str() == "IMPORT_DUPLICATE_DATASET")
    );
}

#[test]
fn aggregate_source_limit_is_checked_before_workbook_decoding() {
    let oversized = vec![0; MAXIMUM_TABULAR_IMPORT_BYTES];
    let error = audit_tabular_import(
        [CsvSource::new(DatasetKind::Teachers, &oversized)],
        [WorkbookImportSource {
            bytes: b"x",
            mappings: &[],
        }],
        &options(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ImportCommandError::ImportResourceLimit {
            stage: "source",
            ..
        }
    ));
}

#[test]
fn normalized_aggregate_limit_is_checked_before_duplicate_import_validation() {
    let value = "x".repeat(120_000);
    let mut csv = String::from("teacher_code,name\n");
    for index in 0..94 {
        use std::fmt::Write as _;
        writeln!(csv, "T{index},{value}").unwrap();
    }
    let workbook = workbook_fixture::from_csv(&[("teachers", csv.as_bytes())]);
    assert!(workbook.len() * 3 < MAXIMUM_TABULAR_IMPORT_BYTES);
    let source = WorkbookImportSource {
        bytes: &workbook,
        mappings: &[],
    };
    let error = audit_tabular_import([], [source; 3], &options()).unwrap_err();
    assert!(matches!(
        error,
        ImportCommandError::ImportResourceLimit {
            stage: "normalized",
            ..
        }
    ));
}
