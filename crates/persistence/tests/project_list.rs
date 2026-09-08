use class_schedule_persistence::{ProjectDocument, SqliteStore};
use rusqlite::{Connection, params};

fn document(id: &str, revision: u64) -> ProjectDocument {
    ProjectDocument {
        project_id: id.to_owned(),
        display_name: format!("高中项目 {id} / {revision}"),
        revision,
        document_schema_version: 1,
        payload: b"{\"source\":\"project-list-test\"}".to_vec(),
    }
}

fn fixture() -> (tempfile::TempDir, SqliteStore, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("projects.sqlite3");
    let store = SqliteStore::open(&path).unwrap();
    let connection = Connection::open(path).unwrap();
    (directory, store, connection)
}

#[test]
fn project_list_orders_recent_updates_then_project_identity_and_paginates() {
    let (_directory, mut store, connection) = fixture();
    for id in ["old", "same-b", "same-a", "new"] {
        store.create_project(&document(id, 0)).unwrap();
        let timestamp = match id {
            "old" => "2000-01-01T00:00:00.000Z",
            "new" => "2002-01-01T00:00:00.000Z",
            _ => "2001-01-01T00:00:00.000Z",
        };
        connection
            .execute(
                "UPDATE projects SET updated_at = ?1 WHERE project_id = ?2",
                params![timestamp, id],
            )
            .unwrap();
    }
    let summaries = store.list_projects(101, 0).unwrap();
    assert_eq!(
        summaries
            .iter()
            .map(|project| project.project_id.as_str())
            .collect::<Vec<_>>(),
        ["new", "same-a", "same-b", "old"]
    );
    assert_eq!(store.list_projects(2, 0).unwrap(), summaries[..2]);
    assert_eq!(store.list_projects(2, 2).unwrap(), summaries[2..]);
    assert!(store.list_projects(2, 4).unwrap().is_empty());
    assert!(store.list_projects(1, u32::MAX).unwrap().is_empty());
}

#[test]
fn project_list_reflects_atomic_replace_and_survives_reopening() {
    let (directory, mut store, connection) = fixture();
    for id in ["project-a", "project-b"] {
        store.create_project(&document(id, 0)).unwrap();
    }
    connection
        .execute(
            "UPDATE projects SET updated_at = '2000-01-01T00:00:00.000Z'",
            [],
        )
        .unwrap();
    let updated = document("project-b", 1);
    store.replace_project(0, &updated).unwrap();
    let projects = store.list_projects(10, 0).unwrap();
    assert_eq!(projects[0].project_id, "project-b");
    assert_eq!(projects[0].display_name, updated.display_name);
    assert_eq!(projects[0].current_revision, 1);
    assert!(chrono::DateTime::parse_from_rfc3339(&projects[0].updated_at).is_ok());
    assert_eq!(projects[1].current_revision, 0);
    drop(store);
    let reopened = SqliteStore::open(directory.path().join("projects.sqlite3")).unwrap();
    assert_eq!(reopened.list_projects(10, 0).unwrap(), projects);
    assert_eq!(reopened.load_project("project-b").unwrap(), updated);
}

#[test]
fn project_list_does_not_load_or_validate_revision_payloads_and_never_mutates_them() {
    let (_directory, mut store, connection) = fixture();
    for id in ["corrupted-payload", "missing-revision"] {
        store.create_project(&document(id, 0)).unwrap();
    }
    connection
        .execute(
            "UPDATE project_revisions SET payload = ?1 WHERE project_id = 'corrupted-payload'",
            [b"not JSON".as_slice()],
        )
        .unwrap();
    connection
        .execute(
            "DELETE FROM project_revisions WHERE project_id = 'missing-revision'",
            [],
        )
        .unwrap();
    let before_version: u32 = connection
        .pragma_query_value(None, "data_version", |row| row.get(0))
        .unwrap();
    let projects = store.list_projects(10, 0).unwrap();
    assert_eq!(projects.len(), 2);
    let after_version: u32 = connection
        .pragma_query_value(None, "data_version", |row| row.get(0))
        .unwrap();
    assert_eq!(after_version, before_version);
    assert_eq!(
        store.load_project("corrupted-payload").unwrap_err().code(),
        "PERSISTENCE_PAYLOAD_HASH_MISMATCH"
    );
    assert_eq!(
        store.load_project("missing-revision").unwrap_err().code(),
        "PERSISTENCE_PROJECT_NOT_FOUND"
    );
    let revision_count: u32 = connection
        .query_row("SELECT count(*) FROM project_revisions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(revision_count, 1);
}

#[test]
fn project_list_rejects_unbounded_limits_with_stable_error_codes() {
    let store = SqliteStore::in_memory().unwrap();
    for limit in [0, 102, u32::MAX] {
        assert_eq!(
            store.list_projects(limit, 0).unwrap_err().code(),
            "PERSISTENCE_INVALID_PROJECT_LIST_LIMIT"
        );
    }
    assert!(store.list_projects(1, 0).unwrap().is_empty());
    assert!(store.list_projects(101, 0).unwrap().is_empty());
}
