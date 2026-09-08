//! Local, independently revalidated timetable projections shared by transports and exports.

use class_schedule_domain::{
    AdministrativeClassId, CourseOfferingId, CoursePlanId, Day, GradeId, MeetingDemandId, RoomId,
    SchoolProjectId, StudentId, SubjectId, TeacherId, TeachingSectionId,
};
use class_schedule_import::TeachingSectionImportRow;
use class_schedule_persistence::SqliteStore;
use class_schedule_scoring::ObjectiveVector;
use serde::{Deserialize, Serialize};
use solver_client::SolverRunStatus;
use thiserror::Error;

use crate::{
    AutoSectioningSolveStatus, CompiledSchoolProblem, CompletedSolve, ImportedProjectSolve,
    LoadedSolveArtifact, SolveArtifactError, SolveExecution, load_solve_artifact,
};

mod projection;

pub const TIMETABLE_READ_MODEL_SCHEMA_VERSION: u32 = 1;
pub const MAXIMUM_TIMETABLE_PAGE_SIZE: u32 = 100;
const MAXIMUM_TIMETABLE_GRID_CELLS: usize = 4096;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimetableView {
    AdministrativeClass,
    TeachingSection,
    Teacher,
    Room,
    Student,
    Subject,
    Grade,
}

/// A selection is mandatory; an absent or unknown ID never falls back to a broader audience.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "view", content = "id", rename_all = "snake_case")]
pub enum TimetableFilter {
    AdministrativeClass(AdministrativeClassId),
    TeachingSection(TeachingSectionId),
    Teacher(TeacherId),
    Room(RoomId),
    Student(StudentId),
    Subject(SubjectId),
    Grade(GradeId),
}

impl TimetableFilter {
    #[must_use]
    pub const fn view(self) -> TimetableView {
        match self {
            Self::AdministrativeClass(_) => TimetableView::AdministrativeClass,
            Self::TeachingSection(_) => TimetableView::TeachingSection,
            Self::Teacher(_) => TimetableView::Teacher,
            Self::Room(_) => TimetableView::Room,
            Self::Student(_) => TimetableView::Student,
            Self::Subject(_) => TimetableView::Subject,
            Self::Grade(_) => TimetableView::Grade,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TimetableQuery {
    pub filter: TimetableFilter,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TimetableEntityOption {
    pub filter: TimetableFilter,
    pub code: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TimetableEntityPage {
    pub schema_version: u32,
    pub run_id: String,
    pub view: TimetableView,
    pub entities: Vec<TimetableEntityOption>,
    pub total_entities: u32,
    pub offset: u32,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TimetableLabel<Id> {
    pub id: Id,
    pub code: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "entity", rename_all = "snake_case")]
pub enum TimetableAudience {
    AdministrativeClass(TimetableLabel<AdministrativeClassId>),
    TeachingSection(TimetableLabel<TeachingSectionId>),
}

/// One meeting, not one occupied period. Indices are zero-based snapshot-local indices.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TimetableRow {
    pub activity_id: MeetingDemandId,
    pub activity_index: u32,
    pub course_offering_id: CourseOfferingId,
    pub meeting_ordinal: u16,
    pub start_timeslot_index: u32,
    pub day: Day,
    pub day_label: String,
    pub period_index: u16,
    pub period_label: String,
    pub duration_periods: u8,
    pub occupied_timeslot_indices: Vec<u32>,
    pub grade: TimetableLabel<GradeId>,
    pub subject: TimetableLabel<SubjectId>,
    pub course_plan: TimetableLabel<CoursePlanId>,
    pub audience: TimetableAudience,
    pub teacher: TimetableLabel<TeacherId>,
    pub room: TimetableLabel<RoomId>,
    pub student_count: u32,
}

/// Every Calendar slot is present, including empty slots and slots whose rows are on other pages.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TimetableGridCell {
    pub timeslot_index: u32,
    pub day: Day,
    pub day_label: String,
    pub period_index: u16,
    pub period_label: String,
    pub instructional_block: u8,
    /// Count across the entire filtered timetable, before row pagination.
    pub occupied_count: u32,
    /// Only IDs of rows included on this page; an empty list does not imply an empty slot.
    pub page_activity_ids: Vec<MeetingDemandId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SavedTimetablePage {
    pub schema_version: u32,
    pub run_id: String,
    pub project_id: SchoolProjectId,
    /// Decimal text preserves the `SQLite` revision across JavaScript transports.
    pub project_revision: String,
    pub project_display_name: String,
    pub source_payload_hash: String,
    pub artifact_payload_hash: String,
    pub input_snapshot_hash: String,
    pub output_hash: String,
    pub adopted: bool,
    pub selected_attempt_index: Option<u32>,
    pub selection: TimetableEntityOption,
    pub rows: Vec<TimetableRow>,
    pub calendar: Vec<TimetableGridCell>,
    pub total_rows: u32,
    pub offset: u32,
    pub has_more: bool,
    pub next_offset: Option<u32>,
    /// Independently recomputed on artifact load; it describes the whole selected timetable.
    pub quality: ObjectiveVector,
}

#[derive(Debug, Error)]
pub enum TimetableQueryError {
    #[error(transparent)]
    Artifact(#[from] SolveArtifactError),
    #[error("timetable page limit or offset is invalid")]
    InvalidPage,
    #[error("selected timetable entity does not exist in this run")]
    EntityNotFound { view: TimetableView },
    #[error("this saved run has no independently validated selected timetable")]
    Unavailable,
    #[error("timetable read model exceeds its resource limit")]
    ResourceLimit,
    #[error("revalidated timetable references cannot be projected: {field}")]
    Inconsistent { field: &'static str },
}

impl TimetableQueryError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Artifact(error) => error.code(),
            Self::InvalidPage => "APPLICATION_TIMETABLE_INVALID_PAGE",
            Self::EntityNotFound { .. } => "APPLICATION_TIMETABLE_ENTITY_NOT_FOUND",
            Self::Unavailable => "APPLICATION_TIMETABLE_UNAVAILABLE",
            Self::ResourceLimit => "APPLICATION_TIMETABLE_RESOURCE_LIMIT",
            Self::Inconsistent { .. } => "APPLICATION_TIMETABLE_READ_MODEL_INVALID",
        }
    }
}

/// Queries one explicitly selected view of an immutable saved run, without writes or a worker.
///
/// Loading first reconstructs the exact historical import/sectioning and independently validates
/// and scores every attempt. Administrative-class and grade views intersect actual student
/// audiences, so they include the students' walking-class activities. No student roster is returned.
///
/// # Errors
/// Rejects invalid pagination, missing entities, non-success runs, and any artifact replay error.
pub fn query_saved_timetable(
    store: &SqliteStore,
    run_id: &str,
    query: &TimetableQuery,
) -> Result<SavedTimetablePage, TimetableQueryError> {
    let next = validate_page(query.limit, query.offset)?;
    let loaded = load_solve_artifact(store, run_id)?;
    let selected = selected_timetable(&loaded)?;
    let selection = projection::entities(&loaded, &selected, query.filter.view())
        .into_iter()
        .find(|option| option.filter == query.filter)
        .ok_or(TimetableQueryError::EntityNotFound {
            view: query.filter.view(),
        })?;
    let (rows, calendar, total_rows) = projection::project(&loaded, &selected, query)?;
    let has_more = next < total_rows;
    Ok(SavedTimetablePage {
        schema_version: TIMETABLE_READ_MODEL_SCHEMA_VERSION,
        run_id: loaded.receipt.run_id.clone(),
        project_id: loaded.receipt.source.project_id,
        project_revision: loaded.receipt.source.revision.to_string(),
        project_display_name: loaded.display_name.clone(),
        source_payload_hash: loaded.receipt.source.payload_hash.clone(),
        artifact_payload_hash: loaded.receipt.payload_hash.clone(),
        input_snapshot_hash: blake3::Hash::from(selected.completed.snapshot_hash)
            .to_hex()
            .to_string(),
        output_hash: blake3::Hash::from(
            selected
                .completed
                .output_hash
                .ok_or(TimetableQueryError::Unavailable)?,
        )
        .to_hex()
        .to_string(),
        adopted: false,
        selected_attempt_index: selected.attempt_index.map(count).transpose()?,
        selection,
        rows,
        calendar,
        total_rows,
        offset: query.offset,
        has_more,
        next_offset: has_more.then_some(next),
        quality: selected.quality.clone(),
    })
}

/// Lists only the requested view's entity labels, with stable code ordering and bounded pagination.
/// Student names are present only when the caller explicitly requests the Student options.
///
/// # Errors
/// Rejects invalid pagination, runs without a selected timetable, and artifact replay failures.
pub fn query_saved_timetable_entities(
    store: &SqliteStore,
    run_id: &str,
    view: TimetableView,
    offset: u32,
    limit: u32,
) -> Result<TimetableEntityPage, TimetableQueryError> {
    let next = validate_page(limit, offset)?;
    let loaded = load_solve_artifact(store, run_id)?;
    let selected = selected_timetable(&loaded)?;
    let entities = projection::entities(&loaded, &selected, view);
    let total_entities = count(entities.len())?;
    let has_more = next < total_entities;
    Ok(TimetableEntityPage {
        schema_version: TIMETABLE_READ_MODEL_SCHEMA_VERSION,
        run_id: loaded.receipt.run_id,
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

struct SelectedTimetable<'a> {
    compiled: &'a CompiledSchoolProblem,
    completed: &'a CompletedSolve,
    quality: &'a ObjectiveVector,
    sections: &'a [TeachingSectionImportRow],
    attempt_index: Option<usize>,
}

fn selected_timetable(
    loaded: &LoadedSolveArtifact,
) -> Result<SelectedTimetable<'_>, TimetableQueryError> {
    let result = loaded.result().ok_or(TimetableQueryError::Unavailable)?;
    let (compiled, execution, quality, sections, attempt_index) = match &result.result {
        ImportedProjectSolve::Existing(existing) => (
            &existing.compiled,
            &existing.execution,
            existing.quality.as_ref(),
            loaded.source_document().import_batch.teaching_sections(),
            None,
        ),
        ImportedProjectSolve::AutoSectioned(sectioned) => {
            if sectioned.status != AutoSectioningSolveStatus::SelectedFeasible {
                return Err(TimetableQueryError::Unavailable);
            }
            let attempt = sectioned
                .selected_attempt()
                .ok_or(TimetableQueryError::Unavailable)?;
            (
                &attempt.compiled,
                &attempt.execution,
                attempt.quality.as_ref(),
                attempt.sectioning.generated_sections(),
                sectioned.selected_attempt_index,
            )
        }
    };
    let SolveExecution::Completed(completed) = execution else {
        return Err(TimetableQueryError::Unavailable);
    };
    if !matches!(
        completed.status,
        SolverRunStatus::Feasible | SolverRunStatus::Optimal
    ) || !completed
        .independent_validation
        .as_ref()
        .is_some_and(class_schedule_validation::ValidationReport::is_valid)
    {
        return Err(TimetableQueryError::Unavailable);
    }
    Ok(SelectedTimetable {
        compiled,
        completed,
        quality: quality.ok_or(TimetableQueryError::Unavailable)?,
        sections,
        attempt_index,
    })
}

fn validate_page(limit: u32, offset: u32) -> Result<u32, TimetableQueryError> {
    if !(1..=MAXIMUM_TIMETABLE_PAGE_SIZE).contains(&limit) {
        return Err(TimetableQueryError::InvalidPage);
    }
    offset
        .checked_add(limit)
        .ok_or(TimetableQueryError::InvalidPage)
}

fn count(value: usize) -> Result<u32, TimetableQueryError> {
    u32::try_from(value).map_err(|_| TimetableQueryError::ResourceLimit)
}

fn required<T>(value: Option<T>, field: &'static str) -> Result<T, TimetableQueryError> {
    value.ok_or(TimetableQueryError::Inconsistent { field })
}
