//! CLI transport for an application-owned frozen `SQLite` project revision.

use super::{
    CliFailure, InputMode, RunOutcome, SavedSourceSummary, SolveSummaryContext, SolverRuntimeArgs,
    build_solver_client, ensure_output_target_available, publish_solved_project,
    sectioning_profile, solve_options, validate_runtime_options,
};
use clap::Args;
use class_schedule_application::{
    AutoSectioningPolicy, StoredProjectSolveCommand, StoredProjectSolveError,
    StoredProjectSolveMode, execute_durable_stored_project_solve, prepare_stored_project_solve,
    save_prepared_solve_artifact,
};
use class_schedule_domain::SchoolProjectId;
use serde_json::json;
use solver_client::CancellationToken;
#[cfg(test)]
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug, Args)]
pub(super) struct SolveProjectArgs {
    /// Existing local `SQLite` database containing the imported project.
    #[arg(long)]
    database: PathBuf,
    #[arg(long)]
    project_id: SchoolProjectId,
    /// The saved revision to freeze; revision 0 is a valid initial import.
    #[arg(long)]
    expected_revision: u64,
    /// Explicitly choose existing teaching sections or a fresh auto-sectioning policy.
    #[arg(long, value_enum)]
    input_mode: InputMode,
    #[arg(long)]
    section_min_size: Option<u16>,
    #[arg(long)]
    section_target_size: Option<u16>,
    #[arg(long)]
    section_max_size: Option<u16>,
    #[arg(long)]
    sectioning_candidates: Option<u8>,
    #[command(flatten)]
    runtime: SolverRuntimeArgs,
}

pub(super) fn run(args: &SolveProjectArgs) -> Result<RunOutcome, CliFailure> {
    let mode = input_mode(args)?;
    ensure_output_target_available(&args.runtime.output_dir)?;
    let store = super::run_history::open_database(&args.database)?;
    let prepared = prepare_stored_project_solve(
        &store,
        &StoredProjectSolveCommand {
            project_id: args.project_id,
            expected_revision: args.expected_revision,
            mode,
        },
    )
    .map_err(command_failure)?;
    // The frozen preparation owns its source. Release SQLite before starting the sidecar.
    drop(store);
    validate_runtime_options(&args.runtime)?;
    let options = solve_options(&args.runtime);
    let client = build_solver_client(&args.runtime)?;
    let artifact = execute_durable_stored_project_solve(
        prepared,
        &options,
        &client,
        &CancellationToken::new(),
    )
    .map_err(super::run_history::artifact_failure)?;
    let mut store = super::run_history::open_database(&args.database)?;
    save_prepared_solve_artifact(&mut store, &artifact)
        .map_err(super::run_history::artifact_failure)?;
    drop(store);
    let saved_run_id = artifact.run_id().to_owned();
    super::run_history::publish_loaded(artifact.into_loaded(), &args.runtime.output_dir).map_err(
        |error| {
            error.with_details(json!({
                "saved_run_id": saved_run_id, "run_saved": true, "recovery_command": "export-run",
            }))
        },
    )
}

pub(super) fn publish_result(
    solved: class_schedule_application::StoredProjectSolve,
    options: &class_schedule_application::SolveOptions,
    output_dir: &std::path::Path,
    run_id: String,
) -> Result<RunOutcome, CliFailure> {
    publish_solved_project(
        &SolveSummaryContext {
            context: &solved.context,
            options,
            output_dir,
            saved_source: Some(SavedSourceSummary {
                project_id: solved.receipt.project_id.to_string(),
                revision: solved.receipt.revision.to_string(),
                request_id: solved.context.request_id.clone(),
                payload_hash_algorithm: "blake3",
                payload_hash: solved.receipt.payload_hash,
                scope: "project-import",
                adopted: false,
                saved_run_id: Some(run_id),
            }),
        },
        solved.result,
    )
}

fn input_mode(args: &SolveProjectArgs) -> Result<StoredProjectSolveMode, CliFailure> {
    match args.input_mode {
        InputMode::ExistingSections => {
            if args.section_min_size.is_some()
                || args.section_target_size.is_some()
                || args.section_max_size.is_some()
                || args.sectioning_candidates.is_some()
            {
                return Err(CliFailure::new(
                    "CLI_SAVED_SOLVE_SECTIONING_POLICY_NOT_ALLOWED",
                    "existing-sections mode does not accept auto-sectioning parameters",
                ));
            }
            Ok(StoredProjectSolveMode::ExistingSections)
        }
        InputMode::AutoSectioning => {
            let (Some(minimum), Some(target), Some(maximum), Some(candidates)) = (
                args.section_min_size,
                args.section_target_size,
                args.section_max_size,
                args.sectioning_candidates,
            ) else {
                return Err(CliFailure::new(
                    "CLI_SAVED_SOLVE_SECTIONING_POLICY_REQUIRED",
                    "auto-sectioning requires explicit minimum, target, maximum size and candidate count",
                ));
            };
            let policy = AutoSectioningPolicy::new(
                minimum,
                target,
                maximum,
                args.runtime.seed,
                sectioning_profile(args.runtime.execution),
                candidates,
            )
            .map_err(|error| CliFailure::new(error.code(), "auto-sectioning policy is invalid"))?;
            Ok(StoredProjectSolveMode::AutoSectioning(policy))
        }
    }
}

fn command_failure(error: StoredProjectSolveError) -> CliFailure {
    match error {
        StoredProjectSolveError::Import(error) => super::import_command::command_failure(error),
        StoredProjectSolveError::ModeMismatch {
            requested,
            sectioning_required,
        } => CliFailure::new(
            "APPLICATION_STORED_PROJECT_MODE_MISMATCH",
            "requested input mode does not match the saved project's sectioning state",
        )
        .with_details(json!({
            "requestedMode": match requested {
                StoredProjectSolveMode::ExistingSections => "existing_sections",
                StoredProjectSolveMode::AutoSectioning(_) => "auto_sectioning",
            },
            "sectioningRequired": sectioning_required,
        })),
        other => CliFailure::new(other.code(), "the saved project solve could not complete"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cli;
    use clap::Parser;
    use class_schedule_application::{
        CalendarDefinition, CsvImportAuditOptions, CsvImportMode, ImportCommitCommand,
        ImportCommitIntent, commit_csv_import,
    };
    use class_schedule_import::CsvSource;
    use class_schedule_persistence::SqliteStore;
    use std::path::Path;

    const PROJECT_ID: &str = "88888888-1111-4111-8111-111111111111";

    fn save_fixture(database: &Path) {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/small");
        let sources: Vec<_> = crate::DATASETS
            .into_iter()
            .map(|kind| {
                (
                    kind,
                    fs::read(fixture.join(format!("{}.csv", kind.as_str()))).unwrap(),
                )
            })
            .collect();
        let mut store = SqliteStore::open(database).unwrap();
        commit_csv_import(
            &mut store,
            &ImportCommitCommand {
                project_id: PROJECT_ID.parse().unwrap(),
                display_name: "CLI saved-source test".into(),
                intent: ImportCommitIntent::Create,
                options: CsvImportAuditOptions {
                    project_stable_key: "cli-saved-source-test".into(),
                    calendar: CalendarDefinition::weekday_with_break(8, 4).unwrap(),
                    exact_subject_choices: 3,
                    mode: CsvImportMode::ExistingSections,
                },
            },
            sources
                .iter()
                .map(|(kind, bytes)| CsvSource::new(*kind, bytes)),
        )
        .unwrap();
    }

    fn arguments(directory: &Path) -> Vec<String> {
        vec![
            "class-schedule".into(),
            "solve-project".into(),
            "--database".into(),
            directory.join("project.sqlite3").to_str().unwrap().into(),
            "--project-id".into(),
            PROJECT_ID.into(),
            "--expected-revision".into(),
            "0".into(),
            "--input-mode".into(),
            "existing-sections".into(),
            "--worker".into(),
            directory
                .join("worker-must-not-start")
                .to_str()
                .unwrap()
                .into(),
            "--output-dir".into(),
            directory.join("output").to_str().unwrap().into(),
            "--time-limit-seconds".into(),
            "1".into(),
        ]
    }

    fn replace_argument(arguments: &mut [String], name: &str, value: &str) {
        let index = arguments
            .iter()
            .position(|argument| argument == name)
            .unwrap();
        arguments[index + 1] = value.to_owned();
    }

    #[test]
    fn saved_solve_requires_typed_identity_revision_and_explicit_input_mode() {
        let directory = tempfile::tempdir().unwrap();
        for required in ["--expected-revision", "--input-mode"] {
            let mut values = arguments(directory.path());
            let index = values.iter().position(|value| value == required).unwrap();
            values.drain(index..=index + 1);
            assert!(Cli::try_parse_from(values).is_err());
        }
        let mut invalid_id = arguments(directory.path());
        replace_argument(&mut invalid_id, "--project-id", "legacy-not-a-uuid");
        assert!(Cli::try_parse_from(invalid_id).is_err());
    }

    #[test]
    fn saved_solve_rejects_stale_revision_before_worker_access_and_keeps_source() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("project.sqlite3");
        save_fixture(&database);
        let mut store = SqliteStore::open(&database).unwrap();
        let mut replacement = store.load_project(PROJECT_ID).unwrap();
        replacement.revision = 1;
        replacement.display_name = "replacement wins".into();
        store.replace_project(0, &replacement).unwrap();
        let error =
            crate::run(Cli::try_parse_from(arguments(directory.path())).unwrap()).unwrap_err();
        assert_eq!(error.code(), "PERSISTENCE_REVISION_CONFLICT");
        assert_eq!(error.details["expectedRevision"], "0");
        assert_eq!(error.details["actualRevision"], "1");
        assert!(!directory.path().join("output").exists());
        assert_eq!(store.load_project(PROJECT_ID).unwrap(), replacement);
        assert!(
            !serde_json::to_string(&error.as_report())
                .unwrap()
                .contains("replacement wins")
        );
    }

    #[test]
    fn saved_solve_missing_project_and_mode_mismatch_precede_worker_access() {
        let directory = tempfile::tempdir().unwrap();
        save_fixture(&directory.path().join("project.sqlite3"));
        let mut missing = arguments(directory.path());
        replace_argument(
            &mut missing,
            "--project-id",
            "88888888-2222-4222-8222-222222222222",
        );
        let error = crate::run(Cli::try_parse_from(missing).unwrap()).unwrap_err();
        assert_eq!(error.code(), "PERSISTENCE_PROJECT_NOT_FOUND");
        let mut mismatch = arguments(directory.path());
        replace_argument(&mut mismatch, "--input-mode", "auto-sectioning");
        mismatch.extend(
            [
                "--section-min-size",
                "10",
                "--section-target-size",
                "12",
                "--section-max-size",
                "16",
                "--sectioning-candidates",
                "3",
            ]
            .map(str::to_owned),
        );
        let error = crate::run(Cli::try_parse_from(mismatch).unwrap()).unwrap_err();
        assert_eq!(error.code(), "APPLICATION_STORED_PROJECT_MODE_MISMATCH");
        assert_eq!(error.details["sectioningRequired"], false);
        assert!(!directory.path().join("output").exists());
    }

    #[test]
    fn saved_solve_accepts_initial_revision_zero_then_checks_real_worker() {
        let directory = tempfile::tempdir().unwrap();
        save_fixture(&directory.path().join("project.sqlite3"));
        let error =
            crate::run(Cli::try_parse_from(arguments(directory.path())).unwrap()).unwrap_err();
        assert_eq!(error.code(), "CLI_WORKER_NOT_ACCESSIBLE");
        assert!(!directory.path().join("output").exists());
    }

    #[test]
    fn saved_solve_requires_explicit_sectioning_parameters_and_refuses_them_for_input_a() {
        let directory = tempfile::tempdir().unwrap();
        let mut values = arguments(directory.path());
        replace_argument(&mut values, "--input-mode", "auto-sectioning");
        let error = crate::run(Cli::try_parse_from(values).unwrap()).unwrap_err();
        assert_eq!(error.code(), "CLI_SAVED_SOLVE_SECTIONING_POLICY_REQUIRED");
        let mut values = arguments(directory.path());
        values.extend(["--section-min-size".into(), "10".into()]);
        let error = crate::run(Cli::try_parse_from(values).unwrap()).unwrap_err();
        assert_eq!(
            error.code(),
            "CLI_SAVED_SOLVE_SECTIONING_POLICY_NOT_ALLOWED"
        );
        assert!(!directory.path().join("project.sqlite3").exists());
    }

    #[test]
    fn saved_solve_never_overwrites_existing_artifacts_or_creates_missing_database() {
        let directory = tempfile::tempdir().unwrap();
        let error =
            crate::run(Cli::try_parse_from(arguments(directory.path())).unwrap()).unwrap_err();
        assert_eq!(error.code(), "CLI_DATABASE_NOT_ACCESSIBLE");
        assert!(!directory.path().join("project.sqlite3").exists());
        let output = directory.path().join("output");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("summary.json"), b"existing artifact").unwrap();
        let error =
            crate::run(Cli::try_parse_from(arguments(directory.path())).unwrap()).unwrap_err();
        assert_eq!(error.code(), "CLI_OUTPUT_DIRECTORY_NOT_EMPTY");
        assert_eq!(
            fs::read(output.join("summary.json")).unwrap(),
            b"existing artifact"
        );
        assert!(!directory.path().join("project.sqlite3").exists());
    }
}
