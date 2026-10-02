use std::collections::BTreeMap;
use std::fs;

use class_schedule_application::{
    ScenarioActivityChange, ScenarioEditDiagnostic, ScenarioEditTimeslot, ScenarioReceipt,
};
use class_schedule_domain::{Day, MeetingDuration};
use class_schedule_validation::{HardProblem, HardProblemCode, ValidationReport};
use rusqlite::{Connection, params};
use serde_json::json;

use super::*;

const SCENARIO_ID: &str = "11111111-1111-4111-8111-111111111111";
const ACTIVITY_ID: &str = "22222222-2222-4222-8222-222222222222";
const TIMETABLE_ID: &str = "33333333-3333-4333-8333-333333333333";
const SLOT_ID: &str = "44444444-4444-4444-8444-444444444444";

fn preview_json() -> Value {
    json!({"schemaVersion":1,"scenarioId":SCENARIO_ID,
        "expectedScenarioRevision":"0","expectedTimetableRevision":"0",
        "operation":{"kind":"move","activityId":ACTIVITY_ID,
            "startTimeslotId":SLOT_ID,"lockAfter":false}})
}

fn preview_request() -> PreviewScenarioEditRequest {
    parse_request(preview_json()).unwrap()
}

fn commit_request() -> CommitScenarioEditRequest {
    let mut value = preview_json();
    value["expectedScenarioPayloadHash"] = json!("a".repeat(64));
    value["expectedTimetablePayloadHash"] = json!("b".repeat(64));
    parse_request(value).unwrap()
}

#[test]
fn all_four_operations_decode_stable_ids_and_preserve_large_revisions() {
    let cases = [
        (
            json!({"kind":"move","activityId":ACTIVITY_ID,"startTimeslotId":SLOT_ID,"lockAfter":true}),
            ScenarioEditOperation::Move {
                activity_id: ACTIVITY_ID.parse().unwrap(),
                start: SLOT_ID.parse().unwrap(),
                lock_after: true,
            },
        ),
        (
            json!({"kind":"swap_starts","leftActivityId":ACTIVITY_ID,"rightActivityId":TIMETABLE_ID}),
            ScenarioEditOperation::SwapStarts {
                left: ACTIVITY_ID.parse().unwrap(),
                right: TIMETABLE_ID.parse().unwrap(),
            },
        ),
        (
            json!({"kind":"lock_current","activityId":ACTIVITY_ID}),
            ScenarioEditOperation::LockCurrent {
                activity_id: ACTIVITY_ID.parse().unwrap(),
            },
        ),
        (
            json!({"kind":"unlock","activityId":ACTIVITY_ID}),
            ScenarioEditOperation::Unlock {
                activity_id: ACTIVITY_ID.parse().unwrap(),
            },
        ),
    ];
    for (operation, expected) in cases {
        let mut value = preview_json();
        value["operation"] = operation;
        value["expectedScenarioRevision"] = json!("9007199254740993");
        value["expectedTimetableRevision"] = json!("9223372036854775807");
        let request: PreviewScenarioEditRequest = parse_request(value).unwrap();
        let command = decode_command(
            request.schema_version,
            &request.scenario_id,
            &request.expected_scenario_revision,
            &request.expected_timetable_revision,
            &request.operation,
        )
        .unwrap();
        assert_eq!(command.operation, expected);
        assert_eq!(
            command.expected_scenario_revision.to_string(),
            "9007199254740993"
        );
        assert_eq!(
            command.expected_timetable_revision.to_string(),
            "9223372036854775807"
        );
    }
    let commit = decode_commit(&commit_request()).unwrap();
    assert_eq!(commit.expected_scenario_payload_hash, "a".repeat(64));
    assert_eq!(commit.expected_timetable_payload_hash, "b".repeat(64));
}

#[test]
fn untrusted_fields_numeric_revisions_and_incomplete_operations_have_safe_errors() {
    let input = preview_json();
    let mut malformed = Vec::new();
    for field in [
        "databasePath",
        "assignments",
        "validation",
        "quality",
        "receipt",
    ] {
        let mut value = input.clone();
        value[field] = json!("CANARY_PRIVATE_REQUEST");
        malformed.push(value);
    }
    for field in [
        "start",
        "activityIndex",
        "roomId",
        "teacherId",
        "durationPeriods",
    ] {
        let mut value = input.clone();
        value["operation"][field] = json!("CANARY_PRIVATE_REQUEST");
        malformed.push(value);
    }
    for field in ["expectedScenarioRevision", "expectedTimetableRevision"] {
        let mut value = input.clone();
        value[field] = json!(0);
        malformed.push(value);
    }
    for field in ["activityId", "startTimeslotId", "lockAfter"] {
        let mut value = input.clone();
        value["operation"].as_object_mut().unwrap().remove(field);
        malformed.push(value);
    }
    for value in malformed {
        let error = parse_request::<PreviewScenarioEditRequest>(value).unwrap_err();
        assert_eq!(error.code, "DESKTOP_SCENARIO_EDIT_INVALID_REQUEST");
        assert!(!serde_json::to_string(&error).unwrap().contains("CANARY"));
    }
    assert!(parse_request::<CommitScenarioEditRequest>(input).is_err());
}

#[test]
fn invalid_identity_revisions_and_hashes_are_rejected_before_database_access() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("uncreated/projects.sqlite3");
    for revision in ["", "00", "+1", "-1", " 1", "1.0", "9223372036854775808"] {
        let mut preview = preview_request();
        preview.expected_scenario_revision = revision.into();
        assert_eq!(
            preview_inner(&preview, &database).unwrap_err().code,
            "DESKTOP_INVALID_EXPECTED_REVISION"
        );
        let mut commit = commit_request();
        commit.expected_timetable_revision = revision.into();
        assert_eq!(
            commit_inner(&commit, &database).unwrap_err().code,
            "DESKTOP_INVALID_EXPECTED_REVISION"
        );
    }
    for hash in [
        String::new(),
        "A".repeat(64),
        "a".repeat(63),
        "g".repeat(64),
    ] {
        let mut request = commit_request();
        request.expected_scenario_payload_hash = hash;
        assert_eq!(
            commit_inner(&request, &database).unwrap_err().code,
            "APPLICATION_SCENARIO_INVALID_HASH"
        );
    }
    for id in ["0", "CANARY_INVALID_ID", "44444444444444448444444444444444"] {
        let mut request = preview_request();
        request.operation = ScenarioEditOperationDto::Move {
            activity_id: ACTIVITY_ID.into(),
            start_timeslot_id: id.into(),
            lock_after: false,
        };
        assert_eq!(
            preview_inner(&request, &database).unwrap_err().code,
            "DESKTOP_SCENARIO_EDIT_INVALID_ID"
        );
    }
    assert!(!database.parent().unwrap().exists());
}

fn both_errors(database: &Path) -> [CommandError; 2] {
    [
        preview_inner(&preview_request(), database).unwrap_err(),
        commit_inner(&commit_request(), database).unwrap_err(),
    ]
}

#[test]
fn missing_store_and_scenario_never_create_or_modify_database_files() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("uncreated/projects.sqlite3");
    for error in both_errors(&database) {
        assert_eq!(error.code, "DESKTOP_DATABASE_NOT_FOUND");
    }
    assert!(!database.parent().unwrap().exists());
    for error in both_errors(directory.path()) {
        assert_eq!(error.code, "DESKTOP_DATABASE_PATH_INVALID");
    }
    let database = directory.path().join("projects.sqlite3");
    drop(SqliteStore::open(&database).unwrap());
    let before = fs::read(&database).unwrap();
    for error in both_errors(&database) {
        assert_eq!(error.code, "PERSISTENCE_SCENARIO_NOT_FOUND");
        assert!(error.message.contains("没有找到"));
    }
    assert_eq!(fs::read(&database).unwrap(), before);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn incompatible_schema_is_neither_migrated_nor_replaced_by_edit_commands() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("projects.sqlite3");
    drop(SqliteStore::open(&database).unwrap());
    for (version, code) in [
        (3, "PERSISTENCE_SCHEMA_UPGRADE_REQUIRED"),
        (5, "PERSISTENCE_UNSUPPORTED_SCHEMA"),
    ] {
        let connection = Connection::open(&database).unwrap();
        connection
            .execute("UPDATE schema_metadata SET version=?1", [version])
            .unwrap();
        drop(connection);
        let before = fs::read(&database).unwrap();
        for error in both_errors(&database) {
            assert_eq!(error.code, code);
        }
        assert_eq!(fs::read(&database).unwrap(), before);
    }
}

#[cfg(unix)]
#[test]
fn edit_commands_refuse_symlink_database_without_touching_its_target() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("projects.sqlite3");
    let link = directory.path().join("link.sqlite3");
    drop(SqliteStore::open(&database).unwrap());
    let before = fs::read(&database).unwrap();
    std::os::unix::fs::symlink(&database, &link).unwrap();
    for error in both_errors(&link) {
        assert_eq!(error.code, "DESKTOP_DATABASE_PATH_INVALID");
    }
    assert_eq!(fs::read(&database).unwrap(), before);
}

#[test]
fn actual_corrupt_scenario_is_rejected_without_private_details_or_partial_writes() {
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
            params![SCENARIO_ID, TIMETABLE_ID, ACTIVITY_ID, now],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO scenario_revisions VALUES (?1,0,?2,0,?3,0,zeroblob(32),?3,
            zeroblob(32),1,?4,zeroblob(32),?5)",
            params![
                SCENARIO_ID,
                TIMETABLE_ID,
                ACTIVITY_ID,
                b"CANARY_PRIVATE_PAYLOAD".as_slice(),
                now
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO timetable_revisions VALUES (?1,0,?2,0,1,?3,zeroblob(32),?4)",
            params![TIMETABLE_ID, SCENARIO_ID, b"{}".as_slice(), now],
        )
        .unwrap();
    drop(connection);
    let before = fs::read(&database).unwrap();
    for error in both_errors(&database) {
        assert_eq!(error.code, "PERSISTENCE_SCENARIO_HASH_MISMATCH");
        let wire = serde_json::to_string(&error).unwrap();
        assert!(!wire.contains("CANARY"));
        assert!(!wire.contains("SELECT"));
        assert!(!error.message.contains("{}"));
    }
    assert_eq!(fs::read(&database).unwrap(), before);
}

fn receipt() -> ScenarioReceipt {
    ScenarioReceipt {
        project_id: ACTIVITY_ID.parse().unwrap(),
        source_project_revision: 9_007_199_254_740_993,
        source_payload_hash: "1".repeat(64),
        scenario_id: SCENARIO_ID.parse().unwrap(),
        scenario_revision: 9_007_199_254_740_995,
        scenario_payload_hash: "2".repeat(64),
        timetable_id: TIMETABLE_ID.parse().unwrap(),
        timetable_revision: 9_223_372_036_854_775_807,
        timetable_payload_hash: "3".repeat(64),
        origin_run_id: ACTIVITY_ID.parse().unwrap(),
        origin_artifact_hash: "4".repeat(64),
        created_at: "2026-09-12T00:00:00+00:00".parse().unwrap(),
    }
}

fn preview() -> ScenarioEditPreview {
    let assignment = |start: &str| {
        MeetingAssignment::new(
            start.parse().unwrap(),
            MeetingDuration::new(2).unwrap(),
            TIMETABLE_ID.parse().unwrap(),
            SCENARIO_ID.parse().unwrap(),
        )
    };
    ScenarioEditPreview {
        receipt: receipt(),
        source_is_current: false,
        status: ScenarioEditStatus::HardRejected,
        can_commit: false,
        changes: vec![ScenarioActivityChange {
            activity_id: ACTIVITY_ID.parse().unwrap(),
            before: assignment(ACTIVITY_ID),
            after: assignment(SLOT_ID),
            was_user_locked: false,
            is_user_locked: true,
        }],
        validation: ValidationReport {
            hard_problems: vec![
                HardProblem {
                    code: HardProblemCode::StudentConflict,
                    activities: Vec::new(),
                    entity_indices: BTreeMap::new(),
                    parameters: BTreeMap::new(),
                };
                101
            ],
        },
        before_quality: serde_json::from_value(json!({"tiers":[{
            "id":"quality","priority":1,"value":9_007_199_254_740_993_i64,
            "metrics":[{"kind":"teacher_gaps","raw_value":9_007_199_254_740_993_i64,
                "weight_within_tier":1,"weighted_value":9_007_199_254_740_993_i64}]
        }]}))
        .unwrap(),
        after_quality: None,
        context: ScenarioEditContext {
            activities: Vec::new(),
            calendar: vec![ScenarioEditTimeslot {
                timeslot_id: SLOT_ID.parse().unwrap(),
                day: Day::Monday,
                day_label: "周一".into(),
                period_index: 2,
                period_label: "第二节".into(),
            }],
            diagnostics: vec![
                ScenarioEditDiagnostic {
                    code: HardProblemCode::StudentConflict,
                    activity_ids: vec![ACTIVITY_ID.parse().unwrap()],
                    activities_truncated: false
                };
                100
            ],
            total_diagnostics: 101,
            diagnostics_truncated: true,
            activities_truncated: false,
        },
    }
}

#[test]
fn preview_dto_preserves_typed_assignments_receipt_quality_and_bounded_hard_evidence() {
    let wire = serde_json::to_value(ScenarioEditPreviewDto::from(preview())).unwrap();
    assert_eq!(wire["schemaVersion"], 1);
    assert_eq!(wire["receipt"]["scenarioRevision"], "9007199254740995");
    assert_eq!(wire["receipt"]["timetableRevision"], "9223372036854775807");
    assert_eq!(wire["status"], "hard_rejected");
    assert_eq!(wire["sourceIsCurrent"], false);
    assert_eq!(wire["canCommit"], false);
    assert_eq!(wire["validation"]["passed"], false);
    assert_eq!(
        wire["validation"]["hardProblems"].as_array().unwrap().len(),
        100
    );
    assert_eq!(wire["validation"]["totalHardProblems"], 101);
    assert_eq!(wire["validation"]["hardProblemsTruncated"], true);
    assert_eq!(
        wire["context"]["diagnostics"][0]["activityIds"],
        json!([ACTIVITY_ID])
    );
    assert_eq!(wire["context"]["calendar"][0]["timeslotId"], SLOT_ID);
    assert_eq!(wire["context"]["calendar"][0]["day"], "monday");
    assert!(
        wire["context"]["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("学生")
    );
    assert_eq!(wire["changes"][0]["before"]["startTimeslotId"], ACTIVITY_ID);
    assert_eq!(wire["changes"][0]["after"]["startTimeslotId"], SLOT_ID);
    assert_eq!(wire["changes"][0]["after"]["durationPeriods"], 2);
    assert_eq!(wire["changes"][0]["isUserLocked"], true);
    assert_eq!(wire["beforeQuality"][0]["value"], "9007199254740993");
    assert_eq!(
        wire["beforeQuality"][0]["metrics"][0]["rawValue"],
        "9007199254740993"
    );
    assert!(wire["afterQuality"].is_null());
    assert!(wire.get("adopted").is_none());
}

#[test]
fn valid_and_no_change_preview_and_commit_states_are_not_conflated() {
    for (status, name) in [
        (ScenarioEditStatus::Valid, "valid"),
        (ScenarioEditStatus::NoChange, "no_change"),
    ] {
        let mut value = preview();
        value.status = status;
        value.source_is_current = true;
        value.can_commit = status == ScenarioEditStatus::Valid;
        if status == ScenarioEditStatus::NoChange {
            value.changes.clear();
        }
        value.validation = ValidationReport::default();
        value.context.diagnostics.clear();
        value.context.total_diagnostics = 0;
        value.context.diagnostics_truncated = false;
        value.after_quality = Some(value.before_quality.clone());
        let wire = serde_json::to_value(ScenarioEditPreviewDto::from(value)).unwrap();
        assert_eq!(wire["status"], name);
        assert_eq!(wire["canCommit"], status == ScenarioEditStatus::Valid);
        assert_eq!(wire["validation"]["passed"], true);
        assert_eq!(wire["afterQuality"], wire["beforeQuality"]);
    }
    for (status, name) in [
        (ScenarioEditCommitStatus::Committed, "committed"),
        (ScenarioEditCommitStatus::NoChange, "no_change"),
    ] {
        let wire = serde_json::to_value(ScenarioEditCommitDto::from(ScenarioEditCommitReceipt {
            status,
            receipt: receipt(),
        }))
        .unwrap();
        assert_eq!(wire["status"], name);
        assert_eq!(wire["receipt"]["scenarioPayloadHash"], "2".repeat(64));
        assert_eq!(wire["receipt"]["timetableRevision"], "9223372036854775807");
        assert_eq!(wire.as_object().unwrap().len(), 3);
    }
}

#[test]
fn one_inflight_guard_is_shared_and_released_on_drop_and_unwind() {
    let jobs = Arc::new(ScenarioEditJobs::default());
    let permit = jobs.try_begin().unwrap();
    let other = Arc::clone(&jobs);
    let error = std::thread::spawn(move || other.try_begin().unwrap_err())
        .join()
        .unwrap();
    assert_eq!(error.code, "DESKTOP_SCENARIO_EDIT_BUSY");
    drop(permit);
    let result = std::panic::catch_unwind(|| {
        let _permit = jobs.try_begin().unwrap();
        panic!("exercise RAII release");
    });
    assert!(result.is_err());
    drop(jobs.try_begin().unwrap());
}

#[test]
fn revision_hash_hard_and_unknown_id_failures_have_stable_actionable_messages() {
    for code in [
        "APPLICATION_SCENARIO_REVISION_CONFLICT",
        "APPLICATION_TIMETABLE_REVISION_CONFLICT",
        "APPLICATION_SCENARIO_HASH_CONFLICT",
        "APPLICATION_TIMETABLE_HASH_CONFLICT",
        "APPLICATION_SCENARIO_EDIT_HARD_REJECTED",
        "APPLICATION_SCENARIO_EDIT_UNKNOWN_ACTIVITY",
        "APPLICATION_SCENARIO_EDIT_UNKNOWN_TIMESLOT",
    ] {
        let error = edit_error(&ScenarioApplicationError::Invalid { code });
        assert_eq!(error.code, code);
        assert!(!error.message.is_empty());
        assert!(!error.message.contains("{}"));
    }
}
