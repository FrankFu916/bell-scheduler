//! Bounded scenario discovery; every open still requires full historical revalidation.

use class_schedule_domain::{ScenarioId, SchoolProjectId};
use class_schedule_persistence::{ScenarioSummary, SqliteStore};

use crate::ScenarioApplicationError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioListEntry {
    pub metadata: ScenarioSummary,
    pub openable_scenario_id: Option<ScenarioId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioListPage {
    pub scenarios: Vec<ScenarioListEntry>,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

/// Reads a metadata page. Opening any entry must use [`crate::load_scenario`].
///
/// Concurrent creations may reorder subsequent pages; refreshing starts at offset zero.
///
/// # Errors
/// Rejects limits outside 1..=100, overflowing offsets, and persistence failures.
pub fn list_project_scenarios(
    store: &SqliteStore,
    project_id: SchoolProjectId,
    limit: u32,
    offset: u32,
) -> Result<ScenarioListPage, ScenarioApplicationError> {
    if !(1..=100).contains(&limit) {
        return Err(ScenarioApplicationError::Invalid {
            code: "APPLICATION_SCENARIO_LIST_INVALID_LIMIT",
        });
    }
    let next = offset
        .checked_add(limit)
        .ok_or(ScenarioApplicationError::Invalid {
            code: "APPLICATION_SCENARIO_LIST_INVALID_OFFSET",
        })?;
    let mut entries = store.list_scenarios(&project_id.to_string(), limit + 1, offset)?;
    let has_more = entries.len() > limit as usize;
    entries.truncate(limit as usize);
    Ok(ScenarioListPage {
        scenarios: entries
            .into_iter()
            .map(|metadata| ScenarioListEntry {
                openable_scenario_id: metadata
                    .scenario_id
                    .parse::<ScenarioId>()
                    .ok()
                    .filter(|id| id.to_string() == metadata.scenario_id),
                metadata,
            })
            .collect(),
        has_more,
        next_offset: has_more.then_some(next),
    })
}

#[cfg(test)]
mod tests {
    use super::list_project_scenarios;
    use class_schedule_domain::SchoolProjectId;
    use class_schedule_persistence::SqliteStore;

    #[test]
    fn scenario_metadata_page_rejects_unbounded_requests_and_accepts_empty_project() {
        let store = SqliteStore::in_memory().unwrap();
        let id = "11111111-1111-4111-8111-111111111111"
            .parse::<SchoolProjectId>()
            .unwrap();
        for limit in [0, 101, u32::MAX] {
            assert_eq!(
                list_project_scenarios(&store, id, limit, 0)
                    .unwrap_err()
                    .code(),
                "APPLICATION_SCENARIO_LIST_INVALID_LIMIT"
            );
        }
        assert_eq!(
            list_project_scenarios(&store, id, 1, u32::MAX)
                .unwrap_err()
                .code(),
            "APPLICATION_SCENARIO_LIST_INVALID_OFFSET"
        );
        let page = list_project_scenarios(&store, id, 20, 0).unwrap();
        assert!(page.scenarios.is_empty());
        assert!(!page.has_more);
        assert_eq!(page.next_offset, None);
    }
}
