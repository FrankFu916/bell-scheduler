use super::{
    clone_command, command, commit_command, edit, fixture, friday, mutate, revision_counts, single,
};
use class_schedule_application::{
    ScenarioEditOperation, ScenarioEditStatus, commit_prepared_scenario_creation,
    commit_prepared_scenario_edit, load_scenario, prepare_clone_scenario,
    prepare_scenario_edit_commit, preview_scenario_edit,
};
use class_schedule_domain::{MeetingDemandId, Revision, TimeslotId};
use class_schedule_persistence::SqliteStore;
use rusqlite::Connection;

#[test]
fn prepared_edits_compare_both_hashes_and_only_one_connection_can_append() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cas.sqlite3");
    let mut first = SqliteStore::open(&path).unwrap();
    let receipt = fixture::saved_scenario(&mut first, false);
    let mut second = SqliteStore::open(&path).unwrap();
    let loaded = load_scenario(&first, receipt.scenario_id).unwrap();
    let activity_id = single(&loaded);
    let operation = ScenarioEditOperation::Move {
        activity_id,
        start: friday(&loaded, 1),
        lock_after: false,
    };
    for scenario_hash in [true, false] {
        let mut bad = commit_command(&receipt, operation.clone());
        if scenario_hash {
            bad.expected_scenario_payload_hash = "00".repeat(32);
        } else {
            bad.expected_timetable_payload_hash = "00".repeat(32);
        }
        assert_eq!(
            prepare_scenario_edit_commit(&first, &bad)
                .unwrap_err()
                .code(),
            if scenario_hash {
                "APPLICATION_SCENARIO_HASH_CONFLICT"
            } else {
                "APPLICATION_TIMETABLE_HASH_CONFLICT"
            }
        );
    }
    for scenario_revision in [true, false] {
        let mut bad = command(&receipt, operation.clone());
        if scenario_revision {
            bad.expected_scenario_revision = Revision::from_u64(u64::MAX);
        } else {
            bad.expected_timetable_revision = Revision::from_u64(u64::MAX);
        }
        assert_eq!(
            preview_scenario_edit(&first, &bad).unwrap_err().code(),
            if scenario_revision {
                "APPLICATION_SCENARIO_REVISION_CONFLICT"
            } else {
                "APPLICATION_TIMETABLE_REVISION_CONFLICT"
            }
        );
    }
    let winning =
        prepare_scenario_edit_commit(&first, &commit_command(&receipt, operation.clone())).unwrap();
    let stale = prepare_scenario_edit_commit(&second, &commit_command(&receipt, operation.clone()))
        .unwrap();
    commit_prepared_scenario_edit(&mut first, winning).unwrap();
    assert_eq!(
        commit_prepared_scenario_edit(&mut second, stale)
            .unwrap_err()
            .code(),
        "PERSISTENCE_SCENARIO_REVISION_CONFLICT"
    );
    assert_eq!(
        prepare_scenario_edit_commit(&second, &commit_command(&receipt, operation))
            .unwrap_err()
            .code(),
        "APPLICATION_SCENARIO_REVISION_CONFLICT"
    );
    assert_eq!(revision_counts(&path), (2, 2));
}

#[test]
fn history_stays_readable_but_source_change_blocks_prepared_and_new_edits() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = fixture::saved_scenario(&mut store, false);
    let loaded = load_scenario(&store, receipt.scenario_id).unwrap();
    let operation = ScenarioEditOperation::Move {
        activity_id: single(&loaded),
        start: friday(&loaded, 1),
        lock_after: false,
    };
    let prepared =
        prepare_scenario_edit_commit(&store, &commit_command(&receipt, operation.clone())).unwrap();
    let mut document = store.load_project(&receipt.project_id.to_string()).unwrap();
    document.revision += 1;
    store.replace_project(0, &document).unwrap();
    assert_eq!(
        commit_prepared_scenario_edit(&mut store, prepared)
            .unwrap_err()
            .code(),
        "PERSISTENCE_REVISION_CONFLICT"
    );
    let before = std::fs::read(&path).unwrap();
    let preview = preview_scenario_edit(&store, &command(&receipt, operation.clone())).unwrap();
    assert_eq!(preview.status, ScenarioEditStatus::Valid);
    assert!(!preview.source_is_current);
    assert!(!preview.can_commit);
    assert!(preview.after_quality.is_some());
    assert_eq!(
        prepare_scenario_edit_commit(&store, &commit_command(&receipt, operation))
            .unwrap_err()
            .code(),
        "PERSISTENCE_REVISION_CONFLICT"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(revision_counts(&path), (1, 1));
}

#[test]
fn edited_b_copy_preserves_own_meetings_locks_and_selected_sections_without_parent_dependency() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = fixture::saved_scenario(&mut store, true);
    let loaded = load_scenario(&store, receipt.scenario_id).unwrap();
    let activity_id = single(&loaded);
    let target = friday(&loaded, 1);
    let moved = edit(
        &mut store,
        &receipt,
        ScenarioEditOperation::Move {
            activity_id,
            start: target,
            lock_after: true,
        },
    );
    let prepared = prepare_clone_scenario(&store, &clone_command(&moved)).unwrap();
    let child = commit_prepared_scenario_creation(&mut store, prepared).unwrap();
    assert_eq!((child.scenario_revision, child.timetable_revision), (0, 0));
    let parent = load_scenario(&store, moved.scenario_id).unwrap();
    let copy = load_scenario(&store, child.scenario_id).unwrap();
    assert_eq!(copy.assignments(), parent.assignments());
    assert_eq!(copy.quality(), parent.quality());
    assert_eq!(
        copy.materialized_sectioning(),
        parent.materialized_sectioning()
    );
    assert!(copy.materialized_sectioning().is_some());
    assert_eq!(copy.user_locks().len(), 1);
    assert_ne!(copy.user_locks()[0].id(), parent.user_locks()[0].id());
    assert_ne!(
        copy.user_locks()[0].scheduled_meeting_id(),
        parent.user_locks()[0].scheduled_meeting_id()
    );
    assert_eq!(
        copy.user_locks()[0].assignment(),
        parent.user_locks()[0].assignment()
    );
    assert_eq!(copy.lineage().unwrap().scenario_revision, 1);
    mutate(&path, moved.scenario_id, 1, false, |value| {
        value["semantics_version"] = "damaged parent".into();
    });
    assert!(load_scenario(&store, moved.scenario_id).is_err());
    assert_eq!(
        load_scenario(&store, child.scenario_id).unwrap().receipt(),
        &child
    );
    let next = edit(
        &mut store,
        &child,
        ScenarioEditOperation::Unlock { activity_id },
    );
    assert!(
        load_scenario(&store, next.scenario_id)
            .unwrap()
            .user_locks()
            .is_empty()
    );
    assert_eq!(
        load_scenario(&store, next.scenario_id)
            .unwrap()
            .timetable()
            .meetings()
            .iter()
            .find(|meeting| meeting.demand_id() == activity_id)
            .unwrap()
            .assignment()
            .start(),
        target
    );
}

#[test]
fn v2_rehash_cannot_change_operation_fixed_resources_locks_or_direct_predecessor() {
    for (case, expected) in [
        ("operation", "APPLICATION_SCENARIO_INVALID_EDIT_EDGE"),
        ("room", "APPLICATION_SCENARIO_FIXED_RESOURCES_CHANGED"),
        ("lock", "APPLICATION_SCENARIO_INVALID_USER_LOCK"),
        (
            "predecessor",
            "APPLICATION_SCENARIO_PREDECESSOR_HASH_MISMATCH",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tamper.sqlite3");
        let mut store = SqliteStore::open(&path).unwrap();
        let receipt = fixture::saved_scenario(&mut store, false);
        let loaded = load_scenario(&store, receipt.scenario_id).unwrap();
        let activity_id = single(&loaded);
        let moved = edit(
            &mut store,
            &receipt,
            ScenarioEditOperation::Move {
                activity_id,
                start: friday(&loaded, 1),
                lock_after: true,
            },
        );
        match case {
            "operation" => mutate(&path, receipt.scenario_id, 1, false, |value| {
                value["edit"]["operation"]["start"] =
                    serde_json::to_value(friday(&loaded, 3)).unwrap();
            }),
            "room" => mutate(&path, receipt.scenario_id, 1, true, |value| {
                value["meetings"][0]["assignment"]["room_id"] =
                    serde_json::to_value(loaded.compiled().problem.rooms()[1].stable_id).unwrap();
            }),
            "lock" => mutate(&path, receipt.scenario_id, 1, true, |value| {
                let duplicate = value["user_locks"][0].clone();
                value["user_locks"].as_array_mut().unwrap().push(duplicate);
            }),
            "predecessor" => mutate(&path, receipt.scenario_id, 0, false, |value| {
                value["display_name"] = "tampered predecessor".into();
            }),
            _ => unreachable!(),
        }
        assert_eq!(
            load_scenario(&store, moved.scenario_id).unwrap_err().code(),
            expected,
            "{case}"
        );
    }
}

#[test]
fn v2_checks_only_direct_predecessor_state_without_recursing_old_edges() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bounded.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let initial = fixture::saved_scenario(&mut store, false);
    let loaded = load_scenario(&store, initial.scenario_id).unwrap();
    let activity_id = single(&loaded);
    let first = edit(
        &mut store,
        &initial,
        ScenarioEditOperation::Move {
            activity_id,
            start: friday(&loaded, 1),
            lock_after: false,
        },
    );
    let second = edit(
        &mut store,
        &first,
        ScenarioEditOperation::LockCurrent { activity_id },
    );
    let third = edit(
        &mut store,
        &second,
        ScenarioEditOperation::Unlock { activity_id },
    );
    mutate(&path, initial.scenario_id, 0, true, |value| {
        value["snapshot_hash"][0] = 255.into();
    });
    assert_eq!(
        load_scenario(&store, initial.scenario_id)
            .unwrap()
            .receipt(),
        &third
    );
    // Altering the immediately preceding immutable payload is detected even after rehashing it.
    mutate(&path, initial.scenario_id, 2, true, |value| {
        value["assignment_hash"][0] = 255.into();
    });
    assert_eq!(
        load_scenario(&store, initial.scenario_id)
            .unwrap_err()
            .code(),
        "APPLICATION_SCENARIO_PREDECESSOR_HASH_MISMATCH"
    );
}

#[test]
fn new_edit_fields_do_not_broaden_v1_schema_even_when_null() {
    for timetable in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("null-field.sqlite3");
        let mut store = SqliteStore::open(&path).unwrap();
        let receipt = fixture::saved_scenario(&mut store, false);
        mutate(&path, receipt.scenario_id, 0, timetable, |value| {
            value[if timetable { "user_locks" } else { "edit" }] = serde_json::Value::Null;
        });
        assert_eq!(
            load_scenario(&store, receipt.scenario_id)
                .unwrap_err()
                .code(),
            "APPLICATION_SCENARIO_DOCUMENT_ENCODING"
        );
    }
}

#[test]
fn bad_direct_predecessor_is_revalidated_even_if_its_reference_hash_was_updated() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bad-previous.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let initial = fixture::saved_scenario(&mut store, false);
    let loaded = load_scenario(&store, initial.scenario_id).unwrap();
    let activity_id = single(&loaded);
    let first = edit(
        &mut store,
        &initial,
        ScenarioEditOperation::Move {
            activity_id,
            start: friday(&loaded, 1),
            lock_after: false,
        },
    );
    let second = edit(
        &mut store,
        &first,
        ScenarioEditOperation::LockCurrent { activity_id },
    );
    mutate(&path, initial.scenario_id, 1, true, |value| {
        value["meetings"][0]["assignment"]["start"] =
            value["meetings"][1]["assignment"]["start"].clone();
    });
    let hash: Vec<u8> = Connection::open(&path).unwrap().query_row("SELECT payload_hash FROM timetable_revisions WHERE scenario_id = ?1 AND timetable_revision = 1", [initial.scenario_id.to_string()], |row| row.get(0)).unwrap();
    mutate(&path, initial.scenario_id, 2, false, |value| {
        value["edit"]["previous_revision"]["timetable_payload_hash"] =
            serde_json::to_value(hash).unwrap();
    });
    assert_eq!(
        load_scenario(&store, second.scenario_id)
            .unwrap_err()
            .code(),
        "APPLICATION_SCENARIO_HARD_VALIDATION_FAILED"
    );
}

#[test]
fn unknown_stable_ids_fail_without_creating_revision() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unknown.sqlite3");
    let mut store = SqliteStore::open(&path).unwrap();
    let receipt = fixture::saved_scenario(&mut store, false);
    let loaded = load_scenario(&store, receipt.scenario_id).unwrap();
    let before = std::fs::read(&path).unwrap();
    for (operation, code) in [
        (
            ScenarioEditOperation::Unlock {
                activity_id: MeetingDemandId::new_v4(),
            },
            "APPLICATION_SCENARIO_EDIT_UNKNOWN_ACTIVITY",
        ),
        (
            ScenarioEditOperation::Move {
                activity_id: single(&loaded),
                start: TimeslotId::new_v4(),
                lock_after: true,
            },
            "APPLICATION_SCENARIO_EDIT_UNKNOWN_TIMESLOT",
        ),
    ] {
        assert_eq!(
            preview_scenario_edit(&store, &command(&receipt, operation))
                .unwrap_err()
                .code(),
            code
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
