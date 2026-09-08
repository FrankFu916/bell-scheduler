use std::sync::{Arc, Barrier};

use chrono::{DateTime, Utc};
use class_schedule_persistence::{
    CopyScenarioSource, DATABASE_SCHEMA_VERSION, MAXIMUM_SCENARIO_PAYLOAD_BYTES, ProjectDocument,
    ScenarioDocument, SolveArtifactDocument, SqliteStore,
};
use rusqlite::{Connection, params};

fn instant() -> DateTime<Utc> {
    "2026-09-08T10:00:00.123Z".parse().unwrap()
}

fn source(revision: u64) -> ProjectDocument {
    ProjectDocument {
        project_id: "project-a".to_owned(),
        display_name: "测试源项目".to_owned(),
        revision,
        document_schema_version: 1,
        payload: format!("{{\"source_revision\":{revision}}}").into_bytes(),
    }
}

fn artifact() -> SolveArtifactDocument {
    SolveArtifactDocument {
        run_id: "run-a".to_owned(),
        project_id: "project-a".to_owned(),
        project_revision: 0,
        source_payload_hash: *source(0).payload_hash().as_bytes(),
        artifact_schema_version: 1,
        status_code: "FEASIBLE".to_owned(),
        started_at: instant(),
        finished_at: instant(),
        payload: b"{\"validated_by_application\":true}".to_vec(),
    }
}

fn scenario(id: &str) -> ScenarioDocument {
    ScenarioDocument {
        scenario_id: id.to_owned(),
        timetable_id: format!("timetable-{id}"),
        project_id: "project-a".to_owned(),
        display_name: format!("方案 {id}"),
        scenario_revision: 0,
        timetable_revision: 0,
        source_project_revision: 0,
        source_payload_hash: *source(0).payload_hash().as_bytes(),
        origin_run_id: "run-a".to_owned(),
        origin_artifact_hash: *blake3::hash(&artifact().payload).as_bytes(),
        scenario_schema_version: 1,
        scenario_payload: format!("{{\"scenario\":\"{id}\",\"materialized\":true}}").into_bytes(),
        timetable_schema_version: 1,
        timetable_payload: format!("{{\"timetable\":\"{id}\",\"assignments\":[1]}}").into_bytes(),
        created_at: instant(),
    }
}

fn parent(document: &ScenarioDocument) -> CopyScenarioSource {
    CopyScenarioSource {
        scenario_id: document.scenario_id.clone(),
        expected_scenario_revision: document.scenario_revision,
        expected_scenario_payload_hash: *blake3::hash(&document.scenario_payload).as_bytes(),
        expected_timetable_id: document.timetable_id.clone(),
        expected_timetable_revision: document.timetable_revision,
        expected_timetable_payload_hash: *blake3::hash(&document.timetable_payload).as_bytes(),
    }
}

fn fixture() -> (tempfile::TempDir, SqliteStore, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("scenarios.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    store.create_project(&source(0)).unwrap();
    store.record_solve_artifact(&artifact(), &[]).unwrap();
    (directory, store, Connection::open(path).unwrap())
}

fn counts(connection: &Connection) -> (u32, u32, u32) {
    connection
        .query_row(
            "SELECT (SELECT count(*) FROM scenarios), (SELECT count(*) FROM scenario_revisions),
         (SELECT count(*) FROM timetable_revisions)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

#[test]
fn adopt_reopens_complete_independent_documents_without_advancing_source_or_run() {
    let (directory, mut store, connection) = fixture();
    let document = scenario("a");
    store.adopt_scenario(&document).unwrap();
    assert_eq!(counts(&connection), (1, 1, 1));
    drop(store);
    let store = SqliteStore::open(directory.path().join("scenarios.sqlite3")).unwrap();
    let loaded = store.load_scenario("a").unwrap();
    assert_eq!(loaded.document, document);
    assert_eq!(
        loaded.scenario_payload_hash,
        *blake3::hash(&document.scenario_payload).as_bytes()
    );
    assert_eq!(
        loaded.timetable_payload_hash,
        *blake3::hash(&document.timetable_payload).as_bytes()
    );
    assert_eq!(store.load_project("project-a").unwrap(), source(0));
    assert_eq!(
        store.load_solve_artifact("run-a").unwrap().document,
        artifact()
    );
    let other_counts: (u32, u32, u32) = connection.query_row(
        "SELECT (SELECT count(*) FROM project_revisions), (SELECT count(*) FROM solve_artifacts),
         (SELECT count(*) FROM pragma_foreign_key_check)", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(other_counts, (1, 1, 0));
}

#[test]
fn duplicate_scenario_or_timetable_ids_have_stable_codes_and_never_overwrite() {
    let (_directory, mut store, connection) = fixture();
    let document = scenario("a");
    store.adopt_scenario(&document).unwrap();
    let mut duplicate = document.clone();
    duplicate.scenario_payload = b"{\"other\":true}".to_vec();
    assert_eq!(
        store.adopt_scenario(&duplicate).unwrap_err().code(),
        "PERSISTENCE_SCENARIO_ALREADY_EXISTS"
    );
    duplicate.scenario_id = "b".to_owned();
    assert_eq!(
        store.adopt_scenario(&duplicate).unwrap_err().code(),
        "PERSISTENCE_TIMETABLE_ALREADY_EXISTS"
    );
    assert_eq!(counts(&connection), (1, 1, 1));
    assert_eq!(store.load_scenario("a").unwrap().document, document);
}

#[test]
fn injected_failure_at_each_insert_rolls_back_all_three_tables() {
    for table in ["scenarios", "scenario_revisions", "timetable_revisions"] {
        let (_directory, mut store, connection) = fixture();
        connection
            .execute_batch(&format!(
                "CREATE TRIGGER inject_failure BEFORE INSERT ON {table}
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END;"
            ))
            .unwrap();
        assert_eq!(
            store.adopt_scenario(&scenario("a")).unwrap_err().code(),
            "PERSISTENCE_DATABASE_ERROR"
        );
        assert_eq!(counts(&connection), (0, 0, 0));
        assert_eq!(store.load_project("project-a").unwrap(), source(0));
        assert_eq!(
            store.load_scenario("a").unwrap_err().code(),
            "PERSISTENCE_SCENARIO_NOT_FOUND"
        );
    }
}

#[test]
fn source_revision_changed_between_preparation_and_commit_is_rejected() {
    let (directory, mut store, connection) = fixture();
    let prepared = scenario("stale");
    let mut competing = SqliteStore::open(directory.path().join("scenarios.sqlite3")).unwrap();
    competing.replace_project(0, &source(1)).unwrap();
    assert_eq!(
        store.adopt_scenario(&prepared).unwrap_err().code(),
        "PERSISTENCE_REVISION_CONFLICT"
    );
    assert_eq!(counts(&connection), (0, 0, 0));
    assert_eq!(store.load_project("project-a").unwrap(), source(1));
}

#[test]
fn source_and_origin_actual_bytes_and_recorded_hashes_are_both_compared() {
    for (table, code) in [
        (
            "project_revisions",
            "PERSISTENCE_SCENARIO_SOURCE_HASH_MISMATCH",
        ),
        ("solve_artifacts", "PERSISTENCE_SCENARIO_ORIGIN_MISMATCH"),
    ] {
        for recompute_hash in [false, true] {
            let (_directory, mut store, connection) = fixture();
            let payload = b"{\"concurrently_replaced\":true}";
            connection
                .execute(
                    &format!("UPDATE {table} SET payload = ?1"),
                    [payload.as_slice()],
                )
                .unwrap();
            if recompute_hash {
                connection
                    .execute(
                        &format!("UPDATE {table} SET payload_hash = ?1"),
                        [blake3::hash(payload).as_bytes()],
                    )
                    .unwrap();
            }
            assert_eq!(
                store.adopt_scenario(&scenario("a")).unwrap_err().code(),
                code
            );
            assert_eq!(counts(&connection), (0, 0, 0));
        }
    }
}

#[test]
fn invalid_json_versions_revisions_and_precision_fail_before_writes() {
    let (_directory, mut store, connection) = fixture();
    let original = scenario("a");
    for field in [
        "scenario_json",
        "timetable_json",
        "schema",
        "scenario_revision",
        "timetable_revision",
        "timestamp",
        "integer",
    ] {
        let mut value = original.clone();
        let code = match field {
            "scenario_json" => {
                value.scenario_payload = vec![0xff];
                "PERSISTENCE_INVALID_JSON_PAYLOAD"
            }
            "timetable_json" => {
                value.timetable_payload = b"{broken".to_vec();
                "PERSISTENCE_INVALID_JSON_PAYLOAD"
            }
            "schema" => {
                value.scenario_schema_version = 0;
                "PERSISTENCE_INVALID_SCENARIO"
            }
            "scenario_revision" => {
                value.scenario_revision = 1;
                "PERSISTENCE_INITIAL_REVISION_NOT_ZERO"
            }
            "timetable_revision" => {
                value.timetable_revision = 1;
                "PERSISTENCE_INITIAL_REVISION_NOT_ZERO"
            }
            "timestamp" => {
                value.created_at += chrono::Duration::nanoseconds(1);
                "PERSISTENCE_INVALID_SCENARIO"
            }
            _ => {
                value.source_project_revision = u64::MAX;
                "PERSISTENCE_INTEGER_OUT_OF_RANGE"
            }
        };
        assert_eq!(store.adopt_scenario(&value).unwrap_err().code(), code);
    }
    assert_eq!(counts(&connection), (0, 0, 0));
}

#[test]
fn copy_stores_its_own_payloads_and_can_reload_independently_of_parent_payload() {
    let (_directory, mut store, connection) = fixture();
    let original = scenario("a");
    store.adopt_scenario(&original).unwrap();
    let copy = scenario("copy");
    store.copy_scenario(&parent(&original), &copy).unwrap();
    assert_eq!(counts(&connection), (2, 2, 2));
    assert_eq!(store.load_scenario("copy").unwrap().document, copy);
    let payload = b"{\"external_tamper\":true}";
    connection
        .execute(
            "UPDATE scenario_revisions SET payload = ?1, payload_hash = ?2 WHERE scenario_id = 'a'",
            params![payload.as_slice(), blake3::hash(payload).as_bytes()],
        )
        .unwrap();
    connection.execute(
        "UPDATE timetable_revisions SET payload = ?1, payload_hash = ?2 WHERE scenario_id = 'a'",
        params![payload.as_slice(), blake3::hash(payload).as_bytes()],
    ).unwrap();
    assert_eq!(store.load_scenario("copy").unwrap().document, copy);
    assert_eq!(store.load_project("project-a").unwrap(), source(0));
}

#[test]
fn copy_checks_every_parent_revision_identity_and_hash() {
    let (_directory, mut store, connection) = fixture();
    let document = scenario("a");
    store.adopt_scenario(&document).unwrap();
    for field in [
        "scenario_revision",
        "scenario_hash",
        "timetable_id",
        "timetable_revision",
        "timetable_hash",
    ] {
        let mut expected = parent(&document);
        let code = match field {
            "scenario_revision" => {
                expected.expected_scenario_revision = 1;
                "PERSISTENCE_SCENARIO_REVISION_CONFLICT"
            }
            "scenario_hash" => {
                expected.expected_scenario_payload_hash = [9; 32];
                "PERSISTENCE_SCENARIO_HASH_MISMATCH"
            }
            "timetable_id" => {
                expected.expected_timetable_id = "other".to_owned();
                "PERSISTENCE_TIMETABLE_REVISION_CONFLICT"
            }
            "timetable_revision" => {
                expected.expected_timetable_revision = 1;
                "PERSISTENCE_TIMETABLE_REVISION_CONFLICT"
            }
            _ => {
                expected.expected_timetable_payload_hash = [9; 32];
                "PERSISTENCE_TIMETABLE_HASH_MISMATCH"
            }
        };
        assert_eq!(
            store
                .copy_scenario(&expected, &scenario("copy"))
                .unwrap_err()
                .code(),
            code
        );
    }
    assert_eq!(counts(&connection), (1, 1, 1));
}

#[test]
fn copy_disallows_aliases_changed_provenance_and_partial_writes() {
    let (_directory, mut store, connection) = fixture();
    let original = scenario("a");
    store.adopt_scenario(&original).unwrap();
    for same_scenario in [true, false] {
        let mut target = scenario("copy");
        if same_scenario {
            target.scenario_id.clone_from(&original.scenario_id);
        } else {
            target.timetable_id.clone_from(&original.timetable_id);
        }
        assert_eq!(
            store
                .copy_scenario(&parent(&original), &target)
                .unwrap_err()
                .code(),
            "PERSISTENCE_INVALID_SCENARIO"
        );
    }
    let mut second_artifact = artifact();
    second_artifact.run_id = "run-b".to_owned();
    store.record_solve_artifact(&second_artifact, &[]).unwrap();
    let mut target = scenario("copy");
    target.origin_run_id = "run-b".to_owned();
    assert_eq!(
        store
            .copy_scenario(&parent(&original), &target)
            .unwrap_err()
            .code(),
        "PERSISTENCE_INVALID_SCENARIO"
    );
    connection
        .execute_batch(
            "CREATE TRIGGER inject_copy_failure BEFORE INSERT ON timetable_revisions
         WHEN NEW.scenario_id = 'copy' BEGIN SELECT RAISE(ABORT, 'copy failure'); END;",
        )
        .unwrap();
    assert_eq!(
        store
            .copy_scenario(&parent(&original), &scenario("copy"))
            .unwrap_err()
            .code(),
        "PERSISTENCE_DATABASE_ERROR"
    );
    assert_eq!(counts(&connection), (1, 1, 1));
}

#[test]
fn historical_scenario_loads_after_source_replace_but_new_copy_requires_current_source() {
    let (_directory, mut store, connection) = fixture();
    let document = scenario("a");
    store.adopt_scenario(&document).unwrap();
    store.replace_project(0, &source(1)).unwrap();
    assert_eq!(store.load_scenario("a").unwrap().document, document);
    assert_eq!(
        store
            .copy_scenario(&parent(&document), &scenario("copy"))
            .unwrap_err()
            .code(),
        "PERSISTENCE_REVISION_CONFLICT"
    );
    assert_eq!(counts(&connection), (1, 1, 1));
}

#[test]
fn load_rejects_bad_json_even_with_correct_hash_and_payload_corruption() {
    for (table, hash_code) in [
        ("scenario_revisions", "PERSISTENCE_SCENARIO_HASH_MISMATCH"),
        ("timetable_revisions", "PERSISTENCE_TIMETABLE_HASH_MISMATCH"),
    ] {
        let (_directory, mut store, connection) = fixture();
        store.adopt_scenario(&scenario("a")).unwrap();
        connection
            .execute(
                &format!("UPDATE {table} SET payload = ?1"),
                [b"{broken".as_slice()],
            )
            .unwrap();
        assert_eq!(store.load_scenario("a").unwrap_err().code(), hash_code);
        connection
            .execute(
                &format!("UPDATE {table} SET payload_hash = ?1"),
                [blake3::hash(b"{broken").as_bytes()],
            )
            .unwrap();
        assert_eq!(
            store.load_scenario("a").unwrap_err().code(),
            "PERSISTENCE_INVALID_JSON_PAYLOAD"
        );
    }
}

#[test]
fn read_checks_foreign_key_links_even_after_external_fk_bypass() {
    for table in [
        "project_revisions",
        "solve_artifacts",
        "scenario_revisions",
        "timetable_revisions",
    ] {
        let (_directory, mut store, connection) = fixture();
        store.adopt_scenario(&scenario("a")).unwrap();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        connection
            .execute(&format!("DELETE FROM {table}"), [])
            .unwrap();
        assert_eq!(
            store.load_scenario("a").unwrap_err().code(),
            "PERSISTENCE_SCENARIO_RELATION_MISMATCH"
        );
    }
}

#[test]
fn size_limits_are_checked_before_writes_and_before_reading_untrusted_blobs() {
    let (_directory, mut store, connection) = fixture();
    let mut oversized = scenario("huge");
    oversized.scenario_payload = vec![b' '; MAXIMUM_SCENARIO_PAYLOAD_BYTES + 1];
    assert_eq!(
        store.adopt_scenario(&oversized).unwrap_err().code(),
        "PERSISTENCE_SCENARIO_RESOURCE_LIMIT"
    );
    assert_eq!(counts(&connection), (0, 0, 0));
    drop(oversized);
    store.adopt_scenario(&scenario("a")).unwrap();
    connection
        .execute_batch("PRAGMA ignore_check_constraints = ON;")
        .unwrap();
    connection
        .execute(
            "UPDATE timetable_revisions SET payload = zeroblob(?1)",
            [MAXIMUM_SCENARIO_PAYLOAD_BYTES + 1],
        )
        .unwrap();
    assert_eq!(
        store.load_scenario("a").unwrap_err().code(),
        "PERSISTENCE_SCENARIO_RESOURCE_LIMIT"
    );
}

#[test]
fn concurrent_adoption_of_same_ids_commits_exactly_one_complete_scenario() {
    let (directory, store, connection) = fixture();
    drop(store);
    let barrier = Arc::new(Barrier::new(2));
    let threads = (0..2)
        .map(|_| {
            let path = directory.path().join("scenarios.sqlite3");
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut store = SqliteStore::open(path).unwrap();
                barrier.wait();
                store
                    .adopt_scenario(&scenario("same"))
                    .map_err(|error| error.code())
            })
        })
        .collect::<Vec<_>>();
    let results = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err("PERSISTENCE_SCENARIO_ALREADY_EXISTS"))
            .count(),
        1
    );
    assert_eq!(counts(&connection), (1, 1, 1));
}

#[test]
fn upgrade_from_schema_three_preserves_source_and_completed_artifact() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("version-three.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch(
        "CREATE TABLE schema_metadata(singleton INTEGER PRIMARY KEY, version INTEGER NOT NULL) STRICT;
         INSERT INTO schema_metadata VALUES (1, 3);"
    ).unwrap();
    for sql in [
        include_str!("../migrations/0001_project_revisions.sql"),
        include_str!("../migrations/0002_solver_run_provenance.sql"),
        include_str!("../migrations/0003_solve_artifacts.sql"),
    ] {
        connection.execute_batch(sql).unwrap();
    }
    let source = source(0);
    connection
        .execute(
            "INSERT INTO projects VALUES (?1, ?2, 0, ?3, ?3)",
            params![
                source.project_id,
                source.display_name,
                instant().to_rfc3339()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO project_revisions VALUES (?1, 0, 1, ?2, ?3, ?4)",
            params![
                source.project_id,
                source.payload,
                source.payload_hash().as_bytes(),
                instant().to_rfc3339()
            ],
        )
        .unwrap();
    let artifact = artifact();
    connection
        .execute(
            "INSERT INTO solve_artifacts VALUES (?1, ?2, 0, ?3, 1, 'FEASIBLE', ?4, ?4, ?5, ?6)",
            params![
                artifact.run_id,
                artifact.project_id,
                artifact.source_payload_hash.as_slice(),
                instant().to_rfc3339(),
                artifact.payload,
                blake3::hash(&artifact.payload).as_bytes()
            ],
        )
        .unwrap();
    let mut store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), DATABASE_SCHEMA_VERSION);
    assert_eq!(DATABASE_SCHEMA_VERSION, 4);
    assert_eq!(store.load_project("project-a").unwrap(), source);
    assert_eq!(
        store.load_solve_artifact("run-a").unwrap().document,
        artifact
    );
    store.adopt_scenario(&scenario("after-upgrade")).unwrap();
    assert_eq!(counts(&connection), (1, 1, 1));
}

#[test]
fn scenario_metadata_list_paginates_without_reading_untrusted_payloads() {
    let (_directory, mut store, connection) = fixture();
    for id in ["b", "a", "recent"] {
        let mut document = scenario(id);
        if id == "recent" {
            document.created_at += chrono::Duration::seconds(1);
        }
        store.adopt_scenario(&document).unwrap();
    }
    connection
        .execute(
            "UPDATE scenario_revisions SET payload = X'FF' WHERE scenario_id = 'a'",
            [],
        )
        .unwrap();
    let all = store.list_scenarios("project-a", 101, 0).unwrap();
    assert_eq!(
        all.iter()
            .map(|row| row.scenario_id.as_str())
            .collect::<Vec<_>>(),
        ["recent", "a", "b"]
    );
    assert_eq!(store.list_scenarios("project-a", 2, 0).unwrap(), all[..2]);
    assert_eq!(store.list_scenarios("project-a", 2, 2).unwrap(), all[2..]);
    assert!(store.list_scenarios("other", 20, 0).unwrap().is_empty());
    assert_eq!(
        store.load_scenario("a").unwrap_err().code(),
        "PERSISTENCE_SCENARIO_HASH_MISMATCH"
    );
    for limit in [0, 102, u32::MAX] {
        assert_eq!(
            store
                .list_scenarios("project-a", limit, 0)
                .unwrap_err()
                .code(),
            "PERSISTENCE_INVALID_SCENARIO_LIST_LIMIT"
        );
    }
    assert_eq!(counts(&connection), (3, 3, 3));
}
