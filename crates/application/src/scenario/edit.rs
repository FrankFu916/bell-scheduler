//! A preview is evidence for a user, never authority for a later commit.

mod operation;

use class_schedule_domain::{MeetingAssignment, MeetingDemandId, Revision, ScenarioId, TimeslotId};
use class_schedule_persistence::{ScenarioRevisionExpectation, SqliteStore};
use class_schedule_scoring::ObjectiveVector;
use class_schedule_validation::ValidationReport;
use serde::{Deserialize, Serialize};

use super::{
    LoadedScenario, PreparedScenarioCreation, ScenarioApplicationError, ScenarioReceipt,
    ensure_current_source, invalid, load_scenario, parse_hash, version_two,
};

pub(super) use operation::{CandidateEdit, apply_operation, effective_problem, make_lock};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScenarioEditOperation {
    Move {
        activity_id: MeetingDemandId,
        start: TimeslotId,
        lock_after: bool,
    },
    SwapStarts {
        left: MeetingDemandId,
        right: MeetingDemandId,
    },
    LockCurrent {
        activity_id: MeetingDemandId,
    },
    Unlock {
        activity_id: MeetingDemandId,
    },
}

#[derive(Clone, Debug)]
pub struct ScenarioEditCommand {
    pub scenario_id: ScenarioId,
    pub expected_scenario_revision: Revision,
    pub expected_timetable_revision: Revision,
    pub operation: ScenarioEditOperation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScenarioEditStatus {
    Valid,
    NoChange,
    HardRejected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioActivityChange {
    pub activity_id: MeetingDemandId,
    pub before: MeetingAssignment,
    pub after: MeetingAssignment,
    pub was_user_locked: bool,
    pub is_user_locked: bool,
}

#[derive(Clone, Debug)]
pub struct ScenarioEditPreview {
    pub receipt: ScenarioReceipt,
    pub source_is_current: bool,
    pub status: ScenarioEditStatus,
    pub can_commit: bool,
    pub changes: Vec<ScenarioActivityChange>,
    pub validation: ValidationReport,
    pub before_quality: ObjectiveVector,
    pub after_quality: Option<ObjectiveVector>,
    pub context: crate::ScenarioEditContext,
}

#[derive(Clone, Debug)]
pub struct ScenarioEditCommitCommand {
    pub edit: ScenarioEditCommand,
    pub expected_scenario_payload_hash: String,
    pub expected_timetable_payload_hash: String,
}

/// Holds only application-revalidated content. Neither a preview nor transport JSON can create it.
#[derive(Debug)]
pub struct PreparedScenarioEdit {
    preview: ScenarioEditPreview,
    command: ScenarioEditCommitCommand,
    next: Option<PreparedScenarioCreation>,
    revision_created_at: chrono::DateTime<chrono::Utc>,
}

impl PreparedScenarioEdit {
    pub const fn preview(&self) -> &ScenarioEditPreview {
        &self.preview
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScenarioEditCommitStatus {
    Committed,
    NoChange,
}

#[derive(Clone, Debug)]
pub struct ScenarioEditCommitReceipt {
    pub status: ScenarioEditCommitStatus,
    pub receipt: ScenarioReceipt,
}

/// Revalidates a candidate against all Hard constraints, without changing any database bytes.
///
/// # Errors
/// Rejects unreadable scenarios, stale revisions and unknown stable activity/timeslot IDs.
pub fn preview_scenario_edit(
    store: &SqliteStore,
    command: &ScenarioEditCommand,
) -> Result<ScenarioEditPreview, ScenarioApplicationError> {
    let loaded = load_expected(store, command)?;
    let candidate = apply_operation(
        &loaded.compiled.problem,
        loaded.timetable(),
        loaded.user_locks(),
        &command.operation,
    )?;
    make_preview(&loaded, candidate)
}

/// Repeats the entire evaluation and compares both hashes; returned preview data is never trusted.
///
/// # Errors
/// Rejects stale source/revisions/hashes, invalid operations, Hard failures and revision overflow.
pub fn prepare_scenario_edit_commit(
    store: &SqliteStore,
    command: &ScenarioEditCommitCommand,
) -> Result<PreparedScenarioEdit, ScenarioApplicationError> {
    let loaded = load_expected(store, &command.edit)?;
    check_hashes(loaded.receipt(), command)?;
    require_current_source(store, loaded.receipt())?;
    let candidate = apply_operation(
        &loaded.compiled.problem,
        loaded.timetable(),
        loaded.user_locks(),
        &command.edit.operation,
    )?;
    let preview = make_preview(&loaded, candidate.clone())?;
    if preview.status == ScenarioEditStatus::HardRejected {
        return Err(invalid("APPLICATION_SCENARIO_EDIT_HARD_REJECTED"));
    }
    let (next, revision_created_at) = if preview.status == ScenarioEditStatus::NoChange {
        (None, loaded.receipt().created_at)
    } else {
        let (scenario, timetable) =
            version_two::edited_documents(&loaded, &command.edit.operation, candidate)?;
        let time = scenario
            .edit
            .as_ref()
            .ok_or_else(|| invalid("APPLICATION_SCENARIO_INVALID_EDIT"))?
            .revised_at;
        (
            Some(super::prepare_documents(&scenario, &timetable, None)?),
            time,
        )
    };
    Ok(PreparedScenarioEdit {
        preview,
        command: command.clone(),
        next,
        revision_created_at,
    })
}

/// Atomically appends the privately prepared state with source and dual revision/hash CAS.
///
/// # Errors
/// Any concurrent change or storage failure leaves both revision tables unchanged.
pub fn commit_prepared_scenario_edit(
    store: &mut SqliteStore,
    prepared: PreparedScenarioEdit,
) -> Result<ScenarioEditCommitReceipt, ScenarioApplicationError> {
    let Some(next) = prepared.next else {
        let loaded = load_expected(store, &prepared.command.edit)?;
        check_hashes(loaded.receipt(), &prepared.command)?;
        require_current_source(store, loaded.receipt())?;
        return Ok(ScenarioEditCommitReceipt {
            status: ScenarioEditCommitStatus::NoChange,
            receipt: loaded.receipt().clone(),
        });
    };
    let receipt = &prepared.preview.receipt;
    let expected = ScenarioRevisionExpectation {
        scenario_id: receipt.scenario_id.to_string(),
        expected_scenario_revision: receipt.scenario_revision,
        expected_scenario_payload_hash: parse_hash(&receipt.scenario_payload_hash)?,
        expected_timetable_id: receipt.timetable_id.to_string(),
        expected_timetable_revision: receipt.timetable_revision,
        expected_timetable_payload_hash: parse_hash(&receipt.timetable_payload_hash)?,
    };
    store.append_scenario_revision(&expected, &next.document, prepared.revision_created_at)?;
    Ok(ScenarioEditCommitReceipt {
        status: ScenarioEditCommitStatus::Committed,
        receipt: next.receipt,
    })
}

fn load_expected(
    store: &SqliteStore,
    command: &ScenarioEditCommand,
) -> Result<LoadedScenario, ScenarioApplicationError> {
    let loaded = load_scenario(store, command.scenario_id)?;
    if loaded.receipt().scenario_revision != command.expected_scenario_revision.get() {
        return Err(invalid("APPLICATION_SCENARIO_REVISION_CONFLICT"));
    }
    if loaded.receipt().timetable_revision != command.expected_timetable_revision.get() {
        return Err(invalid("APPLICATION_TIMETABLE_REVISION_CONFLICT"));
    }
    Ok(loaded)
}

fn check_hashes(
    receipt: &ScenarioReceipt,
    command: &ScenarioEditCommitCommand,
) -> Result<(), ScenarioApplicationError> {
    if parse_hash(&receipt.scenario_payload_hash)?
        != parse_hash(&command.expected_scenario_payload_hash)?
    {
        return Err(invalid("APPLICATION_SCENARIO_HASH_CONFLICT"));
    }
    if parse_hash(&receipt.timetable_payload_hash)?
        != parse_hash(&command.expected_timetable_payload_hash)?
    {
        return Err(invalid("APPLICATION_TIMETABLE_HASH_CONFLICT"));
    }
    Ok(())
}

fn require_current_source(
    store: &SqliteStore,
    receipt: &ScenarioReceipt,
) -> Result<(), ScenarioApplicationError> {
    ensure_current_source(
        store,
        receipt.project_id,
        receipt.source_project_revision,
        parse_hash(&receipt.source_payload_hash)?,
    )
}

fn make_preview(
    loaded: &LoadedScenario,
    candidate: CandidateEdit,
) -> Result<ScenarioEditPreview, ScenarioApplicationError> {
    let context = crate::timetable_read_model::scenario_edit_context(
        loaded,
        &candidate.changes,
        &candidate.validation,
    )
    .map_err(|error| invalid(error.code()))?;
    let status = if !candidate.validation.is_valid() {
        ScenarioEditStatus::HardRejected
    } else if candidate.changes.is_empty() {
        ScenarioEditStatus::NoChange
    } else {
        ScenarioEditStatus::Valid
    };
    Ok(ScenarioEditPreview {
        receipt: loaded.receipt().clone(),
        source_is_current: loaded.source_is_current(),
        status,
        can_commit: status == ScenarioEditStatus::Valid && loaded.source_is_current(),
        changes: candidate.changes,
        validation: candidate.validation,
        before_quality: loaded.quality().clone(),
        after_quality: candidate.quality,
        context,
    })
}
