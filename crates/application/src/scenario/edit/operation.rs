use std::collections::{BTreeMap, BTreeSet};

use class_schedule_domain::{
    Lock, LockId, MeetingDemandId, ScheduledMeeting, Timetable, TimetableId,
};
use class_schedule_scheduling::{Assignment, LockedAssignment, SchedulingProblemSnapshot};
use class_schedule_scoring::{ObjectivePlan, ObjectiveVector, ScoringContext, evaluate};
use class_schedule_validation::{ValidationReport, validate_assignments};

use super::super::document::{from_meetings, to_meetings};
use super::{ScenarioActivityChange, ScenarioApplicationError, ScenarioEditOperation, invalid};
use crate::compile::stable_id;

#[derive(Clone, Debug)]
pub(in crate::scenario) struct CandidateEdit {
    pub meetings: Vec<ScheduledMeeting>,
    pub locks: Vec<Lock>,
    pub changes: Vec<ScenarioActivityChange>,
    pub validation: ValidationReport,
    pub quality: Option<ObjectiveVector>,
}

pub(in crate::scenario) fn make_lock(timetable_id: TimetableId, meeting: ScheduledMeeting) -> Lock {
    let id: LockId = stable_id(
        &timetable_id.to_string(),
        "user_lock",
        &meeting.demand_id().to_string(),
    );
    Lock::from_meeting(id, meeting)
}

pub(in crate::scenario) fn effective_problem(
    problem: &SchedulingProblemSnapshot,
    timetable_id: TimetableId,
    meetings: &[ScheduledMeeting],
    locks: &[Lock],
) -> Result<SchedulingProblemSnapshot, ScenarioApplicationError> {
    let by_id = meetings
        .iter()
        .map(|meeting| (meeting.id(), *meeting))
        .collect::<BTreeMap<_, _>>();
    let mut ids = BTreeSet::new();
    let mut locked_meetings = Vec::with_capacity(locks.len());
    for lock in locks {
        let meeting = by_id
            .get(&lock.scheduled_meeting_id())
            .ok_or_else(|| invalid("APPLICATION_SCENARIO_INVALID_USER_LOCK"))?;
        if *lock != make_lock(timetable_id, *meeting) || !ids.insert(lock.id()) {
            return Err(invalid("APPLICATION_SCENARIO_INVALID_USER_LOCK"));
        }
        locked_meetings.push(*meeting);
    }
    let assignments = from_meetings(problem, &locked_meetings, timetable_id)?;
    problem
        .with_additional_locks(
            &assignments
                .into_iter()
                .map(|assignment| LockedAssignment { assignment })
                .collect::<Vec<_>>(),
        )
        .map_err(|_| invalid("APPLICATION_SCENARIO_INVALID_USER_LOCK"))
}

pub(in crate::scenario) fn apply_operation(
    problem: &SchedulingProblemSnapshot,
    timetable: &Timetable,
    locks: &[Lock],
    operation: &ScenarioEditOperation,
) -> Result<CandidateEdit, ScenarioApplicationError> {
    let effective_before = effective_problem(problem, timetable.id(), timetable.meetings(), locks)?;
    let mut assignments = from_meetings(problem, timetable.meetings(), timetable.id())?;
    let mut candidate_locks = locks.to_vec();
    let mut lock_after = None;
    match *operation {
        ScenarioEditOperation::Move {
            activity_id,
            start,
            lock_after: lock,
        } => {
            let index = assignment_index(problem, &assignments, activity_id)?;
            let start = problem
                .timeslots()
                .iter()
                .position(|slot| slot.stable_id == start)
                .ok_or_else(|| invalid("APPLICATION_SCENARIO_EDIT_UNKNOWN_TIMESLOT"))?;
            assignments[index].start = class_schedule_scheduling::TimeslotIndex(
                u32::try_from(start).map_err(|_| invalid("APPLICATION_SCENARIO_RESOURCE_LIMIT"))?,
            );
            if lock {
                lock_after = Some(activity_id);
            }
        }
        ScenarioEditOperation::SwapStarts { left, right } => {
            let left = assignment_index(problem, &assignments, left)?;
            let right = assignment_index(problem, &assignments, right)?;
            let previous = assignments[left].start;
            assignments[left].start = assignments[right].start;
            assignments[right].start = previous;
        }
        ScenarioEditOperation::LockCurrent { activity_id } => {
            assignment_index(problem, &assignments, activity_id)?;
            lock_after = Some(activity_id);
        }
        ScenarioEditOperation::Unlock { activity_id } => {
            assignment_index(problem, &assignments, activity_id)?;
            let meeting = timetable
                .meetings()
                .iter()
                .find(|meeting| meeting.demand_id() == activity_id)
                .ok_or_else(|| invalid("APPLICATION_SCENARIO_EDIT_UNKNOWN_ACTIVITY"))?;
            candidate_locks.retain(|lock| lock.scheduled_meeting_id() != meeting.id());
        }
    }
    let meetings = to_meetings(problem, &assignments, timetable.id())?;
    // Existing locks validate the move first. A new lock must never replace a lock being violated.
    let mut validation = validate_assignments(&effective_before, &assignments);
    if validation.is_valid() {
        if let Some(activity_id) = lock_after {
            add_lock(
                problem,
                timetable.id(),
                &assignments,
                &meetings,
                &mut candidate_locks,
                activity_id,
            )?;
        }
        let effective_after =
            effective_problem(problem, timetable.id(), &meetings, &candidate_locks)?;
        validation = validate_assignments(&effective_after, &assignments);
    }
    candidate_locks.sort_by_key(|lock| lock.id());
    let quality = if validation.is_valid() {
        let effective_after =
            effective_problem(problem, timetable.id(), &meetings, &candidate_locks)?;
        Some(
            evaluate(
                &effective_after,
                &assignments,
                &ObjectivePlan::balanced_default(),
                &ScoringContext::neutral(&effective_after),
            )
            .map_err(|_| invalid("APPLICATION_SCENARIO_SCORING_FAILED"))?,
        )
    } else {
        None
    };
    let changes = changes(timetable.meetings(), locks, &meetings, &candidate_locks);
    Ok(CandidateEdit {
        meetings,
        locks: candidate_locks,
        changes,
        validation,
        quality,
    })
}

fn assignment_index(
    problem: &SchedulingProblemSnapshot,
    assignments: &[Assignment],
    id: MeetingDemandId,
) -> Result<usize, ScenarioApplicationError> {
    assignments
        .iter()
        .position(|assignment| problem.activities()[assignment.activity.as_usize()].stable_id == id)
        .ok_or_else(|| invalid("APPLICATION_SCENARIO_EDIT_UNKNOWN_ACTIVITY"))
}

fn add_lock(
    problem: &SchedulingProblemSnapshot,
    timetable_id: TimetableId,
    assignments: &[Assignment],
    meetings: &[ScheduledMeeting],
    locks: &mut Vec<Lock>,
    id: MeetingDemandId,
) -> Result<(), ScenarioApplicationError> {
    let index = assignment_index(problem, assignments, id)?;
    if problem
        .locks()
        .iter()
        .any(|lock| lock.assignment.activity == assignments[index].activity)
    {
        return Ok(());
    }
    let meeting = meetings
        .iter()
        .find(|meeting| meeting.demand_id() == id)
        .ok_or_else(|| invalid("APPLICATION_SCENARIO_EDIT_UNKNOWN_ACTIVITY"))?;
    if !locks
        .iter()
        .any(|lock| lock.scheduled_meeting_id() == meeting.id())
    {
        locks.push(make_lock(timetable_id, *meeting));
    }
    Ok(())
}

fn changes(
    before: &[ScheduledMeeting],
    before_locks: &[Lock],
    after: &[ScheduledMeeting],
    after_locks: &[Lock],
) -> Vec<ScenarioActivityChange> {
    let after_map = after
        .iter()
        .map(|meeting| (meeting.demand_id(), meeting))
        .collect::<BTreeMap<_, _>>();
    let before_locked = before_locks
        .iter()
        .map(|lock| lock.scheduled_meeting_id())
        .collect::<BTreeSet<_>>();
    let after_locked = after_locks
        .iter()
        .map(|lock| lock.scheduled_meeting_id())
        .collect::<BTreeSet<_>>();
    let mut result = Vec::new();
    for previous in before {
        if let Some(next) = after_map.get(&previous.demand_id()) {
            let was_user_locked = before_locked.contains(&previous.id());
            let is_user_locked = after_locked.contains(&next.id());
            if previous.assignment() != next.assignment() || was_user_locked != is_user_locked {
                result.push(ScenarioActivityChange {
                    activity_id: previous.demand_id(),
                    before: previous.assignment(),
                    after: next.assignment(),
                    was_user_locked,
                    is_user_locked,
                });
            }
        }
    }
    result.sort_by_key(|change| change.activity_id);
    result
}
