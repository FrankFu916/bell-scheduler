use super::*;
use clap::Parser;

const SCENARIO: &str = "77777777-3333-4333-8333-333333333333";
const ACTIVITY: &str = "77777777-4444-4444-8444-444444444444";
const SLOT: &str = "77777777-5555-4555-8555-555555555555";
const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn base(command: &'static str) -> Vec<&'static str> {
    vec![
        "class-schedule",
        command,
        "--database",
        "school.sqlite3",
        "--scenario-id",
        SCENARIO,
    ]
}

#[test]
fn preview_requires_dual_revisions_and_preserves_full_u64_precision() {
    let mut args = base("preview-scenario-edit");
    let operation = ["lock-current", "--activity-id", ACTIVITY];
    assert!(crate::Cli::try_parse_from(args.iter().copied().chain(operation)).is_err());
    args.extend(["--expected-scenario-revision", "9007199254740993"]);
    assert!(crate::Cli::try_parse_from(args.iter().copied().chain(operation)).is_err());
    args.extend(["--expected-timetable-revision", "18446744073709551615"]);
    let parsed = crate::Cli::try_parse_from(args.iter().copied().chain(operation)).unwrap();
    let crate::Command::PreviewScenarioEdit(parsed) = parsed.command else {
        panic!("wrong command");
    };
    assert_eq!(
        parsed.base.expected_scenario_revision,
        9_007_199_254_740_993
    );
    assert_eq!(parsed.base.expected_timetable_revision, u64::MAX);
}

#[test]
fn commit_requires_both_well_formed_hashes_and_rejects_client_assignments() {
    let mut args = base("commit-scenario-edit");
    args.extend([
        "--expected-scenario-revision",
        "0",
        "--expected-timetable-revision",
        "0",
    ]);
    let operation = [
        "move",
        "--activity-id",
        ACTIVITY,
        "--start-timeslot-id",
        SLOT,
        "--lock-after",
    ];
    assert!(crate::Cli::try_parse_from(args.iter().copied().chain(operation)).is_err());
    args.extend(["--expected-scenario-payload-hash", HASH]);
    assert!(crate::Cli::try_parse_from(args.iter().copied().chain(operation)).is_err());
    args.extend(["--expected-timetable-payload-hash", HASH]);
    let parsed = crate::Cli::try_parse_from(args.iter().copied().chain(operation)).unwrap();
    let crate::Command::CommitScenarioEdit(parsed) = parsed.command else {
        panic!("wrong command");
    };
    assert!(matches!(
        parsed.operation.application(),
        ScenarioEditOperation::Move {
            lock_after: true,
            ..
        }
    ));
    for option in ["--assignments", "--worker", "--quality", "--hard-valid"] {
        assert!(
            crate::Cli::try_parse_from(args.iter().copied().chain([option, "[]"]).chain(operation))
                .is_err()
        );
    }
    *args.last_mut().unwrap() = "invalid";
    assert!(crate::Cli::try_parse_from(args.iter().copied().chain(operation)).is_err());
}

#[test]
fn operations_require_stable_uuids_and_exactly_one_operation() {
    let mut args = base("preview-scenario-edit");
    args.extend([
        "--expected-scenario-revision",
        "0",
        "--expected-timetable-revision",
        "0",
    ]);
    for operation in [
        vec![
            "move",
            "--activity-id",
            ACTIVITY,
            "--start-timeslot-id",
            "0",
        ],
        vec!["swap-starts", "--left", ACTIVITY],
        vec!["unlock", "--activity-id", "student-code"],
        vec![
            "lock-current",
            "--activity-id",
            ACTIVITY,
            "unlock",
            "--activity-id",
            ACTIVITY,
        ],
    ] {
        assert!(crate::Cli::try_parse_from(args.iter().copied().chain(operation)).is_err());
    }
    for operation in [
        vec!["swap-starts", "--left", ACTIVITY, "--right", SLOT],
        vec!["unlock", "--activity-id", ACTIVITY],
    ] {
        assert!(crate::Cli::try_parse_from(args.iter().copied().chain(operation)).is_ok());
    }
}

#[test]
fn neither_preview_nor_commit_creates_a_missing_database() {
    let directory = tempfile::tempdir().unwrap();
    let base = BaseArgs {
        database: directory.path().join("missing.sqlite3"),
        scenario_id: SCENARIO.parse().unwrap(),
        expected_scenario_revision: 0,
        expected_timetable_revision: 0,
    };
    let operation = Operation::LockCurrent {
        activity_id: ACTIVITY.parse().unwrap(),
    };
    assert_eq!(
        preview(&PreviewArgs {
            base: base.clone(),
            operation: operation.clone()
        })
        .unwrap_err()
        .code(),
        "CLI_DATABASE_NOT_ACCESSIBLE"
    );
    assert_eq!(
        commit(&CommitArgs {
            base,
            operation,
            expected_scenario_payload_hash: HASH.to_owned(),
            expected_timetable_payload_hash: HASH.to_owned(),
        })
        .unwrap_err()
        .code(),
        "CLI_DATABASE_NOT_ACCESSIBLE"
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}
