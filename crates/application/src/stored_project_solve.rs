use class_schedule_domain::SchoolProjectId;
use class_schedule_persistence::{PersistenceError, SqliteStore};
use solver_client::{CancellationToken, SolverClient};
use solver_contract::SolveMode;
use thiserror::Error;

use crate::{
    AutoSectioningPolicy, ImportCommandError, ImportCommitReceipt, ImportedProjectDocument,
    ImportedProjectSolve, ImportedSolveError, SolveContext, SolveOptions, load_imported_project,
    solve_imported_project,
};

/// Explicit input interpretation for a solve of a saved import revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredProjectSolveMode {
    ExistingSections,
    AutoSectioning(AutoSectioningPolicy),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoredProjectSolveCommand {
    pub project_id: SchoolProjectId,
    pub expected_revision: u64,
    pub mode: StoredProjectSolveMode,
}

/// Owned, revalidated input frozen at one project revision. Execution never reloads it.
#[derive(Debug)]
pub struct PreparedStoredProjectSolve {
    pub(crate) receipt: ImportCommitReceipt,
    pub(crate) display_name: String,
    pub(crate) document: ImportedProjectDocument,
    pub(crate) mode: StoredProjectSolveMode,
}

impl PreparedStoredProjectSolve {
    #[must_use]
    pub const fn receipt(&self) -> &ImportCommitReceipt {
        &self.receipt
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    #[must_use]
    pub const fn mode(&self) -> StoredProjectSolveMode {
        self.mode
    }
}

/// Run output and its immutable input identity; this is not an adopted project timetable.
#[derive(Debug)]
pub struct StoredProjectSolve {
    pub receipt: ImportCommitReceipt,
    pub display_name: String,
    /// `project-import` is a protocol scope, not a persisted Scenario.
    pub context: SolveContext,
    pub result: ImportedProjectSolve,
}

#[derive(Debug, Error)]
pub enum StoredProjectSolveError {
    #[error(transparent)]
    Import(#[from] ImportCommandError),
    #[error("requested solve input mode does not match the stored project")]
    ModeMismatch {
        requested: StoredProjectSolveMode,
        sectioning_required: bool,
    },
    #[error("saved-project solve currently supports only Generate")]
    UnsupportedSolveMode { mode: SolveMode },
    #[error(transparent)]
    Solve(#[from] ImportedSolveError),
}

impl StoredProjectSolveError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Import(error) => error.code(),
            Self::ModeMismatch { .. } => "APPLICATION_STORED_PROJECT_MODE_MISMATCH",
            Self::UnsupportedSolveMode { .. } => {
                "APPLICATION_STORED_PROJECT_SOLVE_MODE_UNSUPPORTED"
            }
            Self::Solve(error) => error.code(),
        }
    }
}

/// Loads and revalidates the stored import, checks the expected revision, and freezes it.
///
/// This performs no writes or worker calls. Another writer may replace the project after
/// preparation; that does not change the prepared input or the resulting run's identity.
///
/// # Errors
/// Returns existing load/revalidation errors, a revision conflict, an input-mode mismatch,
/// or an invalid explicit sectioning policy.
pub fn prepare_stored_project_solve(
    store: &SqliteStore,
    command: &StoredProjectSolveCommand,
) -> Result<PreparedStoredProjectSolve, StoredProjectSolveError> {
    let loaded = load_imported_project(store, command.project_id)?;
    if loaded.receipt.revision != command.expected_revision {
        return Err(
            ImportCommandError::Persistence(PersistenceError::RevisionConflict {
                project_id: command.project_id.to_string(),
                expected_revision: command.expected_revision,
                actual_revision: loaded.receipt.revision,
            })
            .into(),
        );
    }
    let requested_sectioning = matches!(command.mode, StoredProjectSolveMode::AutoSectioning(_));
    if requested_sectioning != loaded.receipt.sectioning_required {
        return Err(StoredProjectSolveError::ModeMismatch {
            requested: command.mode,
            sectioning_required: loaded.receipt.sectioning_required,
        });
    }
    if let StoredProjectSolveMode::AutoSectioning(policy) = command.mode {
        // Public policy fields can be constructed without the validating constructor.
        AutoSectioningPolicy::new(
            policy.minimum_size,
            policy.target_size,
            policy.maximum_size,
            policy.seed,
            policy.profile,
            policy.candidate_count,
        )
        .map_err(ImportCommandError::Sectioning)?;
    }
    Ok(PreparedStoredProjectSolve {
        receipt: loaded.receipt,
        display_name: loaded.display_name,
        document: loaded.document,
        mode: command.mode,
    })
}

/// Executes the existing independently validated pipeline against the frozen input.
///
/// No database is accessed and no source document, Scenario, or timetable is persisted.
/// Input B's selected candidate belongs only to this run's result.
///
/// # Errors
/// Rejects unsupported solve modes and propagates existing sectioning, worker, protocol,
/// independent Hard-validation, and scoring errors without producing a run artifact.
pub fn execute_stored_project_solve(
    prepared: PreparedStoredProjectSolve,
    options: &SolveOptions,
    client: &SolverClient,
    cancellation: &CancellationToken,
) -> Result<StoredProjectSolve, StoredProjectSolveError> {
    if options.mode != SolveMode::Generate {
        return Err(StoredProjectSolveError::UnsupportedSolveMode { mode: options.mode });
    }
    let context = SolveContext::with_generated_request_id(
        prepared.receipt.project_id.to_string(),
        prepared.receipt.revision,
        "project-import",
        prepared.receipt.revision,
    );
    let policy = match prepared.mode {
        StoredProjectSolveMode::ExistingSections => None,
        StoredProjectSolveMode::AutoSectioning(policy) => Some(policy),
    };
    let result = solve_imported_project(
        &prepared.document.import_batch,
        &prepared.document.calendar,
        &prepared.document.project_stable_key,
        policy,
        &context,
        options,
        client,
        cancellation,
    )?;
    Ok(StoredProjectSolve {
        receipt: prepared.receipt,
        display_name: prepared.display_name,
        context,
        result,
    })
}
