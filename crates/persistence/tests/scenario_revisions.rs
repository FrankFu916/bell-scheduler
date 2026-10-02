use std::fs;
use std::sync::{Arc, Barrier};

use chrono::{DateTime, Duration, Utc};
use class_schedule_persistence::{
    CopyScenarioSource, DATABASE_SCHEMA_VERSION, ProjectDocument, ScenarioDocument,
    ScenarioRevisionExpectation, SolveArtifactDocument, SqliteStore,
};
use rusqlite::Connection;

fn instant() -> DateTime<Utc> {
    "2026-09-12T10:00:00.123Z".parse().unwrap()
}

fn source(revision: u64) -> ProjectDocument {
    ProjectDocument {
        project_id: "source".into(),
        display_name: "测试项目".into(),
        revision,
        document_schema_version: 1,
        payload: format!("{{\"source_revision\":{revision}}}").into_bytes(),
    }
}

fn artifact() -> SolveArtifactDocument {
    SolveArtifactDocument {
        run_id: "origin".into(),
        project_id: "source".into(),
        project_revision: 0,
        source_payload_hash: *source(0).payload_hash().as_bytes(),
        artifact_schema_version: 1,
        status_code: "FEASIBLE".into(),
        started_at: instant(),
        finished_at: instant(),
        payload: b"{\"opaque_application_document\":true}".to_vec(),
    }
}

fn document(revision: u64) -> ScenarioDocument {
    ScenarioDocument {
        scenario_id: "scenario".into(),
        timetable_id: "timetable".into(),
        project_id: "source".into(),
        display_name: "原始方案名称".into(),
        scenario_revision: revision,
        timetable_revision: revision,
        source_project_revision: 0,
        source_payload_hash: *source(0).payload_hash().as_bytes(),
        origin_run_id: "origin".into(),
        origin_artifact_hash: *blake3::hash(&artifact().payload).as_bytes(),
        scenario_schema_version: if revision == 0 { 1 } else { 2 },
        scenario_payload: format!("{{\"scenario_revision\":{revision}}}").into_bytes(),
        timetable_schema_version: if revision == 0 { 1 } else { 2 },
        timetable_payload: format!("{{\"timetable_revision\":{revision}}}").into_bytes(),
        created_at: instant(),
    }
}

fn expected(revision: u64) -> ScenarioRevisionExpectation {
    let document = document(revision);
    ScenarioRevisionExpectation {
        scenario_id: document.scenario_id,
        expected_scenario_revision: revision,
        expected_scenario_payload_hash: *blake3::hash(&document.scenario_payload).as_bytes(),
        expected_timetable_id: document.timetable_id,
        expected_timetable_revision: revision,
        expected_timetable_payload_hash: *blake3::hash(&document.timetable_payload).as_bytes(),
    }
}

fn fixture() -> (tempfile::TempDir, SqliteStore, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("revisions.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    store.create_project(&source(0)).unwrap();
    store.record_solve_artifact(&artifact(), &[]).unwrap();
    store.adopt_scenario(&document(0)).unwrap();
    (directory, store, Connection::open(path).unwrap())
}

fn state(connection: &Connection) -> (u64, u64, u64, String, String) {
    connection
        .query_row(
            "SELECT current_revision, (SELECT count(*) FROM scenario_revisions),
         (SELECT count(*) FROM timetable_revisions), created_at, updated_at
         FROM scenarios WHERE scenario_id = 'scenario'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap()
}

#[test]
fn append_reopens_exact_history_preserves_original_metadata_and_changes_only_revision_tables() {
    let (directory, mut store, connection) = fixture();
    let original_state = state(&connection);
    for revision in 1..=2 {
        store
            .append_scenario_revision(
                &expected(revision - 1),
                &document(revision),
                instant() + Duration::seconds(i64::try_from(revision).unwrap()),
            )
            .unwrap();
    }
    drop(store);
    let path = directory.path().join("revisions.sqlite3");
    let before = fs::read(&path).unwrap();
    let store = SqliteStore::open_readonly(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), DATABASE_SCHEMA_VERSION);
    assert_eq!(DATABASE_SCHEMA_VERSION, 4);
    for revision in 0..=2 {
        let loaded = store
            .load_scenario_revision("scenario", revision, revision)
            .unwrap();
        assert_eq!(loaded.document, document(revision));
        assert_eq!(
            loaded.revision_created_at,
            instant() + Duration::seconds(i64::try_from(revision).unwrap())
        );
        assert_eq!(
            loaded.scenario_payload_hash,
            expected(revision).expected_scenario_payload_hash
        );
        assert_eq!(
            loaded.timetable_payload_hash,
            expected(revision).expected_timetable_payload_hash
        );
    }
    assert_eq!(
        store.load_scenario("scenario").unwrap().document,
        document(2)
    );
    assert_eq!(store.load_project("source").unwrap(), source(0));
    assert_eq!(
        store.load_solve_artifact("origin").unwrap().document,
        artifact()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let after = state(&connection);
    assert_eq!((after.0, after.1, after.2), (2, 3, 3));
    assert_eq!(after.3, original_state.3);
    assert_eq!(
        after.4.parse::<DateTime<Utc>>().unwrap(),
        instant() + Duration::seconds(2)
    );
    assert_eq!(
        store.list_scenarios("source", 20, 0).unwrap()[0].created_at,
        original_state.3
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, u64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn failure_at_either_insert_or_head_update_rolls_back_the_complete_revision_pair() {
    for operation in [
        "INSERT ON scenario_revisions",
        "INSERT ON timetable_revisions",
        "UPDATE ON scenarios",
    ] {
        let (directory, mut store, connection) = fixture();
        connection.execute_batch(&format!(
            "CREATE TRIGGER reject_edit BEFORE {operation} BEGIN SELECT RAISE(ABORT, 'injected'); END;"
        )).unwrap();
        let path = directory.path().join("revisions.sqlite3");
        let before = fs::read(&path).unwrap();
        let initial = state(&connection);
        assert_eq!(
            store
                .append_scenario_revision(&expected(0), &document(1), instant())
                .unwrap_err()
                .code(),
            "PERSISTENCE_DATABASE_ERROR"
        );
        assert_eq!(state(&connection), initial);
        assert_eq!(
            store.load_scenario("scenario").unwrap().document,
            document(0)
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn head_cas_miss_after_both_inserts_rolls_back_without_publishing_rows() {
    let (directory, mut store, connection) = fixture();
    connection
        .execute_batch(
            "CREATE TRIGGER skip_head BEFORE UPDATE ON scenarios BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
    let path = directory.path().join("revisions.sqlite3");
    let before = fs::read(&path).unwrap();
    let initial = state(&connection);
    assert_eq!(
        store
            .append_scenario_revision(&expected(0), &document(1), instant())
            .unwrap_err()
            .code(),
        "PERSISTENCE_SCENARIO_REVISION_CONFLICT"
    );
    assert_eq!(state(&connection), initial);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn two_connections_with_the_same_base_publish_exactly_one_revision() {
    let (directory, store, connection) = fixture();
    drop(store);
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let path = directory.path().join("revisions.sqlite3");
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut store = SqliteStore::open(path).unwrap();
                barrier.wait();
                store
                    .append_scenario_revision(&expected(0), &document(1), instant())
                    .map_err(|error| error.code())
            })
        })
        .collect();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err("PERSISTENCE_SCENARIO_REVISION_CONFLICT"))
            .count(),
        1
    );
    let actual = state(&connection);
    assert_eq!((actual.0, actual.1, actual.2), (1, 2, 2));
}

#[test]
fn expectation_checks_both_revisions_and_payload_hashes_without_writes() {
    let (directory, mut store, connection) = fixture();
    let path = directory.path().join("revisions.sqlite3");
    let before = fs::read(&path).unwrap();
    for field in [
        "scenario_revision",
        "scenario_hash",
        "timetable_revision",
        "timetable_hash",
    ] {
        let mut expected = expected(0);
        let mut next = document(1);
        let code = match field {
            "scenario_revision" => {
                expected.expected_scenario_revision = 1;
                next.scenario_revision = 2;
                "PERSISTENCE_SCENARIO_REVISION_CONFLICT"
            }
            "scenario_hash" => {
                expected.expected_scenario_payload_hash = [9; 32];
                "PERSISTENCE_SCENARIO_HASH_MISMATCH"
            }
            "timetable_revision" => {
                expected.expected_timetable_revision = 1;
                next.timetable_revision = 2;
                "PERSISTENCE_TIMETABLE_REVISION_CONFLICT"
            }
            _ => {
                expected.expected_timetable_payload_hash = [9; 32];
                "PERSISTENCE_TIMETABLE_HASH_MISMATCH"
            }
        };
        assert_eq!(
            store
                .append_scenario_revision(&expected, &next, instant())
                .unwrap_err()
                .code(),
            code
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    assert_eq!(state(&connection).0, 0);
}

#[test]
fn immutable_source_origin_identity_name_and_creation_time_cannot_change() {
    let (_directory, mut store, connection) = fixture();
    let initial = state(&connection);
    for field in [
        "scenario_id",
        "timetable_id",
        "project_id",
        "display_name",
        "created_at",
        "source_revision",
        "source_hash",
        "origin_id",
        "origin_hash",
    ] {
        let mut next = document(1);
        match field {
            "scenario_id" => next.scenario_id = "different".into(),
            "timetable_id" => next.timetable_id = "different".into(),
            "project_id" => next.project_id = "different".into(),
            "display_name" => next.display_name = "different".into(),
            "created_at" => next.created_at += Duration::seconds(1),
            "source_revision" => next.source_project_revision = 1,
            "source_hash" => next.source_payload_hash = [9; 32],
            "origin_id" => next.origin_run_id = "different".into(),
            _ => next.origin_artifact_hash = [9; 32],
        }
        assert_eq!(
            store
                .append_scenario_revision(&expected(0), &next, instant() + Duration::seconds(2))
                .unwrap_err()
                .code(),
            "PERSISTENCE_INVALID_SCENARIO",
            "{field}"
        );
        assert_eq!(state(&connection), initial);
    }
}

#[test]
fn no_skipped_revisions_overflow_or_submillisecond_and_backward_timestamps_are_written() {
    let (directory, mut store, connection) = fixture();
    let path = directory.path().join("revisions.sqlite3");
    let before = fs::read(&path).unwrap();
    for revision in [0, 2] {
        assert_eq!(
            store
                .append_scenario_revision(&expected(0), &document(revision), instant())
                .unwrap_err()
                .code(),
            "PERSISTENCE_SCENARIO_REVISION_NOT_NEXT"
        );
    }
    for value in [i64::MAX as u64, u64::MAX] {
        for timetable in [false, true] {
            let mut expected = expected(0);
            if timetable {
                expected.expected_timetable_revision = value;
            } else {
                expected.expected_scenario_revision = value;
            }
            assert_eq!(
                store
                    .append_scenario_revision(&expected, &document(1), instant())
                    .unwrap_err()
                    .code(),
                "PERSISTENCE_INTEGER_OUT_OF_RANGE"
            );
        }
    }
    for time in [
        instant() - Duration::milliseconds(1),
        instant() + Duration::nanoseconds(1),
    ] {
        assert_eq!(
            store
                .append_scenario_revision(&expected(0), &document(1), time)
                .unwrap_err()
                .code(),
            "PERSISTENCE_SCENARIO_REVISION_TIMESTAMP_INVALID"
        );
    }
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(state(&connection).0, 0);
    store
        .append_scenario_revision(&expected(0), &document(1), instant())
        .unwrap();
    assert_eq!(
        store.load_scenario("scenario").unwrap().revision_created_at,
        instant()
    );
    store
        .append_scenario_revision(&expected(1), &document(2), instant() + Duration::seconds(2))
        .unwrap();
    assert_eq!(
        store
            .append_scenario_revision(&expected(2), &document(3), instant() + Duration::seconds(1))
            .unwrap_err()
            .code(),
        "PERSISTENCE_SCENARIO_REVISION_TIMESTAMP_INVALID"
    );
    assert_eq!(state(&connection).0, 2);
}

#[test]
fn source_replace_allows_exact_history_but_blocks_revision_append() {
    let (directory, mut store, connection) = fixture();
    let mut competitor = SqliteStore::open(directory.path().join("revisions.sqlite3")).unwrap();
    competitor.replace_project(0, &source(1)).unwrap();
    let initial = state(&connection);
    assert_eq!(
        store
            .load_scenario_revision("scenario", 0, 0)
            .unwrap()
            .document,
        document(0)
    );
    assert_eq!(
        store
            .append_scenario_revision(&expected(0), &document(1), instant())
            .unwrap_err()
            .code(),
        "PERSISTENCE_REVISION_CONFLICT"
    );
    assert_eq!(state(&connection), initial);
}

#[test]
fn source_origin_and_base_payload_bytes_are_rechecked_inside_the_append_transaction() {
    for (table, code) in [
        (
            "project_revisions",
            "PERSISTENCE_SCENARIO_SOURCE_HASH_MISMATCH",
        ),
        ("solve_artifacts", "PERSISTENCE_SCENARIO_ORIGIN_MISMATCH"),
        ("scenario_revisions", "PERSISTENCE_SCENARIO_HASH_MISMATCH"),
        ("timetable_revisions", "PERSISTENCE_TIMETABLE_HASH_MISMATCH"),
    ] {
        for rehash in [false, true] {
            let (directory, mut store, connection) = fixture();
            let payload = b"{\"externally_changed\":true}";
            connection
                .execute(
                    &format!("UPDATE {table} SET payload = ?1"),
                    [payload.as_slice()],
                )
                .unwrap();
            if rehash {
                connection
                    .execute(
                        &format!("UPDATE {table} SET payload_hash = ?1"),
                        [blake3::hash(payload).as_bytes()],
                    )
                    .unwrap();
            }
            let path = directory.path().join("revisions.sqlite3");
            let before = fs::read(&path).unwrap();
            assert_eq!(
                store
                    .append_scenario_revision(&expected(0), &document(1), instant())
                    .unwrap_err()
                    .code(),
                code
            );
            assert_eq!(fs::read(&path).unwrap(), before);
            assert_eq!(state(&connection).0, 0);
        }
    }
}

#[test]
fn exact_load_is_independent_of_current_payload_and_rejects_missing_or_mismatched_revisions() {
    let (directory, mut store, connection) = fixture();
    store
        .append_scenario_revision(&expected(0), &document(1), instant())
        .unwrap();
    connection
        .execute(
            "UPDATE scenario_revisions SET payload = X'FF' WHERE scenario_revision = 1",
            [],
        )
        .unwrap();
    let path = directory.path().join("revisions.sqlite3");
    let before = fs::read(&path).unwrap();
    assert_eq!(
        store
            .load_scenario_revision("scenario", 0, 0)
            .unwrap()
            .document,
        document(0)
    );
    assert_eq!(
        store.load_scenario("scenario").unwrap_err().code(),
        "PERSISTENCE_SCENARIO_HASH_MISMATCH"
    );
    for (scenario, revision) in [("missing", 0), ("scenario", 2)] {
        assert_eq!(
            store
                .load_scenario_revision(scenario, revision, revision)
                .unwrap_err()
                .code(),
            "PERSISTENCE_SCENARIO_REVISION_NOT_FOUND"
        );
    }
    assert_eq!(
        store
            .load_scenario_revision("scenario", 0, 1)
            .unwrap_err()
            .code(),
        "PERSISTENCE_TIMETABLE_REVISION_CONFLICT"
    );
    assert_eq!(
        store
            .load_scenario_revision("scenario", u64::MAX, 0)
            .unwrap_err()
            .code(),
        "PERSISTENCE_INTEGER_OUT_OF_RANGE"
    );
    assert_eq!(
        store
            .load_scenario_revision("scenario", 0, u64::MAX)
            .unwrap_err()
            .code(),
        "PERSISTENCE_INTEGER_OUT_OF_RANGE"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn revision_pair_timestamps_must_match_and_initial_creation_time_is_unchanged() {
    let (_directory, mut store, connection) = fixture();
    store
        .append_scenario_revision(&expected(0), &document(1), instant() + Duration::seconds(1))
        .unwrap();
    connection
        .execute(
            "UPDATE timetable_revisions SET created_at = ?1 WHERE timetable_revision = 1",
            [instant().to_rfc3339()],
        )
        .unwrap();
    assert_eq!(
        store.load_scenario("scenario").unwrap_err().code(),
        "PERSISTENCE_SCENARIO_RELATION_MISMATCH"
    );
    connection
        .execute(
            "UPDATE scenarios SET created_at = ?1",
            [(instant() - Duration::seconds(1)).to_rfc3339()],
        )
        .unwrap();
    assert_eq!(
        store
            .load_scenario_revision("scenario", 0, 0)
            .unwrap_err()
            .code(),
        "PERSISTENCE_SCENARIO_RELATION_MISMATCH"
    );
}

#[test]
fn large_exact_revisions_increment_once_without_float_rounding() {
    let (_directory, mut store, connection) = fixture();
    let large = 9_007_199_254_740_993_u64;
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection
        .execute("UPDATE scenarios SET current_revision = ?1", [large])
        .unwrap();
    connection
        .execute(
            "UPDATE scenario_revisions SET scenario_revision = ?1, timetable_revision = ?1",
            [large],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE timetable_revisions SET scenario_revision = ?1, timetable_revision = ?1",
            [large],
        )
        .unwrap();
    let mut expected = expected(0);
    expected.expected_scenario_revision = large;
    expected.expected_timetable_revision = large;
    let mut next = document(1);
    next.scenario_revision = large + 1;
    next.timetable_revision = large + 1;
    store
        .append_scenario_revision(&expected, &next, instant())
        .unwrap();
    assert_eq!(
        store
            .load_scenario_revision("scenario", large, large)
            .unwrap()
            .document
            .scenario_revision,
        large
    );
    assert_eq!(store.load_scenario("scenario").unwrap().document, next);
    assert_eq!(state(&connection).0, large + 1);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, u64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn a_nonzero_parent_revision_can_be_copied_with_independent_initial_metadata() {
    let (_directory, mut store, connection) = fixture();
    store
        .append_scenario_revision(&expected(0), &document(1), instant() + Duration::seconds(1))
        .unwrap();
    let base = expected(1);
    let parent = CopyScenarioSource {
        scenario_id: base.scenario_id,
        expected_scenario_revision: base.expected_scenario_revision,
        expected_scenario_payload_hash: base.expected_scenario_payload_hash,
        expected_timetable_id: base.expected_timetable_id,
        expected_timetable_revision: base.expected_timetable_revision,
        expected_timetable_payload_hash: base.expected_timetable_payload_hash,
    };
    let mut copy = document(1);
    copy.scenario_id = "copy".into();
    copy.timetable_id = "copy-timetable".into();
    copy.scenario_revision = 0;
    copy.timetable_revision = 0;
    copy.display_name = "独立副本".into();
    copy.created_at = instant() + Duration::seconds(2);
    store.copy_scenario(&parent, &copy).unwrap();
    let loaded = store.load_scenario_revision("copy", 0, 0).unwrap();
    assert_eq!(loaded.document, copy);
    assert_eq!(loaded.revision_created_at, copy.created_at);
    connection
        .execute("UPDATE scenario_revisions SET payload = X'FF' WHERE scenario_id = 'scenario' AND scenario_revision = 1", [])
        .unwrap();
    assert_eq!(store.load_scenario("copy").unwrap().document, copy);
}

#[test]
fn invalid_append_is_rejected_before_attempting_a_write_transaction() {
    let (directory, mut store, connection) = fixture();
    let path = directory.path().join("revisions.sqlite3");
    let before = fs::read(&path).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut invalid = document(1);
    invalid.scenario_payload = b"{broken".to_vec();
    assert_eq!(
        store
            .append_scenario_revision(&expected(0), &invalid, instant())
            .unwrap_err()
            .code(),
        "PERSISTENCE_INVALID_JSON_PAYLOAD"
    );
    let mut overflow = expected(0);
    overflow.expected_scenario_revision = u64::MAX;
    assert_eq!(
        store
            .append_scenario_revision(&overflow, &document(1), instant())
            .unwrap_err()
            .code(),
        "PERSISTENCE_INTEGER_OUT_OF_RANGE"
    );
    connection.execute_batch("ROLLBACK").unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
}
