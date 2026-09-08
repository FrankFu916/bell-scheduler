//! Desktop runtime jobs and DTO mapping over shared application solve/save/replay use cases.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use class_schedule_application::{
    AutoSectioningPolicy, PreparedStoredProjectSolve, SectioningProfile, SolveArtifactError,
    SolveArtifactReceipt, SolveOptions, StoredProjectSolveCommand, StoredProjectSolveError,
    StoredProjectSolveMode, execute_durable_stored_project_solve, list_project_solve_artifacts,
    load_solve_artifact, prepare_stored_project_solve, save_prepared_solve_artifact,
};
use serde::{Deserialize, Serialize};
use solver_client::{CancellationToken, SidecarSpec, SolverClient};
use solver_contract::SolverProfile;
use uuid::Uuid;

use crate::import_commit::{
    application_error, database_path, open_store, parse_project_id, parse_revision, validate_schema,
};
use crate::{COMMAND_SCHEMA_VERSION, CommandError, InspectionInputMode};

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionProfileDto {
    Reproducible,
    Fast,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SolveSectioningPolicyDto {
    pub minimum_size: u16,
    pub target_size: u16,
    pub maximum_size: u16,
    pub candidate_count: u8,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartProjectSolveRequest {
    pub schema_version: u32,
    pub project_id: String,
    pub expected_revision: String,
    pub input_mode: InspectionInputMode,
    pub auto_sectioning: Option<SolveSectioningPolicyDto>,
    pub seed: String,
    pub execution: ExecutionProfileDto,
    pub worker_count: u32,
    pub time_limit_seconds: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SolveJobRequest {
    pub schema_version: u32,
    pub job_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListProjectRunsRequest {
    pub schema_version: u32,
    pub project_id: String,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadProjectRunRequest {
    pub schema_version: u32,
    pub run_id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedRunDto {
    pub run_id: String,
    pub project_id: String,
    pub revision: String,
    pub status: String,
    pub termination_code: String,
    pub artifact_schema_version: u32,
    pub source_payload_hash: String,
    pub artifact_payload_hash: String,
    pub started_at: String,
    pub finished_at: String,
    pub adopted: bool,
}

impl From<SolveArtifactReceipt> for SavedRunDto {
    fn from(receipt: SolveArtifactReceipt) -> Self {
        Self {
            run_id: receipt.run_id,
            project_id: receipt.source.project_id.to_string(),
            revision: receipt.source.revision.to_string(),
            status: receipt.status_code,
            termination_code: receipt.termination_code,
            artifact_schema_version: receipt.artifact_schema_version,
            source_payload_hash: receipt.source.payload_hash,
            artifact_payload_hash: receipt.payload_hash,
            started_at: receipt.started_at.to_rfc3339(),
            finished_at: receipt.finished_at.to_rfc3339(),
            adopted: false,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SolveJobResponse {
    pub schema_version: u32,
    pub job_id: String,
    pub project_id: String,
    pub revision: String,
    pub state: &'static str,
    pub cancellation_requested: bool,
    pub elapsed_millis: String,
    pub run: Option<SavedRunDto>,
    pub error: Option<CommandError>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRunSummaryDto {
    pub run_id: String,
    pub project_id: String,
    pub revision: String,
    pub status: String,
    pub artifact_schema_version: u32,
    pub started_at: String,
    pub finished_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRunsResponse {
    pub schema_version: u32,
    pub runs: Vec<ProjectRunSummaryDto>,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedRunResponse {
    pub schema_version: u32,
    pub run: SavedRunDto,
    pub failure_code: Option<String>,
    pub recorded_attempt_count: usize,
}

#[derive(Debug)]
struct ActiveJob {
    project_id: String,
    revision: String,
    started: Instant,
    cancellation: CancellationToken,
    completed: Option<(Duration, Result<SavedRunDto, CommandError>)>,
}

/// Transient process coordination only. Run results are owned by application/SQLite.
#[derive(Clone, Debug, Default)]
pub struct SolveJobs(Arc<Mutex<BTreeMap<String, ActiveJob>>>, Arc<AtomicBool>);

impl SolveJobs {
    fn register(
        &self,
        project_id: String,
        revision: String,
    ) -> Result<(String, CancellationToken), CommandError> {
        let mut jobs = self.0.lock().map_err(|_| job_state_error())?;
        if self.1.load(Ordering::Acquire) {
            return Err(CommandError::new(
                "DESKTOP_SOLVE_SHUTTING_DOWN",
                "应用正在停止排课任务，请稍候退出完成。",
            ));
        }
        if jobs
            .values()
            .any(|job| job.project_id == project_id && job.completed.is_none())
        {
            return Err(CommandError::new(
                "DESKTOP_PROJECT_SOLVE_RUNNING",
                "此项目已有排课任务，请等待完成或先取消。",
            ));
        }
        if jobs.values().filter(|job| job.completed.is_none()).count() >= 2 {
            return Err(CommandError::new(
                "DESKTOP_SOLVE_CONCURRENCY_LIMIT",
                "最多同时运行两个项目，请等待已有任务结束。",
            ));
        }
        while jobs.len() >= 32 {
            let oldest = jobs
                .iter()
                .filter(|(_, job)| job.completed.is_some())
                .min_by_key(|(_, job)| job.started)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                jobs.remove(&id);
            } else {
                break;
            }
        }
        let id = Uuid::new_v4().to_string();
        let cancellation = CancellationToken::new();
        jobs.insert(
            id.clone(),
            ActiveJob {
                project_id,
                revision,
                started: Instant::now(),
                cancellation: cancellation.clone(),
                completed: None,
            },
        );
        Ok((id, cancellation))
    }

    fn finish(&self, job_id: &str, outcome: Result<SavedRunDto, CommandError>) {
        if let Ok(mut jobs) = self.0.lock()
            && let Some(job) = jobs.get_mut(job_id)
            && job.completed.is_none()
        {
            job.completed = Some((job.started.elapsed(), outcome));
        }
    }

    fn query(&self, job_id: &str, cancel: bool) -> Result<SolveJobResponse, CommandError> {
        let jobs = self.0.lock().map_err(|_| job_state_error())?;
        let job = jobs.get(job_id).ok_or_else(|| {
            CommandError::new(
                "DESKTOP_SOLVE_JOB_NOT_FOUND",
                "此任务不在当前应用会话中，请从运行历史重新打开已保存结果。",
            )
        })?;
        if cancel && job.completed.is_none() {
            job.cancellation.cancel();
        }
        let (state, elapsed, run, error) = match &job.completed {
            Some((elapsed, Ok(run))) => ("completed", *elapsed, Some(run.clone()), None),
            Some((elapsed, Err(error))) => ("failed", *elapsed, None, Some(error.clone())),
            None => ("running", job.started.elapsed(), None, None),
        };
        Ok(SolveJobResponse {
            schema_version: COMMAND_SCHEMA_VERSION,
            job_id: job_id.to_owned(),
            project_id: job.project_id.clone(),
            revision: job.revision.clone(),
            state,
            cancellation_requested: job.cancellation.is_cancelled(),
            elapsed_millis: elapsed.as_millis().to_string(),
            run,
            error,
        })
    }

    pub fn cancel_all(&self) {
        if let Ok(jobs) = self.0.lock() {
            for job in jobs.values().filter(|job| job.completed.is_none()) {
                job.cancellation.cancel();
            }
        }
    }

    pub fn has_running(&self) -> bool {
        self.0
            .lock()
            .is_ok_and(|jobs| jobs.values().any(|job| job.completed.is_none()))
    }

    pub fn begin_shutdown(&self) -> bool {
        !self.1.swap(true, Ordering::AcqRel)
    }
}

fn job_state_error() -> CommandError {
    CommandError::new(
        "DESKTOP_SOLVE_STATE_UNAVAILABLE",
        "无法读取当前任务状态，请重启后从运行历史检查结果。",
    )
}

pub(super) fn defer_shutdown(app: &tauri::AppHandle) -> bool {
    use tauri::Manager;
    let jobs = app.state::<SolveJobs>().inner().clone();
    // Set the guard before testing running jobs: a concurrent preparation cannot start a child
    // between the final empty-state check and application exit.
    let first_shutdown = jobs.begin_shutdown();
    if !jobs.has_running() {
        return false;
    }
    if first_shutdown {
        jobs.cancel_all();
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            while jobs.has_running() {
                std::thread::sleep(Duration::from_millis(50));
            }
            app.exit(0);
        });
    }
    true
}

fn command_error(error: StoredProjectSolveError) -> CommandError {
    match error {
        StoredProjectSolveError::Import(error) => application_error(error),
        other => CommandError::new(
            other.code(),
            "排课方式与保存项目不一致，或求解参数无效。请重新打开项目并检查成班策略。",
        ),
    }
}

pub(super) fn artifact_error(error: SolveArtifactError) -> CommandError {
    match error {
        SolveArtifactError::Import(error) => application_error(error),
        SolveArtifactError::Persistence(error) => application_error(error.into()),
        SolveArtifactError::Command(error) => command_error(error),
        other => CommandError::new(
            other.code(),
            "运行结果未通过保存或重载校验，不能作为可用课表。请保留错误代码检查原始数据与应用版本。",
        ),
    }
}

fn decode_request(
    request: &StartProjectSolveRequest,
) -> Result<(StoredProjectSolveCommand, SolveOptions), CommandError> {
    validate_schema(request.schema_version)?;
    let project_id = parse_project_id(&request.project_id)?;
    let expected_revision = parse_revision(&request.expected_revision)?;
    let seed = parse_revision(&request.seed).map_err(|_| {
        CommandError::new(
            "DESKTOP_INVALID_SOLVE_SEED",
            "随机种子必须是非负整数的十进制文本。",
        )
    })?;
    if !(1..=3600).contains(&request.time_limit_seconds)
        || !(1..=16).contains(&request.worker_count)
        || (matches!(request.execution, ExecutionProfileDto::Reproducible)
            && request.worker_count != 1)
    {
        return Err(CommandError::new(
            "DESKTOP_INVALID_SOLVE_LIMITS",
            "每次尝试时限为 1–3600 秒，线程数为 1–16；可复现模式必须使用一个线程。",
        ));
    }
    let mut options = SolveOptions::reproducible(
        seed,
        Duration::from_secs(u64::from(request.time_limit_seconds)),
    );
    if matches!(request.execution, ExecutionProfileDto::Fast) {
        options.reproducible = false;
        options.profile = SolverProfile::Fast;
        options.worker_count = request.worker_count;
    }
    let mode = match (request.input_mode, request.auto_sectioning) {
        (InspectionInputMode::ExistingSections, None) => StoredProjectSolveMode::ExistingSections,
        (InspectionInputMode::AutoSectioning, Some(policy)) => {
            StoredProjectSolveMode::AutoSectioning(
                AutoSectioningPolicy::new(
                    policy.minimum_size,
                    policy.target_size,
                    policy.maximum_size,
                    seed,
                    if matches!(request.execution, ExecutionProfileDto::Fast) {
                        SectioningProfile::Fast
                    } else {
                        SectioningProfile::Balanced
                    },
                    policy.candidate_count,
                )
                .map_err(|error| {
                    CommandError::new(
                        error.code(),
                        "班额需满足最小 ≤ 目标 ≤ 最大，候选数量为 1–16。",
                    )
                })?,
            )
        }
        _ => {
            return Err(CommandError::new(
                "DESKTOP_SOLVE_SECTIONING_POLICY_MISMATCH",
                "自动成班需明确填写班额与候选数量；学校已成班模式不能同时提供自动成班策略。",
            ));
        }
    };
    Ok((
        StoredProjectSolveCommand {
            project_id,
            expected_revision,
            mode,
        },
        options,
    ))
}

fn prepare_start(
    request: &StartProjectSolveRequest,
    path: &Path,
    resolve: impl FnOnce() -> Result<SidecarSpec, CommandError>,
) -> Result<(PreparedStoredProjectSolve, SolveOptions, SolverClient), CommandError> {
    let (command, options) = decode_request(request)?;
    let store = open_store(path)?;
    let prepared = prepare_stored_project_solve(&store, &command).map_err(command_error)?;
    drop(store);
    let client =
        SolverClient::new(resolve()?).timeout(options.time_limit + Duration::from_secs(10));
    Ok((prepared, options, client))
}

#[tauri::command]
pub async fn start_project_solve(
    app: tauri::AppHandle,
    jobs: tauri::State<'_, SolveJobs>,
    request: StartProjectSolveRequest,
) -> Result<SolveJobResponse, CommandError> {
    decode_request(&request)?;
    let path = database_path(&app)?;
    let prepare_path = path.clone();
    let (prepared, options, client) = tauri::async_runtime::spawn_blocking(move || {
        prepare_start(&request, &prepare_path, || crate::managed_worker::resolve_managed_worker(&app)
            .map(|worker| worker.sidecar_spec()).map_err(|error| CommandError::new(error.code(), "本机排课引擎未完整安装或校验失败。请使用包含引擎的 Bell 应用，重新构建或安装后再试。")))
    }).await.map_err(|_| job_state_error())??;
    let jobs = jobs.inner().clone();
    let (job_id, cancellation) = jobs.register(
        prepared.receipt().project_id.to_string(),
        prepared.receipt().revision.to_string(),
    )?;
    let started = jobs.query(&job_id, false)?;
    tauri::async_runtime::spawn(async move {
        let outcome = tauri::async_runtime::spawn_blocking(move || {
            execute_and_save(&path, prepared, &options, &client, &cancellation)
        })
        .await
        .unwrap_or_else(|_| {
            Err(CommandError::new(
                "DESKTOP_SOLVE_TASK_FAILED",
                "本次排课任务异常中止，未确认保存；请检查运行历史后重试。",
            ))
        });
        jobs.finish(&job_id, outcome);
    });
    Ok(started)
}

fn execute_and_save(
    path: &Path,
    prepared: PreparedStoredProjectSolve,
    options: &SolveOptions,
    client: &SolverClient,
    cancellation: &CancellationToken,
) -> Result<SavedRunDto, CommandError> {
    let artifact = execute_durable_stored_project_solve(prepared, options, client, cancellation)
        .map_err(artifact_error)?;
    let mut store = open_store(path)?;
    save_prepared_solve_artifact(&mut store, &artifact)
        .map(SavedRunDto::from)
        .map_err(artifact_error)
}

#[tauri::command]
// Tauri owns and deserializes command arguments at the IPC boundary.
#[allow(clippy::needless_pass_by_value)]
pub fn query_solve_job(
    jobs: tauri::State<'_, SolveJobs>,
    request: SolveJobRequest,
) -> Result<SolveJobResponse, CommandError> {
    validate_schema(request.schema_version)?;
    jobs.query(&request.job_id, false)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn cancel_solve_job(
    jobs: tauri::State<'_, SolveJobs>,
    request: SolveJobRequest,
) -> Result<SolveJobResponse, CommandError> {
    validate_schema(request.schema_version)?;
    jobs.query(&request.job_id, true)
}

#[tauri::command]
pub async fn list_project_runs(
    app: tauri::AppHandle,
    request: ListProjectRunsRequest,
) -> Result<ProjectRunsResponse, CommandError> {
    validate_schema(request.schema_version)?;
    let project_id = parse_project_id(&request.project_id)?;
    let path = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let store = open_store(&path)?;
        let page = list_project_solve_artifacts(&store, project_id, request.limit, request.offset)
            .map_err(artifact_error)?;
        Ok(ProjectRunsResponse {
            schema_version: COMMAND_SCHEMA_VERSION,
            runs: page
                .runs
                .into_iter()
                .map(|run| ProjectRunSummaryDto {
                    run_id: run.run_id,
                    project_id: run.project_id.to_string(),
                    revision: run.project_revision.to_string(),
                    status: run.status_code,
                    artifact_schema_version: run.artifact_schema_version,
                    started_at: run.started_at.to_rfc3339(),
                    finished_at: run.finished_at.to_rfc3339(),
                })
                .collect(),
            has_more: page.has_more,
            next_offset: page.next_offset,
        })
    })
    .await
    .map_err(|_| job_state_error())?
}

#[tauri::command]
pub async fn load_project_run(
    app: tauri::AppHandle,
    request: LoadProjectRunRequest,
) -> Result<LoadedRunResponse, CommandError> {
    validate_schema(request.schema_version)?;
    let path = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let store = open_store(&path)?;
        let loaded = load_solve_artifact(&store, &request.run_id).map_err(artifact_error)?;
        Ok(LoadedRunResponse {
            schema_version: COMMAND_SCHEMA_VERSION,
            run: loaded.receipt.into(),
            failure_code: loaded.failure.map(|failure| failure.code),
            recorded_attempt_count: loaded.attempts.len(),
        })
    })
    .await
    .map_err(|_| job_state_error())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use class_schedule_application::{
        CalendarDefinition, CsvImportAuditOptions, CsvImportMode, ImportCommitCommand,
        ImportCommitIntent, commit_csv_import,
    };
    use class_schedule_import::CsvSource;
    use serde_json::json;

    fn request() -> StartProjectSolveRequest {
        serde_json::from_value(json!({"schemaVersion":1, "projectId":"77777777-1111-4111-8111-111111111111",
            "expectedRevision":"0", "inputMode":"existing_sections", "autoSectioning":null,
            "seed":"9007199254740993", "execution":"reproducible", "workerCount":1, "timeLimitSeconds":30})).unwrap()
    }

    #[test]
    fn solve_dto_keeps_large_seed_and_revision_exact_and_refuses_untrusted_paths() {
        let mut value = serde_json::to_value(json!({"schemaVersion":1, "projectId":request().project_id,
            "expectedRevision":"0", "inputMode":"existing_sections", "autoSectioning":null,
            "seed":"9007199254740993", "execution":"reproducible", "workerCount":1, "timeLimitSeconds":30})).unwrap();
        for field in ["workerPath", "databasePath", "assignments"] {
            value[field] = json!("untrusted");
            assert!(serde_json::from_value::<StartProjectSolveRequest>(value.clone()).is_err());
            value.as_object_mut().unwrap().remove(field);
        }
        let (_, options) = decode_request(&request()).unwrap();
        assert_eq!(options.seed, 9_007_199_254_740_993);
        let mut request = request();
        request.expected_revision = "9007199254740993".into();
        assert_eq!(
            decode_request(&request).unwrap().0.expected_revision,
            9_007_199_254_740_993
        );
    }

    #[test]
    fn invalid_limits_mode_and_seed_fail_before_database_or_worker_access() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("must-not-exist.sqlite3");
        let mut invalid = request();
        invalid.worker_count = 2;
        assert_eq!(
            prepare_start(&invalid, &path, || panic!(
                "worker resolution must not happen"
            ))
            .unwrap_err()
            .code,
            "DESKTOP_INVALID_SOLVE_LIMITS"
        );
        invalid = request();
        invalid.seed = "01".into();
        assert_eq!(
            decode_request(&invalid).unwrap_err().code,
            "DESKTOP_INVALID_SOLVE_SEED"
        );
        invalid = request();
        invalid.input_mode = InspectionInputMode::AutoSectioning;
        assert_eq!(
            decode_request(&invalid).unwrap_err().code,
            "DESKTOP_SOLVE_SECTIONING_POLICY_MISMATCH"
        );
        assert!(!path.exists());
    }

    #[test]
    fn saved_revision_conflict_is_detected_before_managed_worker_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.sqlite3");
        let mut store = open_store(&path).unwrap();
        let datasets = crate::tests::payloads(&[
            "students",
            "administrative_classes",
            "student_subject_choices",
            "teachers",
            "teacher_unavailability",
            "rooms",
            "course_plans",
            "teaching_sections",
            "section_enrollments",
            "course_offerings",
            "fixed_activities",
        ]);
        let sources = datasets
            .iter()
            .map(|dataset| {
                CsvSource::new(
                    crate::parse_dataset_kind(&dataset.dataset).unwrap(),
                    &dataset.bytes,
                )
            })
            .collect::<Vec<_>>();
        commit_csv_import(
            &mut store,
            &ImportCommitCommand {
                project_id: request().project_id.parse().unwrap(),
                display_name: "desktop solve fixture".into(),
                intent: ImportCommitIntent::Create,
                options: CsvImportAuditOptions {
                    project_stable_key: "desktop-solve-fixture".into(),
                    calendar: CalendarDefinition::weekday_with_break(8, 4).unwrap(),
                    exact_subject_choices: 3,
                    mode: CsvImportMode::ExistingSections,
                },
            },
            sources,
        )
        .unwrap();
        let before = store.load_project(&request().project_id).unwrap();
        let mut stale = request();
        stale.expected_revision = "1".into();
        let error = prepare_start(&stale, &path, || {
            panic!("stale request cannot resolve worker")
        })
        .unwrap_err();
        assert_eq!(error.code, "PERSISTENCE_REVISION_CONFLICT");
        assert_eq!(store.load_project(&request().project_id).unwrap(), before);
        assert!(
            store
                .list_solve_artifacts(&request().project_id, 1, 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn runtime_jobs_reject_duplicate_project_and_bound_concurrency() {
        let jobs = SolveJobs::default();
        let (id, _) = jobs.register("a".into(), "0".into()).unwrap();
        assert_eq!(
            jobs.register("a".into(), "1".into()).unwrap_err().code,
            "DESKTOP_PROJECT_SOLVE_RUNNING"
        );
        jobs.register("b".into(), "0".into()).unwrap();
        assert_eq!(
            jobs.register("c".into(), "0".into()).unwrap_err().code,
            "DESKTOP_SOLVE_CONCURRENCY_LIMIT"
        );
        jobs.finish(&id, Err(CommandError::new("TEST_FAILURE", "test failure")));
        assert!(jobs.register("a".into(), "1".into()).is_ok());
    }

    #[test]
    fn cancellation_signals_the_actual_token_without_inventing_a_completed_result() {
        let jobs = SolveJobs::default();
        let (id, token) = jobs.register("project".into(), "0".into()).unwrap();
        let response = jobs.query(&id, true).unwrap();
        assert!(token.is_cancelled());
        assert_eq!(response.state, "running");
        assert!(response.run.is_none());
        jobs.finish(&id, Err(CommandError::new("TEST_FAILURE", "test failure")));
        jobs.finish(
            &id,
            Err(CommandError::new("MUST_NOT_REPLACE", "second completion")),
        );
        assert_eq!(
            jobs.query(&id, true).unwrap().error.unwrap().code,
            "TEST_FAILURE"
        );
        assert_eq!(
            jobs.query("unknown", true).unwrap_err().code,
            "DESKTOP_SOLVE_JOB_NOT_FOUND"
        );
    }

    #[test]
    fn shutdown_blocks_new_jobs_and_cancels_all_live_tokens() {
        let jobs = SolveJobs::default();
        let (_, first) = jobs.register("a".into(), "0".into()).unwrap();
        let (_, second) = jobs.register("b".into(), "0".into()).unwrap();
        assert!(jobs.begin_shutdown());
        assert!(!jobs.begin_shutdown());
        jobs.cancel_all();
        assert!(first.is_cancelled() && second.is_cancelled());
        assert_eq!(
            jobs.register("new".into(), "0".into()).unwrap_err().code,
            "DESKTOP_SOLVE_SHUTTING_DOWN"
        );
    }

    #[test]
    fn old_completed_runtime_entries_are_bounded_but_sqlite_history_is_independent() {
        let jobs = SolveJobs::default();
        let mut first = None;
        for index in 0..40 {
            let (id, _) = jobs.register(format!("p-{index}"), "0".into()).unwrap();
            first.get_or_insert_with(|| id.clone());
            jobs.finish(&id, Err(CommandError::new("TEST_FAILURE", "test failure")));
        }
        assert_eq!(jobs.0.lock().unwrap().len(), 32);
        assert_eq!(
            jobs.query(&first.unwrap(), false).unwrap_err().code,
            "DESKTOP_SOLVE_JOB_NOT_FOUND"
        );
    }
}
