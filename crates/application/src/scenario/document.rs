use std::collections::BTreeMap;

use chrono::{DateTime, SubsecRound, Utc};
use class_schedule_domain::{
    Lock, MeetingAssignment, MeetingDuration, Name, ScenarioId, ScheduledMeeting,
    ScheduledMeetingId, SchoolProjectId, SolverRunId, Timetable, TimetableId,
};
use class_schedule_persistence::StoredScenario;
use class_schedule_scheduling::{
    ActivityIndex, Assignment, RoomIndex, SchedulingProblemSnapshot, TeacherIndex, TimeslotIndex,
};
use class_schedule_scoring::{ObjectivePlan, ObjectiveVector, ScoringContext, evaluate};
use class_schedule_validation::validate_assignments;
use serde::{Deserialize, Serialize};
use solver_client::SolverRunStatus;

use super::{
    SCENARIO_DOCUMENT_SCHEMA_VERSION, SCENARIO_EDIT_DOCUMENT_SCHEMA_VERSION,
    ScenarioApplicationError, ScenarioCloneLineage, TIMETABLE_DOCUMENT_SCHEMA_VERSION, invalid,
    parse_hash,
};
use crate::compile::stable_id;
use crate::{
    AutoSectioningSolveStatus, CompiledSchoolProblem, CompletedSolve, ImportedProjectSolve,
    LoadedSolveArtifact, MaterializedSectioning, SOLVE_ARTIFACT_SEMANTICS_VERSION, SolveExecution,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScenarioPayload {
    pub schema_version: u32,
    pub semantics_version: String,
    pub scenario_id: ScenarioId,
    pub project_id: SchoolProjectId,
    pub display_name: String,
    pub scenario_revision: u64,
    pub source_project_revision: u64,
    pub source_payload_hash: [u8; 32],
    pub origin_run_id: SolverRunId,
    pub origin_artifact_hash: [u8; 32],
    pub selected_attempt_index: Option<usize>,
    pub materialized_sectioning: Option<MaterializedSectioning>,
    pub lineage: Option<ScenarioCloneLineage>,
    pub created_at: DateTime<Utc>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present"
    )]
    pub edit: Option<super::version_two::EditMetadata>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TimetablePayload {
    pub schema_version: u32,
    pub scenario_id: ScenarioId,
    pub scenario_revision: u64,
    pub timetable_id: TimetableId,
    pub timetable_revision: u64,
    pub snapshot_hash: [u8; 32],
    pub assignment_hash: [u8; 32],
    pub meetings: Vec<ScheduledMeeting>,
    pub quality: ObjectiveVector,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present"
    )]
    pub user_locks: Option<Vec<Lock>>,
}

pub(super) struct Selected<'a> {
    pub compiled: &'a CompiledSchoolProblem,
    pub completed: &'a CompletedSolve,
    pub quality: &'a ObjectiveVector,
    materialized: Option<MaterializedSectioning>,
    index: Option<usize>,
}

fn selected(loaded: &LoadedSolveArtifact) -> Result<Selected<'_>, ScenarioApplicationError> {
    let result = loaded
        .result()
        .ok_or_else(|| invalid("APPLICATION_SCENARIO_RUN_NOT_ADOPTABLE"))?;
    let (compiled, execution, quality, materialized, index) = match &result.result {
        ImportedProjectSolve::Existing(existing) => (
            &existing.compiled,
            &existing.execution,
            existing.quality.as_ref(),
            None,
            None,
        ),
        ImportedProjectSolve::AutoSectioned(sectioned) => {
            if sectioned.status != AutoSectioningSolveStatus::SelectedFeasible {
                return Err(invalid("APPLICATION_SCENARIO_RUN_NOT_ADOPTABLE"));
            }
            let attempt = sectioned
                .selected_attempt()
                .ok_or_else(|| invalid("APPLICATION_SCENARIO_RUN_NOT_ADOPTABLE"))?;
            (
                &attempt.compiled,
                &attempt.execution,
                attempt.quality.as_ref(),
                Some(MaterializedSectioning {
                    candidate_hash: attempt.sectioning.candidate().provenance.candidate_hash,
                    sections: attempt.sectioning.generated_sections().to_vec(),
                    enrollments: attempt.sectioning.generated_enrollments().to_vec(),
                }),
                sectioned.selected_attempt_index,
            )
        }
    };
    let SolveExecution::Completed(completed) = execution else {
        return Err(invalid("APPLICATION_SCENARIO_RUN_NOT_ADOPTABLE"));
    };
    if !matches!(
        completed.status,
        SolverRunStatus::Feasible | SolverRunStatus::Optimal
    ) || !completed
        .independent_validation
        .as_ref()
        .is_some_and(class_schedule_validation::ValidationReport::is_valid)
    {
        return Err(invalid("APPLICATION_SCENARIO_RUN_NOT_ADOPTABLE"));
    }
    Ok(Selected {
        compiled,
        completed,
        quality: quality.ok_or_else(|| invalid("APPLICATION_SCENARIO_RUN_NOT_ADOPTABLE"))?,
        materialized,
        index,
    })
}

pub(super) fn build_documents(
    loaded: &LoadedSolveArtifact,
    scenario_id: ScenarioId,
    name: &str,
    lineage: Option<ScenarioCloneLineage>,
) -> Result<(ScenarioPayload, TimetablePayload), ScenarioApplicationError> {
    validate_name(name)?;
    if scenario_id.as_uuid().is_nil() {
        return Err(invalid("APPLICATION_SCENARIO_INVALID_ID"));
    }
    let selected = selected(loaded)?;
    let timetable_id = TimetableId::new_v4();
    let meetings = to_meetings(
        &selected.compiled.problem,
        &selected.completed.assignments,
        timetable_id,
    )?;
    let scenario = ScenarioPayload {
        schema_version: SCENARIO_DOCUMENT_SCHEMA_VERSION,
        semantics_version: SOLVE_ARTIFACT_SEMANTICS_VERSION.to_owned(),
        scenario_id,
        project_id: loaded.receipt.source.project_id,
        display_name: name.to_owned(),
        scenario_revision: 0,
        source_project_revision: loaded.receipt.source.revision,
        source_payload_hash: parse_hash(&loaded.receipt.source.payload_hash)?,
        origin_run_id: loaded
            .receipt
            .run_id
            .parse()
            .map_err(|_| invalid("APPLICATION_SCENARIO_INVALID_ID"))?,
        origin_artifact_hash: parse_hash(&loaded.receipt.payload_hash)?,
        selected_attempt_index: selected.index,
        materialized_sectioning: selected.materialized,
        lineage,
        created_at: Utc::now().trunc_subsecs(3),
        edit: None,
    };
    let timetable = TimetablePayload {
        schema_version: TIMETABLE_DOCUMENT_SCHEMA_VERSION,
        scenario_id,
        scenario_revision: 0,
        timetable_id,
        timetable_revision: 0,
        snapshot_hash: selected.completed.snapshot_hash,
        assignment_hash: hash_meetings(&meetings)?,
        meetings,
        quality: selected.quality.clone(),
        user_locks: None,
    };
    // Validate the stable-ID representation too; serialization never bypasses the Hard gate.
    replay_timetable(loaded, &scenario, &timetable)?;
    Ok((scenario, timetable))
}

pub(super) fn decode_documents(
    stored: &StoredScenario,
) -> Result<(ScenarioPayload, TimetablePayload), ScenarioApplicationError> {
    let doc = &stored.document;
    let scenario: ScenarioPayload = serde_json::from_slice(&doc.scenario_payload)?;
    let timetable: TimetablePayload = serde_json::from_slice(&doc.timetable_payload)?;
    if !matches!(
        scenario.schema_version,
        SCENARIO_DOCUMENT_SCHEMA_VERSION | SCENARIO_EDIT_DOCUMENT_SCHEMA_VERSION
    ) || doc.scenario_schema_version != scenario.schema_version
        || timetable.schema_version != scenario.schema_version
        || doc.timetable_schema_version != timetable.schema_version
        || scenario.semantics_version != SOLVE_ARTIFACT_SEMANTICS_VERSION
    {
        return Err(invalid("APPLICATION_SCENARIO_UNSUPPORTED_SCHEMA"));
    }
    validate_name(&scenario.display_name)?;
    if scenario.scenario_id.to_string() != doc.scenario_id
        || scenario.project_id.to_string() != doc.project_id
        || scenario.display_name != doc.display_name
        || scenario.scenario_revision != doc.scenario_revision
        || scenario.source_project_revision != doc.source_project_revision
        || scenario.source_payload_hash != doc.source_payload_hash
        || scenario.origin_run_id.to_string() != doc.origin_run_id
        || scenario.origin_artifact_hash != doc.origin_artifact_hash
        || scenario.created_at != doc.created_at
        || timetable.scenario_id != scenario.scenario_id
        || timetable.scenario_revision != scenario.scenario_revision
        || timetable.timetable_id.to_string() != doc.timetable_id
        || timetable.timetable_revision != doc.timetable_revision
    {
        return Err(invalid("APPLICATION_SCENARIO_METADATA_MISMATCH"));
    }
    if scenario.scenario_id.as_uuid().is_nil() || timetable.timetable_id.as_uuid().is_nil() {
        return Err(invalid("APPLICATION_SCENARIO_UNSUPPORTED_REVISION"));
    }
    if scenario.lineage.as_ref().is_some_and(|lineage| {
        lineage.scenario_id == scenario.scenario_id
            || lineage.timetable_id == timetable.timetable_id
            || lineage.scenario_id.as_uuid().is_nil()
            || lineage.timetable_id.as_uuid().is_nil()
            || (scenario.schema_version == SCENARIO_DOCUMENT_SCHEMA_VERSION
                && (lineage.scenario_revision != 0 || lineage.timetable_revision != 0))
    }) {
        return Err(invalid("APPLICATION_SCENARIO_INVALID_LINEAGE"));
    }
    if scenario.schema_version == SCENARIO_DOCUMENT_SCHEMA_VERSION {
        if scenario.scenario_revision != 0 || timetable.timetable_revision != 0 {
            return Err(invalid("APPLICATION_SCENARIO_UNSUPPORTED_REVISION"));
        }
        if scenario.edit.is_some() || timetable.user_locks.is_some() {
            return Err(invalid("APPLICATION_SCENARIO_UNSUPPORTED_SCHEMA"));
        }
    } else {
        super::version_two::validate_shape(stored, &scenario, &timetable)?;
    }
    Ok((scenario, timetable))
}

pub(super) type ReplayedTimetable = (
    CompiledSchoolProblem,
    Vec<Assignment>,
    Timetable,
    ObjectiveVector,
);

pub(super) fn replay_timetable(
    loaded: &LoadedSolveArtifact,
    scenario: &ScenarioPayload,
    timetable: &TimetablePayload,
) -> Result<ReplayedTimetable, ScenarioApplicationError> {
    let selected = selected(loaded)?;
    if scenario.project_id != loaded.receipt.source.project_id
        || scenario.source_project_revision != loaded.receipt.source.revision
        || scenario.source_payload_hash != parse_hash(&loaded.receipt.source.payload_hash)?
        || scenario.origin_run_id.to_string() != loaded.receipt.run_id
        || scenario.origin_artifact_hash != parse_hash(&loaded.receipt.payload_hash)?
        || scenario.selected_attempt_index != selected.index
        || scenario.materialized_sectioning != selected.materialized
    {
        return Err(invalid("APPLICATION_SCENARIO_ORIGIN_MISMATCH"));
    }
    if timetable.snapshot_hash != selected.completed.snapshot_hash {
        return Err(invalid("APPLICATION_SCENARIO_SNAPSHOT_MISMATCH"));
    }
    if scenario.schema_version == SCENARIO_EDIT_DOCUMENT_SCHEMA_VERSION {
        return super::version_two::replay_state(&selected, scenario, timetable);
    }
    let problem = &selected.compiled.problem;
    let assignments = from_meetings(problem, &timetable.meetings, timetable.timetable_id)?;
    if !validate_assignments(problem, &assignments).is_valid() {
        return Err(invalid("APPLICATION_SCENARIO_HARD_VALIDATION_FAILED"));
    }
    let quality = evaluate(
        problem,
        &assignments,
        &ObjectivePlan::balanced_default(),
        &ScoringContext::neutral(problem),
    )
    .map_err(|_| invalid("APPLICATION_SCENARIO_SCORING_FAILED"))?;
    if quality != timetable.quality || &quality != selected.quality {
        return Err(invalid("APPLICATION_SCENARIO_QUALITY_MISMATCH"));
    }
    if hash_meetings(&timetable.meetings)? != timetable.assignment_hash
        || assignments != selected.completed.assignments
    {
        return Err(invalid("APPLICATION_SCENARIO_ASSIGNMENT_MISMATCH"));
    }
    let domain_timetable = Timetable::new(
        timetable.timetable_id,
        scenario.scenario_id,
        timetable.meetings.clone(),
    )
    .map_err(|_| invalid("APPLICATION_SCENARIO_INVALID_TIMETABLE"))?;
    Ok((
        selected.compiled.clone(),
        assignments,
        domain_timetable,
        quality,
    ))
}

pub(super) fn to_meetings(
    problem: &SchedulingProblemSnapshot,
    assignments: &[Assignment],
    timetable_id: TimetableId,
) -> Result<Vec<ScheduledMeeting>, ScenarioApplicationError> {
    assignments
        .iter()
        .map(|assignment| {
            let activity = &problem.activities()[assignment.activity.as_usize()];
            let id = meeting_id(timetable_id, activity.stable_id);
            Ok(ScheduledMeeting::new(
                id,
                activity.stable_id,
                MeetingAssignment::new(
                    problem.timeslots()[assignment.start.as_usize()].stable_id,
                    MeetingDuration::new(activity.duration_periods)
                        .map_err(|_| invalid("APPLICATION_SCENARIO_INVALID_DURATION"))?,
                    problem.rooms()[assignment.room.as_usize()].stable_id,
                    problem.teachers()[assignment.teacher.as_usize()].stable_id,
                ),
            ))
        })
        .collect()
}

pub(super) fn from_meetings(
    problem: &SchedulingProblemSnapshot,
    meetings: &[ScheduledMeeting],
    timetable_id: TimetableId,
) -> Result<Vec<Assignment>, ScenarioApplicationError> {
    let activities = index_map(problem.activities().iter().map(|row| row.stable_id))?;
    let timeslots = index_map(problem.timeslots().iter().map(|row| row.stable_id))?;
    let rooms = index_map(problem.rooms().iter().map(|row| row.stable_id))?;
    let teachers = index_map(problem.teachers().iter().map(|row| row.stable_id))?;
    let mut result = Vec::with_capacity(meetings.len());
    for meeting in meetings {
        let assignment = meeting.assignment();
        let activity = *activities
            .get(&meeting.demand_id())
            .ok_or_else(|| invalid("APPLICATION_SCENARIO_UNKNOWN_STABLE_ID"))?;
        if meeting.id() != meeting_id(timetable_id, meeting.demand_id())
            || assignment.duration().get()
                != problem.activities()[activity as usize].duration_periods
        {
            return Err(invalid("APPLICATION_SCENARIO_MEETING_IDENTITY_MISMATCH"));
        }
        result.push(Assignment {
            activity: ActivityIndex(activity),
            start: TimeslotIndex(
                *timeslots
                    .get(&assignment.start())
                    .ok_or_else(|| invalid("APPLICATION_SCENARIO_UNKNOWN_STABLE_ID"))?,
            ),
            room: RoomIndex(
                *rooms
                    .get(&assignment.room_id())
                    .ok_or_else(|| invalid("APPLICATION_SCENARIO_UNKNOWN_STABLE_ID"))?,
            ),
            teacher: TeacherIndex(
                *teachers
                    .get(&assignment.teacher_id())
                    .ok_or_else(|| invalid("APPLICATION_SCENARIO_UNKNOWN_STABLE_ID"))?,
            ),
        });
    }
    result.sort_by_key(|assignment| assignment.activity);
    Ok(result)
}

fn index_map<T: Ord>(
    ids: impl Iterator<Item = T>,
) -> Result<BTreeMap<T, u32>, ScenarioApplicationError> {
    ids.enumerate()
        .map(|(index, id)| {
            u32::try_from(index)
                .map(|index| (id, index))
                .map_err(|_| invalid("APPLICATION_SCENARIO_RESOURCE_LIMIT"))
        })
        .collect()
}

fn meeting_id(
    timetable_id: TimetableId,
    demand_id: class_schedule_domain::MeetingDemandId,
) -> ScheduledMeetingId {
    stable_id(
        &timetable_id.to_string(),
        "scheduled_meeting",
        &demand_id.to_string(),
    )
}

pub(super) fn hash_meetings(
    meetings: &[ScheduledMeeting],
) -> Result<[u8; 32], ScenarioApplicationError> {
    let mut stable = meetings
        .iter()
        .map(|meeting| (meeting.demand_id(), meeting.assignment()))
        .collect::<Vec<_>>();
    stable.sort_by_key(|(demand_id, _)| *demand_id);
    Ok(*blake3::hash(&serde_json::to_vec(&stable)?).as_bytes())
}

pub(super) fn validate_name(name: &str) -> Result<(), ScenarioApplicationError> {
    if Name::new(name).is_err() || name.trim() != name || name.chars().count() > 200 {
        return Err(invalid("APPLICATION_SCENARIO_INVALID_NAME"));
    }
    Ok(())
}

// Absent fields keep the exact v1 representation. An explicitly present null is not a v1 field.
fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
