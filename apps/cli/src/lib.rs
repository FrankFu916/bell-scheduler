#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! Non-interactive CLI transport over the same import, application, solver-client, validator, and
//! scoring components used by the desktop application.

mod import_command;
mod run_history;
mod scenario_commands;
mod scenario_export;
mod scenario_timetable;
mod solve_project;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use class_schedule_application::{
    AutoSectioningPolicy, AutoSectioningSolve, AutoSectioningSolveStatus, CalendarDefinition,
    CompiledCatalog, CompiledSchoolProblem, ImportedProjectSolve, SectioningDiagnostic,
    SectioningObjective, SectioningProfile, SolveContext, SolveExecution, SolveOptions,
    solve_imported_project,
};
use class_schedule_diagnostics::{DiagnosticsReport, aggregate as aggregate_diagnostics};
use class_schedule_domain::Day;
use class_schedule_import::{
    CsvImporter, CsvSource, DatasetKind, ImportBatch, ImportConfig, ImportFailure,
    ImportedAudienceKind, ImportedRoomPolicy, ImportedTeacherAssignment,
    SectionEnrollmentImportRow, TeachingSectionImportRow,
};
use class_schedule_scheduling::{Assignment, SchedulingProblemSnapshot};
use class_schedule_scoring::ObjectiveVector;
use class_schedule_validation::ValidationReport;
use serde::Serialize;
use serde_json::{Value, json};
use solver_client::{CancellationToken, SidecarSpec, SolverClient, SolverRunStatus};
use solver_contract::{
    DiagnosticGroup, EngineVersion, ObjectiveBreakdown, SolverParameters, SolverProfile,
    SolverStatistics,
};
use thiserror::Error;

const SUMMARY_SCHEMA_VERSION: u32 = 1;
const SUCCESS_EXIT_CODE: u8 = 0;
const NO_TIMETABLE_EXIT_CODE: u8 = 3;
const DEFAULT_MAXIMUM_INPUT_BYTES: u64 = 256 * 1024 * 1024;

const DATASETS: [DatasetKind; 11] = [
    DatasetKind::Students,
    DatasetKind::AdministrativeClasses,
    DatasetKind::StudentSubjectChoices,
    DatasetKind::Teachers,
    DatasetKind::TeacherUnavailability,
    DatasetKind::Rooms,
    DatasetKind::CoursePlans,
    DatasetKind::TeachingSections,
    DatasetKind::SectionEnrollments,
    DatasetKind::CourseOfferings,
    DatasetKind::FixedActivities,
];

#[derive(Clone, Debug, Parser)]
#[command(name = "class-schedule", version, about = "中国普通高中排课 CLI")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Debug, Subcommand)]
enum Command {
    /// Import a complete CSV bundle, solve it with the native sidecar, independently validate it,
    /// and write machine-readable artifacts.
    Solve(SolveArgs),
    /// Audit CSV and canonical-sheet XLSX inputs, then atomically create or explicitly replace
    /// a `SQLite` project.
    Import(import_command::ImportArgs),
    /// Solve a frozen, explicitly selected `SQLite` project revision without adopting its results.
    SolveProject(solve_project::SolveProjectArgs),
    /// List completed run metadata for an imported project.
    ListRuns(run_history::ListRunsArgs),
    /// Revalidate a completed run from its historical input and export it without a worker.
    ExportRun(run_history::ExportRunArgs),
    /// Independently revalidate a current-source run and adopt it into a new scenario.
    AdoptRun(scenario_commands::AdoptRunArgs),
    /// Copy an exact scenario/timetable revision into an independent new scenario.
    CloneScenario(scenario_commands::CloneScenarioArgs),
    /// Rebuild and independently validate a saved scenario without launching a worker.
    ShowScenario(scenario_commands::ShowScenarioArgs),
    /// Read a scenario's revalidated timetable or its selectable entities at explicit revisions.
    ScenarioTimetable(scenario_timetable::ScenarioTimetableArgs),
    /// Export the complete selected scenario timetable to a new Excel or CSV file.
    ExportScenario(scenario_export::ExportScenarioArgs),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ExecutionMode {
    /// Fixed seed and one worker. This is the default for auditable runs and bug reports.
    Reproducible,
    /// Allows multiple CP-SAT workers. Runs are not promised to be bit-identical.
    Fast,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum InputMode {
    /// The school supplies teaching sections and every section enrollment.
    ExistingSections,
    /// The application generates teaching sections from student subject choices.
    AutoSectioning,
}

#[derive(Clone, Debug, Args)]
struct SolveArgs {
    /// Directory containing canonical `<dataset>.csv` files.
    #[arg(long)]
    input_dir: PathBuf,

    #[command(flatten)]
    runtime: SolverRuntimeArgs,

    #[arg(long, default_value = "school-project")]
    project_key: String,

    #[arg(long, default_value = "school-project")]
    project_id: String,

    #[arg(long, default_value_t = 1)]
    project_revision: u64,

    #[arg(long, default_value = "baseline")]
    scenario_id: String,

    #[arg(long, default_value_t = 1)]
    scenario_revision: u64,

    /// Number of teaching periods per weekday.
    #[arg(long, default_value_t = 8)]
    periods_per_day: u16,

    /// The lunch/major break is immediately after this period.
    #[arg(long, default_value_t = 4)]
    break_after_period: u16,

    /// Required number of selected subjects per student.
    #[arg(long, default_value_t = 3)]
    exact_subject_choices: usize,

    /// Selects whether teaching-section memberships are imported or generated.
    #[arg(long, value_enum, default_value_t = InputMode::ExistingSections)]
    input_mode: InputMode,

    /// Minimum generated teaching-section size (Input B only).
    #[arg(long, default_value_t = 1)]
    section_min_size: u16,

    /// Target generated teaching-section size (Input B only).
    #[arg(long, default_value_t = 40)]
    section_target_size: u16,

    /// Maximum generated teaching-section size (Input B only).
    #[arg(long, default_value_t = 50)]
    section_max_size: u16,

    /// Number of bounded sectioning candidates to timetable and compare (Input B only).
    #[arg(long, default_value_t = 3)]
    sectioning_candidates: u8,

    /// Aggregate byte limit for the CSV bundle before parsing.
    #[arg(long, default_value_t = DEFAULT_MAXIMUM_INPUT_BYTES)]
    maximum_input_bytes: u64,
}

/// Shared execution and artifact parameters for directory and saved-project transports.
#[derive(Clone, Debug, Args)]
struct SolverRuntimeArgs {
    /// Native one-shot OR-Tools worker executable.
    #[arg(long)]
    worker: PathBuf,

    /// New or empty destination directory for `summary.json` and, on success, `timetable.csv`.
    #[arg(long)]
    output_dir: PathBuf,

    #[arg(long, default_value_t = 1)]
    seed: u64,

    #[arg(long, value_enum, default_value_t = ExecutionMode::Reproducible)]
    execution: ExecutionMode,

    /// CP-SAT worker count. Reproducible execution requires exactly one.
    #[arg(long, default_value_t = 1)]
    workers: u32,

    #[arg(long, default_value_t = 30)]
    time_limit_seconds: u64,

    /// Extra wall-clock allowance for worker startup, framing, and shutdown.
    #[arg(long, default_value_t = 10)]
    process_grace_seconds: u64,
}

#[derive(Clone, Debug)]
pub struct RunOutcome {
    pub rendered_summary: String,
    pub exit_code: u8,
}

#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct CliFailure {
    code: &'static str,
    message: String,
    details: Value,
}

impl CliFailure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: json!({}),
        }
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn as_report(&self) -> ErrorReport<'_> {
        ErrorReport {
            schema_version: SUMMARY_SCHEMA_VERSION,
            code: self.code,
            message: &self.message,
            details: &self.details,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ErrorReport<'a> {
    schema_version: u32,
    code: &'a str,
    message: &'a str,
    details: &'a Value,
}

#[derive(Clone, Debug, Serialize)]
struct Summary {
    schema_version: u32,
    project_id: String,
    project_revision: u64,
    scenario_id: String,
    scenario_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved_source: Option<SavedSourceSummary>,
    result: ResultSummary,
    counts: ProblemCounts,
    independent_validation: Option<ValidationReport>,
    independent_objective: Option<ObjectiveVector>,
    sectioning: Option<SectioningSummary>,
    diagnostics: DiagnosticsReport,
    solver: Option<SolverResponseSummary>,
    process: Option<ProcessSummary>,
    provenance: ProvenanceSummary,
}

#[derive(Clone, Debug, Serialize)]
struct SavedSourceSummary {
    project_id: String,
    revision: String,
    request_id: String,
    payload_hash_algorithm: &'static str,
    payload_hash: String,
    scope: &'static str,
    adopted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved_run_id: Option<String>,
}

#[derive(Debug)]
struct SolveSummaryContext<'a> {
    context: &'a SolveContext,
    options: &'a SolveOptions,
    output_dir: &'a Path,
    saved_source: Option<SavedSourceSummary>,
}

#[derive(Clone, Debug, Serialize)]
struct SectioningSummary {
    status: String,
    algorithm_version: String,
    input_hash: String,
    seed: u64,
    profile: SectioningProfile,
    requested_candidates: u8,
    generated_candidates: u8,
    attempted_candidates: usize,
    selected_candidate_index: Option<usize>,
    selected_candidate_hash: Option<String>,
    diagnostics: Vec<SectioningDiagnostic>,
    attempts: Vec<SectioningAttemptSummary>,
}

#[derive(Clone, Debug, Serialize)]
struct SectioningAttemptSummary {
    candidate_index: usize,
    candidate_hash: String,
    sectioning_objective: SectioningObjective,
    timetable_status: String,
    timetable_snapshot_hash: String,
    timetable_objective: Option<ObjectiveVector>,
    selected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    run: Option<SectioningAttemptRunSummary>,
}

#[derive(Clone, Debug, Serialize)]
struct SectioningAttemptRunSummary {
    independent_validation: Option<ValidationReport>,
    solver: Option<SolverResponseSummary>,
    process: Option<ProcessSummary>,
    rust_output_hash: Option<String>,
    worker_output_hash: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ResultSummary {
    status: String,
    status_detail_code: String,
    phase: String,
    publishable: bool,
    hard_valid: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
struct ProblemCounts {
    students: usize,
    teachers: usize,
    rooms: usize,
    timeslots: usize,
    activities: usize,
    student_conflict_edges: usize,
}

#[derive(Clone, Debug, Serialize)]
struct ProvenanceSummary {
    input_snapshot_hash: String,
    protocol_version: u32,
    seed: u64,
    requested_worker_count: u32,
    requested_time_limit_millis: u64,
    reproducible: bool,
    validation_result: String,
    rust_output_hash: Option<String>,
    worker_output_hash: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ProcessSummary {
    elapsed_millis: u64,
    exit_code: Option<i32>,
    exit_success: bool,
    stderr_captured_bytes: usize,
    stderr_total_bytes: usize,
    stderr_truncated: bool,
    stderr_read_failure: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct SolverResponseSummary {
    engine: Option<EngineSummary>,
    effective_parameters: Option<ParametersSummary>,
    objective: Vec<SolverObjectiveTierSummary>,
    statistics: Option<StatisticsSummary>,
    diagnostics: Vec<DiagnosticSummary>,
}

#[derive(Clone, Debug, Serialize)]
struct EngineSummary {
    engine_name: String,
    engine_version: String,
    adapter_version: String,
    build_revision: String,
}

#[derive(Clone, Debug, Serialize)]
struct ParametersSummary {
    seed: u64,
    mode: i32,
    profile: i32,
    reproducible: bool,
    time_limit_millis: u64,
    worker_count: u32,
    collect_diagnostics: bool,
    memory_limit_bytes: u64,
    relative_gap_limit_ppm: u32,
}

#[derive(Clone, Debug, Serialize)]
struct SolverObjectiveTierSummary {
    tier_id: String,
    priority: u32,
    value: i64,
    best_bound: i64,
    metrics: Vec<SolverMetricSummary>,
}

#[derive(Clone, Debug, Serialize)]
struct SolverMetricSummary {
    metric_kind: i32,
    value: i64,
}

#[derive(Clone, Debug, Serialize)]
struct StatisticsSummary {
    wall_time_millis: u64,
    deterministic_time: f64,
    conflicts: u64,
    branches: u64,
    propagations: u64,
    peak_memory_bytes: u64,
    worker_count: u32,
    seed: u64,
}

#[derive(Clone, Debug, Serialize)]
struct DiagnosticSummary {
    group_id: String,
    problem_code: String,
    signal: i32,
    related_entities: Vec<EntitySummary>,
    parameters: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
struct EntitySummary {
    entity_kind: i32,
    compact_id: u32,
}

/// Executes the selected command without adding transport-specific business rules.
///
/// # Errors
///
/// Returns a stable machine-readable failure for malformed paths/options, import or compile
/// errors, worker failures, scoring failures, and output commit failures.
pub fn run(cli: Cli) -> Result<RunOutcome, CliFailure> {
    match cli.command {
        Command::Solve(args) => solve(&args),
        Command::Import(args) => import_command::run(&args),
        Command::SolveProject(args) => solve_project::run(&args),
        Command::ListRuns(args) => run_history::list(&args),
        Command::ExportRun(args) => run_history::export(&args),
        Command::AdoptRun(args) => scenario_commands::adopt(&args),
        Command::CloneScenario(args) => scenario_commands::clone_scenario(&args),
        Command::ShowScenario(args) => scenario_commands::show(&args),
        Command::ScenarioTimetable(args) => scenario_timetable::run(&args),
        Command::ExportScenario(args) => scenario_export::run(&args),
    }
}

fn solve(args: &SolveArgs) -> Result<RunOutcome, CliFailure> {
    validate_options(args)?;
    ensure_output_target_available(&args.runtime.output_dir)?;
    let (batch, calendar) = load_import_batch(args)?;
    validate_input_mode(args.input_mode, &batch)?;
    let options = solve_options(&args.runtime);
    let client = build_solver_client(&args.runtime)?;
    let context = SolveContext::with_generated_request_id(
        args.project_id.clone(),
        args.project_revision,
        args.scenario_id.clone(),
        args.scenario_revision,
    );
    let sectioning_policy = if args.input_mode == InputMode::AutoSectioning {
        Some(
            AutoSectioningPolicy::new(
                args.section_min_size,
                args.section_target_size,
                args.section_max_size,
                args.runtime.seed,
                sectioning_profile(args.runtime.execution),
                args.sectioning_candidates,
            )
            .map_err(|error| CliFailure::new(error.code(), error.to_string()))?,
        )
    } else {
        None
    };
    let solved = solve_imported_project(
        &batch,
        &calendar,
        &args.project_key,
        sectioning_policy,
        &context,
        &options,
        &client,
        &CancellationToken::new(),
    )
    .map_err(|error| CliFailure::new(error.code(), error.to_string()))?;
    publish_solved_project(
        &SolveSummaryContext {
            context: &context,
            options: &options,
            output_dir: &args.runtime.output_dir,
            saved_source: None,
        },
        solved,
    )
}

// Directory and SQLite transports share every status, validator, sectioning and export mapping.
#[allow(clippy::too_many_lines)]
fn publish_solved_project(
    args: &SolveSummaryContext<'_>,
    solved: ImportedProjectSolve,
) -> Result<RunOutcome, CliFailure> {
    let requested_time_limit_millis =
        u64::try_from(args.options.time_limit.as_millis()).map_err(|_| {
            CliFailure::new(
                "CLI_TIME_LIMIT_OUT_OF_RANGE",
                "saved time limit exceeds u64 milliseconds",
            )
        })?;
    let (summary, timetable, generated_sections, generated_enrollments) = match solved {
        ImportedProjectSolve::Existing(existing) => {
            let (summary, timetable) = summarize_execution(
                args,
                &existing.compiled,
                existing.execution,
                existing.quality,
                requested_time_limit_millis,
            )?;
            (summary, timetable, None, None)
        }
        ImportedProjectSolve::AutoSectioned(mut auto) => {
            let sectioning = sectioning_summary(&auto, args.saved_source.is_some())?;
            if let Some(selected_index) = auto.selected_attempt_index {
                let selected = auto.attempts.remove(selected_index);
                let sections =
                    render_generated_sections_csv(selected.sectioning.generated_sections())?;
                let enrollments =
                    render_generated_enrollments_csv(selected.sectioning.generated_enrollments())?;
                let (mut summary, timetable) = summarize_execution(
                    args,
                    &selected.compiled,
                    selected.execution,
                    selected.quality,
                    requested_time_limit_millis,
                )?;
                summary.sectioning = Some(sectioning);
                (summary, timetable, Some(sections), Some(enrollments))
            } else {
                let representative = auto.attempts.pop().ok_or_else(|| {
                    CliFailure::new(
                        "INTERNAL_ERROR_SECTIONING_NO_ATTEMPTS",
                        "sectioning generated candidates but no timetable attempt was recorded",
                    )
                })?;
                let (mut summary, _) = summarize_execution(
                    args,
                    &representative.compiled,
                    representative.execution,
                    None,
                    requested_time_limit_millis,
                )?;
                summary.result.publishable = false;
                summary.result.hard_valid = None;
                "sectioning_timetable_candidates".clone_into(&mut summary.result.phase);
                match auto.status {
                    AutoSectioningSolveStatus::Cancelled => {
                        "cancelled".clone_into(&mut summary.result.status);
                        "SECTIONING_CANCELLED".clone_into(&mut summary.result.status_detail_code);
                    }
                    AutoSectioningSolveStatus::CandidateBudgetExhausted => {
                        "unknown".clone_into(&mut summary.result.status);
                        "SECTIONING_CANDIDATE_BUDGET_EXHAUSTED"
                            .clone_into(&mut summary.result.status_detail_code);
                    }
                    AutoSectioningSolveStatus::SelectedFeasible => {
                        return Err(CliFailure::new(
                            "INTERNAL_ERROR_SECTIONING_SELECTION_MISSING",
                            "sectioning status selected a timetable without an attempt index",
                        ));
                    }
                }
                summary.sectioning = Some(sectioning);
                (summary, None, None, None)
            }
        }
    };
    let rendered_summary = serde_json::to_string_pretty(&summary)
        .map_err(|error| CliFailure::new("CLI_SUMMARY_SERIALIZATION_FAILED", error.to_string()))?;
    commit_outputs(
        args.output_dir,
        rendered_summary.as_bytes(),
        timetable.as_deref(),
        generated_sections.as_deref(),
        generated_enrollments.as_deref(),
    )?;
    Ok(RunOutcome {
        exit_code: if summary.result.publishable {
            SUCCESS_EXIT_CODE
        } else {
            NO_TIMETABLE_EXIT_CODE
        },
        rendered_summary,
    })
}

fn load_import_batch(args: &SolveArgs) -> Result<(ImportBatch, CalendarDefinition), CliFailure> {
    let sources = read_csv_bundle(&args.input_dir, args.maximum_input_bytes)?;
    let csv_sources = sources
        .iter()
        .map(|(kind, bytes)| CsvSource::new(*kind, bytes))
        .collect::<Vec<_>>();
    let importer = CsvImporter::new(
        ImportConfig::default().with_exact_subject_choices(Some(args.exact_subject_choices)),
    );
    let batch = importer
        .import(csv_sources)
        .map_err(|error| import_failure_to_cli(&error))?;
    let calendar =
        CalendarDefinition::weekday_with_break(args.periods_per_day, args.break_after_period)
            .map_err(|error| CliFailure::new(error.code(), error.to_string()))?;
    Ok((batch, calendar))
}

fn validate_input_mode(mode: InputMode, batch: &ImportBatch) -> Result<(), CliFailure> {
    let has_sections = !batch.teaching_sections().is_empty();
    let has_enrollments = !batch.section_enrollments().is_empty();
    let has_choices = !batch.student_subject_choices().is_empty();
    match mode {
        InputMode::ExistingSections if has_choices && !has_sections => Err(CliFailure::new(
            "CLI_EXISTING_SECTION_MODE_REQUIRES_SECTIONS",
            "existing-sections mode requires teaching_sections.csv and complete enrollments",
        )),
        InputMode::AutoSectioning if has_sections || has_enrollments => Err(CliFailure::new(
            "CLI_AUTO_SECTION_MODE_REQUIRES_UNSECTIONED_INPUT",
            "auto-sectioning mode rejects imported teaching sections and enrollments",
        )),
        InputMode::AutoSectioning if !has_choices => Err(CliFailure::new(
            "CLI_AUTO_SECTION_MODE_REQUIRES_CHOICES",
            "auto-sectioning mode requires student subject choices",
        )),
        InputMode::ExistingSections | InputMode::AutoSectioning => Ok(()),
    }
}

fn summarize_execution(
    args: &SolveSummaryContext<'_>,
    compiled: &CompiledSchoolProblem,
    execution: SolveExecution,
    quality: Option<ObjectiveVector>,
    requested_time_limit_millis: u64,
) -> Result<(Summary, Option<Vec<u8>>), CliFailure> {
    let semantic_hash = hash_semantic_snapshot(&compiled.problem)?;
    let counts = problem_counts(&compiled.problem);
    match execution {
        SolveExecution::PrecheckFailed { report } => Ok((
            precheck_summary(
                args,
                counts,
                report,
                &semantic_hash,
                requested_time_limit_millis,
            ),
            None,
        )),
        SolveExecution::Completed(completed) => {
            let timetable = if quality.is_some() {
                Some(render_timetable_csv(compiled, &completed.assignments)?)
            } else {
                None
            };
            Ok((
                completed_summary(
                    args,
                    counts,
                    &semantic_hash,
                    requested_time_limit_millis,
                    *completed,
                    quality,
                ),
                timetable,
            ))
        }
    }
}

const fn sectioning_profile(execution: ExecutionMode) -> SectioningProfile {
    match execution {
        ExecutionMode::Reproducible => SectioningProfile::Balanced,
        ExecutionMode::Fast => SectioningProfile::Fast,
    }
}

fn sectioning_summary(
    auto: &AutoSectioningSolve,
    include_attempt_runs: bool,
) -> Result<SectioningSummary, CliFailure> {
    let selected_index = auto.selected_attempt_index;
    let attempts = auto
        .attempts
        .iter()
        .enumerate()
        .map(|(index, attempt)| {
            let timetable_status = match &attempt.execution {
                SolveExecution::PrecheckFailed { .. } => "static_precheck_failed".to_owned(),
                SolveExecution::Completed(completed) => status_name(completed.status).to_owned(),
            };
            Ok(SectioningAttemptSummary {
                candidate_index: index + 1,
                candidate_hash: attempt
                    .sectioning
                    .candidate()
                    .provenance
                    .candidate_hash
                    .to_hex(),
                sectioning_objective: attempt.sectioning.candidate().objective,
                timetable_status,
                timetable_snapshot_hash: hash_semantic_snapshot(&attempt.compiled.problem)?,
                timetable_objective: attempt.quality.clone(),
                selected: selected_index == Some(index),
                run: include_attempt_runs.then(|| match &attempt.execution {
                    SolveExecution::PrecheckFailed { report } => SectioningAttemptRunSummary {
                        independent_validation: Some(report.clone()),
                        solver: None,
                        process: None,
                        rust_output_hash: None,
                        worker_output_hash: None,
                    },
                    SolveExecution::Completed(completed) => {
                        let response = completed.response.as_deref();
                        SectioningAttemptRunSummary {
                            independent_validation: completed.independent_validation.clone(),
                            solver: response.map(solver_response_summary),
                            process: Some(process_summary(&completed.process)),
                            rust_output_hash: completed.output_hash.map(|hash| encode_hex(&hash)),
                            worker_output_hash: response
                                .filter(|value| !value.output_hash.is_empty())
                                .map(|value| encode_hex(&value.output_hash)),
                        }
                    }
                }),
            })
        })
        .collect::<Result<Vec<_>, CliFailure>>()?;
    Ok(SectioningSummary {
        status: match auto.status {
            AutoSectioningSolveStatus::SelectedFeasible => "selected_feasible",
            AutoSectioningSolveStatus::CandidateBudgetExhausted => "candidate_budget_exhausted",
            AutoSectioningSolveStatus::Cancelled => "cancelled",
        }
        .to_owned(),
        algorithm_version: auto.provenance.algorithm_version.clone(),
        input_hash: auto.provenance.input_hash.to_hex(),
        seed: auto.provenance.seed,
        profile: auto.provenance.profile,
        requested_candidates: auto.provenance.requested_candidates,
        generated_candidates: auto.provenance.generated_candidates,
        attempted_candidates: auto.attempts.len(),
        selected_candidate_index: selected_index.map(|index| index + 1),
        selected_candidate_hash: selected_index.map(|index| {
            auto.attempts[index]
                .sectioning
                .candidate()
                .provenance
                .candidate_hash
                .to_hex()
        }),
        diagnostics: auto.diagnostics.clone(),
        attempts,
    })
}

fn validate_options(args: &SolveArgs) -> Result<(), CliFailure> {
    if args.project_key.trim().is_empty()
        || args.project_id.trim().is_empty()
        || args.scenario_id.trim().is_empty()
    {
        return Err(CliFailure::new(
            "CLI_BLANK_STABLE_IDENTIFIER",
            "project and scenario identifiers must not be blank",
        ));
    }
    if args.project_revision == 0 || args.scenario_revision == 0 {
        return Err(CliFailure::new(
            "CLI_INVALID_REVISION",
            "project and scenario revisions must be positive",
        ));
    }
    if args.maximum_input_bytes == 0 {
        return Err(CliFailure::new(
            "CLI_INVALID_SOLVER_PARAMETER",
            "seed, time limit, and input byte limit must be positive",
        ));
    }
    validate_runtime_options(&args.runtime)
}

fn validate_runtime_options(args: &SolverRuntimeArgs) -> Result<(), CliFailure> {
    if args.seed == 0 || args.time_limit_seconds == 0 {
        return Err(CliFailure::new(
            "CLI_INVALID_SOLVER_PARAMETER",
            "seed and time limit must be positive",
        ));
    }
    if args.workers == 0 {
        return Err(CliFailure::new(
            "CLI_INVALID_WORKER_COUNT",
            "worker count must be positive",
        ));
    }
    if args.execution == ExecutionMode::Reproducible && args.workers != 1 {
        return Err(CliFailure::new(
            "CLI_REPRODUCIBLE_REQUIRES_ONE_WORKER",
            "reproducible execution requires exactly one CP-SAT worker",
        ));
    }
    let worker_metadata = fs::metadata(&args.worker).map_err(|_| {
        CliFailure::new(
            "CLI_WORKER_NOT_ACCESSIBLE",
            "cannot access the requested worker executable",
        )
    })?;
    if !worker_metadata.is_file() {
        return Err(CliFailure::new(
            "CLI_WORKER_NOT_FILE",
            "the requested worker executable is not a file",
        ));
    }
    Ok(())
}

fn solve_options(args: &SolverRuntimeArgs) -> SolveOptions {
    let time_limit = Duration::from_secs(args.time_limit_seconds);
    let mut options = SolveOptions::reproducible(args.seed, time_limit);
    if args.execution == ExecutionMode::Fast {
        options.profile = SolverProfile::Fast;
        options.reproducible = false;
        options.worker_count = args.workers;
    }
    options
}

fn requested_time_limit_millis(args: &SolverRuntimeArgs) -> Result<u64, CliFailure> {
    args.time_limit_seconds
        .checked_mul(1_000)
        .ok_or_else(|| CliFailure::new("CLI_TIME_LIMIT_OVERFLOW", "time limit is too large"))
}

fn build_solver_client(args: &SolverRuntimeArgs) -> Result<SolverClient, CliFailure> {
    requested_time_limit_millis(args)?;
    let timeout = args
        .time_limit_seconds
        .checked_add(args.process_grace_seconds)
        .ok_or_else(|| CliFailure::new("CLI_PROCESS_TIMEOUT_OVERFLOW", "timeout is too large"))?;
    Ok(SolverClient::new(SidecarSpec::new(&args.worker)).timeout(Duration::from_secs(timeout)))
}

fn read_csv_bundle(
    directory: &Path,
    maximum_bytes: u64,
) -> Result<Vec<(DatasetKind, Vec<u8>)>, CliFailure> {
    let metadata = fs::metadata(directory).map_err(|error| {
        CliFailure::new(
            "CLI_INPUT_DIRECTORY_NOT_ACCESSIBLE",
            format!(
                "cannot access input directory `{}`: {error}",
                directory.display()
            ),
        )
    })?;
    if !metadata.is_dir() {
        return Err(CliFailure::new(
            "CLI_INPUT_NOT_DIRECTORY",
            format!("input path `{}` is not a directory", directory.display()),
        ));
    }
    let known = DATASETS
        .into_iter()
        .map(|kind| (format!("{}.csv", kind.as_str()), kind))
        .collect::<BTreeMap<_, _>>();
    let mut by_kind = BTreeMap::new();
    let entries = fs::read_dir(directory).map_err(|error| {
        CliFailure::new(
            "CLI_INPUT_DIRECTORY_READ_FAILED",
            format!(
                "cannot list input directory `{}`: {error}",
                directory.display()
            ),
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            CliFailure::new("CLI_INPUT_DIRECTORY_READ_FAILED", error.to_string())
        })?;
        if !entry
            .path()
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
        {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Some(kind) = known.get(&file_name).copied() else {
            return Err(CliFailure::new(
                "CLI_UNKNOWN_DATASET_FILE",
                format!("unrecognized CSV dataset `{file_name}`"),
            ));
        };
        let file_metadata = entry.metadata().map_err(|error| {
            CliFailure::new(
                "CLI_INPUT_FILE_NOT_ACCESSIBLE",
                format!("cannot inspect `{}`: {error}", entry.path().display()),
            )
        })?;
        if !file_metadata.is_file() {
            return Err(CliFailure::new(
                "CLI_INPUT_DATASET_NOT_FILE",
                format!("dataset `{file_name}` is not a regular file"),
            ));
        }
        by_kind.insert(kind, (entry.path(), file_metadata.len()));
    }
    let total_bytes = by_kind.values().try_fold(0_u64, |total, (_, size)| {
        total.checked_add(*size).ok_or_else(|| {
            CliFailure::new("CLI_INPUT_SIZE_OVERFLOW", "aggregate input size overflowed")
        })
    })?;
    if total_bytes > maximum_bytes {
        return Err(CliFailure::new(
            "CLI_INPUT_TOO_LARGE",
            "aggregate CSV input exceeds the configured byte limit",
        )
        .with_details(json!({ "maximum_bytes": maximum_bytes, "actual_bytes": total_bytes })));
    }
    by_kind
        .into_iter()
        .map(|(kind, (path, _))| {
            fs::read(&path).map(|bytes| (kind, bytes)).map_err(|error| {
                CliFailure::new(
                    "CLI_INPUT_FILE_READ_FAILED",
                    format!("cannot read `{}`: {error}", path.display()),
                )
            })
        })
        .collect()
}

fn import_failure_to_cli(failure: &ImportFailure) -> CliFailure {
    CliFailure::new("CLI_IMPORT_REJECTED", failure.to_string()).with_details(json!({
        "problems": failure.problems(),
    }))
}

fn hash_semantic_snapshot(problem: &SchedulingProblemSnapshot) -> Result<String, CliFailure> {
    let bytes = serde_json::to_vec(problem)
        .map_err(|error| CliFailure::new("CLI_SNAPSHOT_SERIALIZATION_FAILED", error.to_string()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn problem_counts(problem: &SchedulingProblemSnapshot) -> ProblemCounts {
    ProblemCounts {
        students: problem.students().len(),
        teachers: problem.teachers().len(),
        rooms: problem.rooms().len(),
        timeslots: problem.timeslots().len(),
        activities: problem.activities().len(),
        student_conflict_edges: problem.student_conflict_edges().len(),
    }
}

fn precheck_summary(
    args: &SolveSummaryContext<'_>,
    counts: ProblemCounts,
    report: ValidationReport,
    snapshot_hash: &str,
    time_limit_millis: u64,
) -> Summary {
    let diagnostics = aggregate_diagnostics(Some(&report), None);
    Summary {
        schema_version: SUMMARY_SCHEMA_VERSION,
        project_id: args.context.project_id.clone(),
        project_revision: args.context.project_revision,
        scenario_id: args.context.scenario_id.clone(),
        scenario_revision: args.context.scenario_revision,
        saved_source: args.saved_source.clone(),
        result: ResultSummary {
            status: "invalid_input".to_owned(),
            status_detail_code: "STATIC_PRECHECK_FAILED".to_owned(),
            phase: "static_precheck".to_owned(),
            publishable: false,
            hard_valid: Some(false),
        },
        counts,
        independent_validation: Some(report),
        independent_objective: None,
        sectioning: None,
        diagnostics,
        solver: None,
        process: None,
        provenance: ProvenanceSummary {
            input_snapshot_hash: snapshot_hash.to_owned(),
            protocol_version: solver_contract::PROTOCOL_VERSION,
            seed: args.options.seed,
            requested_worker_count: args.options.worker_count,
            requested_time_limit_millis: time_limit_millis,
            reproducible: args.options.reproducible,
            validation_result: "failed_static_precheck".to_owned(),
            rust_output_hash: None,
            worker_output_hash: None,
        },
    }
}

fn completed_summary(
    args: &SolveSummaryContext<'_>,
    counts: ProblemCounts,
    semantic_hash: &str,
    time_limit_millis: u64,
    completed: class_schedule_application::CompletedSolve,
    quality: Option<ObjectiveVector>,
) -> Summary {
    let publishable = matches!(
        completed.status,
        SolverRunStatus::Optimal | SolverRunStatus::Feasible
    );
    let response = completed.response.as_deref();
    let diagnostics = aggregate_diagnostics(completed.independent_validation.as_ref(), response);
    let status_detail_code = response.map_or_else(
        || {
            if completed.status == SolverRunStatus::Timeout {
                "CLIENT_WALL_CLOCK_TIMEOUT".to_owned()
            } else {
                "NO_WORKER_RESPONSE".to_owned()
            }
        },
        |value| value.status_detail_code.clone(),
    );
    let worker_output_hash = response
        .filter(|value| !value.output_hash.is_empty())
        .map(|value| encode_hex(&value.output_hash));
    let solver = response.map(solver_response_summary);
    let process = process_summary(&completed.process);
    Summary {
        schema_version: SUMMARY_SCHEMA_VERSION,
        project_id: args.context.project_id.clone(),
        project_revision: args.context.project_revision,
        scenario_id: args.context.scenario_id.clone(),
        scenario_revision: args.context.scenario_revision,
        saved_source: args.saved_source.clone(),
        result: ResultSummary {
            status: status_name(completed.status).to_owned(),
            status_detail_code,
            phase: "solver".to_owned(),
            publishable,
            hard_valid: completed
                .independent_validation
                .as_ref()
                .map(ValidationReport::is_valid),
        },
        counts,
        independent_validation: completed.independent_validation,
        independent_objective: quality,
        sectioning: None,
        diagnostics,
        solver,
        process: Some(process),
        provenance: ProvenanceSummary {
            input_snapshot_hash: if completed.snapshot_hash == [0; 32] {
                semantic_hash.to_owned()
            } else {
                encode_hex(&completed.snapshot_hash)
            },
            protocol_version: solver_contract::PROTOCOL_VERSION,
            seed: args.options.seed,
            requested_worker_count: args.options.worker_count,
            requested_time_limit_millis: time_limit_millis,
            reproducible: args.options.reproducible,
            validation_result: if publishable {
                "passed".to_owned()
            } else {
                "not_applicable_no_timetable".to_owned()
            },
            rust_output_hash: completed.output_hash.map(|hash| encode_hex(&hash)),
            worker_output_hash,
        },
    }
}

fn process_summary(process: &solver_client::ProcessReport) -> ProcessSummary {
    ProcessSummary {
        elapsed_millis: duration_millis(process.elapsed),
        exit_code: process.exit_code,
        exit_success: process.exit_success,
        stderr_captured_bytes: process.stderr.captured_bytes,
        stderr_total_bytes: process.stderr.total_bytes,
        stderr_truncated: process.stderr.truncated,
        stderr_read_failure: process.stderr.read_failure.clone(),
    }
}

fn solver_response_summary(response: &solver_contract::SolveResponse) -> SolverResponseSummary {
    SolverResponseSummary {
        engine: response.engine_version.as_ref().map(engine_summary),
        effective_parameters: response
            .effective_parameters
            .as_ref()
            .map(parameters_summary),
        objective: response
            .objective
            .as_ref()
            .map_or_else(Vec::new, objective_summary),
        statistics: response.statistics.as_ref().map(statistics_summary),
        diagnostics: response
            .diagnostic_groups
            .iter()
            .map(diagnostic_summary)
            .collect(),
    }
}

fn engine_summary(value: &EngineVersion) -> EngineSummary {
    EngineSummary {
        engine_name: value.engine_name.clone(),
        engine_version: value.engine_version.clone(),
        adapter_version: value.adapter_version.clone(),
        build_revision: value.build_revision.clone(),
    }
}

const fn parameters_summary(value: &SolverParameters) -> ParametersSummary {
    ParametersSummary {
        seed: value.seed,
        mode: value.mode,
        profile: value.profile,
        reproducible: value.reproducible,
        time_limit_millis: value.time_limit_millis,
        worker_count: value.worker_count,
        collect_diagnostics: value.collect_diagnostics,
        memory_limit_bytes: value.memory_limit_bytes,
        relative_gap_limit_ppm: value.relative_gap_limit_ppm,
    }
}

fn objective_summary(value: &ObjectiveBreakdown) -> Vec<SolverObjectiveTierSummary> {
    value
        .tiers
        .iter()
        .map(|tier| SolverObjectiveTierSummary {
            tier_id: tier.tier_id.clone(),
            priority: tier.priority,
            value: tier.value,
            best_bound: tier.best_bound,
            metrics: tier
                .metrics
                .iter()
                .map(|metric| SolverMetricSummary {
                    metric_kind: metric.metric_kind,
                    value: metric.value,
                })
                .collect(),
        })
        .collect()
}

const fn statistics_summary(value: &SolverStatistics) -> StatisticsSummary {
    StatisticsSummary {
        wall_time_millis: value.wall_time_millis,
        deterministic_time: value.deterministic_time,
        conflicts: value.conflicts,
        branches: value.branches,
        propagations: value.propagations,
        peak_memory_bytes: value.peak_memory_bytes,
        worker_count: value.worker_count,
        seed: value.seed,
    }
}

fn diagnostic_summary(value: &DiagnosticGroup) -> DiagnosticSummary {
    DiagnosticSummary {
        group_id: value.group_id.clone(),
        problem_code: value.problem_code.clone(),
        signal: value.signal,
        related_entities: value
            .related_entities
            .iter()
            .map(|entity| EntitySummary {
                entity_kind: entity.entity_kind,
                compact_id: entity.compact_id,
            })
            .collect(),
        parameters: value.parameters.clone().into_iter().collect(),
    }
}

fn render_timetable_csv(
    compiled: &CompiledSchoolProblem,
    assignments: &[Assignment],
) -> Result<Vec<u8>, CliFailure> {
    let mut ordered = assignments.to_vec();
    ordered.sort_by_key(|assignment| {
        (
            assignment.start,
            assignment.room,
            assignment.teacher,
            assignment.activity,
        )
    });
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_writer(Vec::new());
    writer
        .write_record([
            "activity_index",
            "course_plan_code",
            "audience_kind",
            "audience_code",
            "meeting_ordinal",
            "duration_periods",
            "day",
            "period",
            "timeslot_label",
            "room_code",
            "teacher_code",
        ])
        .map_err(|error| CliFailure::new("CLI_TIMETABLE_CSV_WRITE_FAILED", error.to_string()))?;
    for assignment in ordered {
        write_assignment_row(
            &mut writer,
            &compiled.problem,
            &compiled.catalog,
            assignment,
        )?;
    }
    writer.into_inner().map_err(|error| {
        CliFailure::new("CLI_TIMETABLE_CSV_WRITE_FAILED", error.error().to_string())
    })
}

fn write_assignment_row(
    writer: &mut csv::Writer<Vec<u8>>,
    problem: &SchedulingProblemSnapshot,
    catalog: &CompiledCatalog,
    assignment: Assignment,
) -> Result<(), CliFailure> {
    let label = catalog
        .activities
        .get(assignment.activity.as_usize())
        .ok_or_else(|| catalog_failure("activity", assignment.activity.0))?;
    let timeslot = problem
        .timeslots()
        .get(assignment.start.as_usize())
        .ok_or_else(|| catalog_failure("timeslot", assignment.start.0))?;
    let timeslot_label = catalog
        .timeslot_labels
        .get(assignment.start.as_usize())
        .ok_or_else(|| catalog_failure("timeslot_label", assignment.start.0))?;
    let room = catalog
        .room_codes
        .get(assignment.room.as_usize())
        .ok_or_else(|| catalog_failure("room", assignment.room.0))?;
    let teacher = catalog
        .teacher_codes
        .get(assignment.teacher.as_usize())
        .ok_or_else(|| catalog_failure("teacher", assignment.teacher.0))?;
    writer
        .write_record([
            assignment.activity.0.to_string(),
            label.course_plan_code.clone(),
            audience_kind_name(label.audience_kind).to_owned(),
            label.audience_code.clone(),
            label.meeting_ordinal.to_string(),
            label.duration_periods.to_string(),
            day_name(timeslot.day).to_owned(),
            timeslot.period_index.to_string(),
            timeslot_label.clone(),
            room.clone(),
            teacher.clone(),
        ])
        .map_err(|error| CliFailure::new("CLI_TIMETABLE_CSV_WRITE_FAILED", error.to_string()))
}

fn catalog_failure(kind: &str, index: u32) -> CliFailure {
    CliFailure::new(
        "CLI_COMPILED_CATALOG_INCONSISTENT",
        format!("compiled {kind} catalog has no index {index}"),
    )
}

fn render_generated_sections_csv(
    sections: &[TeachingSectionImportRow],
) -> Result<Vec<u8>, CliFailure> {
    let mut ordered = sections.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|section| section.section_code.as_str());
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_writer(Vec::new());
    writer
        .write_record([
            "section_code",
            "name",
            "grade_code",
            "subject_code",
            "min_size",
            "target_size",
            "max_size",
            "room_policy",
            "room_candidates",
            "preferred_rooms",
            "fallback_rooms",
            "teacher_assignment",
            "teacher_codes",
        ])
        .map_err(|error| CliFailure::new("CLI_SECTIONING_CSV_WRITE_FAILED", error.to_string()))?;
    for section in ordered {
        let (room_policy, room_candidates, preferred_rooms, fallback_rooms) =
            room_policy_columns(&section.room_policy);
        let (teacher_assignment, teacher_codes) =
            teacher_assignment_columns(&section.teacher_assignment);
        writer
            .write_record([
                section.section_code.clone(),
                section.name.clone(),
                section.grade_code.clone(),
                section.subject_code.clone(),
                section.min_size.to_string(),
                section.target_size.to_string(),
                section.max_size.to_string(),
                room_policy.to_owned(),
                room_candidates,
                preferred_rooms,
                fallback_rooms,
                teacher_assignment.to_owned(),
                teacher_codes,
            ])
            .map_err(|error| {
                CliFailure::new("CLI_SECTIONING_CSV_WRITE_FAILED", error.to_string())
            })?;
    }
    writer.into_inner().map_err(|error| {
        CliFailure::new("CLI_SECTIONING_CSV_WRITE_FAILED", error.error().to_string())
    })
}

fn render_generated_enrollments_csv(
    enrollments: &[SectionEnrollmentImportRow],
) -> Result<Vec<u8>, CliFailure> {
    let mut ordered = enrollments.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|row| (&row.section_code, &row.student_code));
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_writer(Vec::new());
    writer
        .write_record(["section_code", "student_code"])
        .map_err(|error| CliFailure::new("CLI_SECTIONING_CSV_WRITE_FAILED", error.to_string()))?;
    for row in ordered {
        writer
            .write_record([&row.section_code, &row.student_code])
            .map_err(|error| {
                CliFailure::new("CLI_SECTIONING_CSV_WRITE_FAILED", error.to_string())
            })?;
    }
    writer.into_inner().map_err(|error| {
        CliFailure::new("CLI_SECTIONING_CSV_WRITE_FAILED", error.error().to_string())
    })
}

fn room_policy_columns(policy: &ImportedRoomPolicy) -> (&'static str, String, String, String) {
    match policy {
        ImportedRoomPolicy::AdminHomeRoom => (
            "admin_home_room",
            String::new(),
            String::new(),
            String::new(),
        ),
        ImportedRoomPolicy::Fixed { room_code } => {
            ("fixed", room_code.clone(), String::new(), String::new())
        }
        ImportedRoomPolicy::SectionFixed {
            candidate_room_codes,
        } => (
            "section_fixed",
            candidate_room_codes.join(";"),
            String::new(),
            String::new(),
        ),
        ImportedRoomPolicy::PreferredFixed {
            preferred_room_codes,
            fallback_room_codes,
        } => (
            "preferred_fixed",
            String::new(),
            preferred_room_codes.join(";"),
            fallback_room_codes.join(";"),
        ),
        ImportedRoomPolicy::Flexible {
            candidate_room_codes,
        } => (
            "flexible",
            candidate_room_codes.join(";"),
            String::new(),
            String::new(),
        ),
    }
}

fn teacher_assignment_columns(assignment: &ImportedTeacherAssignment) -> (&'static str, String) {
    match assignment {
        ImportedTeacherAssignment::Fixed { teacher_code } => ("fixed", teacher_code.clone()),
        ImportedTeacherAssignment::Candidates { teacher_codes } => {
            ("candidates", teacher_codes.join(";"))
        }
    }
}

fn ensure_output_target_available(path: &Path) -> Result<(), CliFailure> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Err(CliFailure::new(
            "CLI_OUTPUT_PARENT_MISSING",
            "output directory must have a parent",
        ));
    };
    fs::create_dir_all(parent).map_err(|error| {
        CliFailure::new(
            "CLI_OUTPUT_PARENT_CREATE_FAILED",
            format!(
                "cannot create output parent `{}`: {error}",
                parent.display()
            ),
        )
    })?;
    match fs::metadata(path) {
        Ok(metadata) if !metadata.is_dir() => Err(CliFailure::new(
            "CLI_OUTPUT_NOT_DIRECTORY",
            format!("output path `{}` is not a directory", path.display()),
        )),
        Ok(_) => {
            let mut entries = fs::read_dir(path).map_err(|error| {
                CliFailure::new("CLI_OUTPUT_DIRECTORY_READ_FAILED", error.to_string())
            })?;
            if entries
                .next()
                .transpose()
                .map_err(|error| {
                    CliFailure::new("CLI_OUTPUT_DIRECTORY_READ_FAILED", error.to_string())
                })?
                .is_some()
            {
                return Err(CliFailure::new(
                    "CLI_OUTPUT_DIRECTORY_NOT_EMPTY",
                    "output directory must be new or empty; existing artifacts are never overwritten",
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CliFailure::new(
            "CLI_OUTPUT_NOT_ACCESSIBLE",
            format!("cannot inspect output path `{}`: {error}", path.display()),
        )),
    }
}

fn commit_outputs(
    output_directory: &Path,
    summary: &[u8],
    timetable: Option<&[u8]>,
    generated_sections: Option<&[u8]>,
    generated_enrollments: Option<&[u8]>,
) -> Result<(), CliFailure> {
    ensure_output_target_available(output_directory)?;
    let parent = output_directory
        .parent()
        .ok_or_else(|| CliFailure::new("CLI_OUTPUT_PARENT_MISSING", "output parent is missing"))?;
    let file_name = output_directory
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            CliFailure::new(
                "CLI_OUTPUT_NAME_INVALID",
                "output directory name must be valid UTF-8",
            )
        })?;
    let staging = allocate_staging_directory(parent, file_name)?;
    let result = (|| {
        write_synced(&staging.join("summary.json"), summary)?;
        if let Some(bytes) = timetable {
            write_synced(&staging.join("timetable.csv"), bytes)?;
        }
        if let Some(bytes) = generated_sections {
            write_synced(&staging.join("teaching_sections.csv"), bytes)?;
        }
        if let Some(bytes) = generated_enrollments {
            write_synced(&staging.join("section_enrollments.csv"), bytes)?;
        }
        if output_directory.exists() {
            fs::remove_dir(output_directory).map_err(|error| {
                CliFailure::new(
                    "CLI_OUTPUT_EMPTY_DIRECTORY_REMOVE_FAILED",
                    format!("cannot replace empty output directory: {error}"),
                )
            })?;
        }
        fs::rename(&staging, output_directory).map_err(|error| {
            CliFailure::new(
                "CLI_OUTPUT_COMMIT_FAILED",
                format!("cannot atomically publish output directory: {error}"),
            )
        })?;
        Ok(())
    })();
    if result.is_err() && staging.exists() {
        let _ignored = fs::remove_dir_all(&staging);
    }
    result
}

fn allocate_staging_directory(parent: &Path, file_name: &str) -> Result<PathBuf, CliFailure> {
    for attempt in 0..100_u32 {
        let candidate = parent.join(format!(
            ".{file_name}.stage-{}-{attempt}",
            std::process::id()
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(CliFailure::new(
                    "CLI_OUTPUT_STAGING_CREATE_FAILED",
                    format!("cannot create staging directory: {error}"),
                ));
            }
        }
    }
    Err(CliFailure::new(
        "CLI_OUTPUT_STAGING_EXHAUSTED",
        "could not allocate a unique staging directory",
    ))
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), CliFailure> {
    let mut file = fs::File::create(path).map_err(|error| {
        CliFailure::new(
            "CLI_OUTPUT_WRITE_FAILED",
            format!("cannot create `{}`: {error}", path.display()),
        )
    })?;
    file.write_all(bytes).map_err(|error| {
        CliFailure::new(
            "CLI_OUTPUT_WRITE_FAILED",
            format!("cannot write `{}`: {error}", path.display()),
        )
    })?;
    file.sync_all().map_err(|error| {
        CliFailure::new(
            "CLI_OUTPUT_SYNC_FAILED",
            format!("cannot sync `{}`: {error}", path.display()),
        )
    })
}

const fn status_name(status: SolverRunStatus) -> &'static str {
    match status {
        SolverRunStatus::Optimal => "optimal",
        SolverRunStatus::Feasible => "feasible",
        SolverRunStatus::ProvenInfeasible => "proven_infeasible",
        SolverRunStatus::Timeout => "timeout",
        SolverRunStatus::Unknown => "unknown",
        SolverRunStatus::Cancelled => "cancelled",
        SolverRunStatus::InvalidInput => "invalid_input",
        SolverRunStatus::InvalidModel => "invalid_model",
        SolverRunStatus::InternalError => "internal_error",
    }
}

const fn audience_kind_name(kind: ImportedAudienceKind) -> &'static str {
    match kind {
        ImportedAudienceKind::AdministrativeClass => "administrative_class",
        ImportedAudienceKind::TeachingSection => "teaching_section",
    }
}

const fn day_name(day: Day) -> &'static str {
    match day {
        Day::Monday => "monday",
        Day::Tuesday => "tuesday",
        Day::Wednesday => "wednesday",
        Day::Thursday => "thursday",
        Day::Friday => "friday",
        Day::Saturday => "saturday",
        Day::Sunday => "sunday",
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ignored = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_discovery_is_canonical_and_size_bounded() {
        let directory = tempfile::tempdir().expect("temp directory");
        fs::write(directory.path().join("students.csv"), b"header\n").expect("write source");
        fs::write(directory.path().join("notes.txt"), b"ignored").expect("write note");

        let sources = read_csv_bundle(directory.path(), 100).expect("discover source");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].0, DatasetKind::Students);

        let error = read_csv_bundle(directory.path(), 2).expect_err("size must be bounded");
        assert_eq!(error.code(), "CLI_INPUT_TOO_LARGE");
    }

    #[test]
    fn unknown_csv_is_rejected_instead_of_silently_ignored() {
        let directory = tempfile::tempdir().expect("temp directory");
        fs::write(directory.path().join("student.csv"), b"header\n").expect("write source");

        let error = read_csv_bundle(directory.path(), 100).expect_err("typo must fail");
        assert_eq!(error.code(), "CLI_UNKNOWN_DATASET_FILE");
    }

    #[test]
    fn output_commit_is_directory_atomic_and_refuses_existing_content() {
        let parent = tempfile::tempdir().expect("temp directory");
        let output = parent.path().join("run");
        commit_outputs(
            &output,
            br#"{"ok":true}"#,
            Some(b"header\r\n"),
            Some(b"sections\r\n"),
            Some(b"enrollments\r\n"),
        )
        .expect("commit outputs");
        assert_eq!(
            fs::read(output.join("summary.json")).expect("read summary"),
            br#"{"ok":true}"#
        );
        assert!(output.join("timetable.csv").is_file());
        assert!(output.join("teaching_sections.csv").is_file());
        assert!(output.join("section_enrollments.csv").is_file());

        let error = ensure_output_target_available(&output).expect_err("must refuse overwrite");
        assert_eq!(error.code(), "CLI_OUTPUT_DIRECTORY_NOT_EMPTY");
    }

    #[test]
    fn every_solver_status_has_a_distinct_stable_name() {
        let statuses = [
            SolverRunStatus::Optimal,
            SolverRunStatus::Feasible,
            SolverRunStatus::ProvenInfeasible,
            SolverRunStatus::Timeout,
            SolverRunStatus::Unknown,
            SolverRunStatus::Cancelled,
            SolverRunStatus::InvalidInput,
            SolverRunStatus::InvalidModel,
            SolverRunStatus::InternalError,
        ];
        let names = statuses.map(status_name);
        assert_eq!(
            names
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            9
        );
    }
}
