#[path = "support/scenario_edit_fixture.rs"]
mod fixture;
#[path = "scenario_edits/history.rs"]
mod history;
#[path = "scenario_edits/projection.rs"]
mod projection;

use class_schedule_application::{
    CloneScenarioCommand, LoadedScenario, ScenarioEditCommand, ScenarioEditCommitCommand,
    ScenarioEditCommitStatus, ScenarioEditOperation, ScenarioEditStatus, ScenarioReceipt,
    TimetableQuery, TimetableView, commit_prepared_scenario_edit, load_scenario,
    prepare_scenario_edit_commit, preview_scenario_edit, query_scenario_timetable,
    query_scenario_timetable_entities,
};
use class_schedule_domain::{MeetingDemandId, Revision, ScenarioId, TimeslotId};
use class_schedule_persistence::SqliteStore;
use class_schedule_validation::HardProblemCode;
use rusqlite::{Connection, params};
use std::path::Path;

fn command(receipt: &ScenarioReceipt, operation: ScenarioEditOperation) -> ScenarioEditCommand {
    ScenarioEditCommand {
        scenario_id: receipt.scenario_id,
        expected_scenario_revision: Revision::from_u64(receipt.scenario_revision),
        expected_timetable_revision: Revision::from_u64(receipt.timetable_revision),
        operation,
    }
}

fn commit_command(
    receipt: &ScenarioReceipt,
    operation: ScenarioEditOperation,
) -> ScenarioEditCommitCommand {
    ScenarioEditCommitCommand {
        edit: command(receipt, operation),
        expected_scenario_payload_hash: receipt.scenario_payload_hash.clone(),
        expected_timetable_payload_hash: receipt.timetable_payload_hash.clone(),
    }
}

fn edit(
    store: &mut SqliteStore,
    receipt: &ScenarioReceipt,
    operation: ScenarioEditOperation,
) -> ScenarioReceipt {
    let prepared =
        prepare_scenario_edit_commit(store, &commit_command(receipt, operation)).unwrap();
    let result = commit_prepared_scenario_edit(store, prepared).unwrap();
    assert_eq!(result.status, ScenarioEditCommitStatus::Committed);
    result.receipt
}

fn single(loaded: &LoadedScenario) -> MeetingDemandId {
    loaded
        .timetable()
        .meetings()
        .iter()
        .find(|meeting| meeting.assignment().duration().get() == 1)
        .unwrap()
        .demand_id()
}

fn friday(loaded: &LoadedScenario, period: u16) -> TimeslotId {
    loaded
        .compiled()
        .problem
        .timeslots()
        .iter()
        .find(|slot| slot.day == class_schedule_domain::Day::Friday && slot.period_index == period)
        .unwrap()
        .stable_id
}

fn revision_counts(path: &Path) -> (u64, u64) {
    Connection::open(path).unwrap().query_row("SELECT (SELECT count(*) FROM scenario_revisions), (SELECT count(*) FROM timetable_revisions)", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
}

fn clone_command(receipt: &ScenarioReceipt) -> CloneScenarioCommand {
    CloneScenarioCommand {
        project_id: receipt.project_id,
        expected_source_revision: receipt.source_project_revision,
        parent_scenario_id: receipt.scenario_id,
        expected_scenario_revision: receipt.scenario_revision,
        expected_timetable_revision: receipt.timetable_revision,
        scenario_id: ScenarioId::new_v4(),
        display_name: "独立副本".to_owned(),
    }
}

fn lock_state(
    store: &SqliteStore,
    receipt: &ScenarioReceipt,
    activity_id: MeetingDemandId,
) -> class_schedule_application::ScenarioActivityLockState {
    let revision = Revision::from_u64(receipt.scenario_revision);
    let entities = query_scenario_timetable_entities(
        store,
        receipt.scenario_id,
        revision,
        revision,
        TimetableView::Grade,
        0,
        100,
    )
    .unwrap();
    query_scenario_timetable(
        store,
        receipt.scenario_id,
        revision,
        revision,
        &TimetableQuery {
            filter: entities.entities[0].filter,
            offset: 0,
            limit: 100,
        },
    )
    .unwrap()
    .activity_locks
    .into_iter()
    .find(|lock| lock.activity_id == activity_id)
    .unwrap()
}

fn entity_filter(
    store: &SqliteStore,
    receipt: &ScenarioReceipt,
    view: TimetableView,
    code: &str,
) -> class_schedule_application::TimetableFilter {
    query_scenario_timetable_entities(
        store,
        receipt.scenario_id,
        Revision::from_u64(receipt.scenario_revision),
        Revision::from_u64(receipt.timetable_revision),
        view,
        0,
        100,
    )
    .unwrap()
    .entities
    .into_iter()
    .find(|entity| entity.code == code)
    .unwrap()
    .filter
}

fn timetable_page(
    store: &SqliteStore,
    receipt: &ScenarioReceipt,
    filter: class_schedule_application::TimetableFilter,
) -> class_schedule_application::ScenarioTimetablePage {
    query_scenario_timetable(
        store,
        receipt.scenario_id,
        Revision::from_u64(receipt.scenario_revision),
        Revision::from_u64(receipt.timetable_revision),
        &TimetableQuery {
            filter,
            offset: 0,
            limit: 100,
        },
    )
    .unwrap()
}

fn mutate(
    path: &Path,
    scenario_id: ScenarioId,
    revision: u64,
    timetable: bool,
    change: impl FnOnce(&mut serde_json::Value),
) {
    let connection = Connection::open(path).unwrap();
    let (table, column) = if timetable {
        ("timetable_revisions", "timetable_revision")
    } else {
        ("scenario_revisions", "scenario_revision")
    };
    let bytes: Vec<u8> = connection
        .query_row(
            &format!("SELECT payload FROM {table} WHERE scenario_id = ?1 AND {column} = ?2"),
            params![scenario_id.to_string(), revision],
            |row| row.get(0),
        )
        .unwrap();
    let mut value = serde_json::from_slice(&bytes).unwrap();
    change(&mut value);
    let bytes = serde_json::to_vec(&value).unwrap();
    connection.execute(&format!("UPDATE {table} SET payload = ?1, payload_hash = ?2 WHERE scenario_id = ?3 AND {column} = ?4"), params![bytes, blake3::hash(&bytes).as_bytes(), scenario_id.to_string(), revision]).unwrap();
}

#[test]
fn move_preview_is_readonly_and_commit_reopens_one_complete_revision() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("move.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = fixture::saved_scenario(&mut store, false);
    let loaded = load_scenario(&store, receipt.scenario_id).unwrap();
    let source = store.load_project(&receipt.project_id.to_string()).unwrap();
    let activity_id = single(&loaded);
    let start = friday(&loaded, 1);
    let operation = ScenarioEditOperation::Move {
        activity_id,
        start,
        lock_after: false,
    };
    let before = std::fs::read(&path).unwrap();
    let readonly = SqliteStore::open_readonly(&path).unwrap();
    let preview = preview_scenario_edit(&readonly, &command(&receipt, operation.clone())).unwrap();
    assert_eq!(preview.status, ScenarioEditStatus::Valid);
    assert!(preview.can_commit);
    assert!(preview.validation.is_valid());
    assert_eq!(preview.changes.len(), 1);
    assert_eq!(preview.changes[0].after.start(), start);
    assert!(preview.after_quality.is_some());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(readonly);
    let current = edit(&mut store, &receipt, operation);
    assert_eq!(
        (current.scenario_revision, current.timetable_revision),
        (1, 1)
    );
    assert_eq!(current.created_at, receipt.created_at);
    assert_eq!(revision_counts(&path), (2, 2));
    assert_eq!(
        store.load_project(&receipt.project_id.to_string()).unwrap(),
        source
    );
    drop(store);
    let store = SqliteStore::open_readonly(&path).unwrap();
    let reopened = load_scenario(&store, receipt.scenario_id).unwrap();
    assert_eq!(reopened.timetable().revision().get(), 1);
    assert_eq!(
        reopened
            .timetable()
            .meetings()
            .iter()
            .find(|meeting| meeting.demand_id() == activity_id)
            .unwrap()
            .assignment()
            .start(),
        start
    );
    assert_eq!(reopened.quality(), preview.after_quality.as_ref().unwrap());
    assert_eq!(
        store
            .load_scenario(&receipt.scenario_id.to_string())
            .unwrap()
            .document
            .scenario_schema_version,
        2
    );
}

#[test]
fn hard_preview_uses_real_cross_class_enrollment_and_does_not_score_invalid_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hard.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = fixture::saved_scenario(&mut store, false);
    let phy1 = entity_filter(&store, &receipt, TimetableView::TeachingSection, "PHY1");
    let phy2 = entity_filter(&store, &receipt, TimetableView::TeachingSection, "PHY2");
    let row = |filter| timetable_page(&store, &receipt, filter).rows[0].clone();
    let left = row(phy1);
    let right = row(phy2);
    let loaded = load_scenario(&store, receipt.scenario_id).unwrap();
    let operation = ScenarioEditOperation::Move {
        activity_id: left.activity_id,
        start: loaded.compiled().problem.timeslots()[right.start_timeslot_index as usize].stable_id,
        lock_after: false,
    };
    let preview = preview_scenario_edit(&store, &command(&receipt, operation)).unwrap();
    assert!(
        preview
            .validation
            .contains(HardProblemCode::TeacherConflict)
    );
    assert!(
        !preview
            .validation
            .contains(HardProblemCode::StudentConflict)
    );
    let class_two = entity_filter(&store, &receipt, TimetableView::AdministrativeClass, "AC2");
    let rows = timetable_page(&store, &receipt, class_two);
    let chinese = rows
        .rows
        .iter()
        .find(|row| row.subject.code == "chinese")
        .unwrap();
    let operation = ScenarioEditOperation::Move {
        activity_id: left.activity_id,
        start: loaded.compiled().problem.timeslots()[chinese.start_timeslot_index as usize]
            .stable_id,
        lock_after: true,
    };
    let before = std::fs::read(&path).unwrap();
    let preview = preview_scenario_edit(&store, &command(&receipt, operation.clone())).unwrap();
    assert_eq!(preview.status, ScenarioEditStatus::HardRejected);
    assert!(!preview.can_commit);
    assert!(
        preview
            .context
            .diagnostics
            .iter()
            .any(
                |diagnostic| diagnostic.code == HardProblemCode::StudentConflict
                    && diagnostic.activity_ids.contains(&left.activity_id)
                    && diagnostic.activity_ids.contains(&chinese.activity_id)
            )
    );
    assert!(
        preview
            .context
            .activities
            .iter()
            .any(|row| row.activity_id == chinese.activity_id)
    );
    assert_eq!(preview.changes.len(), 1);
    assert_eq!(
        preview.context.calendar.len(),
        loaded.compiled().problem.timeslots().len()
    );
    assert!(
        preview
            .validation
            .contains(HardProblemCode::StudentConflict)
    );
    assert!(preview.after_quality.is_none());
    assert_eq!(
        prepare_scenario_edit_commit(&store, &commit_command(&receipt, operation))
            .unwrap_err()
            .code(),
        "APPLICATION_SCENARIO_EDIT_HARD_REJECTED"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn double_period_break_and_end_of_day_remain_hard() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("double.sqlite3")).unwrap();
    let receipt = fixture::saved_scenario(&mut store, false);
    let loaded = load_scenario(&store, receipt.scenario_id).unwrap();
    let activity_id = loaded
        .timetable()
        .meetings()
        .iter()
        .find(|meeting| meeting.assignment().duration().get() == 2)
        .unwrap()
        .demand_id();
    for period in [2, 4] {
        let preview = preview_scenario_edit(
            &store,
            &command(
                &receipt,
                ScenarioEditOperation::Move {
                    activity_id,
                    start: friday(&loaded, period),
                    lock_after: false,
                },
            ),
        )
        .unwrap();
        assert_eq!(preview.status, ScenarioEditStatus::HardRejected);
        assert!(preview.after_quality.is_none());
        assert!(
            preview
                .validation
                .contains(HardProblemCode::StartNotAllowed)
                || preview
                    .validation
                    .contains(HardProblemCode::DurationInvalid)
        );
    }
}

#[test]
fn locks_require_explicit_unlock_and_noops_do_not_upgrade_or_append() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("locks.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = fixture::saved_scenario(&mut store, false);
    let loaded = load_scenario(&store, receipt.scenario_id).unwrap();
    let activity_id = single(&loaded);
    let initial = std::fs::read(&path).unwrap();
    for operation in [
        ScenarioEditOperation::Unlock { activity_id },
        ScenarioEditOperation::SwapStarts {
            left: activity_id,
            right: activity_id,
        },
    ] {
        let prepared =
            prepare_scenario_edit_commit(&store, &commit_command(&receipt, operation)).unwrap();
        assert_eq!(prepared.preview().status, ScenarioEditStatus::NoChange);
        assert_eq!(
            commit_prepared_scenario_edit(&mut store, prepared)
                .unwrap()
                .status,
            ScenarioEditCommitStatus::NoChange
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), initial);
    assert_eq!(revision_counts(&path), (1, 1));
    let locked = edit(
        &mut store,
        &receipt,
        ScenarioEditOperation::LockCurrent { activity_id },
    );
    assert!(lock_state(&store, &locked, activity_id).user_locked);
    assert!(!lock_state(&store, &locked, activity_id).source_locked);
    let lock_bytes = std::fs::read(&path).unwrap();
    let prepared = prepare_scenario_edit_commit(
        &store,
        &commit_command(&locked, ScenarioEditOperation::LockCurrent { activity_id }),
    )
    .unwrap();
    assert_eq!(
        commit_prepared_scenario_edit(&mut store, prepared)
            .unwrap()
            .status,
        ScenarioEditCommitStatus::NoChange
    );
    assert_eq!(std::fs::read(&path).unwrap(), lock_bytes);
    for lock_after in [false, true] {
        let preview = preview_scenario_edit(
            &store,
            &command(
                &locked,
                ScenarioEditOperation::Move {
                    activity_id,
                    start: friday(&loaded, 1),
                    lock_after,
                },
            ),
        )
        .unwrap();
        assert!(
            preview
                .validation
                .contains(HardProblemCode::LockedAssignmentChanged)
        );
        assert!(preview.after_quality.is_none());
    }
    let unlocked = edit(
        &mut store,
        &locked,
        ScenarioEditOperation::Unlock { activity_id },
    );
    assert!(!lock_state(&store, &unlocked, activity_id).user_locked);
    let moved = edit(
        &mut store,
        &unlocked,
        ScenarioEditOperation::Move {
            activity_id,
            start: friday(&loaded, 1),
            lock_after: true,
        },
    );
    let reopened = load_scenario(&store, moved.scenario_id).unwrap();
    assert_eq!(reopened.user_locks().len(), 1);
    assert_eq!(
        reopened.user_locks()[0].assignment().start(),
        friday(&loaded, 1)
    );
    assert_eq!(revision_counts(&path), (4, 4));
}

#[test]
fn swap_is_simultaneous_and_creates_exactly_one_revision() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("swap.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = fixture::saved_scenario(&mut store, false);
    let loaded = load_scenario(&store, receipt.scenario_id).unwrap();
    let meetings = loaded
        .timetable()
        .meetings()
        .iter()
        .filter(|meeting| meeting.assignment().duration().get() == 1)
        .take(2)
        .copied()
        .collect::<Vec<_>>();
    let operation = ScenarioEditOperation::SwapStarts {
        left: meetings[0].demand_id(),
        right: meetings[1].demand_id(),
    };
    let preview = preview_scenario_edit(&store, &command(&receipt, operation.clone())).unwrap();
    assert_eq!(preview.status, ScenarioEditStatus::Valid);
    let next = edit(&mut store, &receipt, operation);
    assert_eq!(next.timetable_revision, 1);
    assert_eq!(revision_counts(&path), (2, 2));
    let reopened = load_scenario(&store, receipt.scenario_id).unwrap();
    for (before, other) in [(&meetings[0], &meetings[1]), (&meetings[1], &meetings[0])] {
        assert_eq!(
            reopened
                .timetable()
                .meetings()
                .iter()
                .find(|meeting| meeting.demand_id() == before.demand_id())
                .unwrap()
                .assignment()
                .start(),
            other.assignment().start()
        );
    }
}

#[test]
fn source_fixed_lock_cannot_be_removed_or_duplicated() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source-lock.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = fixture::scenario_with_mode(&mut store, false, "fixed-edit");
    let loaded = load_scenario(&store, receipt.scenario_id).unwrap();
    let activity_id = loaded.compiled().problem.activities()[loaded.compiled().problem.locks()[0]
        .assignment
        .activity
        .as_usize()]
    .stable_id;
    assert!(lock_state(&store, &receipt, activity_id).source_locked);
    assert!(!lock_state(&store, &receipt, activity_id).user_locked);
    let initial = std::fs::read(&path).unwrap();
    for operation in [
        ScenarioEditOperation::Unlock { activity_id },
        ScenarioEditOperation::LockCurrent { activity_id },
    ] {
        let prepared =
            prepare_scenario_edit_commit(&store, &commit_command(&receipt, operation)).unwrap();
        assert_eq!(
            commit_prepared_scenario_edit(&mut store, prepared)
                .unwrap()
                .status,
            ScenarioEditCommitStatus::NoChange
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), initial);
    let preview = preview_scenario_edit(
        &store,
        &command(
            &receipt,
            ScenarioEditOperation::Move {
                activity_id,
                start: friday(&loaded, 1),
                lock_after: true,
            },
        ),
    )
    .unwrap();
    assert!(
        preview
            .validation
            .contains(HardProblemCode::LockedAssignmentChanged)
    );
    assert!(preview.after_quality.is_none());
}

#[test]
fn a_legal_manual_change_may_worsen_quality_without_being_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = SqliteStore::open(directory.path().join("quality.sqlite3")).unwrap();
    let initial = fixture::saved_scenario(&mut store, false);
    let loaded = load_scenario(&store, initial.scenario_id).unwrap();
    let mut chosen = None;
    for meeting in loaded.timetable().meetings() {
        let operation = ScenarioEditOperation::Move {
            activity_id: meeting.demand_id(),
            start: friday(&loaded, 1),
            lock_after: false,
        };
        let preview = preview_scenario_edit(&store, &command(&initial, operation.clone())).unwrap();
        if preview
            .after_quality
            .as_ref()
            .is_some_and(|quality| quality.lexicographic_cmp(&preview.before_quality).is_gt())
        {
            assert_eq!(preview.status, ScenarioEditStatus::Valid);
            assert!(preview.can_commit);
            chosen = Some((operation, preview.after_quality.unwrap()));
            break;
        }
    }
    let (operation, expected) =
        chosen.expect("this fixture has a legal move that creates a pupil gap");
    let committed = edit(&mut store, &initial, operation);
    assert_eq!(
        load_scenario(&store, committed.scenario_id)
            .unwrap()
            .quality(),
        &expected
    );
}
