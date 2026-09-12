use std::fs;

use class_schedule_application::{
    ScenarioReceipt, TimetableEntityOption, TimetableFilter, TimetableGridCell,
    TimetableQueryError, TimetableView,
};
use class_schedule_domain::Day;
use rusqlite::{Connection, params};
use serde_json::json;

use super::*;

const SCENARIO_ID: &str = "11111111-1111-4111-8111-111111111111";
const ENTITY_ID: &str = "22222222-2222-4222-8222-222222222222";
const TIMETABLE_ID: &str = "33333333-3333-4333-8333-333333333333";

fn entities_request() -> ScenarioTimetableEntitiesRequest {
    ScenarioTimetableEntitiesRequest {
        schema_version: 1,
        scenario_id: SCENARIO_ID.into(),
        expected_scenario_revision: "0".into(),
        expected_timetable_revision: "0".into(),
        view: TimetableViewDto::Student,
        offset: 0,
        limit: 20,
    }
}

fn timetable_request() -> ScenarioTimetableRequest {
    ScenarioTimetableRequest {
        schema_version: 1,
        scenario_id: SCENARIO_ID.into(),
        expected_scenario_revision: "0".into(),
        expected_timetable_revision: "0".into(),
        view: TimetableViewDto::Student,
        entity_id: ENTITY_ID.into(),
        offset: 0,
        limit: 20,
    }
}

#[test]
fn scenario_requests_preserve_large_revisions_and_all_seven_explicit_views() {
    let identity =
        decode_identity(1, SCENARIO_ID, "9007199254740993", "9223372036854775807").unwrap();
    assert_eq!(identity.scenario_id.to_string(), SCENARIO_ID);
    assert_eq!(identity.scenario_revision.to_string(), "9007199254740993");
    assert_eq!(
        identity.timetable_revision.to_string(),
        "9223372036854775807"
    );
    for view in [
        TimetableViewDto::AdministrativeClass,
        TimetableViewDto::TeachingSection,
        TimetableViewDto::Teacher,
        TimetableViewDto::Room,
        TimetableViewDto::Student,
        TimetableViewDto::Subject,
        TimetableViewDto::Grade,
    ] {
        let filter = decode_filter(view, ENTITY_ID).unwrap();
        assert_eq!(TimetableViewDto::from(filter.view()), view);
    }
}

#[test]
fn requests_reject_unknown_fields_numeric_revisions_and_missing_selection() {
    let input = json!({
        "schemaVersion": 1, "scenarioId": SCENARIO_ID,
        "expectedScenarioRevision": "0", "expectedTimetableRevision": "0",
        "view": "student", "entityId": ENTITY_ID, "offset": 0, "limit": 20,
    });
    assert!(serde_json::from_value::<ScenarioTimetableRequest>(input.clone()).is_ok());
    for field in [
        "runId",
        "databasePath",
        "workerPath",
        "assignments",
        "adopted",
    ] {
        let mut injected = input.clone();
        injected[field] = json!("CANARY_UNTRUSTED");
        assert!(serde_json::from_value::<ScenarioTimetableRequest>(injected).is_err());
    }
    for field in ["expectedScenarioRevision", "expectedTimetableRevision"] {
        let mut numeric = input.clone();
        numeric[field] = json!(0);
        assert!(serde_json::from_value::<ScenarioTimetableRequest>(numeric).is_err());
    }
    let mut entities = input;
    entities.as_object_mut().unwrap().remove("entityId");
    assert!(serde_json::from_value::<ScenarioTimetableRequest>(entities.clone()).is_err());
    assert!(serde_json::from_value::<ScenarioTimetableEntitiesRequest>(entities.clone()).is_ok());
    entities["databasePath"] = json!("CANARY_UNTRUSTED");
    assert!(serde_json::from_value::<ScenarioTimetableEntitiesRequest>(entities).is_err());
}

#[test]
fn invalid_requests_are_rejected_before_any_database_or_directory_is_created() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("uncreated/projects.sqlite3");
    for revision in ["", "00", "+1", "-1", "1.0", " 1", "9223372036854775808"] {
        let mut request = entities_request();
        request.expected_scenario_revision = revision.into();
        assert_eq!(
            query_entities_inner(&request, &path).unwrap_err().code,
            "DESKTOP_INVALID_EXPECTED_REVISION"
        );
        let mut request = timetable_request();
        request.expected_timetable_revision = revision.into();
        assert_eq!(
            query_timetable_inner(&request, &path).unwrap_err().code,
            "DESKTOP_INVALID_EXPECTED_REVISION"
        );
    }
    for (offset, limit) in [(0, 0), (0, 101), (u32::MAX, 1)] {
        let mut request = entities_request();
        request.offset = offset;
        request.limit = limit;
        assert_eq!(
            query_entities_inner(&request, &path).unwrap_err().code,
            "APPLICATION_TIMETABLE_INVALID_PAGE"
        );
        let mut request = timetable_request();
        request.offset = offset;
        request.limit = limit;
        assert_eq!(
            query_timetable_inner(&request, &path).unwrap_err().code,
            "APPLICATION_TIMETABLE_INVALID_PAGE"
        );
    }
    let mut request = timetable_request();
    request.entity_id.clear();
    assert_eq!(
        query_timetable_inner(&request, &path).unwrap_err().code,
        "DESKTOP_TIMETABLE_ENTITY_ID_INVALID"
    );
    let mut request = entities_request();
    request.scenario_id = "CANARY_INVALID_ID".into();
    assert_eq!(
        query_entities_inner(&request, &path).unwrap_err().code,
        "DESKTOP_SCENARIO_INVALID_ID"
    );
    request.schema_version = 2;
    assert!(query_entities_inner(&request, &path).is_err());
    assert!(!path.parent().unwrap().exists());
}

#[test]
fn missing_database_and_nonregular_paths_are_rejected_without_creating_a_store() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing/projects.sqlite3");
    assert_eq!(
        query_entities_inner(&entities_request(), &path)
            .unwrap_err()
            .code,
        "DESKTOP_DATABASE_NOT_FOUND"
    );
    assert_eq!(
        query_timetable_inner(&timetable_request(), directory.path())
            .unwrap_err()
            .code,
        "DESKTOP_DATABASE_PATH_INVALID"
    );
    assert!(!path.parent().unwrap().exists());
}

#[test]
fn actual_sqlite_missing_scenario_queries_leave_database_bytes_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("queries.sqlite3");
    drop(SqliteStore::open(&path).unwrap());
    let before = fs::read(&path).unwrap();
    for error in [
        query_entities_inner(&entities_request(), &path).unwrap_err(),
        query_timetable_inner(&timetable_request(), &path).unwrap_err(),
    ] {
        assert_eq!(error.code, "PERSISTENCE_SCENARIO_NOT_FOUND");
        assert!(error.message.contains("没有找到"));
    }
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn database_symlinks_are_rejected_before_opening_their_target() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target.sqlite3");
    let link = directory.path().join("link.sqlite3");
    drop(SqliteStore::open(&target).unwrap());
    let before = fs::read(&target).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(
        query_entities_inner(&entities_request(), &link)
            .unwrap_err()
            .code,
        "DESKTOP_DATABASE_PATH_INVALID"
    );
    assert_eq!(fs::read(&target).unwrap(), before);
}

#[test]
fn incompatible_database_schema_is_reported_without_migration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("schema.sqlite3");
    drop(SqliteStore::open(&path).unwrap());
    for (version, expected_code) in [
        (3, "PERSISTENCE_SCHEMA_UPGRADE_REQUIRED"),
        (5, "PERSISTENCE_UNSUPPORTED_SCHEMA"),
    ] {
        let connection = Connection::open(&path).unwrap();
        connection
            .execute("UPDATE schema_metadata SET version = ?1", [version])
            .unwrap();
        drop(connection);
        let before = fs::read(&path).unwrap();
        let error = query_entities_inner(&entities_request(), &path).unwrap_err();
        assert_eq!(error.code, expected_code);
        assert!(!error.message.contains("{}"));
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn actual_sqlite_damaged_payload_is_rejected_without_leaking_or_rewriting_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("damaged.sqlite3");
    drop(SqliteStore::open(&path).unwrap());
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    let now = "2026-09-08T00:00:00+00:00";
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
    let before = fs::read(&path).unwrap();
    for error in [
        query_entities_inner(&entities_request(), &path).unwrap_err(),
        query_timetable_inner(&timetable_request(), &path).unwrap_err(),
    ] {
        assert_eq!(error.code, "PERSISTENCE_SCENARIO_HASH_MISMATCH");
        let wire = serde_json::to_string(&error).unwrap();
        assert!(!wire.contains("CANARY"));
        assert!(!wire.contains("SELECT"));
        assert!(!error.message.contains("{}"));
    }
    assert_eq!(fs::read(&path).unwrap(), before);
}

fn receipt() -> ScenarioReceipt {
    ScenarioReceipt {
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
        created_at: "2026-09-08T00:00:00+00:00".parse().unwrap(),
    }
}

fn entity() -> TimetableEntityOption {
    TimetableEntityOption {
        filter: TimetableFilter::Student(ENTITY_ID.parse().unwrap()),
        code: "S001".into(),
        label: "合成学生".into(),
    }
}

#[test]
fn scenario_dtos_preserve_identity_history_large_integers_and_shared_grid_semantics() {
    let entities = ScenarioTimetableEntitiesResponse::from(ScenarioTimetableEntityPage {
        schema_version: 1,
        receipt: receipt(),
        source_is_current: false,
        scenario_display_name: "历史方案".into(),
        view: TimetableView::Student,
        entities: vec![entity()],
        total_entities: 2,
        offset: 0,
        has_more: true,
        next_offset: Some(1),
    });
    let page = ScenarioTimetableResponse::from(ScenarioTimetablePage {
        schema_version: 1,
        receipt: receipt(),
        source_is_current: false,
        scenario_display_name: "历史方案".into(),
        selection: entity(),
        rows: Vec::new(),
        calendar: vec![TimetableGridCell {
            timeslot_index: 0,
            day: Day::Monday,
            day_label: "周一".into(),
            period_index: 1,
            period_label: "第一节".into(),
            instructional_block: 0,
            occupied_count: 2,
            page_activity_ids: Vec::new(),
        }],
        total_rows: 2,
        offset: 2,
        has_more: false,
        next_offset: None,
        quality: serde_json::from_value(json!({"tiers": [{
            "id": "quality", "priority": 1, "value": 9_007_199_254_740_993_i64,
            "metrics": [{"kind": "teacher_gaps", "raw_value": 9_007_199_254_740_993_i64,
                "weight_within_tier": 1, "weighted_value": 9_007_199_254_740_993_i64}],
        }]}))
        .unwrap(),
    });
    let entities = serde_json::to_value(entities).unwrap();
    let page = serde_json::to_value(page).unwrap();
    for wire in [&entities, &page] {
        assert_eq!(wire["schemaVersion"], 1);
        assert_eq!(wire["receipt"]["schemaVersion"], 1);
        assert_eq!(wire["receipt"]["scenarioId"], SCENARIO_ID);
        assert_eq!(wire["receipt"]["timetableId"], TIMETABLE_ID);
        assert_eq!(wire["receipt"]["sourceProjectRevision"], "9007199254740993");
        assert_eq!(wire["receipt"]["scenarioRevision"], "9007199254740995");
        assert_eq!(wire["receipt"]["timetableRevision"], "9223372036854775807");
        assert_eq!(wire["sourceIsCurrent"], false);
        assert_eq!(wire["scenarioDisplayName"], "历史方案");
        assert!(wire.get("runId").is_none());
        assert!(wire.get("adopted").is_none());
    }
    assert_eq!(entities["view"], "student");
    assert_eq!(entities["entities"][0]["id"], ENTITY_ID);
    assert_eq!(page["calendar"][0]["occupiedCount"], 2);
    assert_eq!(page["calendar"][0]["day"], "monday");
    assert_eq!(page["calendar"][0]["pageActivityIds"], json!([]));
    assert_eq!(page["quality"][0]["value"], "9007199254740993");
    assert_eq!(
        page["quality"][0]["metrics"][0]["rawValue"],
        "9007199254740993"
    );
    assert_eq!(
        page["quality"][0]["metrics"][0]["weightedValue"],
        "9007199254740993"
    );
}

#[test]
fn revision_conflicts_and_selection_errors_keep_stable_codes_and_scenario_messages() {
    for error in [
        ScenarioTimetableQueryError::ScenarioRevisionConflict {
            expected: Revision::from_u64(0),
            actual: Revision::from_u64(1),
        },
        ScenarioTimetableQueryError::TimetableRevisionConflict {
            expected: Revision::from_u64(0),
            actual: Revision::from_u64(1),
        },
    ] {
        let wire = query_error(&error);
        assert_eq!(wire.code, error.code());
        assert!(wire.message.contains("版本已变化"));
        assert!(!wire.message.contains("运行"));
    }
    let error = query_error(
        &TimetableQueryError::EntityNotFound {
            view: TimetableView::Student,
        }
        .into(),
    );
    assert_eq!(error.code, "APPLICATION_TIMETABLE_ENTITY_NOT_FOUND");
    assert!(error.message.contains("方案"));
}
