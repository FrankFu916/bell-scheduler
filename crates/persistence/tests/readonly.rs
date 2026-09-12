use class_schedule_persistence::{DATABASE_SCHEMA_VERSION, ProjectDocument, SqliteStore};
use rusqlite::Connection;

#[test]
fn readonly_connection_loads_existing_projects_and_rejects_all_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("school.sqlite3");
    let document = ProjectDocument {
        project_id: "school".to_owned(),
        display_name: "学校".to_owned(),
        revision: 0,
        document_schema_version: 1,
        payload: br#"{"students":[]}"#.to_vec(),
    };
    let mut writer = SqliteStore::open(&path).unwrap();
    writer.create_project(&document).unwrap();
    drop(writer);
    let before = std::fs::read(&path).unwrap();
    let mut reader = SqliteStore::open_readonly(&path).unwrap();
    assert_eq!(reader.load_project("school").unwrap(), document);
    let mut another = document;
    another.project_id = "another".to_owned();
    let error = reader.create_project(&another).unwrap_err();
    assert_eq!(error.code(), "PERSISTENCE_DATABASE_ERROR");
    assert_eq!(reader.list_projects(10, 0).unwrap().len(), 1);
    drop(reader);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn readonly_open_never_creates_or_migrates_a_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing.sqlite3");
    assert!(SqliteStore::open_readonly(&path).is_err());
    assert!(!path.exists());
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE schema_metadata(singleton INTEGER PRIMARY KEY, version INTEGER NOT NULL);
             INSERT INTO schema_metadata VALUES (1, 3);",
        )
        .unwrap();
    drop(connection);
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        SqliteStore::open_readonly(&path).unwrap_err().code(),
        "PERSISTENCE_SCHEMA_UPGRADE_REQUIRED"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE schema_metadata SET version=?1",
            [DATABASE_SCHEMA_VERSION + 1],
        )
        .unwrap();
    drop(connection);
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        SqliteStore::open_readonly(&path).unwrap_err().code(),
        "PERSISTENCE_UNSUPPORTED_SCHEMA"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
