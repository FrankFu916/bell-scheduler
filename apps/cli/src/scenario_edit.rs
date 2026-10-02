//! Typed edit commands only; the application computes, validates and persists each change.

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use clap::{Args, Subcommand};
use class_schedule_application::{
    ScenarioApplicationError, ScenarioEditCommand, ScenarioEditCommitCommand,
    ScenarioEditCommitStatus, ScenarioEditOperation, ScenarioEditStatus,
    commit_prepared_scenario_edit, prepare_scenario_edit_commit, preview_scenario_edit,
};
use class_schedule_domain::{MeetingDemandId, Revision, ScenarioId, TimeslotId};
use serde_json::json;

use crate::{
    CliFailure, RunOutcome,
    run_history::{open_current_database, open_readonly_database},
    scenario_commands::{receipt_dto, render},
};

#[derive(Clone, Debug, Args)]
struct BaseArgs {
    #[arg(long)]
    database: PathBuf,
    #[arg(long)]
    scenario_id: ScenarioId,
    #[arg(long)]
    expected_scenario_revision: u64,
    #[arg(long)]
    expected_timetable_revision: u64,
}

impl BaseArgs {
    fn command(&self, operation: &Operation) -> ScenarioEditCommand {
        ScenarioEditCommand {
            scenario_id: self.scenario_id,
            expected_scenario_revision: Revision::from_u64(self.expected_scenario_revision),
            expected_timetable_revision: Revision::from_u64(self.expected_timetable_revision),
            operation: operation.application(),
        }
    }
}

#[derive(Clone, Debug, Args)]
pub(super) struct PreviewArgs {
    #[command(flatten)]
    base: BaseArgs,
    #[command(subcommand)]
    operation: Operation,
}

#[derive(Clone, Debug, Args)]
pub(super) struct CommitArgs {
    #[command(flatten)]
    base: BaseArgs,
    /// Exact scenario hash returned by preview-scenario-edit.
    #[arg(long, value_parser = parse_hash)]
    expected_scenario_payload_hash: String,
    /// Exact timetable hash returned by preview-scenario-edit.
    #[arg(long, value_parser = parse_hash)]
    expected_timetable_payload_hash: String,
    #[command(subcommand)]
    operation: Operation,
}

#[derive(Clone, Debug, Subcommand)]
enum Operation {
    /// Move an activity to a stable timeslot UUID from scenario-timetable's grid.
    Move {
        #[arg(long)]
        activity_id: MeetingDemandId,
        #[arg(long)]
        start_timeslot_id: TimeslotId,
        #[arg(long)]
        lock_after: bool,
    },
    /// Exchange two start slots in one simultaneous, independently validated change.
    SwapStarts {
        #[arg(long)]
        left: MeetingDemandId,
        #[arg(long)]
        right: MeetingDemandId,
    },
    /// Add a user lock for the current assignment.
    LockCurrent {
        #[arg(long)]
        activity_id: MeetingDemandId,
    },
    /// Remove a user lock; source fixed activities remain mandatory.
    Unlock {
        #[arg(long)]
        activity_id: MeetingDemandId,
    },
}

impl Operation {
    const fn application(&self) -> ScenarioEditOperation {
        match *self {
            Self::Move {
                activity_id,
                start_timeslot_id,
                lock_after,
            } => ScenarioEditOperation::Move {
                activity_id,
                start: start_timeslot_id,
                lock_after,
            },
            Self::SwapStarts { left, right } => ScenarioEditOperation::SwapStarts { left, right },
            Self::LockCurrent { activity_id } => ScenarioEditOperation::LockCurrent { activity_id },
            Self::Unlock { activity_id } => ScenarioEditOperation::Unlock { activity_id },
        }
    }
}

fn parse_hash(value: &str) -> Result<String, String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("expected a 64-digit BLAKE3 hexadecimal hash from the preview".to_owned());
    }
    Ok(value.to_ascii_lowercase())
}

fn failure(error: &ScenarioApplicationError) -> CliFailure {
    CliFailure::new(
        error.code(),
        "the scenario edit failed revision checks or independent validation",
    )
}

pub(super) fn preview(args: &PreviewArgs) -> Result<RunOutcome, CliFailure> {
    let store = open_readonly_database(&args.base.database)?;
    let command = args.base.command(&args.operation);
    let preview = preview_scenario_edit(&store, &command).map_err(|error| failure(&error))?;
    let status = match preview.status {
        ScenarioEditStatus::Valid => "valid",
        ScenarioEditStatus::NoChange => "no_change",
        ScenarioEditStatus::HardRejected => "hard_rejected",
    };
    let changes = preview
        .changes
        .iter()
        .map(|change| {
            json!({
                "activity_id": change.activity_id,
                "before": change.before,
                "after": change.after,
                "was_user_locked": change.was_user_locked,
                "is_user_locked": change.is_user_locked,
            })
        })
        .collect::<Vec<_>>();
    render(&json!({
        "schema_version": 1,
        "operation": "preview_scenario_edit",
        "edit": command.operation,
        "scenario": receipt_dto(&preview.receipt),
        "source_is_current": preview.source_is_current,
        "status": status,
        "can_commit": preview.can_commit,
        "changes": changes,
        "validation": preview.validation,
        "context": preview.context,
        "before_quality": preview.before_quality,
        "after_quality": preview.after_quality,
    }))
}

pub(super) fn commit(args: &CommitArgs) -> Result<RunOutcome, CliFailure> {
    // Invalid input and Hard failures are evaluated before opening a writable connection.
    let readonly = open_readonly_database(&args.base.database)?;
    let prepared = prepare_scenario_edit_commit(
        &readonly,
        &ScenarioEditCommitCommand {
            edit: args.base.command(&args.operation),
            expected_scenario_payload_hash: args.expected_scenario_payload_hash.clone(),
            expected_timetable_payload_hash: args.expected_timetable_payload_hash.clone(),
        },
    )
    .map_err(|error| failure(&error))?;
    drop(readonly);
    let mut store = open_current_database(&args.base.database)?;
    let receipt =
        commit_prepared_scenario_edit(&mut store, prepared).map_err(|error| failure(&error))?;
    let status = match receipt.status {
        ScenarioEditCommitStatus::Committed => "committed",
        ScenarioEditCommitStatus::NoChange => "no_change",
    };
    render(&json!({
        "schema_version": 1,
        "operation": "commit_scenario_edit",
        "status": status,
        "scenario": receipt_dto(&receipt.receipt),
    }))
}
