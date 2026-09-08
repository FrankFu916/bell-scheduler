//! Versioned local run evidence. Stored responses are untrusted until historical replay.

use std::time::{Duration, Instant};

use chrono::{DateTime, Timelike, Utc};
use class_schedule_domain::SchoolProjectId;
use class_schedule_persistence::{
    MAXIMUM_SOLVE_ARTIFACT_BYTES, PersistenceError, SolveArtifactDocument, SolverRunRecord,
    SqliteStore,
};
use class_schedule_scheduling::SchedulingProblemSnapshot;
use class_schedule_scoring::ObjectiveVector;
use class_schedule_sectioning::SECTIONING_ALGORITHM_VERSION;
use class_schedule_validation::ValidationReport;
use prost::Message;
use serde::{Deserialize, Serialize};
use solver_client::{
    CancellationToken, ProcessReport, SolverClient, SolverClientError, SolverRunStatus,
};
use solver_contract::{
    ObjectiveTierDefinition, SolveMode, SolverParameters, SolverProfile, ValidateContract,
};
use thiserror::Error;

use crate::pipeline::{score_success, solve_imported_project_with_executor};
use crate::{
    AutoSectioningPolicy, AutoSectioningSolveStatus, ImportCommandError, ImportCommitReceipt,
    ImportedProjectDocument, ImportedProjectSolve, PreparedStoredProjectSolve,
    SolveApplicationError, SolveContext, SolveExecution, SolveOptions, StoredProjectSolve,
    StoredProjectSolveError, StoredProjectSolveMode, execute_solve, prepare_solve,
};

mod replay;
pub use replay::{list_project_solve_artifacts, load_solve_artifact};

/// A change to semantic compilation, canonical JSON, Hard validation or balanced scoring must
/// explicitly revise this compatibility selector before old artifacts can be replayed.
pub const SOLVE_ARTIFACT_SEMANTICS_VERSION: &str = "semantic-json-blake3-hard-balanced.v1";
pub const SOLVE_ARTIFACT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum SolveArtifactError {
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    Import(#[from] ImportCommandError),
    #[error(transparent)]
    Command(#[from] StoredProjectSolveError),
    #[error("run artifact encoding failed")]
    Encoding(#[from] serde_json::Error),
    #[error("run artifact failed validation: {code}")]
    Invalid { code: &'static str },
}

impl SolveArtifactError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Persistence(error) => error.code(),
            Self::Import(error) => error.code(),
            Self::Command(error) => error.code(),
            Self::Encoding(_) => "APPLICATION_SOLVE_ARTIFACT_ENCODING",
            Self::Invalid { code } => code,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSolveFailure {
    pub code: String,
    pub application_code: String,
    pub validation_problem_codes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SolveArtifactReceipt {
    pub run_id: String,
    pub source: ImportCommitReceipt,
    pub status_code: String,
    pub termination_code: String,
    pub artifact_schema_version: u32,
    pub payload_hash: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

/// Revalidated result or an honestly recorded failure. Neither variant adopts a timetable.
#[derive(Debug)]
pub struct LoadedSolveArtifact {
    pub receipt: SolveArtifactReceipt,
    pub display_name: String,
    pub mode: StoredProjectSolveMode,
    pub options: SolveOptions,
    pub result: Option<StoredProjectSolve>,
    pub failure: Option<StoredSolveFailure>,
    pub attempts: Vec<SolverRunRecord>,
    source_document: ImportedProjectDocument,
}

impl LoadedSolveArtifact {
    #[must_use]
    pub const fn source_document(&self) -> &ImportedProjectDocument {
        &self.source_document
    }
    #[must_use]
    pub const fn options(&self) -> &SolveOptions {
        &self.options
    }
    #[must_use]
    pub const fn receipt(&self) -> &SolveArtifactReceipt {
        &self.receipt
    }
    #[must_use]
    pub fn result(&self) -> Option<&StoredProjectSolve> {
        self.result.as_ref()
    }
    #[must_use]
    pub fn into_result(self) -> Option<StoredProjectSolve> {
        self.result
    }
}

/// All validation and JSON encoding complete before the persistence transaction starts.
#[derive(Debug)]
pub struct PreparedSolveArtifact {
    document: SolveArtifactDocument,
    loaded: LoadedSolveArtifact,
}

impl PreparedSolveArtifact {
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.document.run_id
    }
    #[must_use]
    pub fn status_code(&self) -> &str {
        &self.document.status_code
    }
    #[must_use]
    pub fn result(&self) -> Option<&StoredProjectSolve> {
        self.loaded.result.as_ref()
    }
    #[must_use]
    pub fn failure(&self) -> Option<&StoredSolveFailure> {
        self.loaded.failure.as_ref()
    }
    #[must_use]
    pub const fn receipt(&self) -> &SolveArtifactReceipt {
        &self.loaded.receipt
    }
    #[must_use]
    pub fn into_loaded(self) -> LoadedSolveArtifact {
        self.loaded
    }
    #[must_use]
    pub fn into_result(self) -> Option<StoredProjectSolve> {
        self.loaded.result
    }
    #[must_use]
    pub const fn options(&self) -> &SolveOptions {
        &self.loaded.options
    }
    #[must_use]
    pub const fn source_document(&self) -> &ImportedProjectDocument {
        &self.loaded.source_document
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectSolveArtifactSummary {
    pub run_id: String,
    pub project_id: SchoolProjectId,
    pub project_revision: u64,
    pub artifact_schema_version: u32,
    pub status_code: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectSolveArtifactPage {
    pub runs: Vec<ProjectSolveArtifactSummary>,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactPayload {
    schema_version: u32,
    semantics_version: String,
    sectioning_algorithm_version: String,
    protocol_version: u32,
    run_id: String,
    project_id: SchoolProjectId,
    project_revision: u64,
    source_payload_hash: [u8; 32],
    display_name: String,
    sectioning_policy: Option<AutoSectioningPolicy>,
    options: ArtifactOptions,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    terminal: TerminalSummary,
    cancellation_observed: bool,
    attempts: Vec<ArtifactAttempt>,
    failure: Option<RecordedFailure>,
    sectioning: Option<SectioningEvidence>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct TerminalSummary {
    status_code: String,
    termination_code: String,
    selected_attempt_index: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SectioningEvidence {
    provenance: class_schedule_sectioning::SectioningRunProvenance,
    candidate_provenance: Vec<class_schedule_sectioning::CandidateProvenance>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactOptions {
    parameters: Vec<u8>,
    objective_tiers: Vec<Vec<u8>>,
}

impl ArtifactOptions {
    fn encode(options: &SolveOptions) -> Result<Self, SolveArtifactError> {
        require_generate(options)?;
        let time_limit_millis = u64::try_from(options.time_limit.as_millis())
            .map_err(|_| invalid("APPLICATION_TIME_LIMIT_OVERFLOW"))?;
        let parameters = SolverParameters {
            seed: options.seed,
            mode: options.mode as i32,
            profile: options.profile as i32,
            reproducible: options.reproducible,
            time_limit_millis,
            worker_count: options.worker_count,
            collect_diagnostics: options.collect_diagnostics,
            memory_limit_bytes: options.memory_limit_bytes,
            relative_gap_limit_ppm: options.relative_gap_limit_ppm,
        };
        parameters
            .validate_contract()
            .map_err(|_| invalid("APPLICATION_INVALID_SOLVER_PARAMETERS"))?;
        // Existing SQLite provenance uses signed INTEGER. Reject before launching a worker.
        if parameters.seed > i64::MAX as u64 || parameters.time_limit_millis > i64::MAX as u64 {
            return Err(invalid("APPLICATION_SOLVE_ARTIFACT_PARAMETER_RANGE"));
        }
        Ok(Self {
            parameters: parameters.encode_to_vec(),
            objective_tiers: options
                .objective_tiers
                .iter()
                .map(Message::encode_to_vec)
                .collect(),
        })
    }

    fn decode(&self) -> Result<SolveOptions, SolveArtifactError> {
        let parameters = decode::<SolverParameters>(&self.parameters)?;
        parameters
            .validate_contract()
            .map_err(|_| invalid("APPLICATION_SOLVE_ARTIFACT_PARAMETERS"))?;
        if parameters.mode != SolveMode::Generate as i32 {
            return Err(invalid("APPLICATION_SOLVE_ARTIFACT_PARAMETERS"));
        }
        let options = SolveOptions {
            mode: SolveMode::Generate,
            profile: SolverProfile::try_from(parameters.profile)
                .map_err(|_| invalid("APPLICATION_SOLVE_ARTIFACT_PARAMETERS"))?,
            seed: parameters.seed,
            reproducible: parameters.reproducible,
            time_limit: Duration::from_millis(parameters.time_limit_millis),
            worker_count: parameters.worker_count,
            collect_diagnostics: parameters.collect_diagnostics,
            memory_limit_bytes: parameters.memory_limit_bytes,
            relative_gap_limit_ppm: parameters.relative_gap_limit_ppm,
            objective_tiers: self
                .objective_tiers
                .iter()
                .map(|bytes| decode::<ObjectiveTierDefinition>(bytes))
                .collect::<Result<_, _>>()?,
            incumbent_assignments: Vec::new(),
        };
        Self::encode(&options)?;
        Ok(options)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactAttempt {
    request_id: String,
    snapshot_hash: [u8; 32],
    request: Option<Vec<u8>>,
    response: Option<Vec<u8>>,
    status_code: String,
    process: Option<ProcessEvidence>,
    validation: Option<ValidationReport>,
    quality: Option<ObjectiveVector>,
    output_hash: Option<[u8; 32]>,
    validation_elapsed: Option<Duration>,
    scoring_elapsed: Option<Duration>,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecordedFailure {
    failure: StoredSolveFailure,
    /// Present only when failure occurred in the timetable executor.
    request: Option<Vec<u8>>,
    after_attempts: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProcessEvidence {
    elapsed: Duration,
    exit_code: Option<i32>,
    exit_success: bool,
    captured_stderr_bytes: usize,
    total_stderr_bytes: usize,
    stderr_truncated: bool,
    stderr_read_failed: bool,
}

impl From<&ProcessReport> for ProcessEvidence {
    fn from(process: &ProcessReport) -> Self {
        Self {
            elapsed: process.elapsed,
            exit_code: process.exit_code,
            exit_success: process.exit_success,
            captured_stderr_bytes: process.stderr.captured_bytes,
            total_stderr_bytes: process.stderr.total_bytes,
            stderr_truncated: process.stderr.truncated,
            stderr_read_failed: process.stderr.read_failure.is_some(),
        }
    }
}

/// Executes a frozen project and prepares its immutable terminal evidence without writing.
/// Worker and pipeline failures become non-success artifacts; invalid command parameters fail
/// before any worker starts. There is no running journal and no automatic timetable adoption.
///
/// # Errors
/// Returns command, encoding or bounded-artifact failures. Execution failures are in `failure()`.
pub fn execute_durable_stored_project_solve(
    prepared: PreparedStoredProjectSolve,
    options: &SolveOptions,
    client: &SolverClient,
    cancellation: &CancellationToken,
) -> Result<PreparedSolveArtifact, SolveArtifactError> {
    require_generate(options)?;
    let context = SolveContext::with_generated_request_id(
        prepared.receipt.project_id.to_string(),
        prepared.receipt.revision,
        "project-import",
        prepared.receipt.revision,
    );
    crate::solve::validate_context_and_options(&context, options).map_err(|error| {
        SolveArtifactError::Command(StoredProjectSolveError::Solve(error.into()))
    })?;
    let encoded_options = ArtifactOptions::encode(options)?;
    let policy = match prepared.mode {
        StoredProjectSolveMode::ExistingSections => None,
        StoredProjectSolveMode::AutoSectioning(policy) => Some(policy),
    };
    let mut attempts = Vec::new();
    let mut failure = None;
    let started_at = now_millis();
    let result = solve_imported_project_with_executor(
        &prepared.document.import_batch,
        &prepared.document.calendar,
        &prepared.document.project_stable_key,
        policy,
        &context,
        options,
        cancellation,
        &mut |problem, context, options| {
            let started = now_millis();
            let execution = execute_solve(problem, context, options, client, cancellation)
                .and_then(|execution| {
                    attempts.push(capture_attempt(
                        problem,
                        context,
                        options,
                        &execution,
                        started,
                        now_millis(),
                    )?);
                    Ok(execution)
                });
            if let Err(error) = &execution {
                failure = Some(RecordedFailure {
                    failure: execution_failure(error),
                    request: prepare_solve(problem, context, options)
                        .ok()
                        .map(|prepared| prepared.envelope.encode_to_vec()),
                    after_attempts: attempts.len(),
                });
            }
            execution
        },
    );
    let finished_at = now_millis();
    let result = match result {
        Err(
            error @ crate::ImportedSolveError::Solve(SolveApplicationError::Client(
                SolverClientError::InvalidRequest(_),
            )),
        ) => return Err(StoredProjectSolveError::Solve(error).into()),
        result => result,
    };
    if let Err(error) = &result {
        failure.get_or_insert_with(|| RecordedFailure {
            failure: StoredSolveFailure {
                code: error.code().to_owned(),
                application_code: error.code().to_owned(),
                validation_problem_codes: Vec::new(),
            },
            request: None,
            after_attempts: attempts.len(),
        });
    }
    let source_hash = decode_hash(&prepared.receipt.payload_hash)?;
    let terminal = terminal_summary(
        result.as_ref().ok(),
        failure.as_ref().map(|value| &value.failure),
    );
    // The completed pipeline decision is the boundary. Never sample the mutable token here:
    // cancellation arriving after selection must not change the recorded or replayed result.
    let cancellation_observed = terminal.status_code == "Cancelled";
    let sectioning = sectioning_evidence(&prepared.document, policy, attempts.len());
    let payload = ArtifactPayload {
        schema_version: SOLVE_ARTIFACT_SCHEMA_VERSION,
        semantics_version: SOLVE_ARTIFACT_SEMANTICS_VERSION.to_owned(),
        sectioning_algorithm_version: SECTIONING_ALGORITHM_VERSION.to_owned(),
        protocol_version: solver_contract::PROTOCOL_VERSION,
        run_id: context.request_id.clone(),
        project_id: prepared.receipt.project_id,
        project_revision: prepared.receipt.revision,
        source_payload_hash: source_hash,
        display_name: prepared.display_name.clone(),
        sectioning_policy: policy,
        options: encoded_options,
        started_at,
        finished_at,
        terminal,
        cancellation_observed,
        attempts,
        failure,
        sectioning,
    };
    prepare_artifact_document(prepared, options, context, payload, result.ok())
}

fn prepare_artifact_document(
    prepared: PreparedStoredProjectSolve,
    options: &SolveOptions,
    context: SolveContext,
    payload: ArtifactPayload,
    result: Option<ImportedProjectSolve>,
) -> Result<PreparedSolveArtifact, SolveArtifactError> {
    let records = provenance_records(&payload)?;
    let bytes = serde_json::to_vec(&payload)?;
    if bytes.len() > MAXIMUM_SOLVE_ARTIFACT_BYTES || payload.attempts.len() > 16 {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_RESOURCE_LIMIT"));
    }
    let document = SolveArtifactDocument {
        run_id: payload.run_id.clone(),
        project_id: payload.project_id.to_string(),
        project_revision: payload.project_revision,
        source_payload_hash: payload.source_payload_hash,
        artifact_schema_version: SOLVE_ARTIFACT_SCHEMA_VERSION,
        status_code: payload.terminal.status_code.clone(),
        started_at: payload.started_at,
        finished_at: payload.finished_at,
        payload: bytes,
    };
    let receipt = artifact_receipt(&document, &payload, prepared.receipt.clone());
    let result = result.map(|result| StoredProjectSolve {
        receipt: prepared.receipt,
        display_name: prepared.display_name.clone(),
        context,
        result,
    });
    Ok(PreparedSolveArtifact {
        document,
        loaded: LoadedSolveArtifact {
            receipt,
            display_name: prepared.display_name,
            mode: prepared.mode,
            options: options.clone(),
            result,
            failure: payload.failure.map(|value| value.failure),
            attempts: records,
            source_document: prepared.document,
        },
    })
}

fn require_generate(options: &SolveOptions) -> Result<(), SolveArtifactError> {
    if options.mode == SolveMode::Generate {
        Ok(())
    } else {
        Err(StoredProjectSolveError::UnsupportedSolveMode { mode: options.mode }.into())
    }
}

/// Commits already prepared terminal evidence and all available attempt provenance atomically.
///
/// # Errors
/// Returns persistence errors, including duplicate run IDs and unavailable historical sources.
pub fn save_prepared_solve_artifact(
    store: &mut SqliteStore,
    artifact: &PreparedSolveArtifact,
) -> Result<SolveArtifactReceipt, SolveArtifactError> {
    store.record_solve_artifact(&artifact.document, &artifact.loaded.attempts)?;
    Ok(artifact.loaded.receipt.clone())
}

fn capture_attempt(
    problem: &SchedulingProblemSnapshot,
    context: &SolveContext,
    options: &SolveOptions,
    execution: &SolveExecution,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
) -> Result<ArtifactAttempt, SolveApplicationError> {
    if let SolveExecution::Completed(completed) = execution
        && let Some(response) = &completed.response
    {
        replay::validate_response(
            &prepare_solve(problem, context, options)?.envelope,
            response,
        )
        .map_err(|_| SolveApplicationError::Adapter {
            code: "APPLICATION_SOLVE_ARTIFACT_RESPONSE_IDENTITY",
            detail: "response does not echo the requested parameters and identity".to_owned(),
        })?;
    }
    let scoring_started = Instant::now();
    let quality =
        score_success(problem, execution).map_err(|_| SolveApplicationError::Adapter {
            code: "APPLICATION_INDEPENDENT_SCORING_FAILED",
            detail: "independent scoring failed".to_owned(),
        })?;
    let scoring_elapsed = quality.as_ref().map(|_| scoring_started.elapsed());
    let validation_elapsed = match execution {
        SolveExecution::Completed(completed) => completed.validation_elapsed,
        SolveExecution::PrecheckFailed { .. } => None,
    };
    let (snapshot_hash, request, response, status_code, process, validation, output_hash) =
        match execution {
            SolveExecution::PrecheckFailed { report } => (
                *blake3::hash(&serde_json::to_vec(problem)?).as_bytes(),
                None,
                None,
                "StaticPrecheckFailed".to_owned(),
                None,
                Some(report.clone()),
                None,
            ),
            SolveExecution::Completed(completed) => (
                completed.snapshot_hash,
                Some(
                    prepare_solve(problem, context, options)?
                        .envelope
                        .encode_to_vec(),
                ),
                completed.response.as_deref().map(Message::encode_to_vec),
                status_code(completed.status).to_owned(),
                Some(ProcessEvidence::from(&completed.process)),
                completed.independent_validation.clone(),
                completed.output_hash,
            ),
        };
    Ok(ArtifactAttempt {
        request_id: context.request_id.clone(),
        snapshot_hash,
        request,
        response,
        status_code,
        process,
        validation,
        quality,
        output_hash,
        validation_elapsed,
        scoring_elapsed,
        started_at,
        finished_at,
    })
}

fn execution_failure(error: &SolveApplicationError) -> StoredSolveFailure {
    let code = match error {
        SolveApplicationError::Client(error) => match error {
            SolverClientError::InvalidRequest(_) => "SOLVER_CLIENT_INVALID_REQUEST",
            SolverClientError::ExpectedRequestPayload => "SOLVER_CLIENT_EXPECTED_REQUEST",
            SolverClientError::RequestFrame(_) => "SOLVER_CLIENT_REQUEST_FRAME",
            SolverClientError::Spawn { .. } => "SOLVER_CLIENT_SPAWN_FAILED",
            SolverClientError::MissingPipe(_) => "SOLVER_CLIENT_MISSING_PIPE",
            SolverClientError::ThreadSpawn { .. } => "SOLVER_CLIENT_THREAD_SPAWN",
            SolverClientError::ThreadPanicked(_) => "SOLVER_CLIENT_THREAD_PANICKED",
            SolverClientError::ProcessWait(_) => "SOLVER_CLIENT_PROCESS_WAIT",
            SolverClientError::ProcessTermination(_) => "SOLVER_CLIENT_PROCESS_TERMINATION",
            SolverClientError::WorkerExited { .. } => "SOLVER_CLIENT_WORKER_EXITED",
            SolverClientError::RequestWrite { .. } => "SOLVER_CLIENT_REQUEST_WRITE",
            SolverClientError::Protocol { source, .. } => source.code(),
            SolverClientError::WorkerInternalError { .. } => "SOLVER_CLIENT_WORKER_INTERNAL_ERROR",
        },
        error => error.code(),
    };
    let validation_problem_codes = match error {
        SolveApplicationError::InvalidSolverOutput { report }
        | SolveApplicationError::InvalidIncumbent { report } => report
            .hard_problems
            .iter()
            .map(|problem| problem.code.as_str().to_owned())
            .collect(),
        _ => Vec::new(),
    };
    StoredSolveFailure {
        code: code.to_owned(),
        application_code: error.code().to_owned(),
        validation_problem_codes,
    }
}

fn terminal_summary(
    result: Option<&ImportedProjectSolve>,
    failure: Option<&StoredSolveFailure>,
) -> TerminalSummary {
    let (status, termination, selected_attempt_index) = match result {
        Some(ImportedProjectSolve::Existing(result)) => match &result.execution {
            SolveExecution::PrecheckFailed { .. } => ("InvalidInput", "StaticPrecheckFailed", None),
            SolveExecution::Completed(completed) => {
                (status_code(completed.status), "Completed", None)
            }
        },
        Some(ImportedProjectSolve::AutoSectioned(result)) => match result.status {
            AutoSectioningSolveStatus::SelectedFeasible => (
                "Feasible",
                "SelectedFeasible",
                result.selected_attempt_index,
            ),
            AutoSectioningSolveStatus::CandidateBudgetExhausted => {
                ("Unknown", "CandidateBudgetExhausted", None)
            }
            AutoSectioningSolveStatus::Cancelled => ("Cancelled", "Cancelled", None),
        },
        None => (
            "InternalError",
            failure.map_or("APPLICATION_SOLVE_ARTIFACT_MISSING_RESULT", |value| {
                value.code.as_str()
            }),
            None,
        ),
    };
    TerminalSummary {
        status_code: status.to_owned(),
        termination_code: termination.to_owned(),
        selected_attempt_index,
    }
}

fn sectioning_evidence(
    document: &ImportedProjectDocument,
    policy: Option<AutoSectioningPolicy>,
    attempted: usize,
) -> Option<SectioningEvidence> {
    let preparation = crate::prepare_auto_sectioning(
        &document.import_batch,
        &document.project_stable_key,
        policy?,
    )
    .ok()?;
    Some(SectioningEvidence {
        provenance: preparation.provenance,
        candidate_provenance: preparation
            .candidates
            .iter()
            .take(attempted)
            .map(|candidate| candidate.candidate().provenance.clone())
            .collect(),
    })
}

fn status_code(status: SolverRunStatus) -> &'static str {
    match status {
        SolverRunStatus::Optimal => "Optimal",
        SolverRunStatus::Feasible => "Feasible",
        SolverRunStatus::ProvenInfeasible => "ProvenInfeasible",
        SolverRunStatus::Timeout => "Timeout",
        SolverRunStatus::Unknown => "Unknown",
        SolverRunStatus::Cancelled => "Cancelled",
        SolverRunStatus::InvalidInput => "InvalidInput",
        SolverRunStatus::InvalidModel => "InvalidModel",
        SolverRunStatus::InternalError => "InternalError",
    }
}

fn now_millis() -> DateTime<Utc> {
    let now = Utc::now();
    now.with_nanosecond(now.nanosecond() / 1_000_000 * 1_000_000)
        .unwrap_or(now)
}

fn invalid(code: &'static str) -> SolveArtifactError {
    SolveArtifactError::Invalid { code }
}

fn decode<T: Message + Default>(bytes: &[u8]) -> Result<T, SolveArtifactError> {
    T::decode(bytes).map_err(|_| invalid("APPLICATION_SOLVE_ARTIFACT_PROTOCOL"))
}

fn decode_hash(value: &str) -> Result<[u8; 32], SolveArtifactError> {
    blake3::Hash::from_hex(value)
        .map(|hash| *hash.as_bytes())
        .map_err(|_| invalid("APPLICATION_SOLVE_ARTIFACT_SOURCE"))
}

fn artifact_receipt(
    document: &SolveArtifactDocument,
    payload: &ArtifactPayload,
    source: ImportCommitReceipt,
) -> SolveArtifactReceipt {
    SolveArtifactReceipt {
        run_id: document.run_id.clone(),
        source,
        status_code: document.status_code.clone(),
        termination_code: payload.terminal.termination_code.clone(),
        artifact_schema_version: document.artifact_schema_version,
        payload_hash: document.payload_hash().to_hex().to_string(),
        started_at: document.started_at,
        finished_at: document.finished_at,
    }
}

fn provenance_records(
    payload: &ArtifactPayload,
) -> Result<Vec<SolverRunRecord>, SolveArtifactError> {
    let mut records = Vec::new();
    for (index, attempt) in payload.attempts.iter().enumerate() {
        // An absent worker response cannot supply effective parameters or an actual engine.
        let Some(bytes) = &attempt.response else {
            continue;
        };
        let response = decode::<solver_contract::SolveResponse>(bytes)?;
        let parameters = response
            .effective_parameters
            .ok_or_else(|| invalid("APPLICATION_SOLVE_ARTIFACT_PROTOCOL"))?;
        let engine = response
            .engine_version
            .ok_or_else(|| invalid("APPLICATION_SOLVE_ARTIFACT_PROTOCOL"))?;
        let parameters_json = serde_json::to_string(&serde_json::json!({
            "mode": parameters.mode, "profile": parameters.profile, "seed": parameters.seed,
            "reproducible": parameters.reproducible, "worker_count": parameters.worker_count,
            "time_limit_millis": parameters.time_limit_millis, "collect_diagnostics": parameters.collect_diagnostics,
            "memory_limit_bytes": parameters.memory_limit_bytes, "relative_gap_limit_ppm": parameters.relative_gap_limit_ppm,
            "engine_name": engine.engine_name, "adapter_version": engine.adapter_version,
            "build_revision": engine.build_revision, "process": attempt.process,
            "validation_elapsed": attempt.validation_elapsed, "scoring_elapsed": attempt.scoring_elapsed,
        }))?;
        records.push(SolverRunRecord {
            run_id: format!("{}:attempt:{}", payload.run_id, index + 1),
            project_id: payload.project_id.to_string(),
            project_revision: payload.project_revision,
            scenario_id: None,
            request_id: attempt.request_id.clone(),
            input_snapshot_hash: attempt.snapshot_hash,
            solver_engine_version: engine.engine_version,
            protocol_version: payload.protocol_version,
            seed: parameters.seed,
            parameters_json,
            worker_count: parameters.worker_count,
            time_limit_ms: parameters.time_limit_millis,
            status_code: attempt.status_code.clone(),
            objective_json: serde_json::to_string(&attempt.quality)?,
            validation_code: if attempt
                .validation
                .as_ref()
                .is_some_and(ValidationReport::is_valid)
            {
                "Passed"
            } else {
                "NotApplicable"
            }
            .to_owned(),
            output_hash: attempt.output_hash,
            started_at: attempt.started_at,
            finished_at: Some(attempt.finished_at),
        });
    }
    Ok(records)
}
