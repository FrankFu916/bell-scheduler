use std::fs;

use class_schedule_persistence::{DATABASE_SCHEMA_VERSION, ProjectDocument, SqliteStore};
use rusqlite::Connection;

fn document(revision: u64) -> ProjectDocument {
    ProjectDocument {
        project_id: "project".into(),
        display_name: "测试项目".into(),
        revision,
        document_schema_version: 1,
        payload: format!("{{\"revision\":{revision}}}").into_bytes(),
    }
}

#[test]
fn existing_open_does_not_create_a_missing_file_or_parent_directory() {
    let directory = tempfile::tempdir().unwrap();
    for relative in ["missing.sqlite3", "missing-parent/project.sqlite3"] {
        let path = directory.path().join(relative);
        assert_eq!(
            SqliteStore::open_existing(&path).unwrap_err().code(),
            "PERSISTENCE_DATABASE_ERROR"
        );
        assert!(!path.exists());
    }
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn existing_open_rejects_old_future_and_malformed_databases_without_migration_or_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("schema.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE schema_metadata(singleton INTEGER PRIMARY KEY, version INTEGER NOT NULL);
         INSERT INTO schema_metadata VALUES (1, 3);",
        )
        .unwrap();
    for (version, code) in [
        (
            DATABASE_SCHEMA_VERSION - 1,
            "PERSISTENCE_SCHEMA_UPGRADE_REQUIRED",
        ),
        (
            DATABASE_SCHEMA_VERSION + 1,
            "PERSISTENCE_UNSUPPORTED_SCHEMA",
        ),
    ] {
        connection
            .execute("UPDATE schema_metadata SET version = ?1", [version])
            .unwrap();
        let before = fs::read(&path).unwrap();
        assert_eq!(SqliteStore::open_existing(&path).unwrap_err().code(), code);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
                    [],
                    |row| row.get::<_, u64>(0)
                )
                .unwrap(),
            1
        );
    }
    for bytes in [b"".as_slice(), b"invalid database".as_slice()] {
        let path = directory.path().join("malformed.sqlite3");
        fs::write(&path, bytes).unwrap();
        assert!(SqliteStore::open_existing(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn existing_open_preserves_bytes_until_a_requested_revision_write_and_can_reopen_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    store.create_project(&document(0)).unwrap();
    drop(store);
    let before = fs::read(&path).unwrap();
    let mut store = SqliteStore::open_existing(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), DATABASE_SCHEMA_VERSION);
    assert_eq!(store.load_project("project").unwrap(), document(0));
    assert_eq!(fs::read(&path).unwrap(), before);
    store.replace_project(0, &document(1)).unwrap();
    drop(store);
    let store = SqliteStore::open_existing(&path).unwrap();
    assert_eq!(store.load_project("project").unwrap(), document(1));
    drop(store);
    let preserved = directory.path().join("preserved.sqlite3");
    fs::rename(&path, &preserved).unwrap();
    let before = fs::read(&preserved).unwrap();
    assert!(SqliteStore::open_existing(&path).is_err());
    assert!(!path.exists());
    assert_eq!(fs::read(&preserved).unwrap(), before);
}

#[test]
fn existing_write_connection_enforces_foreign_keys_and_rolls_back_orphan_insertion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("foreign-keys.sqlite3");
    drop(SqliteStore::open(&path).unwrap());
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch(
        "CREATE TRIGGER inject_orphan AFTER INSERT ON projects BEGIN
         INSERT INTO project_revisions(project_id, revision, document_schema_version, payload, payload_hash, created_at)
         VALUES ('missing-parent', 0, 1, X'7B7D', zeroblob(32), '2026-09-12T00:00:00.000Z'); END;"
    ).unwrap();
    let before = fs::read(&path).unwrap();
    let mut store = SqliteStore::open_existing(&path).unwrap();
    assert_eq!(
        store.create_project(&document(0)).unwrap_err().code(),
        "PERSISTENCE_DATABASE_ERROR"
    );
    assert!(store.list_projects(20, 0).unwrap().is_empty());
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM project_revisions", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}
