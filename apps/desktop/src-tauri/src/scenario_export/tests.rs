use std::fs;
use std::sync::atomic::AtomicUsize;

use class_schedule_application::{ScenarioReceipt, TimetableEntityOption, TimetableFilter};
use class_schedule_persistence::SqliteStore;
use rusqlite::{Connection, params};
use serde_json::json;

use super::*;

const SCENARIO_ID: &str = "11111111-1111-4111-8111-111111111111";
const ENTITY_ID: &str = "22222222-2222-4222-8222-222222222222";
const TIMETABLE_ID: &str = "33333333-3333-4333-8333-333333333333";

fn request() -> ExportScenarioTimetableRequest {
    ExportScenarioTimetableRequest {
        schema_version: 1,
        scenario_id: SCENARIO_ID.into(),
        expected_scenario_revision: "0".into(),
        expected_timetable_revision: "0".into(),
        view: TimetableViewDto::Student,
        entity_id: ENTITY_ID.into(),
        format: ScenarioExportFormatDto::Xlsx,
    }
}

fn metadata() -> ScenarioTimetableExportMetadata {
    ScenarioTimetableExportMetadata {
        schema_version: 1,
        receipt: ScenarioReceipt {
            project_id: ENTITY_ID.parse().unwrap(),
            source_project_revision: 9_007_199_254_740_993,
            source_payload_hash: "1".repeat(64),
            scenario_id: SCENARIO_ID.parse().unwrap(),
            scenario_revision: 9_007_199_254_740_995,
            scenario_payload_hash: "2".repeat(64),
            timetable_id: TIMETABLE_ID.parse().unwrap(),
            timetable_revision: 9_223_372_036_854_775_807,
            timetable_payload_hash: "3".repeat(64),
            origin_run_id: ENTITY_ID.parse().unwrap(),
            origin_artifact_hash: "4".repeat(64),
            created_at: "2026-09-12T00:00:00+00:00".parse().unwrap(),
        },
        source_is_current: false,
        scenario_display_name: "历史方案".into(),
        selection: TimetableEntityOption {
            filter: TimetableFilter::Student(ENTITY_ID.parse().unwrap()),
            code: "S001".into(),
            label: "合成学生".into(),
        },
        format: ScenarioTimetableExportFormat::Xlsx,
        meeting_count: 101,
        generated_at: "2026-09-12T00:01:00+00:00".parse().unwrap(),
        byte_length: 9_007_199_254_740_993,
        payload_hash: "a".repeat(64),
    }
}

#[test]
fn requests_preserve_both_revisions_and_reject_paths_pagination_and_numeric_revisions() {
    let input = json!({
        "schemaVersion": 1, "scenarioId": SCENARIO_ID,
        "expectedScenarioRevision": "9007199254740993",
        "expectedTimetableRevision": "9223372036854775807",
        "view": "student", "entityId": ENTITY_ID, "format": "xlsx",
    });
    let parsed = serde_json::from_value::<ExportScenarioTimetableRequest>(input.clone()).unwrap();
    let command = decode_request(&parsed).unwrap();
    assert_eq!(
        command.expected_scenario_revision.to_string(),
        "9007199254740993"
    );
    assert_eq!(
        command.expected_timetable_revision.to_string(),
        "9223372036854775807"
    );
    for field in [
        "filePath",
        "fileName",
        "databasePath",
        "assignments",
        "offset",
        "limit",
        "rows",
    ] {
        let mut value = input.clone();
        value[field] = json!("CANARY_UNTRUSTED");
        assert!(serde_json::from_value::<ExportScenarioTimetableRequest>(value).is_err());
    }
    for field in ["expectedScenarioRevision", "expectedTimetableRevision"] {
        let mut value = input.clone();
        value[field] = json!(0);
        assert!(serde_json::from_value::<ExportScenarioTimetableRequest>(value).is_err());
    }
    for format in ["pdf", "XLSX", "", "../../file"] {
        let mut value = input.clone();
        value["format"] = json!(format);
        assert!(serde_json::from_value::<ExportScenarioTimetableRequest>(value).is_err());
    }
}

#[test]
fn invalid_request_and_missing_store_never_open_picker_or_create_directories() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("uncreated/projects.sqlite3");
    for revision in ["", "00", "-1", "+1", "1.0", " 1", "9223372036854775808"] {
        let mut input = request();
        input.expected_timetable_revision = revision.into();
        let error =
            export_with_picker(&input, &database, |_| panic!("must not show picker")).unwrap_err();
        assert_eq!(error.code, "DESKTOP_INVALID_EXPECTED_REVISION");
    }
    let error =
        export_with_picker(&request(), &database, |_| panic!("must not show picker")).unwrap_err();
    assert_eq!(error.code, "DESKTOP_DATABASE_NOT_FOUND");
    assert!(!database.parent().unwrap().exists());
}

#[test]
fn missing_scenario_in_actual_sqlite_never_opens_picker_and_does_not_write() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("projects.sqlite3");
    drop(SqliteStore::open(&database).unwrap());
    let before = fs::read(&database).unwrap();
    let error =
        export_with_picker(&request(), &database, |_| panic!("must not show picker")).unwrap_err();
    assert_eq!(error.code, "PERSISTENCE_SCENARIO_NOT_FOUND");
    assert!(error.message.contains("没有找到"));
    assert_eq!(fs::read(&database).unwrap(), before);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn damaged_sqlite_payload_is_rejected_before_picker_without_private_error_text() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("projects.sqlite3");
    drop(SqliteStore::open(&database).unwrap());
    let connection = Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    let now = "2026-09-12T00:00:00+00:00";
    connection
        .execute(
            "INSERT INTO scenarios VALUES (?1, ?2, ?3, 'CANARY_PRIVATE_NAME', 0, ?4, ?4)",
            params![SCENARIO_ID, TIMETABLE_ID, ENTITY_ID, now],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO scenario_revisions VALUES (?1, 0, ?2, 0, ?3, 0, zeroblob(32), ?3,
         zeroblob(32), 1, ?4, zeroblob(32), ?5)",
            params![
                SCENARIO_ID,
                TIMETABLE_ID,
                ENTITY_ID,
                b"CANARY_PRIVATE_PAYLOAD".as_slice(),
                now
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO timetable_revisions VALUES (?1, 0, ?2, 0, 1, ?3, zeroblob(32), ?4)",
            params![TIMETABLE_ID, SCENARIO_ID, b"{}".as_slice(), now],
        )
        .unwrap();
    drop(connection);
    let before = fs::read(&database).unwrap();
    let error =
        export_with_picker(&request(), &database, |_| panic!("must not show picker")).unwrap_err();
    assert_eq!(error.code, "PERSISTENCE_SCENARIO_HASH_MISMATCH");
    let wire = serde_json::to_string(&error).unwrap();
    assert!(!wire.contains("CANARY"));
    assert!(!wire.contains("SELECT"));
    assert!(!error.message.contains("{}"));
    assert_eq!(fs::read(&database).unwrap(), before);
}

#[test]
fn success_dto_keeps_exact_frozen_identity_full_count_and_u64_strings() {
    let metadata = metadata();
    let response = saved_response(&metadata, "Bell-完整课表.xlsx".into());
    let wire = serde_json::to_value(response).unwrap();
    assert_eq!(wire["schemaVersion"], 1);
    assert_eq!(wire["outcome"], "saved");
    assert_eq!(wire["receipt"]["scenarioId"], SCENARIO_ID);
    assert_eq!(wire["receipt"]["scenarioRevision"], "9007199254740995");
    assert_eq!(wire["receipt"]["timetableRevision"], "9223372036854775807");
    assert_eq!(wire["sourceIsCurrent"], false);
    assert_eq!(wire["selection"]["id"], ENTITY_ID);
    assert_eq!(wire["view"], "student");
    assert_eq!(wire["exportedActivityCount"], 101);
    assert_eq!(wire["byteLength"], "9007199254740993");
    assert_eq!(wire["payloadHashAlgorithm"], "blake3");
    assert_eq!(wire["payloadHash"], "a".repeat(64));
    assert_eq!(wire["format"], "xlsx");
    assert!(wire.get("runId").is_none());
    assert!(wire.get("path").is_none());
    assert!(wire.get("adopted").is_none());
    let cancelled =
        serde_json::to_value(ExportScenarioTimetableResponse::Cancelled { schema_version: 1 })
            .unwrap();
    assert_eq!(
        cancelled,
        json!({"schemaVersion": 1, "outcome": "cancelled"})
    );
}

#[test]
fn native_file_name_suggestion_is_bounded_and_sanitized_with_exact_format() {
    let mut metadata = metadata();
    metadata.scenario_display_name = format!(".. /\\:{}", "中".repeat(100));
    metadata.selection.label = "\n\0<>|*?\"学生".repeat(30);
    for format in [
        ScenarioTimetableExportFormat::Csv,
        ScenarioTimetableExportFormat::Xlsx,
    ] {
        metadata.format = format;
        let name = suggested_file_name(&metadata);
        assert!(name.len() <= 240);
        assert!(name.starts_with("Bell-"));
        assert!(name.ends_with(format.extension()));
        assert!(
            !name
                .chars()
                .any(|value| value.is_control() || "/\\:*?\"<>|".contains(value))
        );
        assert!(name.contains("s9007199254740995-t9223372036854775807"));
    }
    metadata.scenario_display_name = "😀".repeat(100);
    metadata.selection.label = "😀".repeat(100);
    metadata.receipt.scenario_revision = u64::MAX;
    metadata.receipt.timetable_revision = u64::MAX;
    let name = suggested_file_name(&metadata);
    assert!(name.len() <= 240);
    assert!(name.contains("s18446744073709551615-t18446744073709551615"));
}

#[test]
fn single_export_guard_rejects_overlap_and_releases_after_success_error_and_unwind() {
    let jobs = ScenarioExportJobs::default();
    let first = jobs.try_begin().unwrap();
    assert_eq!(
        jobs.try_begin().unwrap_err().code,
        "DESKTOP_SCENARIO_EXPORT_BUSY"
    );
    drop(first);
    let count = AtomicUsize::new(0);
    let permit = jobs.try_begin().unwrap();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                if jobs.try_begin().is_err() {
                    count.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    drop(permit);
    assert_eq!(count.load(Ordering::SeqCst), 8);
    let result: Result<(), CommandError> = (|| {
        let _permit = jobs.try_begin()?;
        Err(CommandError::new("TEST_FAILURE", "测试失败"))
    })();
    assert!(result.is_err());
    assert!(
        std::panic::catch_unwind(|| {
            let _permit = jobs.try_begin().unwrap();
            panic!("test unwinding cleanup");
        })
        .is_err()
    );
    assert!(jobs.try_begin().is_ok());
}

#[test]
fn application_save_errors_preserve_codes_without_echoing_private_paths() {
    for code in [
        "APPLICATION_TIMETABLE_EXPORT_TARGET_EXISTS",
        "APPLICATION_TIMETABLE_EXPORT_EXTENSION_MISMATCH",
        "APPLICATION_TIMETABLE_EXPORT_PARENT_UNAVAILABLE",
        "APPLICATION_TIMETABLE_EXPORT_WRITE_FAILED",
        "APPLICATION_TIMETABLE_EXPORT_PRINT_LAYOUT_LIMIT",
        "APPLICATION_SCENARIO_REVISION_CONFLICT",
        "APPLICATION_TIMETABLE_REVISION_CONFLICT",
    ] {
        let error = ScenarioTimetableExportError::Io {
            code,
            source: std::io::Error::other("CANARY_PRIVATE_PATH_AND_NAME"),
        };
        let mapped = export_error(&error);
        assert_eq!(mapped.code, code);
        assert!(!mapped.message.contains("{}"));
        assert!(!serde_json::to_string(&mapped).unwrap().contains("CANARY"));
        if code.ends_with("TARGET_EXISTS") {
            assert!(mapped.message.contains("未被覆盖"));
        }
        if code.ends_with("PRINT_LAYOUT_LIMIT") {
            assert!(mapped.message.contains("CSV"));
            assert!(mapped.message.contains("排版容量"));
        }
    }
}
