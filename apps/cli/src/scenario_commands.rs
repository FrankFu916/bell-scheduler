//! Thin CLI transport for explicitly adopted, independently revalidated scenarios.

use std::path::PathBuf;

use clap::Args;
use class_schedule_application::{
    AdoptRunCommand, CloneScenarioCommand, ScenarioApplicationError, ScenarioReceipt,
    commit_prepared_scenario_creation, load_scenario, prepare_adopt_run, prepare_clone_scenario,
};
use class_schedule_domain::{ScenarioId, SchoolProjectId, SolverRunId};
use serde_json::{Value, json};

use crate::{
    CliFailure, RunOutcome,
    run_history::{open_database, open_readonly_database},
};

#[derive(Clone, Debug, Args)]
pub(super) struct AdoptRunArgs {
    #[arg(long)]
    database: PathBuf,
    #[arg(long)]
    project_id: SchoolProjectId,
    #[arg(long)]
    expected_source_revision: u64,
    #[arg(long)]
    run_id: SolverRunId,
    /// New scenario UUID. Existing scenarios are never replaced by this command.
    #[arg(long)]
    scenario_id: ScenarioId,
    #[arg(long)]
    display_name: String,
}

#[derive(Clone, Debug, Args)]
pub(super) struct CloneScenarioArgs {
    #[arg(long)]
    database: PathBuf,
    #[arg(long)]
    project_id: SchoolProjectId,
    #[arg(long)]
    expected_source_revision: u64,
    #[arg(long)]
    parent_scenario_id: ScenarioId,
    #[arg(long)]
    expected_scenario_revision: u64,
    #[arg(long)]
    expected_timetable_revision: u64,
    #[arg(long)]
    scenario_id: ScenarioId,
    #[arg(long)]
    display_name: String,
}

#[derive(Clone, Debug, Args)]
pub(super) struct ShowScenarioArgs {
    #[arg(long)]
    database: PathBuf,
    #[arg(long)]
    scenario_id: ScenarioId,
}

fn failure(error: &ScenarioApplicationError) -> CliFailure {
    CliFailure::new(
        error.code(),
        "the scenario command failed revision checks or independent validation",
    )
}

pub(super) fn adopt(args: &AdoptRunArgs) -> Result<RunOutcome, CliFailure> {
    let mut store = open_database(&args.database)?;
    let prepared = prepare_adopt_run(
        &store,
        &AdoptRunCommand {
            project_id: args.project_id,
            expected_source_revision: args.expected_source_revision,
            run_id: args.run_id,
            scenario_id: args.scenario_id,
            display_name: args.display_name.clone(),
        },
    )
    .map_err(|error| failure(&error))?;
    let receipt =
        commit_prepared_scenario_creation(&mut store, prepared).map_err(|error| failure(&error))?;
    render(
        &json!({"schema_version": 1, "operation": "adopt_run", "scenario": receipt_dto(&receipt)}),
    )
}

pub(super) fn clone_scenario(args: &CloneScenarioArgs) -> Result<RunOutcome, CliFailure> {
    let mut store = open_database(&args.database)?;
    let prepared = prepare_clone_scenario(
        &store,
        &CloneScenarioCommand {
            project_id: args.project_id,
            expected_source_revision: args.expected_source_revision,
            parent_scenario_id: args.parent_scenario_id,
            expected_scenario_revision: args.expected_scenario_revision,
            expected_timetable_revision: args.expected_timetable_revision,
            scenario_id: args.scenario_id,
            display_name: args.display_name.clone(),
        },
    )
    .map_err(|error| failure(&error))?;
    let receipt =
        commit_prepared_scenario_creation(&mut store, prepared).map_err(|error| failure(&error))?;
    render(
        &json!({"schema_version": 1, "operation": "clone_scenario", "scenario": receipt_dto(&receipt)}),
    )
}

pub(super) fn show(args: &ShowScenarioArgs) -> Result<RunOutcome, CliFailure> {
    let store = open_readonly_database(&args.database)?;
    let loaded = load_scenario(&store, args.scenario_id).map_err(|error| failure(&error))?;
    let lineage = loaded.lineage().map(|parent| {
        json!({
            "scenario_id": parent.scenario_id.to_string(),
            "scenario_revision": parent.scenario_revision.to_string(),
            "scenario_payload_hash": hex(&parent.scenario_payload_hash),
            "timetable_id": parent.timetable_id.to_string(),
            "timetable_revision": parent.timetable_revision.to_string(),
            "timetable_payload_hash": hex(&parent.timetable_payload_hash),
        })
    });
    render(&json!({
        "schema_version": 1, "operation": "show_scenario", "validation": "independently_revalidated",
        "scenario": receipt_dto(loaded.receipt()), "display_name": loaded.display_name(),
        "source_is_current": loaded.source_is_current(), "hard_valid": true,
        "activity_count": loaded.assignments().len(), "quality": loaded.quality(),
        "has_materialized_sectioning": loaded.materialized_sectioning().is_some(), "clone_lineage": lineage,
    }))
}

pub(super) fn receipt_dto(receipt: &ScenarioReceipt) -> Value {
    json!({
        "project_id": receipt.project_id.to_string(),
        "source_project_revision": receipt.source_project_revision.to_string(),
        "source_payload_hash": receipt.source_payload_hash,
        "scenario_id": receipt.scenario_id.to_string(),
        "scenario_revision": receipt.scenario_revision.to_string(),
        "scenario_payload_hash": receipt.scenario_payload_hash,
        "timetable_id": receipt.timetable_id.to_string(),
        "timetable_revision": receipt.timetable_revision.to_string(),
        "timetable_payload_hash": receipt.timetable_payload_hash,
        "origin_run_id": receipt.origin_run_id.to_string(),
        "origin_artifact_hash": receipt.origin_artifact_hash,
        "created_at": receipt.created_at.to_rfc3339(), "adopted_to_scenario": true,
    })
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            use std::fmt::Write;
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        })
}

pub(super) fn render(summary: &Value) -> Result<RunOutcome, CliFailure> {
    Ok(RunOutcome {
        rendered_summary: serde_json::to_string_pretty(summary).map_err(|_| {
            CliFailure::new(
                "CLI_SUMMARY_SERIALIZATION_FAILED",
                "cannot encode scenario receipt",
            )
        })?,
        exit_code: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn adopt_requires_explicit_source_revision_and_rejects_worker_paths() {
        let required = [
            "class-schedule",
            "adopt-run",
            "--database",
            "school.sqlite3",
            "--project-id",
            "77777777-1111-4111-8111-111111111111",
            "--run-id",
            "77777777-2222-4222-8222-222222222222",
            "--scenario-id",
            "77777777-3333-4333-8333-333333333333",
            "--display-name",
            "方案 A",
        ];
        assert!(crate::Cli::try_parse_from(required).is_err());
        let mut valid = required.to_vec();
        valid.extend(["--expected-source-revision", "0"]);
        assert!(crate::Cli::try_parse_from(&valid).is_ok());
        valid.extend(["--worker", "/tmp/untrusted"]);
        assert!(crate::Cli::try_parse_from(valid).is_err());
    }

    #[test]
    fn show_never_creates_a_missing_database() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("missing.sqlite3");
        let error = show(&ShowScenarioArgs {
            database: database.clone(),
            scenario_id: "77777777-3333-4333-8333-333333333333".parse().unwrap(),
        })
        .unwrap_err();
        assert_eq!(error.code(), "CLI_DATABASE_NOT_ACCESSIBLE");
        assert!(!database.exists());
    }

    #[test]
    fn clone_requires_all_three_revisions() {
        let base = [
            "class-schedule",
            "clone-scenario",
            "--database",
            "school.sqlite3",
            "--project-id",
            "77777777-1111-4111-8111-111111111111",
            "--parent-scenario-id",
            "77777777-2222-4222-8222-222222222222",
            "--scenario-id",
            "77777777-3333-4333-8333-333333333333",
            "--display-name",
            "方案 B",
        ];
        for missing in [
            "--expected-source-revision",
            "--expected-scenario-revision",
            "--expected-timetable-revision",
        ] {
            let mut arguments = base.to_vec();
            for flag in [
                "--expected-source-revision",
                "--expected-scenario-revision",
                "--expected-timetable-revision",
            ] {
                if flag != missing {
                    arguments.extend([flag, "0"]);
                }
            }
            assert!(crate::Cli::try_parse_from(arguments).is_err());
        }
        let mut valid = base.to_vec();
        valid.extend([
            "--expected-source-revision",
            "0",
            "--expected-scenario-revision",
            "0",
            "--expected-timetable-revision",
            "0",
        ]);
        assert!(crate::Cli::try_parse_from(valid).is_ok());
    }
}
