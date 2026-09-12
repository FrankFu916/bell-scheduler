use class_schedule_domain::{Revision, ScenarioId};
use class_schedule_persistence::SqliteStore;
use class_schedule_scoring::ObjectiveVector;
use thiserror::Error;

use super::{
    TimetableEntityOption, TimetableGridCell, TimetableProjectionInput, TimetableQuery,
    TimetableQueryError, TimetableRow, TimetableView, count, projection, validate_page,
};
use crate::{LoadedScenario, ScenarioApplicationError, ScenarioReceipt, load_scenario};

pub const SCENARIO_TIMETABLE_READ_MODEL_SCHEMA_VERSION: u32 = 1;

/// A projection of this adopted scenario's own revalidated timetable and complete identity.
/// Transports map the receipt's revisions and quality integers to their explicit wire DTOs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioTimetablePage {
    pub schema_version: u32,
    pub receipt: ScenarioReceipt,
    pub source_is_current: bool,
    pub scenario_display_name: String,
    pub selection: TimetableEntityOption,
    pub rows: Vec<TimetableRow>,
    pub calendar: Vec<TimetableGridCell>,
    pub total_rows: u32,
    pub offset: u32,
    pub has_more: bool,
    pub next_offset: Option<u32>,
    pub quality: ObjectiveVector,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioTimetableEntityPage {
    pub schema_version: u32,
    pub receipt: ScenarioReceipt,
    pub source_is_current: bool,
    pub scenario_display_name: String,
    pub view: TimetableView,
    pub entities: Vec<TimetableEntityOption>,
    pub total_entities: u32,
    pub offset: u32,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

#[derive(Debug, Error)]
pub enum ScenarioTimetableQueryError {
    #[error(transparent)]
    Scenario(#[from] ScenarioApplicationError),
    #[error(transparent)]
    Query(#[from] TimetableQueryError),
    #[error("scenario revision changed: expected {expected}, actual {actual}")]
    ScenarioRevisionConflict {
        expected: Revision,
        actual: Revision,
    },
    #[error("timetable revision changed: expected {expected}, actual {actual}")]
    TimetableRevisionConflict {
        expected: Revision,
        actual: Revision,
    },
}

impl ScenarioTimetableQueryError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Scenario(error) => error.code(),
            Self::Query(error) => error.code(),
            Self::ScenarioRevisionConflict { .. } => "APPLICATION_SCENARIO_REVISION_CONFLICT",
            Self::TimetableRevisionConflict { .. } => "APPLICATION_TIMETABLE_REVISION_CONFLICT",
        }
    }
}

/// Projects an adopted scenario after full historical revalidation and exact revision checks.
///
/// This is read-only and starts no worker. An outdated import source remains readable and is
/// marked by `source_is_current`; a different scenario/timetable revision is never substituted.
///
/// # Errors
/// Rejects invalid pagination, damaged or missing scenarios, mismatched revisions and unknown
/// filter entities. All existing enrollment, Hard-validation and scoring checks remain active.
pub fn query_scenario_timetable(
    store: &SqliteStore,
    scenario_id: ScenarioId,
    expected_scenario_revision: Revision,
    expected_timetable_revision: Revision,
    query: &TimetableQuery,
) -> Result<ScenarioTimetablePage, ScenarioTimetableQueryError> {
    let next = validate_page(query.limit, query.offset)?;
    let loaded = load_expected_scenario(
        store,
        scenario_id,
        expected_scenario_revision,
        expected_timetable_revision,
    )?;
    let input = projection_input(&loaded);
    let selection = projection::entities(&input, query.filter.view())
        .into_iter()
        .find(|option| option.filter == query.filter)
        .ok_or(TimetableQueryError::EntityNotFound {
            view: query.filter.view(),
        })?;
    let (rows, calendar, total_rows) = projection::project(&input, query)?;
    let has_more = next < total_rows;
    Ok(ScenarioTimetablePage {
        schema_version: SCENARIO_TIMETABLE_READ_MODEL_SCHEMA_VERSION,
        receipt: loaded.receipt().clone(),
        source_is_current: loaded.source_is_current(),
        scenario_display_name: loaded.display_name().to_owned(),
        selection,
        rows,
        calendar,
        total_rows,
        offset: query.offset,
        has_more,
        next_offset: has_more.then_some(next),
        quality: loaded.quality().clone(),
    })
}

/// Lists one view's labels from the exact revalidated scenario and timetable revisions.
///
/// # Errors
/// Rejects invalid pagination, missing/damaged scenarios, and either revision mismatch.
pub fn query_scenario_timetable_entities(
    store: &SqliteStore,
    scenario_id: ScenarioId,
    expected_scenario_revision: Revision,
    expected_timetable_revision: Revision,
    view: TimetableView,
    offset: u32,
    limit: u32,
) -> Result<ScenarioTimetableEntityPage, ScenarioTimetableQueryError> {
    let next = validate_page(limit, offset)?;
    let loaded = load_expected_scenario(
        store,
        scenario_id,
        expected_scenario_revision,
        expected_timetable_revision,
    )?;
    let entities = projection::entities(&projection_input(&loaded), view);
    let total_entities = count(entities.len())?;
    let has_more = next < total_entities;
    Ok(ScenarioTimetableEntityPage {
        schema_version: SCENARIO_TIMETABLE_READ_MODEL_SCHEMA_VERSION,
        receipt: loaded.receipt().clone(),
        source_is_current: loaded.source_is_current(),
        scenario_display_name: loaded.display_name().to_owned(),
        view,
        entities: entities
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect(),
        total_entities,
        offset,
        has_more,
        next_offset: has_more.then_some(next),
    })
}

fn load_expected_scenario(
    store: &SqliteStore,
    scenario_id: ScenarioId,
    expected_scenario_revision: Revision,
    expected_timetable_revision: Revision,
) -> Result<LoadedScenario, ScenarioTimetableQueryError> {
    // Revision checks follow complete load: a stale caller must not hide a corrupt payload.
    let loaded = load_scenario(store, scenario_id)?;
    let receipt = loaded.receipt();
    let actual = Revision::from_u64(receipt.scenario_revision);
    if actual != expected_scenario_revision {
        return Err(ScenarioTimetableQueryError::ScenarioRevisionConflict {
            expected: expected_scenario_revision,
            actual,
        });
    }
    let actual = Revision::from_u64(receipt.timetable_revision);
    if actual != expected_timetable_revision {
        return Err(ScenarioTimetableQueryError::TimetableRevisionConflict {
            expected: expected_timetable_revision,
            actual,
        });
    }
    Ok(loaded)
}

fn projection_input(loaded: &LoadedScenario) -> TimetableProjectionInput<'_> {
    TimetableProjectionInput {
        source: loaded.source_document(),
        compiled: loaded.compiled(),
        assignments: loaded.assignments(),
        sections: loaded.materialized_sectioning().map_or_else(
            || loaded.source_document().import_batch.teaching_sections(),
            |materialized| materialized.sections.as_slice(),
        ),
    }
}
