//! Complete edited states plus one independently checked predecessor edge, never a recursive log.

mod build;

pub(super) use build::{copy_documents, edited_documents};

use chrono::{DateTime, Utc};
use class_schedule_domain::{Revision, Timetable};
use class_schedule_persistence::{SqliteStore, StoredScenario};
use class_schedule_scoring::{ObjectivePlan, ScoringContext, evaluate};
use class_schedule_validation::validate_assignments;
use serde::{Deserialize, Serialize};

use super::document::{
    ReplayedTimetable, ScenarioPayload, Selected, TimetablePayload, decode_documents,
    from_meetings, hash_meetings, replay_timetable, to_meetings,
};
use super::edit::{apply_operation, effective_problem};
use super::{ScenarioApplicationError, ScenarioCloneLineage, ScenarioEditOperation, invalid};
use crate::LoadedSolveArtifact;

const EDIT_SEMANTICS_VERSION: &str = "scenario-single-edit-v1";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::scenario) struct EditMetadata {
    pub semantics_version: String,
    pub previous_revision: Option<ScenarioCloneLineage>,
    pub operation: Option<ScenarioEditOperation>,
    pub revised_at: DateTime<Utc>,
}

pub(super) fn validate_shape(
    stored: &StoredScenario,
    scenario: &ScenarioPayload,
    timetable: &TimetablePayload,
) -> Result<(), ScenarioApplicationError> {
    let edit = scenario
        .edit
        .as_ref()
        .ok_or_else(|| invalid("APPLICATION_SCENARIO_INVALID_EDIT"))?;
    if edit.semantics_version != EDIT_SEMANTICS_VERSION || timetable.user_locks.is_none() {
        return Err(invalid("APPLICATION_SCENARIO_UNSUPPORTED_EDIT_SEMANTICS"));
    }
    if edit.revised_at != stored.revision_created_at || edit.revised_at < scenario.created_at {
        return Err(invalid("APPLICATION_SCENARIO_METADATA_MISMATCH"));
    }
    if scenario.scenario_revision != timetable.timetable_revision {
        return Err(invalid("APPLICATION_SCENARIO_UNSUPPORTED_REVISION"));
    }
    if scenario.lineage.as_ref().is_some_and(|lineage| {
        lineage.scenario_revision != lineage.timetable_revision
            || lineage.scenario_revision > i64::MAX as u64
    }) {
        return Err(invalid("APPLICATION_SCENARIO_INVALID_LINEAGE"));
    }
    if scenario.scenario_revision == 0 {
        if scenario.lineage.is_none()
            || edit.previous_revision.is_some()
            || edit.operation.is_some()
            || edit.revised_at != scenario.created_at
        {
            return Err(invalid("APPLICATION_SCENARIO_INVALID_EDIT"));
        }
    } else {
        let previous = edit
            .previous_revision
            .as_ref()
            .ok_or_else(|| invalid("APPLICATION_SCENARIO_INVALID_EDIT"))?;
        if edit.operation.is_none()
            || previous.scenario_id != scenario.scenario_id
            || previous.timetable_id != timetable.timetable_id
            || previous.scenario_revision.checked_add(1) != Some(scenario.scenario_revision)
            || previous.timetable_revision.checked_add(1) != Some(timetable.timetable_revision)
        {
            return Err(invalid("APPLICATION_SCENARIO_INVALID_EDIT"));
        }
    }
    Ok(())
}

pub(super) fn replay_state(
    selected: &Selected<'_>,
    scenario: &ScenarioPayload,
    timetable: &TimetablePayload,
) -> Result<ReplayedTimetable, ScenarioApplicationError> {
    let problem = &selected.compiled.problem;
    let assignments = from_meetings(problem, &timetable.meetings, timetable.timetable_id)?;
    let origin = selected
        .completed
        .assignments
        .iter()
        .map(|assignment| (assignment.activity, assignment))
        .collect::<std::collections::BTreeMap<_, _>>();
    if assignments.iter().any(|assignment| {
        origin.get(&assignment.activity).is_none_or(|original| {
            original.teacher != assignment.teacher || original.room != assignment.room
        })
    }) {
        return Err(invalid("APPLICATION_SCENARIO_FIXED_RESOURCES_CHANGED"));
    }
    let locks = timetable
        .user_locks
        .as_deref()
        .ok_or_else(|| invalid("APPLICATION_SCENARIO_INVALID_USER_LOCK"))?;
    if locks.windows(2).any(|pair| pair[0].id() >= pair[1].id()) {
        return Err(invalid("APPLICATION_SCENARIO_INVALID_USER_LOCK"));
    }
    let effective = effective_problem(problem, timetable.timetable_id, &timetable.meetings, locks)?;
    if !validate_assignments(&effective, &assignments).is_valid() {
        return Err(invalid("APPLICATION_SCENARIO_HARD_VALIDATION_FAILED"));
    }
    let quality = evaluate(
        &effective,
        &assignments,
        &ObjectivePlan::balanced_default(),
        &ScoringContext::neutral(&effective),
    )
    .map_err(|_| invalid("APPLICATION_SCENARIO_SCORING_FAILED"))?;
    if quality != timetable.quality {
        return Err(invalid("APPLICATION_SCENARIO_QUALITY_MISMATCH"));
    }
    if hash_meetings(&timetable.meetings)? != timetable.assignment_hash
        || timetable.meetings != to_meetings(problem, &assignments, timetable.timetable_id)?
    {
        return Err(invalid("APPLICATION_SCENARIO_ASSIGNMENT_MISMATCH"));
    }
    let domain = Timetable::restore(
        timetable.timetable_id,
        scenario.scenario_id,
        Revision::from_u64(timetable.timetable_revision),
        timetable.meetings.clone(),
    )
    .map_err(|_| invalid("APPLICATION_SCENARIO_INVALID_TIMETABLE"))?;
    Ok((selected.compiled.clone(), assignments, domain, quality))
}

pub(super) fn validate_previous_edge(
    store: &SqliteStore,
    artifact: &LoadedSolveArtifact,
    scenario: &ScenarioPayload,
    timetable: &TimetablePayload,
) -> Result<(), ScenarioApplicationError> {
    let Some(edit) = &scenario.edit else {
        return Ok(());
    };
    let Some(previous) = &edit.previous_revision else {
        return Ok(());
    };
    let stored = store.load_scenario_revision(
        &previous.scenario_id.to_string(),
        previous.scenario_revision,
        previous.timetable_revision,
    )?;
    if stored.scenario_payload_hash != previous.scenario_payload_hash
        || stored.timetable_payload_hash != previous.timetable_payload_hash
    {
        return Err(invalid("APPLICATION_SCENARIO_PREDECESSOR_HASH_MISMATCH"));
    }
    let (previous_scenario, previous_timetable) = decode_documents(&stored)?;
    // Only the direct predecessor state is replayed. Its own predecessor edge is intentionally not followed.
    let (compiled, _, domain, _) =
        replay_timetable(artifact, &previous_scenario, &previous_timetable)?;
    let operation = edit
        .operation
        .as_ref()
        .ok_or_else(|| invalid("APPLICATION_SCENARIO_INVALID_EDIT"))?;
    let candidate = apply_operation(
        &compiled.problem,
        &domain,
        previous_timetable.user_locks.as_deref().unwrap_or_default(),
        operation,
    )?;
    if !candidate.validation.is_valid()
        || candidate.changes.is_empty()
        || edit.revised_at < stored.revision_created_at
    {
        return Err(invalid("APPLICATION_SCENARIO_INVALID_EDIT_EDGE"));
    }
    let (expected_scenario, expected_timetable) = build::next_documents(
        &previous_scenario,
        &previous_timetable,
        previous.clone(),
        operation,
        candidate,
        edit.revised_at,
    )?;
    if &expected_scenario != scenario || &expected_timetable != timetable {
        return Err(invalid("APPLICATION_SCENARIO_INVALID_EDIT_EDGE"));
    }
    Ok(())
}
