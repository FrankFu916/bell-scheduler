//! Bounded project discovery. Opening a project remains a separate, fully revalidated use case.

use class_schedule_domain::SchoolProjectId;
use class_schedule_persistence::{PersistenceError, SqliteStore};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedProjectSummary {
    /// Raw persistence identity, retained even for unsupported legacy identifiers.
    pub project_id: String,
    /// Present only when the raw identity round-trips through the typed import command identity.
    /// This permits opening; it is not a claim that the unexamined payload is valid.
    pub openable_project_id: Option<SchoolProjectId>,
    pub display_name: String,
    pub current_revision: u64,
    pub updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedProjectPage {
    pub projects: Vec<ImportedProjectSummary>,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

#[derive(Debug, Error)]
pub enum ProjectQueryError {
    #[error("project page size must be between 1 and 100")]
    InvalidLimit,
    #[error("project page offset cannot represent the next page")]
    InvalidOffset,
    #[error("project metadata could not be read")]
    Persistence(#[from] PersistenceError),
}

impl ProjectQueryError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidLimit => "APPLICATION_PROJECT_LIST_INVALID_LIMIT",
            Self::InvalidOffset => "APPLICATION_PROJECT_LIST_INVALID_OFFSET",
            Self::Persistence(error) => error.code(),
        }
    }
}

/// Lists project metadata in one read query without compiling or validating every saved payload.
///
/// Legacy/noncanonical IDs remain visible and receive no typed opening ID. Opening a supported
/// entry must call [`crate::load_imported_project`], which checks its current hash, document,
/// typed rows, references and semantics. List metadata must never substitute for that check.
///
/// Pagination is relative to the database state at each query. Concurrent creates/replacements
/// can reorder subsequent pages; refreshing starts at offset zero rather than promising a frozen
/// cross-request snapshot.
///
/// # Errors
///
/// Rejects limits outside 1..=100 and offsets whose next-page position exceeds `u32`, or returns
/// a stable persistence error. No database transaction is opened for writing.
pub fn list_imported_projects(
    store: &SqliteStore,
    limit: u32,
    offset: u32,
) -> Result<ImportedProjectPage, ProjectQueryError> {
    if !(1..=100).contains(&limit) {
        return Err(ProjectQueryError::InvalidLimit);
    }
    let next_offset = offset
        .checked_add(limit)
        .ok_or(ProjectQueryError::InvalidOffset)?;
    let mut summaries = store.list_projects(limit + 1, offset)?;
    let page_size = usize::try_from(limit).map_err(|_| ProjectQueryError::InvalidLimit)?;
    let has_more = summaries.len() > page_size;
    summaries.truncate(page_size);
    let projects = summaries
        .into_iter()
        .map(|summary| ImportedProjectSummary {
            openable_project_id: summary
                .project_id
                .parse::<SchoolProjectId>()
                .ok()
                .filter(|id| id.to_string() == summary.project_id),
            project_id: summary.project_id,
            display_name: summary.display_name,
            current_revision: summary.current_revision,
            updated_at: summary.updated_at,
        })
        .collect();
    Ok(ImportedProjectPage {
        projects,
        has_more,
        next_offset: has_more.then_some(next_offset),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use class_schedule_persistence::ProjectDocument;

    fn create_metadata_project(store: &mut SqliteStore, id: &str) {
        store
            .create_project(&ProjectDocument {
                project_id: id.to_owned(),
                display_name: format!("项目 {id}"),
                revision: 0,
                document_schema_version: 99,
                payload: b"{\"legacy\":true}".to_vec(),
            })
            .unwrap();
    }

    #[test]
    fn project_list_keeps_legacy_ids_visible_and_requires_load_to_validate_payload() {
        let mut store = SqliteStore::in_memory().unwrap();
        let canonical = "11111111-aaaa-4111-8111-111111111111";
        for id in [
            canonical,
            "legacy-project",
            "11111111-AAAA-4111-8111-111111111111",
        ] {
            create_metadata_project(&mut store, id);
        }
        let page = list_imported_projects(&store, 100, 0).unwrap();
        assert_eq!(page.projects.len(), 3);
        assert!(!page.has_more);
        assert_eq!(page.next_offset, None);
        for summary in page.projects {
            assert_eq!(
                summary.openable_project_id.is_some(),
                summary.project_id == canonical
            );
            if let Some(id) = summary.openable_project_id {
                // Metadata listing is intentionally possible even when the payload is unsupported.
                assert_eq!(
                    crate::load_imported_project(&store, id).unwrap_err().code(),
                    "APPLICATION_IMPORT_DOCUMENT_INVALID"
                );
            }
        }
    }

    #[test]
    fn project_list_has_more_uses_one_extra_row_and_does_not_lose_page_entries() {
        let mut store = SqliteStore::in_memory().unwrap();
        for index in 0..5 {
            create_metadata_project(&mut store, &format!("project-{index}"));
        }
        let all = list_imported_projects(&store, 100, 0).unwrap().projects;
        let mut actual = Vec::new();
        let mut offset = 0;
        loop {
            let page = list_imported_projects(&store, 2, offset).unwrap();
            assert!(page.projects.len() <= 2);
            assert_eq!(page.has_more, page.next_offset.is_some());
            actual.extend(page.projects);
            match page.next_offset {
                Some(next) => {
                    assert_eq!(next, offset + 2);
                    offset = next;
                }
                None => break,
            }
        }
        assert_eq!(actual, all);
        let beyond = list_imported_projects(&store, 2, 20).unwrap();
        assert!(beyond.projects.is_empty());
        assert!(!beyond.has_more);
        assert_eq!(beyond.next_offset, None);
    }

    #[test]
    fn project_list_rejects_invalid_bounds_and_allows_maximum_page_size() {
        let mut store = SqliteStore::in_memory().unwrap();
        for limit in [0, 101, u32::MAX] {
            assert_eq!(
                list_imported_projects(&store, limit, 0).unwrap_err().code(),
                "APPLICATION_PROJECT_LIST_INVALID_LIMIT"
            );
        }
        assert_eq!(
            list_imported_projects(&store, 1, u32::MAX)
                .unwrap_err()
                .code(),
            "APPLICATION_PROJECT_LIST_INVALID_OFFSET"
        );
        assert!(
            list_imported_projects(&store, 100, 0)
                .unwrap()
                .projects
                .is_empty()
        );
        for index in 0..101 {
            create_metadata_project(&mut store, &format!("project-{index:03}"));
        }
        let page = list_imported_projects(&store, 100, 0).unwrap();
        assert_eq!(page.projects.len(), 100);
        assert!(page.has_more);
        assert_eq!(page.next_offset, Some(100));
        let last = list_imported_projects(&store, 100, 100).unwrap();
        assert_eq!(last.projects.len(), 1);
        assert!(!last.has_more);
    }
}
