use chrono::{DateTime, Utc};
use class_schedule_persistence::{
    DATABASE_SCHEMA_VERSION, ProjectDocument, SolveArtifactDocument, SolverRunRecord, SqliteStore,
};
use rusqlite::{Connection, params};

fn source(revision: u64) -> ProjectDocument {
    ProjectDocument {
        project_id: "project-a".to_owned(),
        display_name: format!("验证项目 revision {revision}"),
        revision,
        document_schema_version: 1,
        payload: format!("{{\"revision\":{revision}}}").into_bytes(),
    }
}

fn instant(second: u32) -> DateTime<Utc> {
    format!("2026-09-08T00:00:{second:02}.000Z")
        .parse()
        .unwrap()
}

fn artifact(id: &str, revision: u64) -> SolveArtifactDocument {
    SolveArtifactDocument {
        run_id: id.to_owned(),
        project_id: "project-a".to_owned(),
        project_revision: revision,
        source_payload_hash: *source(revision).payload_hash().as_bytes(),
        artifact_schema_version: 1,
        status_code: "FEASIBLE".to_owned(),
        started_at: instant(0),
        finished_at: instant(10),
        payload: b"{\"artifact_schema_version\":1,\"opaque\":true}".to_vec(),
    }
}

fn attempt(root: &str, index: usize) -> SolverRunRecord {
    SolverRunRecord {
        run_id: format!("{root}:attempt:{index}"),
        project_id: "project-a".to_owned(),
        project_revision: 0,
        scenario_id: None,
        request_id: format!("{root}:request:{index}"),
        input_snapshot_hash: [1; 32],
        solver_engine_version: "or-tools-9.15.6755".to_owned(),
        protocol_version: 1,
        seed: 20_260_908,
        parameters_json: "{\"worker_count\":1}".to_owned(),
        worker_count: 1,
        time_limit_ms: 1000,
        status_code: "FEASIBLE".to_owned(),
        objective_json: "{}".to_owned(),
        validation_code: "PASSED".to_owned(),
        output_hash: Some([2; 32]),
        started_at: instant(1),
        finished_at: Some(instant(9)),
    }
}

fn fixture() -> (tempfile::TempDir, SqliteStore, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runs.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    store.create_project(&source(0)).unwrap();
    (directory, store, Connection::open(path).unwrap())
}

fn counts(connection: &Connection) -> (u32, u32, u32, u32) {
    connection
        .query_row(
            "SELECT (SELECT count(*) FROM solve_artifacts), (SELECT count(*) FROM solver_runs),
         (SELECT count(*) FROM solve_artifact_attempts), (SELECT count(*) FROM project_revisions)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

#[test]
fn completed_run_round_trip_preserves_attempt_order_without_new_revision() {
    let (directory, mut store, connection) = fixture();
    let document = artifact("run-a", 0);
    let attempts = [attempt("run-a", 0), attempt("run-a", 1)];
    store.record_solve_artifact(&document, &attempts).unwrap();
    assert_eq!(counts(&connection), (1, 2, 2, 1));
    drop(store);
    let reopened = SqliteStore::open(directory.path().join("runs.sqlite3")).unwrap();
    let loaded = reopened.load_solve_artifact("run-a").unwrap();
    assert_eq!(loaded.document, document);
    assert_eq!(loaded.attempts, attempts);
    assert_eq!(reopened.load_project("project-a").unwrap(), source(0));
    let fk: u32 = connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(fk, 0);
}

#[test]
fn historical_input_remains_readable_and_can_receive_a_completed_run_after_replace() {
    let (_directory, mut store, connection) = fixture();
    store.replace_project(0, &source(1)).unwrap();
    let historical = store.load_project_revision("project-a", 0).unwrap();
    assert_eq!(historical.payload, source(0).payload);
    assert_eq!(historical.revision, 0);
    assert_eq!(historical.display_name, source(1).display_name);
    store
        .record_solve_artifact(&artifact("old-run", 0), &[attempt("old-run", 0)])
        .unwrap();
    assert_eq!(store.load_project("project-a").unwrap(), source(1));
    assert_eq!(counts(&connection), (1, 1, 1, 2));
    assert_eq!(
        store
            .load_project_revision("project-a", 2)
            .unwrap_err()
            .code(),
        "PERSISTENCE_PROJECT_REVISION_NOT_FOUND"
    );
    assert_eq!(
        store
            .load_project_revision("project-a", u64::MAX)
            .unwrap_err()
            .code(),
        "PERSISTENCE_INTEGER_OUT_OF_RANGE"
    );
}

#[test]
fn failure_or_cancelled_root_can_be_recorded_without_invented_attempts() {
    let (_directory, mut store, connection) = fixture();
    for (id, status) in [("failed", "INTERNAL_ERROR"), ("cancelled", "CANCELLED")] {
        let mut document = artifact(id, 0);
        document.status_code = status.to_owned();
        store.record_solve_artifact(&document, &[]).unwrap();
        assert_eq!(store.load_solve_artifact(id).unwrap().document, document);
    }
    assert_eq!(counts(&connection), (2, 0, 0, 1));
}

#[test]
fn duplicate_root_is_stable_and_never_overwrites_artifact_or_provenance() {
    let (_directory, mut store, connection) = fixture();
    let document = artifact("immutable", 0);
    store
        .record_solve_artifact(&document, &[attempt("immutable", 0)])
        .unwrap();
    let mut other = document.clone();
    other.payload = b"{\"replacement\":true}".to_vec();
    assert_eq!(
        store
            .record_solve_artifact(&other, &[attempt("new", 0)])
            .unwrap_err()
            .code(),
        "PERSISTENCE_SOLVE_RUN_ALREADY_EXISTS"
    );
    assert_eq!(
        store.load_solve_artifact("immutable").unwrap().document,
        document
    );
    assert_eq!(counts(&connection), (1, 1, 1, 1));
}

#[test]
fn actual_sql_failure_after_second_provenance_insert_rolls_back_the_whole_run() {
    let (_directory, mut store, connection) = fixture();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_second_attempt BEFORE INSERT ON solve_artifact_attempts
         WHEN NEW.ordinal = 1 BEGIN SELECT RAISE(ABORT, 'injected link failure'); END;",
        )
        .unwrap();
    let attempts = [attempt("partial", 0), attempt("partial", 1)];
    assert_eq!(
        store
            .record_solve_artifact(&artifact("partial", 0), &attempts)
            .unwrap_err()
            .code(),
        "PERSISTENCE_DATABASE_ERROR"
    );
    assert_eq!(counts(&connection), (0, 0, 0, 1));
    assert_eq!(store.load_project("project-a").unwrap(), source(0));
    assert_eq!(
        store.load_solve_artifact("partial").unwrap_err().code(),
        "PERSISTENCE_SOLVE_RUN_NOT_FOUND"
    );
}

#[test]
fn existing_attempt_unique_conflict_rolls_back_new_root() {
    let (_directory, mut store, connection) = fixture();
    let record = attempt("already-recorded", 0);
    store.record_solver_run(&record).unwrap();
    assert!(
        store
            .record_solve_artifact(&artifact("new-root", 0), &[record])
            .is_err()
    );
    assert_eq!(counts(&connection), (0, 1, 0, 1));
}

#[test]
fn source_hash_mismatch_or_corrupted_historical_payload_cannot_be_saved() {
    let (_directory, mut store, connection) = fixture();
    let mut document = artifact("mismatch", 0);
    document.source_payload_hash = [9; 32];
    assert_eq!(
        store
            .record_solve_artifact(&document, &[])
            .unwrap_err()
            .code(),
        "PERSISTENCE_SOLVE_SOURCE_HASH_MISMATCH"
    );
    connection
        .execute(
            "UPDATE project_revisions SET payload = ?1",
            [b"{\"corrupt\":true}".as_slice()],
        )
        .unwrap();
    assert_eq!(
        store
            .record_solve_artifact(&artifact("corrupt", 0), &[])
            .unwrap_err()
            .code(),
        "PERSISTENCE_SOLVE_SOURCE_HASH_MISMATCH"
    );
    assert_eq!(
        store
            .load_project_revision("project-a", 0)
            .unwrap_err()
            .code(),
        "PERSISTENCE_PAYLOAD_HASH_MISMATCH"
    );
    assert_eq!(counts(&connection), (0, 0, 0, 1));
}

#[test]
fn attempt_scope_time_and_count_are_checked_before_any_write() {
    let (_directory, mut store, connection) = fixture();
    let document = artifact("invalid", 0);
    let original = attempt("invalid", 0);
    let mut variants = Vec::new();
    let mut value = original.clone();
    value.project_revision = 1;
    variants.push(value);
    let mut value = original.clone();
    value.project_id = "other".to_owned();
    variants.push(value);
    let mut value = original.clone();
    value.finished_at = None;
    variants.push(value);
    let mut value = original.clone();
    value.finished_at = Some(instant(11));
    variants.push(value);
    let mut value = original.clone();
    value.finished_at = Some(instant(0));
    variants.push(value);
    for value in variants {
        assert_eq!(
            store
                .record_solve_artifact(&document, &[value])
                .unwrap_err()
                .code(),
            "PERSISTENCE_SOLVE_ATTEMPT_MISMATCH"
        );
    }
    assert_eq!(
        store
            .record_solve_artifact(&document, &[original.clone(), original])
            .unwrap_err()
            .code(),
        "PERSISTENCE_SOLVE_ATTEMPT_MISMATCH"
    );
    let attempts = (0..17).map(|i| attempt("too-many", i)).collect::<Vec<_>>();
    assert_eq!(
        store
            .record_solve_artifact(&document, &attempts)
            .unwrap_err()
            .code(),
        "PERSISTENCE_SOLVE_ARTIFACT_RESOURCE_LIMIT"
    );
    assert_eq!(counts(&connection), (0, 0, 0, 1));
}

#[test]
fn corrupted_artifact_hash_and_non_contiguous_attempts_fail_closed() {
    let (_directory, mut store, connection) = fixture();
    store
        .record_solve_artifact(&artifact("corrupt", 0), &[])
        .unwrap();
    connection
        .execute(
            "UPDATE solve_artifacts SET payload = ?1 WHERE run_id = 'corrupt'",
            [b"{\"tamper\":true}".as_slice()],
        )
        .unwrap();
    assert_eq!(
        store.load_solve_artifact("corrupt").unwrap_err().code(),
        "PERSISTENCE_SOLVE_ARTIFACT_HASH_MISMATCH"
    );
    store
        .record_solve_artifact(&artifact("ordinal", 0), &[attempt("ordinal", 0)])
        .unwrap();
    connection
        .execute(
            "UPDATE solve_artifact_attempts SET ordinal = 1 WHERE artifact_run_id = 'ordinal'",
            [],
        )
        .unwrap();
    assert_eq!(
        store.load_solve_artifact("ordinal").unwrap_err().code(),
        "PERSISTENCE_SOLVE_ATTEMPT_MISMATCH"
    );
}

#[test]
fn artifact_metadata_is_bounded_sorted_and_independent_of_corrupt_payloads() {
    let (_directory, mut store, connection) = fixture();
    for id in ["b", "a", "recent"] {
        let mut document = artifact(id, 0);
        if id == "recent" {
            document.started_at = instant(2);
        }
        store.record_solve_artifact(&document, &[]).unwrap();
    }
    connection
        .execute(
            "UPDATE solve_artifacts SET payload = X'FF' WHERE run_id = 'a'",
            [],
        )
        .unwrap();
    let all = store.list_solve_artifacts("project-a", 101, 0).unwrap();
    assert_eq!(
        all.iter()
            .map(|item| item.run_id.as_str())
            .collect::<Vec<_>>(),
        ["recent", "a", "b"]
    );
    assert_eq!(
        store.list_solve_artifacts("project-a", 2, 0).unwrap(),
        all[..2]
    );
    assert_eq!(
        store.list_solve_artifacts("project-a", 2, 2).unwrap(),
        all[2..]
    );
    assert!(
        store
            .list_solve_artifacts("other", 1, 0)
            .unwrap()
            .is_empty()
    );
    for limit in [0, 102, u32::MAX] {
        assert_eq!(
            store
                .list_solve_artifacts("project-a", limit, 0)
                .unwrap_err()
                .code(),
            "PERSISTENCE_INVALID_SOLVE_LIST_LIMIT"
        );
    }
}

#[test]
fn upgrades_version_two_with_legacy_provenance_and_revisions_intact() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("version-two.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch(
        "CREATE TABLE schema_metadata(singleton INTEGER PRIMARY KEY, version INTEGER NOT NULL) STRICT;
         INSERT INTO schema_metadata VALUES (1, 2);",
    ).unwrap();
    connection
        .execute_batch(include_str!("../migrations/0001_project_revisions.sql"))
        .unwrap();
    connection
        .execute_batch(include_str!("../migrations/0002_solver_run_provenance.sql"))
        .unwrap();
    let document = source(0);
    connection
        .execute(
            "INSERT INTO projects VALUES (?1, ?2, 0, ?3, ?3)",
            params![
                document.project_id,
                document.display_name,
                instant(0).to_rfc3339()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO project_revisions VALUES (?1, 0, 1, ?2, ?3, ?4)",
            params![
                document.project_id,
                document.payload,
                document.payload_hash().as_bytes(),
                instant(0).to_rfc3339()
            ],
        )
        .unwrap();
    connection.execute(
        "INSERT INTO solver_runs(run_id, project_id, project_revision, scenario_id, request_id,
         input_snapshot_hash, solver_engine_version, protocol_version, seed, parameters_json,
         worker_count, time_limit_ms, status_code, objective_json, validation_code, output_hash,
         started_at, finished_at) VALUES ('legacy', ?1, 0, NULL, 'legacy-request', ?2,
         'or-tools-9.15.6755', 1, 42, '{}', 1, 1000, 'UNKNOWN', '{}', 'NOT_APPLICABLE', NULL, ?3, ?3)",
        params![document.project_id, [1_u8; 32].as_slice(), instant(0).to_rfc3339()],
    ).unwrap();
    let mut store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), DATABASE_SCHEMA_VERSION);
    assert_eq!(store.load_project("project-a").unwrap(), document);
    store
        .record_solve_artifact(
            &artifact("after-upgrade", 0),
            &[attempt("after-upgrade", 0)],
        )
        .unwrap();
    assert_eq!(counts(&connection), (1, 2, 1, 1));
}
