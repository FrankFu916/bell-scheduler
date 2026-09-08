use solver_client::{SolverRunOutcome, StderrCapture};
use solver_contract::{SolveResponse, SolverStatus, solver_envelope};

use class_schedule_domain::SchoolProjectId;
use class_schedule_persistence::{SolveArtifactDocument, SqliteStore};
use class_schedule_scheduling::SchedulingProblemSnapshot;
use class_schedule_sectioning::SECTIONING_ALGORITHM_VERSION;
use solver_client::{CancellationToken, ProcessReport, SolverRunStatus};
use solver_contract::{SolverEnvelope, ValidateContract};

use super::{
    ArtifactAttempt, ArtifactPayload, AutoSectioningPolicy, LoadedSolveArtifact,
    ProjectSolveArtifactPage, ProjectSolveArtifactSummary, RecordedFailure,
    SOLVE_ARTIFACT_SCHEMA_VERSION, SOLVE_ARTIFACT_SEMANTICS_VERSION, SolveApplicationError,
    SolveArtifactError, SolveContext, SolveExecution, SolveOptions, StoredProjectSolve,
    StoredProjectSolveMode, artifact_receipt, decode, invalid, prepare_solve, provenance_records,
    score_success, sectioning_evidence, solve_imported_project_with_executor, status_code,
    terminal_summary,
};

const REPLAY_STOP: &str = "APPLICATION_SOLVE_ARTIFACT_REPLAY_STOP";

/// Opens local terminal evidence only after reconstructing its exact historical input and
/// independently validating each response. No worker, source update or promotion occurs.
///
/// # Errors
/// Rejects unknown versions, corrupt identities/provenance, invalid assignments and selection.
pub fn load_solve_artifact(
    store: &SqliteStore,
    run_id: &str,
) -> Result<LoadedSolveArtifact, SolveArtifactError> {
    let stored = store.load_solve_artifact(run_id)?;
    let payload: ArtifactPayload = serde_json::from_slice(&stored.document.payload)?;
    validate_metadata(&stored.document, &payload)?;
    let (loaded, mode) = load_source(store, &payload)?;
    let options = payload.options.decode()?;
    let context = SolveContext {
        project_id: payload.project_id.to_string(),
        project_revision: payload.project_revision,
        scenario_id: "project-import".to_owned(),
        scenario_revision: payload.project_revision,
        request_id: payload.run_id.clone(),
    };
    let cancellation = CancellationToken::new();
    if payload.cancellation_observed {
        cancellation.cancel();
    }
    let mut cursor = 0;
    let mut replay_error = None;
    let mut failure_replayed = false;
    let result = solve_imported_project_with_executor(
        &loaded.document.import_batch,
        &loaded.document.calendar,
        &loaded.document.project_stable_key,
        payload.sectioning_policy,
        &context,
        &options,
        &cancellation,
        &mut |problem, context, options| {
            if let Some(attempt) = payload.attempts.get(cursor) {
                cursor += 1;
                return replay_attempt(problem, context, options, attempt).map_err(|error| {
                    replay_error = Some(error);
                    replay_stop()
                });
            }
            if let Some(failure) = &payload.failure
                && let Some(bytes) = &failure.request
            {
                let verified = verify_request(problem, context, options, bytes);
                if let Err(error) = verified {
                    replay_error = Some(error);
                }
                failure_replayed = true;
                return Err(replay_stop());
            }
            replay_error = Some(invalid("APPLICATION_SOLVE_ARTIFACT_ATTEMPTS"));
            Err(replay_stop())
        },
    );
    if let Some(error) = replay_error {
        return Err(error);
    }
    if cursor != payload.attempts.len() {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_ATTEMPTS"));
    }
    match (&result, &payload.failure) {
        (Ok(_), None) => {}
        (Err(error), Some(failure)) if failure_replayed && error.code() == REPLAY_STOP => {
            validate_recorded_failure(failure, cursor)?;
        }
        (Err(error), Some(failure))
            if failure.request.is_none() && error.code() == failure.failure.application_code =>
        {
            validate_recorded_failure(failure, cursor)?;
        }
        _ => return Err(invalid("APPLICATION_SOLVE_ARTIFACT_TERMINAL")),
    }
    let terminal = terminal_summary(
        result.as_ref().ok(),
        payload.failure.as_ref().map(|value| &value.failure),
    );
    if terminal != payload.terminal
        || sectioning_evidence(
            &loaded.document,
            payload.sectioning_policy,
            payload.attempts.len(),
        ) != payload.sectioning
    {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_SELECTION"));
    }
    let records = provenance_records(&payload)?;
    if records != stored.attempts {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_PROVENANCE"));
    }
    let receipt = artifact_receipt(&stored.document, &payload, loaded.receipt.clone());
    Ok(LoadedSolveArtifact {
        receipt,
        display_name: payload.display_name.clone(),
        mode,
        options,
        result: result.ok().map(|result| StoredProjectSolve {
            receipt: loaded.receipt,
            display_name: payload.display_name.clone(),
            context,
            result,
        }),
        failure: payload.failure.map(|value| value.failure),
        attempts: records,
        source_document: loaded.document,
    })
}

fn load_source(
    store: &SqliteStore,
    payload: &ArtifactPayload,
) -> Result<(crate::LoadedImportedProject, StoredProjectSolveMode), SolveArtifactError> {
    let source =
        store.load_project_revision(&payload.project_id.to_string(), payload.project_revision)?;
    if source.payload_hash().as_bytes() != &payload.source_payload_hash {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_SOURCE"));
    }
    let loaded = crate::import_commit::revalidate_stored_import(source, payload.project_id)?;
    let mode = match payload.sectioning_policy {
        Some(policy) => {
            AutoSectioningPolicy::new(
                policy.minimum_size,
                policy.target_size,
                policy.maximum_size,
                policy.seed,
                policy.profile,
                policy.candidate_count,
            )
            .map_err(|_| invalid("APPLICATION_SOLVE_ARTIFACT_POLICY"))?;
            StoredProjectSolveMode::AutoSectioning(policy)
        }
        None => StoredProjectSolveMode::ExistingSections,
    };
    if loaded.receipt.sectioning_required != payload.sectioning_policy.is_some() {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_SOURCE"));
    }
    Ok((loaded, mode))
}

/// Lists bounded local run metadata. A listed run must still pass `load_solve_artifact` before
/// its assignments can be used.
///
/// # Errors
/// Returns invalid pagination or persistence errors.
pub fn list_project_solve_artifacts(
    store: &SqliteStore,
    project_id: SchoolProjectId,
    limit: u32,
    offset: u32,
) -> Result<ProjectSolveArtifactPage, SolveArtifactError> {
    if !(1..=100).contains(&limit) {
        return Err(invalid("APPLICATION_SOLVE_LIST_INVALID_LIMIT"));
    }
    let next = offset
        .checked_add(limit)
        .ok_or_else(|| invalid("APPLICATION_SOLVE_LIST_INVALID_OFFSET"))?;
    let mut rows = store.list_solve_artifacts(&project_id.to_string(), limit + 1, offset)?;
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let runs = rows
        .into_iter()
        .map(|row| ProjectSolveArtifactSummary {
            run_id: row.run_id,
            project_id,
            project_revision: row.project_revision,
            artifact_schema_version: row.artifact_schema_version,
            status_code: row.status_code,
            started_at: row.started_at,
            finished_at: row.finished_at,
        })
        .collect();
    Ok(ProjectSolveArtifactPage {
        runs,
        has_more,
        next_offset: has_more.then_some(next),
    })
}

fn validate_metadata(
    document: &SolveArtifactDocument,
    payload: &ArtifactPayload,
) -> Result<(), SolveArtifactError> {
    if document.artifact_schema_version != SOLVE_ARTIFACT_SCHEMA_VERSION
        || payload.schema_version != SOLVE_ARTIFACT_SCHEMA_VERSION
        || payload.semantics_version != SOLVE_ARTIFACT_SEMANTICS_VERSION
        || payload.sectioning_algorithm_version != SECTIONING_ALGORITHM_VERSION
        || payload.protocol_version != solver_contract::PROTOCOL_VERSION
    {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_UNSUPPORTED_VERSION"));
    }
    if document.run_id != payload.run_id
        || document.project_id != payload.project_id.to_string()
        || document.project_revision != payload.project_revision
        || document.source_payload_hash != payload.source_payload_hash
        || document.status_code != payload.terminal.status_code
        || document.started_at != payload.started_at
        || document.finished_at != payload.finished_at
        || uuid::Uuid::parse_str(&payload.run_id).is_err()
        || payload.display_name.trim().is_empty()
    {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_IDENTITY"));
    }
    if payload.attempts.len() > 16 {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_RESOURCE_LIMIT"));
    }
    let mut previous_end = payload.started_at;
    for attempt in &payload.attempts {
        if attempt.started_at < previous_end
            || attempt.finished_at < attempt.started_at
            || attempt.finished_at > payload.finished_at
        {
            return Err(invalid("APPLICATION_SOLVE_ARTIFACT_TIMESTAMPS"));
        }
        previous_end = attempt.finished_at;
    }
    Ok(())
}

fn replay_attempt(
    problem: &SchedulingProblemSnapshot,
    context: &SolveContext,
    options: &SolveOptions,
    attempt: &ArtifactAttempt,
) -> Result<SolveExecution, SolveArtifactError> {
    let hash = *blake3::hash(&serde_json::to_vec(problem)?).as_bytes();
    if hash != attempt.snapshot_hash || context.request_id != attempt.request_id {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_SNAPSHOT"));
    }
    let precheck = class_schedule_validation::static_feasibility_check(problem);
    if !precheck.is_valid() {
        if attempt.status_code != "StaticPrecheckFailed"
            || attempt.request.is_some()
            || attempt.response.is_some()
            || attempt.process.is_some()
            || attempt.quality.is_some()
            || attempt.output_hash.is_some()
            || attempt.validation_elapsed.is_some()
            || attempt.scoring_elapsed.is_some()
            || attempt.validation.as_ref() != Some(&precheck)
        {
            return Err(invalid("APPLICATION_SOLVE_ARTIFACT_PRECHECK"));
        }
        return Ok(SolveExecution::PrecheckFailed { report: precheck });
    }
    let request = verify_request(
        problem,
        context,
        options,
        attempt
            .request
            .as_deref()
            .ok_or_else(|| invalid("APPLICATION_SOLVE_ARTIFACT_REQUEST"))?,
    )?;
    let process = attempt
        .process
        .as_ref()
        .ok_or_else(|| invalid("APPLICATION_SOLVE_ARTIFACT_PROCESS"))?;
    if process.captured_stderr_bytes > process.total_stderr_bytes
        || process.exit_success && process.exit_code != Some(0)
    {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_PROCESS"));
    }
    let (status, response) = if let Some(bytes) = &attempt.response {
        let response = decode::<SolveResponse>(bytes)?;
        response
            .validate_contract()
            .map_err(|_| invalid("APPLICATION_SOLVE_ARTIFACT_PROTOCOL"))?;
        if !process.exit_success {
            return Err(invalid("APPLICATION_SOLVE_ARTIFACT_PROCESS"));
        }
        validate_response(&request, &response)?;
        (response_status(response.status)?, Some(response))
    } else {
        if process.exit_success {
            return Err(invalid("APPLICATION_SOLVE_ARTIFACT_PROCESS"));
        }
        let status = match attempt.status_code.as_str() {
            "Timeout" => SolverRunStatus::Timeout,
            "Cancelled" => SolverRunStatus::Cancelled,
            _ => return Err(invalid("APPLICATION_SOLVE_ARTIFACT_PROTOCOL")),
        };
        (status, None)
    };
    if status_code(status) != attempt.status_code {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_TERMINAL"));
    }
    let execution = crate::solve::complete_solve(
        problem,
        hash,
        SolverRunOutcome {
            status,
            response,
            process: ProcessReport {
                elapsed: process.elapsed,
                exit_code: process.exit_code,
                exit_success: process.exit_success,
                stderr: StderrCapture {
                    text: String::new(),
                    captured_bytes: process.captured_stderr_bytes,
                    total_bytes: process.total_stderr_bytes,
                    truncated: process.stderr_truncated,
                    read_failure: process
                        .stderr_read_failed
                        .then(|| "Recorded stderr read failure".to_owned()),
                },
            },
        },
    )
    .map_err(|_| invalid("APPLICATION_SOLVE_ARTIFACT_INVALID_OUTPUT"))?;
    let SolveExecution::Completed(completed) = &execution else {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_INVALID_OUTPUT"));
    };
    let quality = score_success(problem, &execution)
        .map_err(|_| invalid("APPLICATION_SOLVE_ARTIFACT_INVALID_OUTPUT"))?;
    if completed.independent_validation != attempt.validation
        || completed.output_hash != attempt.output_hash
        || quality != attempt.quality
        || completed.validation_elapsed.is_some() != attempt.validation_elapsed.is_some()
        || quality.is_some() != attempt.scoring_elapsed.is_some()
    {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_VALIDATION"));
    }
    Ok(execution)
}

fn verify_request(
    problem: &SchedulingProblemSnapshot,
    context: &SolveContext,
    options: &SolveOptions,
    bytes: &[u8],
) -> Result<SolverEnvelope, SolveArtifactError> {
    let saved = decode::<SolverEnvelope>(bytes)?;
    saved
        .validate_contract()
        .map_err(|_| invalid("APPLICATION_SOLVE_ARTIFACT_REQUEST"))?;
    let rebuilt = prepare_solve(problem, context, options)
        .map_err(|_| invalid("APPLICATION_SOLVE_ARTIFACT_REQUEST"))?;
    // Protobuf maps do not have a canonical wire order. Compare decoded semantic fields.
    if saved != rebuilt.envelope {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_REQUEST"));
    }
    Ok(rebuilt.envelope)
}

pub(super) fn validate_response(
    request: &SolverEnvelope,
    response: &SolveResponse,
) -> Result<(), SolveArtifactError> {
    let Some(solver_envelope::Payload::SolveRequest(request)) = &request.payload else {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_REQUEST"));
    };
    let expected = request
        .required_engine_version
        .as_ref()
        .ok_or_else(|| invalid("APPLICATION_SOLVE_ARTIFACT_REQUEST"))?;
    let actual = response
        .engine_version
        .as_ref()
        .ok_or_else(|| invalid("APPLICATION_SOLVE_ARTIFACT_PROTOCOL"))?;
    let snapshot = request
        .problem
        .as_ref()
        .ok_or_else(|| invalid("APPLICATION_SOLVE_ARTIFACT_REQUEST"))?;
    if response.effective_parameters != request.parameters
        || response.input_snapshot_hash != snapshot.snapshot_hash
        || actual.engine_name != expected.engine_name
        || actual.engine_version != expected.engine_version
        || actual.adapter_version != expected.adapter_version
    {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_RESPONSE_IDENTITY"));
    }
    Ok(())
}

fn response_status(status: i32) -> Result<SolverRunStatus, SolveArtifactError> {
    match SolverStatus::try_from(status) {
        Ok(SolverStatus::Optimal) => Ok(SolverRunStatus::Optimal),
        Ok(SolverStatus::Feasible) => Ok(SolverRunStatus::Feasible),
        Ok(SolverStatus::ProvenInfeasible) => Ok(SolverRunStatus::ProvenInfeasible),
        Ok(SolverStatus::Timeout) => Ok(SolverRunStatus::Timeout),
        Ok(SolverStatus::Unknown) => Ok(SolverRunStatus::Unknown),
        Ok(SolverStatus::Cancelled) => Ok(SolverRunStatus::Cancelled),
        Ok(SolverStatus::InvalidInput) => Ok(SolverRunStatus::InvalidInput),
        Ok(SolverStatus::InvalidModel) => Ok(SolverRunStatus::InvalidModel),
        _ => Err(invalid("APPLICATION_SOLVE_ARTIFACT_TERMINAL")),
    }
}

fn validate_recorded_failure(
    failure: &RecordedFailure,
    cursor: usize,
) -> Result<(), SolveArtifactError> {
    let stable_code = |code: &str| {
        !code.is_empty()
            && code.len() <= 128
            && code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    };
    if failure.after_attempts != cursor
        || !stable_code(&failure.failure.code)
        || !stable_code(&failure.failure.application_code)
        || failure
            .failure
            .validation_problem_codes
            .iter()
            .any(|code| !stable_code(code))
    {
        return Err(invalid("APPLICATION_SOLVE_ARTIFACT_FAILURE"));
    }
    Ok(())
}

fn replay_stop() -> SolveApplicationError {
    SolveApplicationError::Adapter {
        code: REPLAY_STOP,
        detail: "recorded terminal evidence".to_owned(),
    }
}
