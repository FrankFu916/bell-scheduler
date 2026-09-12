//! Persistence/replay transport; completed output is rebuilt by application before export.

use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use class_schedule_application::{
    LoadedSolveArtifact, SolveArtifactError, list_project_solve_artifacts, load_solve_artifact,
};
use class_schedule_domain::SchoolProjectId;
use class_schedule_persistence::SqliteStore;
use serde_json::json;

use crate::{CliFailure, RunOutcome, commit_outputs, ensure_output_target_available};

#[derive(Clone, Debug, Args)]
pub(super) struct ListRunsArgs {
    #[arg(long)]
    database: PathBuf,
    #[arg(long)]
    project_id: SchoolProjectId,
    #[arg(long, default_value_t = 20)]
    limit: u32,
    #[arg(long, default_value_t = 0)]
    offset: u32,
}

#[derive(Clone, Debug, Args)]
pub(super) struct ExportRunArgs {
    #[arg(long)]
    database: PathBuf,
    #[arg(long)]
    run_id: String,
    /// New or empty directory. Existing export files are never overwritten.
    #[arg(long)]
    output_dir: PathBuf,
}

pub(super) fn open_database(path: &Path) -> Result<SqliteStore, CliFailure> {
    open_existing_database(path, false)
}

pub(super) fn open_readonly_database(path: &Path) -> Result<SqliteStore, CliFailure> {
    open_existing_database(path, true)
}

fn open_existing_database(path: &Path, readonly: bool) -> Result<SqliteStore, CliFailure> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        CliFailure::new(
            "CLI_DATABASE_NOT_ACCESSIBLE",
            "cannot access the existing project database",
        )
    })?;
    if !metadata.is_file() {
        return Err(CliFailure::new(
            "CLI_DATABASE_NOT_FILE",
            "project database must be a regular file",
        ));
    }
    let result = if readonly {
        SqliteStore::open_readonly(path)
    } else {
        SqliteStore::open(path)
    };
    result
        .map_err(|error| CliFailure::new(error.code(), "cannot open the existing project database"))
}

pub(super) fn artifact_failure(error: SolveArtifactError) -> CliFailure {
    match error {
        SolveArtifactError::Import(error) => crate::import_command::command_failure(error),
        other => CliFailure::new(
            other.code(),
            "the completed run could not be saved or independently revalidated",
        ),
    }
}

pub(super) fn list(args: &ListRunsArgs) -> Result<RunOutcome, CliFailure> {
    let store = open_database(&args.database)?;
    let page = list_project_solve_artifacts(&store, args.project_id, args.limit, args.offset)
        .map_err(artifact_failure)?;
    let rows = page.runs.into_iter().map(|run| json!({
        "run_id": run.run_id, "project_id": run.project_id.to_string(),
        "project_revision": run.project_revision.to_string(), "status": run.status_code,
        "artifact_schema_version": run.artifact_schema_version,
        "started_at": run.started_at.to_rfc3339(), "finished_at": run.finished_at.to_rfc3339(),
    })).collect::<Vec<_>>();
    let summary = json!({
        "schema_version": 1, "runs": rows, "has_more": page.has_more,
        "next_offset": page.next_offset, "validation": "metadata_only",
    });
    Ok(RunOutcome {
        rendered_summary: render(&summary)?,
        exit_code: 0,
    })
}

pub(super) fn export(args: &ExportRunArgs) -> Result<RunOutcome, CliFailure> {
    ensure_output_target_available(&args.output_dir)?;
    let store = open_database(&args.database)?;
    let loaded = load_solve_artifact(&store, &args.run_id).map_err(artifact_failure)?;
    publish_loaded(loaded, &args.output_dir)
}

pub(super) fn publish_loaded(
    loaded: LoadedSolveArtifact,
    output_dir: &Path,
) -> Result<RunOutcome, CliFailure> {
    if let Some(solved) = loaded.result {
        return super::solve_project::publish_result(
            solved,
            &loaded.options,
            output_dir,
            loaded.receipt.run_id,
        );
    }
    // A recorded failure remains a failure even though saving its diagnostic artifact succeeded.
    let summary = json!({
        "schema_version": 1,
        "saved_source": {
            "project_id": loaded.receipt.source.project_id.to_string(),
            "revision": loaded.receipt.source.revision.to_string(),
            "request_id": loaded.receipt.run_id, "saved_run_id": loaded.receipt.run_id,
            "payload_hash_algorithm": "blake3", "payload_hash": loaded.receipt.source.payload_hash,
            "scope": "project-import", "adopted": false,
        },
        "result": {"status": loaded.receipt.status_code, "status_detail_code": loaded.receipt.termination_code,
            "publishable": false, "hard_valid": null},
        "failure": loaded.failure,
    });
    let rendered_summary = render(&summary)?;
    commit_outputs(output_dir, rendered_summary.as_bytes(), None, None, None)?;
    Ok(RunOutcome {
        rendered_summary,
        exit_code: 2,
    })
}

fn render(value: &serde_json::Value) -> Result<String, CliFailure> {
    serde_json::to_string_pretty(value).map_err(|_| {
        CliFailure::new(
            "CLI_SUMMARY_SERIALIZATION_FAILED",
            "cannot encode run metadata",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn history_and_replay_commands_do_not_require_a_worker() {
        assert!(
            crate::Cli::try_parse_from([
                "class-schedule",
                "list-runs",
                "--database",
                "project.sqlite3",
                "--project-id",
                "88888888-1111-4111-8111-111111111111",
            ])
            .is_ok()
        );
        assert!(
            crate::Cli::try_parse_from([
                "class-schedule",
                "export-run",
                "--database",
                "project.sqlite3",
                "--run-id",
                "saved-run",
                "--output-dir",
                "new-output",
            ])
            .is_ok()
        );
    }

    #[test]
    fn history_never_creates_a_missing_database() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("missing.sqlite3");
        let error = list(&ListRunsArgs {
            database: database.clone(),
            project_id: "88888888-1111-4111-8111-111111111111".parse().unwrap(),
            limit: 20,
            offset: 0,
        })
        .unwrap_err();
        assert_eq!(error.code(), "CLI_DATABASE_NOT_ACCESSIBLE");
        assert!(!database.exists());
    }

    #[test]
    fn replay_refuses_existing_export_before_accessing_database() {
        let directory = tempfile::tempdir().unwrap();
        let output_dir = directory.path().join("export");
        fs::create_dir(&output_dir).unwrap();
        fs::write(output_dir.join("summary.json"), b"existing").unwrap();
        let error = export(&ExportRunArgs {
            database: directory.path().join("missing.sqlite3"),
            run_id: "missing".to_owned(),
            output_dir: output_dir.clone(),
        })
        .unwrap_err();
        assert_eq!(error.code(), "CLI_OUTPUT_DIRECTORY_NOT_EMPTY");
        assert_eq!(
            fs::read(output_dir.join("summary.json")).unwrap(),
            b"existing"
        );
    }

    #[test]
    fn missing_run_and_unbounded_history_return_stable_codes() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("empty.sqlite3");
        SqliteStore::open(&database).unwrap();
        let error = export(&ExportRunArgs {
            database: database.clone(),
            run_id: "missing".to_owned(),
            output_dir: directory.path().join("output"),
        })
        .unwrap_err();
        assert_eq!(error.code(), "PERSISTENCE_SOLVE_RUN_NOT_FOUND");
        assert!(!directory.path().join("output").exists());
        let error = list(&ListRunsArgs {
            database,
            project_id: "88888888-1111-4111-8111-111111111111".parse().unwrap(),
            limit: 101,
            offset: 0,
        })
        .unwrap_err();
        assert_eq!(error.code(), "APPLICATION_SOLVE_LIST_INVALID_LIMIT");
    }
}
