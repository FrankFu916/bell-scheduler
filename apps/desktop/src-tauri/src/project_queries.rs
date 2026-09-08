//! Versioned project-discovery DTOs. Opening remains a separately revalidated command.

use std::path::PathBuf;

use class_schedule_application::{
    ProjectQueryError, list_imported_projects as query_imported_projects,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::import_commit::{application_error, database_path, open_store, validate_schema};
use crate::{COMMAND_SCHEMA_VERSION, CommandError};

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListImportedProjectsRequest {
    pub schema_version: u32,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedProjectSummaryDto {
    pub project_id: String,
    pub display_name: String,
    pub revision: String,
    pub updated_at: String,
    /// Only indicates supported identity syntax; the payload has not been validated by listing.
    pub can_open: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListImportedProjectsResponse {
    pub schema_version: u32,
    pub projects: Vec<ImportedProjectSummaryDto>,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

#[tauri::command]
pub async fn list_imported_projects(
    app: tauri::AppHandle,
    request: ListImportedProjectsRequest,
) -> Result<ListImportedProjectsResponse, CommandError> {
    tauri::async_runtime::spawn_blocking(move || {
        list_imported_projects_inner(request, || database_path(&app))
    })
    .await
    .map_err(|_| {
        CommandError::new(
            "DESKTOP_BACKGROUND_TASK_FAILED",
            "本地项目列表读取未正常完成，请重新刷新。",
        )
    })?
}

fn list_imported_projects_inner(
    request: ListImportedProjectsRequest,
    resolve_database: impl FnOnce() -> Result<PathBuf, CommandError>,
) -> Result<ListImportedProjectsResponse, CommandError> {
    validate_schema(request.schema_version)?;
    let store = open_store(&resolve_database()?)?;
    let page = query_imported_projects(&store, request.limit, request.offset)
        .map_err(project_query_error)?;
    Ok(ListImportedProjectsResponse {
        schema_version: COMMAND_SCHEMA_VERSION,
        projects: page
            .projects
            .into_iter()
            .map(|summary| ImportedProjectSummaryDto {
                project_id: summary.project_id,
                display_name: summary.display_name,
                revision: summary.current_revision.to_string(),
                updated_at: summary.updated_at,
                can_open: summary.openable_project_id.is_some(),
            })
            .collect(),
        has_more: page.has_more,
        next_offset: page.next_offset,
    })
}

fn project_query_error(error: ProjectQueryError) -> CommandError {
    match error {
        ProjectQueryError::InvalidLimit => {
            CommandError::new(error.code(), "项目列表每页数量必须为 1 到 100。")
                .with_details(json!({"field": "limit", "minimum": 1, "maximum": 100}))
        }
        ProjectQueryError::InvalidOffset => CommandError::new(
            error.code(),
            "项目列表分页位置超出范围，请从第一页重新刷新。",
        )
        .with_details(json!({"field": "offset"})),
        ProjectQueryError::Persistence(error) => application_error(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use class_schedule_application::{
        CalendarDefinition, CsvImportAuditOptions, CsvImportMode, ImportCommitCommand,
        ImportCommitIntent, commit_csv_import, load_imported_project,
    };
    use class_schedule_domain::SchoolProjectId;
    use class_schedule_import::CsvSource;
    use class_schedule_persistence::{PersistenceError, ProjectDocument, SqliteStore};
    use rusqlite::{Connection, params};

    use super::*;

    fn query(
        path: &Path,
        limit: u32,
        offset: u32,
    ) -> Result<ListImportedProjectsResponse, CommandError> {
        list_imported_projects_inner(
            ListImportedProjectsRequest {
                schema_version: 1,
                limit,
                offset,
            },
            || Ok(path.to_path_buf()),
        )
    }

    fn project_counts(path: &Path) -> (u32, u32) {
        Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM projects), (SELECT COUNT(*) FROM project_revisions)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    fn create_valid_project(store: &mut SqliteStore, id: SchoolProjectId) {
        let sources = crate::tests::payloads(&[
            "students",
            "administrative_classes",
            "student_subject_choices",
            "teachers",
            "teacher_unavailability",
            "rooms",
            "course_plans",
            "teaching_sections",
            "section_enrollments",
            "course_offerings",
            "fixed_activities",
        ]);
        let command = ImportCommitCommand {
            project_id: id,
            display_name: "真实 CSV 项目".to_owned(),
            intent: ImportCommitIntent::Create,
            options: CsvImportAuditOptions {
                project_stable_key: "project-list-small".to_owned(),
                calendar: CalendarDefinition::weekday_with_break(8, 4).unwrap(),
                exact_subject_choices: 3,
                mode: CsvImportMode::ExistingSections,
            },
        };
        commit_csv_import(
            store,
            &command,
            sources.iter().map(|source| {
                CsvSource::new(
                    crate::parse_dataset_kind(&source.dataset).unwrap(),
                    &source.bytes,
                )
            }),
        )
        .unwrap();
    }

    #[test]
    fn empty_project_list_and_invalid_bounds_create_no_project_or_revision() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("projects.sqlite3");
        let page = query(&path, 100, 0).unwrap();
        assert!(page.projects.is_empty());
        assert!(!page.has_more);
        assert_eq!(page.next_offset, None);
        for limit in [0, 101, u32::MAX] {
            assert_eq!(
                query(&path, limit, 0).unwrap_err().code,
                "APPLICATION_PROJECT_LIST_INVALID_LIMIT"
            );
        }
        assert_eq!(
            query(&path, 1, u32::MAX).unwrap_err().code,
            "APPLICATION_PROJECT_LIST_INVALID_OFFSET"
        );
        assert_eq!(project_counts(&path), (0, 0));
    }

    #[test]
    fn list_keeps_pagination_and_legacy_ids_when_one_real_payload_is_corrupted() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("projects.sqlite3");
        let mut store = SqliteStore::open(&path).unwrap();
        let valid_id = SchoolProjectId::new_v4();
        let corrupt_id = SchoolProjectId::new_v4();
        create_valid_project(&mut store, valid_id);
        create_valid_project(&mut store, corrupt_id);
        store
            .create_project(&ProjectDocument {
                project_id: "legacy-school-project".to_owned(),
                display_name: "旧版本项目".to_owned(),
                revision: 0,
                document_schema_version: 1,
                payload: b"{\"legacy\":true}".to_vec(),
            })
            .unwrap();
        Connection::open(&path)
            .unwrap()
            .execute(
                "UPDATE project_revisions SET payload = ?1 WHERE project_id = ?2",
                params![
                    b"CANARY_CORRUPT_PRIVATE_PAYLOAD".as_slice(),
                    corrupt_id.to_string()
                ],
            )
            .unwrap();
        let first = query(&path, 2, 0).unwrap();
        assert!(first.has_more);
        assert_eq!(first.next_offset, Some(2));
        let second = query(&path, 2, first.next_offset.unwrap()).unwrap();
        assert!(!second.has_more);
        assert_eq!(second.next_offset, None);
        let mut summaries = first.projects;
        summaries.extend(second.projects);
        assert_eq!(summaries.len(), 3);
        for summary in &summaries {
            assert_eq!(
                summary.can_open,
                summary.project_id != "legacy-school-project"
            );
            assert_eq!(summary.revision, "0");
            assert!(!summary.updated_at.is_empty());
        }
        assert!(
            !serde_json::to_string(&summaries)
                .unwrap()
                .contains("CANARY")
        );
        assert!(load_imported_project(&store, valid_id).is_ok());
        assert!(load_imported_project(&store, corrupt_id).is_err());
        assert_eq!(project_counts(&path), (3, 3));
    }

    #[test]
    fn revision_dto_preserves_values_above_javascript_integer_precision() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("projects.sqlite3");
        let mut store = SqliteStore::open(&path).unwrap();
        let id = SchoolProjectId::new_v4();
        create_valid_project(&mut store, id);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE projects SET current_revision = 9007199254740993",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE project_revisions SET revision = 9007199254740993",
                [],
            )
            .unwrap();
        let page = query(&path, 1, 0).unwrap();
        let value = serde_json::to_value(page).unwrap();
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["projects"][0]["projectId"], id.to_string());
        assert_eq!(value["projects"][0]["revision"], "9007199254740993");
        assert_eq!(value["projects"][0]["canOpen"], true);
        assert_eq!(value["hasMore"], false);
        assert!(value["nextOffset"].is_null());
        assert_eq!(project_counts(&path), (1, 1));
    }

    #[test]
    fn query_boundary_rejects_unknown_schema_paths_and_redacts_database_errors() {
        let error = list_imported_projects_inner(
            ListImportedProjectsRequest {
                schema_version: 2,
                limit: 20,
                offset: 0,
            },
            || panic!("unknown schema must not open database"),
        )
        .unwrap_err();
        assert_eq!(error.code, "DESKTOP_UNSUPPORTED_COMMAND_SCHEMA");
        assert!(serde_json::from_value::<ListImportedProjectsRequest>(json!({
            "schemaVersion": 1, "limit": 20, "offset": 0, "databasePath": "/tmp/untrusted.sqlite3",
        })).is_err());
        let error = project_query_error(ProjectQueryError::Persistence(
            PersistenceError::InvalidDocument {
                code: "PERSISTENCE_INVALID_JSON_PAYLOAD",
                detail: "CANARY_SQL_PRIVATE_DETAIL".to_owned(),
            },
        ));
        assert_eq!(error.code, "PERSISTENCE_INVALID_JSON_PAYLOAD");
        assert!(!serde_json::to_string(&error).unwrap().contains("CANARY"));
    }
}
