//! New-file export transport; the application owns validation, rendering and atomic publication.

use std::path::PathBuf;

use clap::{Args, ValueEnum};
use class_schedule_application::{
    ScenarioTimetableExportCommand, ScenarioTimetableExportFormat,
    prepare_scenario_timetable_export, publish_scenario_timetable_export,
};
use class_schedule_domain::{Revision, ScenarioId};
use serde_json::json;

use crate::{
    CliFailure, RunOutcome,
    run_history::open_readonly_database,
    scenario_commands::{receipt_dto, render},
    scenario_timetable::View,
};

#[derive(Clone, Debug, Args)]
pub(super) struct ExportScenarioArgs {
    #[arg(long)]
    database: PathBuf,
    #[arg(long)]
    scenario_id: ScenarioId,
    #[arg(long)]
    expected_scenario_revision: u64,
    #[arg(long)]
    expected_timetable_revision: u64,
    #[arg(long, value_enum)]
    view: View,
    /// Entity UUID from scenario-timetable's entity list. Exports all its meetings.
    #[arg(long)]
    entity_id: String,
    #[arg(long, value_enum, default_value_t = Format::Xlsx)]
    format: Format,
    /// A new .xlsx or .csv file in an existing directory. Never overwrites a file.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Format {
    Xlsx,
    Csv,
}

impl Format {
    const fn application(self) -> ScenarioTimetableExportFormat {
        match self {
            Self::Xlsx => ScenarioTimetableExportFormat::Xlsx,
            Self::Csv => ScenarioTimetableExportFormat::Csv,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Xlsx => "xlsx",
            Self::Csv => "csv",
        }
    }
}

pub(super) fn run(args: &ExportScenarioArgs) -> Result<RunOutcome, CliFailure> {
    let filter = args.view.filter(&args.entity_id)?;
    let store = open_readonly_database(&args.database)?;
    let failure = |error: class_schedule_application::ScenarioTimetableExportError| {
        CliFailure::new(
            error.code(),
            "cannot export the selected scenario timetable",
        )
    };
    let prepared = prepare_scenario_timetable_export(
        &store,
        &ScenarioTimetableExportCommand {
            scenario_id: args.scenario_id,
            expected_scenario_revision: Revision::from_u64(args.expected_scenario_revision),
            expected_timetable_revision: Revision::from_u64(args.expected_timetable_revision),
            filter,
            format: args.format.application(),
        },
    )
    .map_err(failure)?;
    let metadata = prepared.metadata();
    let outcome = render(&json!({
        "schema_version": metadata.schema_version,
        "operation": "export_scenario",
        "scenario": receipt_dto(&metadata.receipt),
        "source_is_current": metadata.source_is_current,
        "scenario_display_name": metadata.scenario_display_name,
        "selection": metadata.selection,
        "validation": "independently_revalidated",
        "format": args.format.label(),
        "meeting_count": metadata.meeting_count,
        "generated_at": metadata.generated_at.to_rfc3339(),
        "byte_length": metadata.byte_length.to_string(),
        "payload_hash": metadata.payload_hash,
        "payload_hash_algorithm": "blake3",
        "output": args.output.to_string_lossy(),
    }))?;
    publish_scenario_timetable_export(&prepared, &args.output).map_err(failure)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn arguments() -> Vec<&'static str> {
        vec![
            "class-schedule",
            "export-scenario",
            "--database",
            "school.sqlite3",
            "--scenario-id",
            "77777777-3333-4333-8333-333333333333",
            "--view",
            "student",
            "--entity-id",
            "77777777-4444-4444-8444-444444444444",
            "--output",
            "student.xlsx",
        ]
    }

    #[test]
    fn export_requires_both_revisions_and_has_no_pagination_or_worker() {
        let mut args = arguments();
        assert!(crate::Cli::try_parse_from(&args).is_err());
        args.extend(["--expected-scenario-revision", "9007199254740993"]);
        assert!(crate::Cli::try_parse_from(&args).is_err());
        args.extend(["--expected-timetable-revision", "18446744073709551615"]);
        let cli = crate::Cli::try_parse_from(&args).unwrap();
        let crate::Command::ExportScenario(parsed) = cli.command else {
            panic!("wrong command");
        };
        assert_eq!(parsed.expected_scenario_revision, 9_007_199_254_740_993);
        assert_eq!(parsed.expected_timetable_revision, u64::MAX);
        for disallowed in ["--limit", "--offset", "--worker"] {
            let mut extra = args.clone();
            extra.extend([disallowed, "1"]);
            assert!(crate::Cli::try_parse_from(extra).is_err());
        }
    }

    #[test]
    fn invalid_input_creates_neither_database_nor_export() {
        let directory = tempfile::tempdir().unwrap();
        let mut args = ExportScenarioArgs {
            database: directory.path().join("missing.sqlite3"),
            scenario_id: "77777777-3333-4333-8333-333333333333".parse().unwrap(),
            expected_scenario_revision: 0,
            expected_timetable_revision: 0,
            view: View::Student,
            entity_id: "77777777-4444-4444-8444-444444444444".to_owned(),
            format: Format::Xlsx,
            output: directory.path().join("student.xlsx"),
        };
        assert_eq!(
            run(&args).unwrap_err().code(),
            "CLI_DATABASE_NOT_ACCESSIBLE"
        );
        args.entity_id = "student-code".to_owned();
        assert_eq!(
            run(&args).unwrap_err().code(),
            "CLI_TIMETABLE_ENTITY_ID_INVALID"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
