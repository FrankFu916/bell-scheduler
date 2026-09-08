use std::cmp::Ordering;

use class_schedule_import::ImportBatch;
use class_schedule_scoring::{
    ObjectivePlan, ObjectiveVector, ScoringContext, ScoringError, evaluate,
};
use class_schedule_sectioning::{
    SectioningDiagnostic, SectioningObjective, SectioningRunProvenance,
};
use solver_client::{CancellationToken, SolverClient, SolverRunStatus};
use thiserror::Error;

use crate::{
    AutoSectioningError, AutoSectioningPolicy, CalendarDefinition, CompileError,
    CompiledSchoolProblem, PreparedSectioningCandidate, SolveApplicationError, SolveContext,
    SolveExecution, SolveOptions, compile_import_batch, compile_import_batch_with_sectioning,
    execute_solve, prepare_auto_sectioning,
};

#[derive(Debug)]
pub struct ExistingSectionSolve {
    pub compiled: CompiledSchoolProblem,
    pub execution: SolveExecution,
    pub quality: Option<ObjectiveVector>,
}

#[derive(Debug)]
pub struct SectioningTimetableAttempt {
    pub sectioning: PreparedSectioningCandidate,
    pub compiled: CompiledSchoolProblem,
    pub execution: SolveExecution,
    pub quality: Option<ObjectiveVector>,
}

impl SectioningTimetableAttempt {
    #[must_use]
    pub fn solver_status(&self) -> Option<SolverRunStatus> {
        match &self.execution {
            SolveExecution::PrecheckFailed { .. } => None,
            SolveExecution::Completed(completed) => Some(completed.status),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoSectioningSolveStatus {
    SelectedFeasible,
    CandidateBudgetExhausted,
    Cancelled,
}

#[derive(Debug)]
pub struct AutoSectioningSolve {
    pub status: AutoSectioningSolveStatus,
    pub provenance: SectioningRunProvenance,
    pub diagnostics: Vec<SectioningDiagnostic>,
    pub attempts: Vec<SectioningTimetableAttempt>,
    pub selected_attempt_index: Option<usize>,
}

impl AutoSectioningSolve {
    #[must_use]
    pub fn selected_attempt(&self) -> Option<&SectioningTimetableAttempt> {
        self.selected_attempt_index
            .and_then(|index| self.attempts.get(index))
    }
}

#[derive(Debug)]
pub enum ImportedProjectSolve {
    Existing(ExistingSectionSolve),
    AutoSectioned(AutoSectioningSolve),
}

#[derive(Debug, Error)]
pub enum ImportedSolveError {
    #[error("student choices require an explicit auto-sectioning policy")]
    AutoSectioningPolicyRequired,
    #[error("auto sectioning failed: {0}")]
    Sectioning(#[from] AutoSectioningError),
    #[error("timetable compilation failed: {0}")]
    Compile(#[from] CompileError),
    #[error("solver application failed: {0}")]
    Solve(#[from] SolveApplicationError),
    #[error("independent timetable scoring failed: {0}")]
    Scoring(#[from] ScoringError),
}

impl ImportedSolveError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::AutoSectioningPolicyRequired => "APPLICATION_SECTIONING_POLICY_REQUIRED",
            Self::Sectioning(error) => error.code(),
            Self::Compile(error) => error.code(),
            Self::Solve(error) => error.code(),
            Self::Scoring(_) => "APPLICATION_INDEPENDENT_SCORING_FAILED",
        }
    }
}

/// Runs the shared Input-A/Input-B decomposition path used by CLI and future desktop transports.
///
/// Input B tries every bounded, independently validated sectioning candidate. A finite candidate
/// budget with no timetable is reported as `CandidateBudgetExhausted`; it is never upgraded to a
/// proof that the original combined problem is infeasible.
///
/// # Errors
///
/// Returns stable errors for missing Input-B policy, sectioning/compile failures, worker failures,
/// invalid solver output, or independent scoring failures.
#[allow(clippy::too_many_arguments)]
pub fn solve_imported_project(
    batch: &ImportBatch,
    calendar: &CalendarDefinition,
    project_stable_key: &str,
    auto_sectioning_policy: Option<AutoSectioningPolicy>,
    context: &SolveContext,
    options: &SolveOptions,
    client: &SolverClient,
    cancellation: &CancellationToken,
) -> Result<ImportedProjectSolve, ImportedSolveError> {
    solve_imported_project_with_executor(
        batch,
        calendar,
        project_stable_key,
        auto_sectioning_policy,
        context,
        options,
        cancellation,
        &mut |problem, context, options| {
            execute_solve(problem, context, options, client, cancellation)
        },
    )
}

type SolveExecutor<'a> = dyn FnMut(
        &class_schedule_scheduling::SchedulingProblemSnapshot,
        &SolveContext,
        &SolveOptions,
    ) -> Result<SolveExecution, SolveApplicationError>
    + 'a;

#[allow(clippy::too_many_arguments)]
pub(crate) fn solve_imported_project_with_executor(
    batch: &ImportBatch,
    calendar: &CalendarDefinition,
    project_stable_key: &str,
    auto_sectioning_policy: Option<AutoSectioningPolicy>,
    context: &SolveContext,
    options: &SolveOptions,
    cancellation: &CancellationToken,
    executor: &mut SolveExecutor<'_>,
) -> Result<ImportedProjectSolve, ImportedSolveError> {
    let requires_sectioning =
        batch.teaching_sections().is_empty() && !batch.student_subject_choices().is_empty();
    if !requires_sectioning {
        let compiled = compile_import_batch(batch, calendar, project_stable_key)?;
        let execution = executor(&compiled.problem, context, options)?;
        let quality = score_success(&compiled.problem, &execution)?;
        return Ok(ImportedProjectSolve::Existing(ExistingSectionSolve {
            compiled,
            execution,
            quality,
        }));
    }

    let policy = auto_sectioning_policy.ok_or(ImportedSolveError::AutoSectioningPolicyRequired)?;
    let preparation = prepare_auto_sectioning(batch, project_stable_key, policy)?;
    let mut attempts = Vec::with_capacity(preparation.candidates.len());
    for (index, sectioning) in preparation.candidates.into_iter().enumerate() {
        let compiled =
            compile_import_batch_with_sectioning(batch, calendar, project_stable_key, &sectioning)?;
        let mut candidate_context = context.clone();
        candidate_context.request_id = format!("{}:sectioning:{}", context.request_id, index + 1);
        let execution = executor(&compiled.problem, &candidate_context, options)?;
        let quality = score_success(&compiled.problem, &execution)?;
        let cancelled = matches!(
            &execution,
            SolveExecution::Completed(completed)
                if completed.status == SolverRunStatus::Cancelled
        );
        attempts.push(SectioningTimetableAttempt {
            sectioning,
            compiled,
            execution,
            quality,
        });
        if cancelled {
            break;
        }
    }
    let cancelled = cancellation.is_cancelled()
        || attempts
            .iter()
            .any(|attempt| attempt.solver_status() == Some(SolverRunStatus::Cancelled));
    let selected_attempt_index = if cancelled {
        None
    } else {
        select_best_attempt(&attempts)
    };
    let status = if cancelled {
        AutoSectioningSolveStatus::Cancelled
    } else if selected_attempt_index.is_some() {
        AutoSectioningSolveStatus::SelectedFeasible
    } else {
        AutoSectioningSolveStatus::CandidateBudgetExhausted
    };
    Ok(ImportedProjectSolve::AutoSectioned(AutoSectioningSolve {
        status,
        provenance: preparation.provenance,
        diagnostics: preparation.diagnostics,
        attempts,
        selected_attempt_index,
    }))
}

pub(crate) fn score_success(
    problem: &class_schedule_scheduling::SchedulingProblemSnapshot,
    execution: &SolveExecution,
) -> Result<Option<ObjectiveVector>, ScoringError> {
    match execution {
        SolveExecution::Completed(completed)
            if matches!(
                completed.status,
                SolverRunStatus::Optimal | SolverRunStatus::Feasible
            ) =>
        {
            evaluate(
                problem,
                &completed.assignments,
                &ObjectivePlan::balanced_default(),
                &ScoringContext::neutral(problem),
            )
            .map(Some)
        }
        SolveExecution::PrecheckFailed { .. } | SolveExecution::Completed(_) => Ok(None),
    }
}

fn select_best_attempt(attempts: &[SectioningTimetableAttempt]) -> Option<usize> {
    attempts
        .iter()
        .enumerate()
        .filter(|(_, attempt)| attempt.quality.is_some())
        .min_by(|(_, left), (_, right)| compare_attempts(left, right))
        .map(|(index, _)| index)
}

fn compare_attempts(
    left: &SectioningTimetableAttempt,
    right: &SectioningTimetableAttempt,
) -> Ordering {
    let quality = left
        .quality
        .as_ref()
        .expect("filtered feasible attempt")
        .lexicographic_cmp(right.quality.as_ref().expect("filtered feasible attempt"));
    quality
        .then_with(|| sectioning_objective(left).cmp(sectioning_objective(right)))
        .then_with(|| {
            left.sectioning
                .candidate()
                .provenance
                .candidate_hash
                .cmp(&right.sectioning.candidate().provenance.candidate_hash)
        })
}

fn sectioning_objective(attempt: &SectioningTimetableAttempt) -> &SectioningObjective {
    &attempt.sectioning.candidate().objective
}
