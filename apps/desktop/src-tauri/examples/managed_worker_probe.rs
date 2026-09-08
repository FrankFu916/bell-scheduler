//! Developer install-tree gate: uses the real saved-project application pipeline and worker.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use class_schedule_application::{
    AutoSectioningPolicy, AutoSectioningSolveStatus, CalendarDefinition, CsvImportAuditOptions,
    CsvImportMode, ImportCommitCommand, ImportCommitIntent, ImportedProjectSolve,
    PreparedSolveArtifact, SectioningProfile, SolveExecution, SolveOptions,
    StoredProjectSolveCommand, StoredProjectSolveMode, commit_csv_import,
    execute_durable_stored_project_solve, load_solve_artifact, prepare_stored_project_solve,
    save_prepared_solve_artifact,
};
use class_schedule_desktop::managed_worker::verify_managed_worker_install;
use class_schedule_domain::SchoolProjectId;
use class_schedule_import::{CsvSource, DatasetKind};
use class_schedule_persistence::SqliteStore;
use solver_client::{CancellationToken, SolverClient, SolverRunStatus};

fn fixture_files(
    directory: &Path,
    unsectioned: bool,
) -> Result<BTreeMap<DatasetKind, Vec<u8>>, Box<dyn Error>> {
    let mut files = BTreeMap::new();
    for kind in DatasetKind::ALL {
        if unsectioned
            && matches!(
                kind,
                DatasetKind::TeachingSections
                    | DatasetKind::SectionEnrollments
                    | DatasetKind::CourseOfferings
                    | DatasetKind::FixedActivities
            )
        {
            continue;
        }
        let path = directory.join(format!("{}.csv", kind.as_str()));
        if path.is_file() {
            files.insert(kind, fs::read(path)?);
        }
    }
    Ok(files)
}

fn checked_execution(
    execution: &SolveExecution,
    mode: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let SolveExecution::Completed(completed) = execution else {
        return Err("native fixture precheck failed".into());
    };
    if completed.process.elapsed.is_zero() {
        return Err("no worker process was launched".into());
    }
    if mode == "cancel" || mode == "timeout" {
        let expected = if mode == "cancel" {
            SolverRunStatus::Cancelled
        } else {
            SolverRunStatus::Timeout
        };
        if completed.status != expected
            || !completed.assignments.is_empty()
            || completed.independent_validation.is_some()
            || completed.process.exit_success
        {
            return Err(
                "native cancellation/timeout did not terminate a running worker cleanly".into(),
            );
        }
    } else if !matches!(
        completed.status,
        SolverRunStatus::Feasible | SolverRunStatus::Optimal
    ) || !completed
        .independent_validation
        .as_ref()
        .is_some_and(class_schedule_validation::ValidationReport::is_valid)
        || !completed.process.exit_success
    {
        return Err("native result did not pass independent Hard validation".into());
    }
    Ok(
        serde_json::json!({"status": format!("{:?}", completed.status), "assignmentCount": completed.assignments.len(),
        "independentlyValidated": completed.independent_validation.as_ref().is_some_and(class_schedule_validation::ValidationReport::is_valid),
        "workerWasLaunched": true, "workerExitedSuccessfully": completed.process.exit_success}),
    )
}

fn checked_result(
    result: &ImportedProjectSolve,
    mode: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    match result {
        ImportedProjectSolve::Existing(existing) => {
            let mut report = checked_execution(&existing.execution, mode)?;
            report["executionAttemptCount"] = 1.into();
            Ok(report)
        }
        ImportedProjectSolve::AutoSectioned(sectioning) => {
            if sectioning.status != AutoSectioningSolveStatus::SelectedFeasible
                || sectioning.attempts.len() != 2
            {
                return Err("Input B native candidate loop did not complete".into());
            }
            let selected = sectioning
                .selected_attempt()
                .ok_or("Input B did not select a candidate")?;
            let mut report = checked_execution(&selected.execution, mode)?;
            report["attemptCount"] = sectioning.attempts.len().into();
            report["executionAttemptCount"] = sectioning.attempts.len().into();
            Ok(report)
        }
    }
}

fn durable_round_trip(
    database_path: &Path,
    artifact: PreparedSolveArtifact,
    mode: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let before = checked_result(
        &artifact.result().ok_or("native run has no result")?.result,
        mode,
    )?;
    let mut store = SqliteStore::open(database_path)?;
    let saved = save_prepared_solve_artifact(&mut store, &artifact)?;
    if saved != *artifact.receipt() || artifact.failure().is_some() {
        return Err("saved native terminal receipt differs from the completed run".into());
    }
    drop(store);
    // A killed worker has no response and cannot supply engine/effective-parameter
    // provenance. Compare the actual records instead of inventing one per spawn.
    let expected_provenance = artifact.into_loaded().attempts;

    let reopened = SqliteStore::open(database_path)?;
    let restored = load_solve_artifact(&reopened, &saved.run_id)?;
    if restored.receipt != saved
        || restored.failure.is_some()
        || restored.attempts != expected_provenance
    {
        let difference = serde_json::json!({"mode": mode,
            "receiptEqual": restored.receipt == saved,
            "sourceEqual": restored.receipt.source == saved.source,
            "statusEqual": restored.receipt.status_code == saved.status_code,
            "startedAtEqual": restored.receipt.started_at == saved.started_at,
            "finishedAtEqual": restored.receipt.finished_at == saved.finished_at,
            "artifactHashEqual": restored.receipt.payload_hash == saved.payload_hash,
            "restoredFailurePresent": restored.failure.is_some(),
            "provenanceEqual": restored.attempts == expected_provenance,
            "restoredProvenanceCount": restored.attempts.len(),
            "expectedProvenanceCount": expected_provenance.len()});
        return Err(format!("native artifact round-trip mismatch: {difference}").into());
    }
    let mut report = checked_result(
        &restored
            .result()
            .ok_or("reopened native run has no result")?
            .result,
        mode,
    )?;
    if report != before {
        return Err("native result changed after independent artifact replay".into());
    }
    report["sourcePayloadHash"] = saved.source.payload_hash.into();
    report["artifactPayloadHash"] = saved.payload_hash.into();
    report["runId"] = saved.run_id.into();
    report["runSaved"] = true.into();
    report["databaseReopened"] = true.into();
    report["restoredStatus"] = restored.receipt.status_code.into();
    report["restoredAttemptCount"] = restored.attempts.len().into();
    report["provenanceRestoredExactly"] = true.into();
    report["restoredIndependentlyValidated"] = report["independentlyValidated"].clone();
    Ok(report)
}

fn probe(
    contents: &Path,
    fixtures: &Path,
    mode: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let verified = verify_managed_worker_install(contents)?;
    let unsectioned = mode == "auto";
    let files = fixture_files(fixtures, unsectioned)?;
    let policy = AutoSectioningPolicy::new(10, 12, 16, 20_260_904, SectioningProfile::Balanced, 2)?;
    let project_id = SchoolProjectId::new_v4();
    let command = ImportCommitCommand {
        project_id,
        display_name: "Managed worker install gate".to_owned(),
        intent: ImportCommitIntent::Create,
        options: CsvImportAuditOptions {
            project_stable_key: "install-gate".to_owned(),
            calendar: CalendarDefinition::weekday_with_break(8, 4)?,
            exact_subject_choices: 3,
            mode: if unsectioned {
                CsvImportMode::Unsectioned(policy)
            } else {
                CsvImportMode::ExistingSections
            },
        },
    };
    let database_directory = tempfile::Builder::new()
        .prefix("Bell 运行复核 ")
        .tempdir_in("/private/tmp")?;
    let database_path = database_directory.path().join("projects.sqlite3");
    let mut store = SqliteStore::open(&database_path)?;
    let receipt = commit_csv_import(
        &mut store,
        &command,
        files
            .iter()
            .map(|(&kind, bytes)| CsvSource::new(kind, bytes)),
    )?;
    let original = store.load_project(&project_id.to_string())?;
    let prepared = prepare_stored_project_solve(
        &store,
        &StoredProjectSolveCommand {
            project_id,
            expected_revision: receipt.revision,
            mode: if unsectioned {
                StoredProjectSolveMode::AutoSectioning(policy)
            } else {
                StoredProjectSolveMode::ExistingSections
            },
        },
    )?;
    drop(store);
    let cancellation = CancellationToken::new();
    let cancellation_thread = if mode == "cancel" {
        let trigger = cancellation.clone();
        Some(thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            trigger.cancel();
        }))
    } else {
        None
    };
    let timeout = if mode == "timeout" {
        Duration::from_millis(100)
    } else {
        Duration::from_secs(40)
    };
    let client = SolverClient::new(verified.sidecar_spec()).timeout(timeout);
    let seconds = if mode == "cancel" || mode == "timeout" {
        30
    } else {
        5
    };
    let outcome = execute_durable_stored_project_solve(
        prepared,
        &SolveOptions::reproducible(20_260_904, Duration::from_secs(seconds)),
        &client,
        &cancellation,
    );
    if let Some(thread) = cancellation_thread {
        thread.join().map_err(|_| "cancellation thread failed")?;
    }
    drop(client);
    let mut report = durable_round_trip(&database_path, outcome?, mode)?;
    let store = SqliteStore::open(&database_path)?;
    if store.load_project(&project_id.to_string())? != original {
        return Err("install gate changed source project".into());
    }
    report["mode"] = mode.into();
    report["manifestSha256"] = verified.manifest_sha256().into();
    report["sourceUnchanged"] = true.into();
    Ok(report)
}

fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() != 3 {
        eprintln!(
            "usage: managed_worker_probe APP_CONTENTS FIXTURE_DIRECTORY existing|auto|cancel|timeout"
        );
        std::process::exit(2);
    }
    let mode = arguments[2].to_string_lossy();
    if !matches!(mode.as_ref(), "existing" | "auto" | "cancel" | "timeout") {
        eprintln!("unsupported probe mode");
        std::process::exit(2);
    }
    match probe(
        &PathBuf::from(&arguments[0]),
        &PathBuf::from(&arguments[1]),
        &mode,
    ) {
        Ok(report) => println!("{report}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
