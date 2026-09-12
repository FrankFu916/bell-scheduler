//! Read-only CLI projection of an explicitly selected scenario and timetable revision.

use std::path::PathBuf;

use clap::{Args, ValueEnum};
use class_schedule_application::{
    TimetableFilter, TimetableQuery, TimetableView, query_scenario_timetable,
    query_scenario_timetable_entities,
};
use class_schedule_domain::{Revision, ScenarioId};
use serde_json::json;

use crate::{
    CliFailure, RunOutcome,
    run_history::open_readonly_database,
    scenario_commands::{receipt_dto, render},
};

#[derive(Clone, Debug, Args)]
pub(super) struct ScenarioTimetableArgs {
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
    /// Entity UUID from this view's entity list. Omit to list entities instead of meetings.
    #[arg(long)]
    entity_id: Option<String>,
    #[arg(long, default_value_t = 0)]
    offset: u32,
    #[arg(long, default_value_t = 20)]
    limit: u32,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum View {
    AdministrativeClass,
    TeachingSection,
    Teacher,
    Room,
    Student,
    Subject,
    Grade,
}

impl View {
    const fn application(self) -> TimetableView {
        match self {
            Self::AdministrativeClass => TimetableView::AdministrativeClass,
            Self::TeachingSection => TimetableView::TeachingSection,
            Self::Teacher => TimetableView::Teacher,
            Self::Room => TimetableView::Room,
            Self::Student => TimetableView::Student,
            Self::Subject => TimetableView::Subject,
            Self::Grade => TimetableView::Grade,
        }
    }

    fn filter(self, id: &str) -> Result<TimetableFilter, CliFailure> {
        let filter = match self {
            Self::AdministrativeClass => id.parse().map(TimetableFilter::AdministrativeClass),
            Self::TeachingSection => id.parse().map(TimetableFilter::TeachingSection),
            Self::Teacher => id.parse().map(TimetableFilter::Teacher),
            Self::Room => id.parse().map(TimetableFilter::Room),
            Self::Student => id.parse().map(TimetableFilter::Student),
            Self::Subject => id.parse().map(TimetableFilter::Subject),
            Self::Grade => id.parse().map(TimetableFilter::Grade),
        };
        filter.map_err(|_| {
            CliFailure::new(
                "CLI_TIMETABLE_ENTITY_ID_INVALID",
                "entity ID must be a UUID returned by this timetable view",
            )
        })
    }
}

pub(super) fn run(args: &ScenarioTimetableArgs) -> Result<RunOutcome, CliFailure> {
    let filter = args
        .entity_id
        .as_deref()
        .map(|id| args.view.filter(id))
        .transpose()?;
    let store = open_readonly_database(&args.database)?;
    let scenario_revision = Revision::from_u64(args.expected_scenario_revision);
    let timetable_revision = Revision::from_u64(args.expected_timetable_revision);
    let failure = |error: class_schedule_application::ScenarioTimetableQueryError| {
        CliFailure::new(error.code(), error.to_string())
    };
    if let Some(filter) = filter {
        let page = query_scenario_timetable(
            &store,
            args.scenario_id,
            scenario_revision,
            timetable_revision,
            &TimetableQuery {
                filter,
                offset: args.offset,
                limit: args.limit,
            },
        )
        .map_err(failure)?;
        render(&json!({
            "schema_version": page.schema_version,
            "operation": "scenario_timetable",
            "scenario": receipt_dto(&page.receipt),
            "source_is_current": page.source_is_current,
            "scenario_display_name": page.scenario_display_name,
            "validation": "independently_revalidated",
            "selection": page.selection,
            "rows": page.rows,
            "calendar": page.calendar,
            "total_rows": page.total_rows,
            "offset": page.offset,
            "has_more": page.has_more,
            "next_offset": page.next_offset,
            "quality": page.quality,
        }))
    } else {
        let page = query_scenario_timetable_entities(
            &store,
            args.scenario_id,
            scenario_revision,
            timetable_revision,
            args.view.application(),
            args.offset,
            args.limit,
        )
        .map_err(failure)?;
        render(&json!({
            "schema_version": page.schema_version,
            "operation": "scenario_timetable_entities",
            "scenario": receipt_dto(&page.receipt),
            "source_is_current": page.source_is_current,
            "scenario_display_name": page.scenario_display_name,
            "validation": "independently_revalidated",
            "view": page.view,
            "entities": page.entities,
            "total_entities": page.total_entities,
            "offset": page.offset,
            "has_more": page.has_more,
            "next_offset": page.next_offset,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn arguments() -> Vec<&'static str> {
        vec![
            "class-schedule",
            "scenario-timetable",
            "--database",
            "school.sqlite3",
            "--scenario-id",
            "77777777-3333-4333-8333-333333333333",
            "--view",
            "student",
        ]
    }

    #[test]
    fn query_requires_both_revisions_and_preserves_u64_precision() {
        let mut args = arguments();
        assert!(crate::Cli::try_parse_from(&args).is_err());
        args.extend(["--expected-scenario-revision", "9007199254740993"]);
        assert!(crate::Cli::try_parse_from(&args).is_err());
        args.extend(["--expected-timetable-revision", "18446744073709551615"]);
        let cli = crate::Cli::try_parse_from(&args).unwrap();
        let crate::Command::ScenarioTimetable(parsed) = cli.command else {
            panic!("wrong command");
        };
        assert_eq!(parsed.expected_scenario_revision, 9_007_199_254_740_993);
        assert_eq!(parsed.expected_timetable_revision, u64::MAX);
        args.extend(["--worker", "/untrusted/worker"]);
        assert!(crate::Cli::try_parse_from(args).is_err());
    }

    #[test]
    fn query_never_creates_a_missing_database_or_accepts_malformed_entity_ids() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("missing.sqlite3");
        let mut args = ScenarioTimetableArgs {
            database: database.clone(),
            scenario_id: "77777777-3333-4333-8333-333333333333".parse().unwrap(),
            expected_scenario_revision: 0,
            expected_timetable_revision: 0,
            view: View::Student,
            entity_id: None,
            offset: 0,
            limit: 20,
        };
        assert_eq!(
            run(&args).unwrap_err().code(),
            "CLI_DATABASE_NOT_ACCESSIBLE"
        );
        args.entity_id = Some("S01".to_owned());
        assert_eq!(
            run(&args).unwrap_err().code(),
            "CLI_TIMETABLE_ENTITY_ID_INVALID"
        );
        assert!(!database.exists());
    }
}
