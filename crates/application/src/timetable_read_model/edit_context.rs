//! Bounded, stable references for a manual edit, including conflicts outside the visible page.

use std::collections::{BTreeMap, BTreeSet};

use class_schedule_domain::{Day, MeetingDemandId, TimeslotId};
use class_schedule_validation::{HardProblemCode, ValidationReport};
use serde::Serialize;

use super::{
    TimetableQueryError, TimetableRow, count, projection, required, scenario::projection_input,
};
use crate::{LoadedScenario, ScenarioActivityChange};

const MAXIMUM_EDIT_DIAGNOSTICS: usize = 100;
const MAXIMUM_EDIT_ACTIVITIES: usize = 202;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ScenarioEditDiagnostic {
    pub code: HardProblemCode,
    pub activity_ids: Vec<MeetingDemandId>,
    pub activities_truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ScenarioEditTimeslot {
    pub timeslot_id: TimeslotId,
    pub day: Day,
    pub day_label: String,
    pub period_index: u16,
    pub period_label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ScenarioEditContext {
    /// Existing assignments, before the proposed change. Candidate changes are returned separately.
    pub activities: Vec<TimetableRow>,
    pub calendar: Vec<ScenarioEditTimeslot>,
    pub diagnostics: Vec<ScenarioEditDiagnostic>,
    pub total_diagnostics: u32,
    pub diagnostics_truncated: bool,
    pub activities_truncated: bool,
}

pub(crate) fn scenario_edit_context(
    loaded: &LoadedScenario,
    changes: &[ScenarioActivityChange],
    validation: &ValidationReport,
) -> Result<ScenarioEditContext, TimetableQueryError> {
    let input = projection_input(loaded);
    let problem = &input.compiled.problem;
    // Always include the edited activities before filling the remaining diagnostic budget.
    let mut activity_ids: BTreeSet<_> = changes.iter().map(|change| change.activity_id).collect();
    if activity_ids.len() > MAXIMUM_EDIT_ACTIVITIES {
        return Err(TimetableQueryError::ResourceLimit);
    }
    let mut activities_truncated = false;
    let mut diagnostics = Vec::new();
    for hard in validation
        .hard_problems
        .iter()
        .take(MAXIMUM_EDIT_DIAGNOSTICS)
    {
        let mut ids = BTreeSet::new();
        let mut truncated = false;
        for index in &hard.activities {
            let activity = required(problem.activities().get(index.as_usize()), "edit_activity")?;
            if activity_ids.contains(&activity.stable_id)
                || activity_ids.len() < MAXIMUM_EDIT_ACTIVITIES
            {
                activity_ids.insert(activity.stable_id);
                ids.insert(activity.stable_id);
            } else {
                truncated = true;
            }
        }
        activities_truncated |= truncated;
        diagnostics.push(ScenarioEditDiagnostic {
            code: hard.code,
            activity_ids: ids.into_iter().collect(),
            activities_truncated: truncated,
        });
    }
    let mut activities = BTreeMap::new();
    for assignment in input.assignments {
        let activity = required(
            problem.activities().get(assignment.activity.as_usize()),
            "edit_activity",
        )?;
        if activity_ids.contains(&activity.stable_id) {
            let occupied = problem
                .occupied_slots(assignment.activity, assignment.start)
                .map_err(|_| TimetableQueryError::Inconsistent {
                    field: "edit_duration",
                })?;
            activities.insert(
                activity.stable_id,
                projection::row(
                    &input,
                    assignment,
                    activity,
                    occupied.into_iter().map(|slot| slot.0).collect(),
                )?,
            );
        }
    }
    if activities.len() != activity_ids.len() {
        return Err(TimetableQueryError::Inconsistent {
            field: "edit_assignment",
        });
    }
    let calendar = projection::calendar(&input)?
        .into_iter()
        .map(|cell| ScenarioEditTimeslot {
            timeslot_id: cell.timeslot_id,
            day: cell.day,
            day_label: cell.day_label,
            period_index: cell.period_index,
            period_label: cell.period_label,
        })
        .collect();
    Ok(ScenarioEditContext {
        activities: activities.into_values().collect(),
        calendar,
        diagnostics,
        total_diagnostics: count(validation.hard_problems.len())?,
        diagnostics_truncated: validation.hard_problems.len() > MAXIMUM_EDIT_DIAGNOSTICS,
        activities_truncated,
    })
}
