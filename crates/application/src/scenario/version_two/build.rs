use chrono::{DateTime, SubsecRound, Utc};
use class_schedule_domain::{ScenarioId, TimetableId};

use super::super::document::{
    ScenarioPayload, TimetablePayload, hash_meetings, to_meetings, validate_name,
};
use super::super::edit::{CandidateEdit, make_lock};
use super::super::{
    LoadedScenario, SCENARIO_EDIT_DOCUMENT_SCHEMA_VERSION, ScenarioApplicationError,
    ScenarioCloneLineage, ScenarioEditOperation, invalid, parse_hash,
};
use super::{EDIT_SEMANTICS_VERSION, EditMetadata};

pub(in crate::scenario) fn edited_documents(
    loaded: &LoadedScenario,
    operation: &ScenarioEditOperation,
    candidate: CandidateEdit,
) -> Result<(ScenarioPayload, TimetablePayload), ScenarioApplicationError> {
    let receipt = loaded.receipt();
    let previous = ScenarioCloneLineage {
        scenario_id: receipt.scenario_id,
        scenario_revision: receipt.scenario_revision,
        scenario_payload_hash: parse_hash(&receipt.scenario_payload_hash)?,
        timetable_id: receipt.timetable_id,
        timetable_revision: receipt.timetable_revision,
        timetable_payload_hash: parse_hash(&receipt.timetable_payload_hash)?,
    };
    next_documents(
        &loaded.scenario_payload,
        &loaded.timetable_payload,
        previous,
        operation,
        candidate,
        Utc::now().trunc_subsecs(3),
    )
}

pub(super) fn next_documents(
    scenario: &ScenarioPayload,
    timetable: &TimetablePayload,
    previous: ScenarioCloneLineage,
    operation: &ScenarioEditOperation,
    candidate: CandidateEdit,
    revised_at: DateTime<Utc>,
) -> Result<(ScenarioPayload, TimetablePayload), ScenarioApplicationError> {
    if !candidate.validation.is_valid() || candidate.changes.is_empty() {
        return Err(invalid("APPLICATION_SCENARIO_INVALID_EDIT_EDGE"));
    }
    let mut scenario = scenario.clone();
    let mut timetable = timetable.clone();
    scenario.schema_version = SCENARIO_EDIT_DOCUMENT_SCHEMA_VERSION;
    scenario.scenario_revision = next_revision(scenario.scenario_revision)?;
    scenario.edit = Some(EditMetadata {
        semantics_version: EDIT_SEMANTICS_VERSION.to_owned(),
        previous_revision: Some(previous),
        operation: Some(operation.clone()),
        revised_at,
    });
    timetable.schema_version = SCENARIO_EDIT_DOCUMENT_SCHEMA_VERSION;
    timetable.scenario_revision = scenario.scenario_revision;
    timetable.timetable_revision = next_revision(timetable.timetable_revision)?;
    timetable.assignment_hash = hash_meetings(&candidate.meetings)?;
    timetable.meetings = candidate.meetings;
    timetable.user_locks = Some(candidate.locks);
    timetable.quality = candidate
        .quality
        .ok_or_else(|| invalid("APPLICATION_SCENARIO_SCORING_FAILED"))?;
    Ok((scenario, timetable))
}

pub(in crate::scenario) fn copy_documents(
    loaded: &LoadedScenario,
    scenario_id: ScenarioId,
    name: &str,
    lineage: ScenarioCloneLineage,
) -> Result<(ScenarioPayload, TimetablePayload), ScenarioApplicationError> {
    validate_name(name)?;
    if scenario_id.as_uuid().is_nil() {
        return Err(invalid("APPLICATION_SCENARIO_INVALID_ID"));
    }
    let mut scenario = loaded.scenario_payload.clone();
    let mut timetable = loaded.timetable_payload.clone();
    let timetable_id = TimetableId::new_v4();
    let meetings = to_meetings(&loaded.compiled.problem, &loaded.assignments, timetable_id)?;
    let locked_ids = loaded
        .user_locks()
        .iter()
        .map(|lock| lock.scheduled_meeting_id())
        .collect::<std::collections::BTreeSet<_>>();
    let old_locked = loaded
        .timetable()
        .meetings()
        .iter()
        .filter(|meeting| locked_ids.contains(&meeting.id()))
        .map(|meeting| meeting.demand_id())
        .collect::<std::collections::BTreeSet<_>>();
    let mut locks = meetings
        .iter()
        .filter(|meeting| old_locked.contains(&meeting.demand_id()))
        .map(|meeting| make_lock(timetable_id, *meeting))
        .collect::<Vec<_>>();
    locks.sort_by_key(|lock| lock.id());
    scenario.scenario_id = scenario_id;
    name.clone_into(&mut scenario.display_name);
    scenario.scenario_revision = 0;
    scenario.lineage = Some(lineage);
    scenario.created_at = Utc::now().trunc_subsecs(3);
    scenario.edit = Some(EditMetadata {
        semantics_version: EDIT_SEMANTICS_VERSION.to_owned(),
        previous_revision: None,
        operation: None,
        revised_at: scenario.created_at,
    });
    timetable.scenario_id = scenario_id;
    timetable.scenario_revision = 0;
    timetable.timetable_id = timetable_id;
    timetable.timetable_revision = 0;
    timetable.meetings = meetings;
    timetable.user_locks = Some(locks);
    timetable.assignment_hash = hash_meetings(&timetable.meetings)?;
    timetable.quality = loaded.quality().clone();
    Ok((scenario, timetable))
}

fn next_revision(revision: u64) -> Result<u64, ScenarioApplicationError> {
    revision
        .checked_add(1)
        .filter(|revision| i64::try_from(*revision).is_ok())
        .ok_or_else(|| invalid("APPLICATION_SCENARIO_REVISION_OVERFLOW"))
}
