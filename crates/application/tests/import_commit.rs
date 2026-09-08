use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use class_schedule_application::{
    AutoSectioningPolicy, CalendarDefinition, CompiledSchoolProblem, CsvImportAuditOptions,
    CsvImportMode, ImportCommandError, ImportCommitCommand, ImportCommitIntent, SectioningProfile,
    SolveContext, SolveOptions, audit_csv_import, commit_csv_import, commit_prepared_import,
    compile_import_batch_with_sectioning, load_imported_project, prepare_auto_sectioning,
    prepare_csv_import_commit, prepare_solve,
};
use class_schedule_domain::SchoolProjectId;
use class_schedule_import::{CsvSource, DatasetKind};
use class_schedule_persistence::{PersistenceError, ProjectDocument, SqliteStore};
use class_schedule_validation::{HardProblemCode, static_feasibility_check};
use rusqlite::Connection;

type CsvFiles = BTreeMap<DatasetKind, Vec<u8>>;

const DATASETS: [DatasetKind; 11] = [
    DatasetKind::Students,
    DatasetKind::AdministrativeClasses,
    DatasetKind::StudentSubjectChoices,
    DatasetKind::Teachers,
    DatasetKind::TeacherUnavailability,
    DatasetKind::Rooms,
    DatasetKind::CoursePlans,
    DatasetKind::TeachingSections,
    DatasetKind::SectionEnrollments,
    DatasetKind::CourseOfferings,
    DatasetKind::FixedActivities,
];

fn fixture_files(unsectioned: bool) -> CsvFiles {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/small");
    DATASETS
        .into_iter()
        .filter(|kind| {
            !unsectioned
                || !matches!(
                    kind,
                    DatasetKind::TeachingSections
                        | DatasetKind::SectionEnrollments
                        | DatasetKind::CourseOfferings
                        | DatasetKind::FixedActivities
                )
        })
        .map(|kind| {
            let path = root.join(format!("{}.csv", kind.as_str()));
            (kind, fs::read(path).expect("checked-in small fixture"))
        })
        .collect()
}

fn sources(files: &CsvFiles) -> impl Iterator<Item = CsvSource<'_>> {
    files
        .iter()
        .map(|(&kind, bytes)| CsvSource::new(kind, bytes))
}

fn command(project_id: SchoolProjectId, intent: ImportCommitIntent) -> ImportCommitCommand {
    ImportCommitCommand {
        project_id,
        display_name: "测试高中 2026 秋季".to_owned(),
        intent,
        options: CsvImportAuditOptions {
            project_stable_key: "fixture-small".to_owned(),
            calendar: CalendarDefinition::weekday_with_break(8, 4).expect("calendar"),
            exact_subject_choices: 3,
            mode: CsvImportMode::ExistingSections,
        },
    }
}

fn policy() -> AutoSectioningPolicy {
    AutoSectioningPolicy::new(10, 12, 16, 20_260_904, SectioningProfile::Balanced, 3)
        .expect("explicit sectioning audit policy")
}

fn replace_text(files: &mut CsvFiles, kind: DatasetKind, old: &str, new: &str) {
    let csv = String::from_utf8(files[&kind].clone()).expect("UTF-8 fixture");
    assert!(csv.contains(old), "mutation must affect the real fixture");
    files.insert(kind, csv.replace(old, new).into_bytes());
}

fn row_counts(path: &Path) -> (u64, u64) {
    let connection = Connection::open(path).expect("inspection connection");
    connection
        .query_row(
            "SELECT (SELECT count(*) FROM projects), (SELECT count(*) FROM project_revisions)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("project and history counts")
}

fn snapshot_identity(compiled: &CompiledSchoolProblem) -> (Vec<u8>, [u8; 32]) {
    let bytes = serde_json::to_vec(&compiled.problem).expect("semantic snapshot JSON");
    let prepared = prepare_solve(
        &compiled.problem,
        &SolveContext {
            project_id: "fixture-small".to_owned(),
            project_revision: 0,
            scenario_id: "baseline".to_owned(),
            scenario_revision: 0,
            request_id: "import-roundtrip".to_owned(),
        },
        &SolveOptions::reproducible(20_260_904, Duration::from_secs(1)),
    )
    .expect("existing semantic solver adapter");
    assert_eq!(prepared.snapshot_hash, *blake3::hash(&bytes).as_bytes());
    (bytes, prepared.snapshot_hash)
}

fn assert_current_unchanged(store: &SqliteStore, path: &Path, initial: &ProjectDocument) {
    assert_eq!(
        store
            .load_project(&initial.project_id)
            .expect("existing project"),
        *initial,
    );
    assert_eq!(row_counts(path), (1, 1));
}

#[test]
fn input_a_commit_reopen_preserves_import_semantic_json_and_existing_blake3_hash() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.sqlite3");
    let files = fixture_files(false);
    let project_id = SchoolProjectId::new_v4();
    let command = command(project_id, ImportCommitIntent::Create);
    let audit = audit_csv_import(sources(&files), &command.options).unwrap();
    let before = snapshot_identity(&audit.candidates[0].compiled);
    let prepared = prepare_csv_import_commit(&command, sources(&files)).unwrap();
    let mut store = SqliteStore::open(&path).unwrap();
    assert_eq!(row_counts(&path), (0, 0));

    let receipt = commit_prepared_import(&mut store, prepared).unwrap();
    assert_eq!(receipt.project_id, project_id);
    assert_eq!(receipt.revision, 0);
    assert_eq!(receipt.document_schema_version, 1);
    assert!(!receipt.sectioning_required);
    assert_eq!(row_counts(&path), (1, 1));
    let stored = store.load_project(&project_id.to_string()).unwrap();
    assert_eq!(
        receipt.payload_hash,
        blake3::hash(&stored.payload).to_hex().to_string(),
    );
    drop(store);

    let reopened = SqliteStore::open(&path).unwrap();
    let loaded = load_imported_project(&reopened, project_id).unwrap();
    assert_eq!(loaded.receipt, receipt);
    assert_eq!(loaded.display_name, command.display_name);
    assert_eq!(loaded.document.import_batch, audit.batch);
    assert_eq!(loaded.document.calendar, command.options.calendar);
    assert!(loaded.document.generated_sectioning.is_none());
    let compiled = loaded
        .compiled
        .expect("Input A compiles after revalidation");
    assert_eq!(snapshot_identity(&compiled), before);
    assert_eq!(compiled.catalog, audit.candidates[0].compiled.catalog);
}

#[test]
fn input_b_commit_keeps_raw_choices_and_reproduces_candidates_only_with_explicit_policy() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.sqlite3");
    let files = fixture_files(true);
    let project_id = SchoolProjectId::new_v4();
    let mut command = command(project_id, ImportCommitIntent::Create);
    command.options.mode = CsvImportMode::Unsectioned(policy());
    let audit = audit_csv_import(sources(&files), &command.options).unwrap();
    assert_eq!(audit.candidates.len(), 3);
    let before_hashes = audit
        .sectioning
        .as_ref()
        .unwrap()
        .candidates
        .iter()
        .map(|candidate| candidate.candidate().provenance.candidate_hash)
        .collect::<Vec<_>>();
    let before_snapshots = audit
        .candidates
        .iter()
        .map(|candidate| snapshot_identity(&candidate.compiled))
        .collect::<Vec<_>>();
    let prepared = prepare_csv_import_commit(&command, sources(&files)).unwrap();
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = commit_prepared_import(&mut store, prepared).unwrap();
    assert!(receipt.sectioning_required);
    assert_eq!(receipt.revision, 0);
    drop(store);

    let reopened = SqliteStore::open(&path).unwrap();
    let loaded = load_imported_project(&reopened, project_id).unwrap();
    assert_eq!(loaded.receipt, receipt);
    assert!(loaded.compiled.is_none());
    assert!(loaded.document.generated_sectioning.is_none());
    assert_eq!(loaded.document.import_batch, audit.batch);
    assert_eq!(
        loaded.document.import_batch.student_subject_choices().len(),
        72
    );
    assert!(loaded.document.import_batch.teaching_sections().is_empty());
    assert!(
        loaded
            .document
            .import_batch
            .section_enrollments()
            .is_empty()
    );

    let regenerated = prepare_auto_sectioning(
        &loaded.document.import_batch,
        &loaded.document.project_stable_key,
        policy(),
    )
    .unwrap();
    let after_hashes = regenerated
        .candidates
        .iter()
        .map(|candidate| candidate.candidate().provenance.candidate_hash)
        .collect::<Vec<_>>();
    assert_eq!(after_hashes, before_hashes);
    let after_snapshots = regenerated
        .candidates
        .iter()
        .map(|candidate| {
            let compiled = compile_import_batch_with_sectioning(
                &loaded.document.import_batch,
                &loaded.document.calendar,
                &loaded.document.project_stable_key,
                candidate,
            )
            .unwrap();
            assert!(static_feasibility_check(&compiled.problem).is_valid());
            snapshot_identity(&compiled)
        })
        .collect::<Vec<_>>();
    assert_eq!(after_snapshots, before_snapshots);
    assert_eq!(row_counts(&path), (1, 1));
}

fn assert_preparation_failure_is_atomic(
    files: &CsvFiles,
    expected_code: &str,
) -> ImportCommandError {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let project_id = SchoolProjectId::new_v4();
    let mut command = command(project_id, ImportCommitIntent::Create);
    let create_error = commit_csv_import(&mut store, &command, sources(files)).unwrap_err();
    assert_eq!(create_error.code(), expected_code);
    assert_eq!(row_counts(&path), (0, 0));

    commit_csv_import(&mut store, &command, sources(&fixture_files(false))).unwrap();
    let initial = store.load_project(&project_id.to_string()).unwrap();
    command.intent = ImportCommitIntent::Replace {
        expected_revision: 0,
    };
    "不应被保存的替换项目名".clone_into(&mut command.display_name);
    let replace_error = commit_csv_import(&mut store, &command, sources(files)).unwrap_err();
    assert_eq!(replace_error.code(), expected_code);
    assert_current_unchanged(&store, &path, &initial);
    replace_error
}

#[test]
fn malformed_final_csv_cannot_create_a_project_or_append_a_revision() {
    let mut files = fixture_files(false);
    files
        .get_mut(&DatasetKind::FixedActivities)
        .unwrap()
        .extend_from_slice(b"malformed,short,row\n");
    let error = assert_preparation_failure_is_atomic(&files, "APPLICATION_IMPORT_REJECTED");
    assert!(matches!(error, ImportCommandError::Import(_)));
}

#[test]
fn cross_table_reference_failure_cannot_create_a_project_or_append_a_revision() {
    let mut files = fixture_files(false);
    replace_text(
        &mut files,
        DatasetKind::AdministrativeClasses,
        ",R-A1",
        ",MISSING-ROOM",
    );
    let error = assert_preparation_failure_is_atomic(&files, "APPLICATION_IMPORT_REJECTED");
    let ImportCommandError::Import(failure) = error else {
        panic!("expected a structured cross-table import failure");
    };
    assert!(failure.problems().iter().any(|problem| {
        problem.code() == class_schedule_import::ImportProblemCode::ImportMissingReference
    }));
}

#[test]
fn semantic_compile_failure_cannot_create_a_project_or_append_a_revision() {
    let mut files = fixture_files(false);
    replace_text(
        &mut files,
        DatasetKind::TeacherUnavailability,
        "T-CHN-1,周五,8",
        "T-CHN-1,周五,9",
    );
    let error =
        assert_preparation_failure_is_atomic(&files, "APPLICATION_CALENDAR_REFERENCE_NOT_FOUND");
    assert!(matches!(error, ImportCommandError::Compile(_)));
}

#[test]
fn static_hard_failure_cannot_create_a_project_or_append_a_revision() {
    let mut files = fixture_files(false);
    replace_text(
        &mut files,
        DatasetKind::Rooms,
        "B-TEACH,36,multimedia",
        "B-TEACH,1,multimedia",
    );
    let error =
        assert_preparation_failure_is_atomic(&files, "APPLICATION_IMPORT_PRECHECK_REJECTED");
    let ImportCommandError::PrecheckRejected { reports } = error else {
        panic!("expected independent static Hard reports");
    };
    assert!(reports.iter().all(|report| !report.is_valid()));
    assert!(
        reports
            .iter()
            .any(|report| { report.contains(HardProblemCode::ActivityNoEligibleRoom) })
    );
}

#[test]
fn create_and_replace_are_explicit_and_conflicts_preserve_the_winning_revision() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let files = fixture_files(false);
    let project_id = SchoolProjectId::new_v4();
    let mut command = command(
        project_id,
        ImportCommitIntent::Replace {
            expected_revision: 0,
        },
    );
    let error = commit_csv_import(&mut store, &command, sources(&files)).unwrap_err();
    assert_eq!(error.code(), "PERSISTENCE_PROJECT_NOT_FOUND");
    assert_eq!(row_counts(&path), (0, 0));

    command.intent = ImportCommitIntent::Create;
    commit_csv_import(&mut store, &command, sources(&files)).unwrap();
    let initial = store.load_project(&project_id.to_string()).unwrap();
    "不应覆盖的重复创建".clone_into(&mut command.display_name);
    let error = commit_csv_import(&mut store, &command, sources(&files)).unwrap_err();
    assert_eq!(error.code(), "PERSISTENCE_PROJECT_ALREADY_EXISTS");
    assert_current_unchanged(&store, &path, &initial);

    command.intent = ImportCommitIntent::Replace {
        expected_revision: 0,
    };
    "正确的新 revision".clone_into(&mut command.display_name);
    let receipt = commit_csv_import(&mut store, &command, sources(&files)).unwrap();
    assert_eq!(receipt.revision, 1);
    let winner = store.load_project(&project_id.to_string()).unwrap();
    "不应覆盖的过期替换".clone_into(&mut command.display_name);
    let error = commit_csv_import(&mut store, &command, sources(&files)).unwrap_err();
    assert_eq!(error.code(), "PERSISTENCE_REVISION_CONFLICT");
    assert!(matches!(
        error,
        ImportCommandError::Persistence(PersistenceError::RevisionConflict {
            project_id: conflict_id,
            expected_revision: 0,
            actual_revision: 1,
        }) if conflict_id == project_id.to_string()
    ));
    assert_eq!(store.load_project(&project_id.to_string()).unwrap(), winner);
    assert_eq!(row_counts(&path), (1, 2));
}

#[test]
fn replacement_cannot_change_the_stable_entity_id_namespace() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let files = fixture_files(false);
    let project_id = SchoolProjectId::new_v4();
    let mut command = command(project_id, ImportCommitIntent::Create);
    commit_csv_import(&mut store, &command, sources(&files)).unwrap();
    let initial = store.load_project(&project_id.to_string()).unwrap();
    command.intent = ImportCommitIntent::Replace {
        expected_revision: 0,
    };
    "different-entity-namespace".clone_into(&mut command.options.project_stable_key);

    let error = commit_csv_import(&mut store, &command, sources(&files)).unwrap_err();
    assert_eq!(error.code(), "APPLICATION_IMPORT_STABLE_KEY_MISMATCH");
    assert_current_unchanged(&store, &path, &initial);
}

#[test]
fn commit_v1_rejects_other_choice_counts_for_both_create_and_replace() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let files = fixture_files(false);
    let project_id = SchoolProjectId::new_v4();
    let mut command = command(project_id, ImportCommitIntent::Create);
    command.options.exact_subject_choices = 2;
    let error = commit_csv_import(&mut store, &command, sources(&files)).unwrap_err();
    assert_eq!(error.code(), "APPLICATION_IMPORT_CHOICE_POLICY_UNSUPPORTED");
    assert_eq!(row_counts(&path), (0, 0));

    command.options.exact_subject_choices = 3;
    commit_csv_import(&mut store, &command, sources(&files)).unwrap();
    let initial = store.load_project(&project_id.to_string()).unwrap();
    command.intent = ImportCommitIntent::Replace {
        expected_revision: 0,
    };
    command.options.exact_subject_choices = 4;
    let error = commit_csv_import(&mut store, &command, sources(&files)).unwrap_err();
    assert_eq!(error.code(), "APPLICATION_IMPORT_CHOICE_POLICY_UNSUPPORTED");
    assert_current_unchanged(&store, &path, &initial);
}

#[test]
fn two_prepared_commands_on_separate_connections_have_exactly_one_cas_winner() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let project_id = SchoolProjectId::new_v4();
    commit_csv_import(
        &mut store,
        &command(project_id, ImportCommitIntent::Create),
        sources(&fixture_files(false)),
    )
    .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let handles = ["race-first", "race-second"].map(|display_name| {
        let mut connection = SqliteStore::open(&path).unwrap();
        let mut command = command(
            project_id,
            ImportCommitIntent::Replace {
                expected_revision: 0,
            },
        );
        display_name.clone_into(&mut command.display_name);
        let mut files = fixture_files(false);
        replace_text(
            &mut files,
            DatasetKind::Teachers,
            "T-CHN-1,",
            &format!("T-CHN-1,{display_name} "),
        );
        let prepared = prepare_csv_import_commit(&command, sources(&files)).unwrap();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            (
                display_name,
                commit_prepared_import(&mut connection, prepared),
            )
        })
    });
    let results = handles.map(|handle| handle.join().expect("concurrent command panicked"));
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_err()).count(),
        1
    );
    let (_, error) = results.iter().find(|(_, result)| result.is_err()).unwrap();
    assert_eq!(
        error.as_ref().unwrap_err().code(),
        "PERSISTENCE_REVISION_CONFLICT"
    );
    assert!(matches!(
        error,
        Err(ImportCommandError::Persistence(
            PersistenceError::RevisionConflict {
                expected_revision: 0,
                actual_revision: 1,
                ..
            }
        ))
    ));
    let (winner_name, winner) = results.iter().find(|(_, result)| result.is_ok()).unwrap();
    let loaded = load_imported_project(&store, project_id).unwrap();
    assert_eq!(loaded.display_name, *winner_name);
    assert_eq!(&loaded.receipt, winner.as_ref().unwrap());
    assert_eq!(loaded.receipt.revision, 1);
    assert_eq!(row_counts(&path), (1, 2));
}

#[test]
fn sqlite_failure_after_initial_revision_insert_rolls_back_the_new_project() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_import_revision AFTER INSERT ON project_revisions
             BEGIN SELECT RAISE(ABORT, 'injected revision insert failure'); END;",
        )
        .unwrap();
    let project_id = SchoolProjectId::new_v4();
    let prepared = prepare_csv_import_commit(
        &command(project_id, ImportCommitIntent::Create),
        sources(&fixture_files(false)),
    )
    .unwrap();
    let error = commit_prepared_import(&mut store, prepared).unwrap_err();
    assert_eq!(error.code(), "PERSISTENCE_DATABASE_ERROR");
    assert!(matches!(
        error,
        ImportCommandError::Persistence(PersistenceError::Database(
            rusqlite::Error::SqliteFailure(_, Some(message)),
        )) if message == "injected revision insert failure"
    ));
    assert_eq!(row_counts(&path), (0, 0));
    assert_eq!(
        store
            .load_project(&project_id.to_string())
            .unwrap_err()
            .code(),
        "PERSISTENCE_PROJECT_NOT_FOUND",
    );
}

#[test]
fn sqlite_failures_after_revision_insert_and_project_update_roll_back_complete_replacement() {
    for (trigger, expected_message) in [
        (
            "CREATE TRIGGER reject_import_revision AFTER INSERT ON project_revisions
             BEGIN SELECT RAISE(ABORT, 'injected revision insert failure'); END;",
            "injected revision insert failure",
        ),
        (
            "CREATE TRIGGER reject_import_update AFTER UPDATE ON projects
             BEGIN SELECT RAISE(ABORT, 'injected project update failure'); END;",
            "injected project update failure",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("project.sqlite3");
        let mut store = SqliteStore::open(&path).unwrap();
        let project_id = SchoolProjectId::new_v4();
        let mut command = command(project_id, ImportCommitIntent::Create);
        let mut files = fixture_files(false);
        commit_csv_import(&mut store, &command, sources(&files)).unwrap();
        let initial = store.load_project(&project_id.to_string()).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(trigger).unwrap();
        command.intent = ImportCommitIntent::Replace {
            expected_revision: 0,
        };
        "不得留下的新名称".clone_into(&mut command.display_name);
        replace_text(
            &mut files,
            DatasetKind::Teachers,
            "T-CHN-1,",
            "T-CHN-1,不得留下的新教师名称 ",
        );
        let prepared = prepare_csv_import_commit(&command, sources(&files)).unwrap();

        let error = commit_prepared_import(&mut store, prepared).unwrap_err();
        assert_eq!(error.code(), "PERSISTENCE_DATABASE_ERROR");
        assert!(matches!(
            error,
            ImportCommandError::Persistence(PersistenceError::Database(
                rusqlite::Error::SqliteFailure(_, Some(message)),
            )) if message == expected_message
        ));
        assert_current_unchanged(&store, &path, &initial);
        drop(store);
        let reopened = SqliteStore::open(&path).unwrap();
        assert_current_unchanged(&reopened, &path, &initial);
    }
}

#[test]
fn load_revalidates_correctly_hashed_json_and_checks_row_document_schema_agreement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let project_id = SchoolProjectId::new_v4();
    commit_csv_import(
        &mut store,
        &command(project_id, ImportCommitIntent::Create),
        sources(&fixture_files(false)),
    )
    .unwrap();
    let initial = store.load_project(&project_id.to_string()).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&initial.payload).unwrap();
    let mut invalid_ordinal = original.clone();
    invalid_ordinal["import_batch"]["fixed_activities"][0]["meeting_ordinal"] =
        serde_json::json!(0);
    let mut missing_class = original.clone();
    missing_class["import_batch"]["students"][0]["administrative_class_code"] =
        serde_json::json!("MISSING-CLASS");

    for (expected_revision, payload, row_version, expected_code) in [
        (0, invalid_ordinal, 1, "APPLICATION_IMPORT_REJECTED"),
        (1, missing_class, 1, "APPLICATION_IMPORT_REJECTED"),
        (
            2,
            original,
            2,
            "APPLICATION_IMPORT_DOCUMENT_VERSION_MISMATCH",
        ),
    ] {
        let changed = ProjectDocument {
            revision: expected_revision + 1,
            document_schema_version: row_version,
            payload: serde_json::to_vec(&payload).unwrap(),
            ..initial.clone()
        };
        store.replace_project(expected_revision, &changed).unwrap();
        // This read verifies that the stored JSON and its real BLAKE3 digest agree. Application
        // loading must still reject the broken typed values, references or document version.
        assert_eq!(
            store.load_project(&project_id.to_string()).unwrap(),
            changed
        );
        assert_eq!(
            load_imported_project(&store, project_id)
                .unwrap_err()
                .code(),
            expected_code,
        );
        assert_eq!(row_counts(&path), (1, expected_revision + 2));
    }
}
