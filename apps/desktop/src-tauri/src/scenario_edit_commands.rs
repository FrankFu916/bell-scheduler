//! Versioned edit DTOs over one application preview/prepare/commit pipeline.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use class_schedule_application::{
    ScenarioApplicationError, ScenarioEditCommand, ScenarioEditCommitCommand,
    ScenarioEditCommitReceipt, ScenarioEditCommitStatus, ScenarioEditContext,
    ScenarioEditOperation, ScenarioEditPreview, ScenarioEditStatus,
};
use class_schedule_domain::MeetingAssignment;
use class_schedule_persistence::SqliteStore;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::import_commit::database_path;
use crate::scenario_commands::ScenarioReceiptDto;
use crate::scenario_timetable_queries::{
    decode_identity, open_query_store, validate_database_path,
};
use crate::timetable_queries::{TimetableQualityTierDto, TimetableRowDto, day_code, quality_dto};
use crate::{COMMAND_SCHEMA_VERSION, CommandError, HardProblemDto};

#[cfg(test)]
mod tests;

#[derive(Debug, Default)]
pub(super) struct ScenarioEditJobs {
    active: Arc<AtomicBool>,
}

impl ScenarioEditJobs {
    fn try_begin(&self) -> Result<EditPermit, CommandError> {
        self.active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                CommandError::new(
                    "DESKTOP_SCENARIO_EDIT_BUSY",
                    "正在预览或保存调课，请等待当前操作完成。",
                )
            })?;
        Ok(EditPermit(Arc::clone(&self.active)))
    }
}

#[derive(Debug)]
struct EditPermit(Arc<AtomicBool>);

impl Drop for EditPermit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ScenarioEditOperationDto {
    Move {
        activity_id: String,
        start_timeslot_id: String,
        lock_after: bool,
    },
    SwapStarts {
        left_activity_id: String,
        right_activity_id: String,
    },
    LockCurrent {
        activity_id: String,
    },
    Unlock {
        activity_id: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewScenarioEditRequest {
    pub schema_version: u32,
    pub scenario_id: String,
    pub expected_scenario_revision: String,
    pub expected_timetable_revision: String,
    pub operation: ScenarioEditOperationDto,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitScenarioEditRequest {
    pub schema_version: u32,
    pub scenario_id: String,
    pub expected_scenario_revision: String,
    pub expected_timetable_revision: String,
    pub expected_scenario_payload_hash: String,
    pub expected_timetable_payload_hash: String,
    pub operation: ScenarioEditOperationDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditAssignmentDto {
    pub start_timeslot_id: String,
    pub duration_periods: u8,
    pub room_id: String,
    pub teacher_id: String,
}

impl From<MeetingAssignment> for EditAssignmentDto {
    fn from(assignment: MeetingAssignment) -> Self {
        Self {
            start_timeslot_id: assignment.start().to_string(),
            duration_periods: assignment.duration().get(),
            room_id: assignment.room_id().to_string(),
            teacher_id: assignment.teacher_id().to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioActivityChangeDto {
    pub activity_id: String,
    pub before: EditAssignmentDto,
    pub after: EditAssignmentDto,
    pub was_user_locked: bool,
    pub is_user_locked: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioEditValidationDto {
    pub passed: bool,
    pub hard_problems: Vec<HardProblemDto>,
    pub total_hard_problems: u32,
    pub hard_problems_truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioEditTimeslotDto {
    pub timeslot_id: String,
    pub day: &'static str,
    pub day_label: String,
    pub period_index: u16,
    pub period_label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioEditDiagnosticDto {
    pub code: &'static str,
    pub message: &'static str,
    pub activity_ids: Vec<String>,
    pub activities_truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioEditContextDto {
    pub activities: Vec<TimetableRowDto>,
    pub calendar: Vec<ScenarioEditTimeslotDto>,
    pub diagnostics: Vec<ScenarioEditDiagnosticDto>,
    pub total_diagnostics: u32,
    pub diagnostics_truncated: bool,
    pub activities_truncated: bool,
}

impl From<ScenarioEditContext> for ScenarioEditContextDto {
    fn from(context: ScenarioEditContext) -> Self {
        Self {
            activities: context.activities.into_iter().map(Into::into).collect(),
            calendar: context
                .calendar
                .into_iter()
                .map(|slot| ScenarioEditTimeslotDto {
                    timeslot_id: slot.timeslot_id.to_string(),
                    day: day_code(slot.day),
                    day_label: slot.day_label,
                    period_index: slot.period_index,
                    period_label: slot.period_label,
                })
                .collect(),
            diagnostics: context
                .diagnostics
                .into_iter()
                .map(|problem| ScenarioEditDiagnosticDto {
                    code: problem.code.as_str(),
                    message: crate::problem_messages::hard_problem_message(problem.code),
                    activity_ids: problem
                        .activity_ids
                        .into_iter()
                        .map(|id| id.to_string())
                        .collect(),
                    activities_truncated: problem.activities_truncated,
                })
                .collect(),
            total_diagnostics: context.total_diagnostics,
            diagnostics_truncated: context.diagnostics_truncated,
            activities_truncated: context.activities_truncated,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioEditPreviewDto {
    pub schema_version: u32,
    pub receipt: ScenarioReceiptDto,
    pub source_is_current: bool,
    pub status: &'static str,
    pub can_commit: bool,
    pub changes: Vec<ScenarioActivityChangeDto>,
    pub validation: ScenarioEditValidationDto,
    pub before_quality: Vec<TimetableQualityTierDto>,
    pub after_quality: Option<Vec<TimetableQualityTierDto>>,
    pub context: ScenarioEditContextDto,
}

impl From<ScenarioEditPreview> for ScenarioEditPreviewDto {
    fn from(preview: ScenarioEditPreview) -> Self {
        let passed = preview.validation.is_valid();
        let mut report = preview.validation;
        // Use the application's bounded diagnostic prefix, while preserving its full verdict.
        report
            .hard_problems
            .truncate(preview.context.diagnostics.len());
        let validation = ScenarioEditValidationDto {
            passed,
            hard_problems: crate::static_validation_dto(report).hard_problems,
            total_hard_problems: preview.context.total_diagnostics,
            hard_problems_truncated: preview.context.diagnostics_truncated,
        };
        Self {
            schema_version: COMMAND_SCHEMA_VERSION,
            receipt: (&preview.receipt).into(),
            source_is_current: preview.source_is_current,
            status: match preview.status {
                ScenarioEditStatus::Valid => "valid",
                ScenarioEditStatus::NoChange => "no_change",
                ScenarioEditStatus::HardRejected => "hard_rejected",
            },
            can_commit: preview.can_commit,
            changes: preview
                .changes
                .into_iter()
                .map(|change| ScenarioActivityChangeDto {
                    activity_id: change.activity_id.to_string(),
                    before: change.before.into(),
                    after: change.after.into(),
                    was_user_locked: change.was_user_locked,
                    is_user_locked: change.is_user_locked,
                })
                .collect(),
            validation,
            before_quality: quality_dto(&preview.before_quality),
            after_quality: preview.after_quality.as_ref().map(quality_dto),
            context: preview.context.into(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioEditCommitDto {
    pub schema_version: u32,
    pub status: &'static str,
    pub receipt: ScenarioReceiptDto,
}

impl From<ScenarioEditCommitReceipt> for ScenarioEditCommitDto {
    fn from(result: ScenarioEditCommitReceipt) -> Self {
        Self {
            schema_version: COMMAND_SCHEMA_VERSION,
            status: match result.status {
                ScenarioEditCommitStatus::Committed => "committed",
                ScenarioEditCommitStatus::NoChange => "no_change",
            },
            receipt: (&result.receipt).into(),
        }
    }
}

fn parse_request<T: DeserializeOwned>(value: Value) -> Result<T, CommandError> {
    serde_json::from_value(value).map_err(|_| {
        CommandError::new(
            "DESKTOP_SCENARIO_EDIT_INVALID_REQUEST",
            "调课请求格式无效，请重新选择课次、操作和目标课节。",
        )
    })
}

fn stable_id(value: &str) -> Result<Uuid, CommandError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|id| id.to_string() == value)
        .ok_or_else(|| {
            CommandError::new(
                "DESKTOP_SCENARIO_EDIT_INVALID_ID",
                "请选择有效的课次和目标课节。",
            )
        })
}

fn decode_operation(
    operation: &ScenarioEditOperationDto,
) -> Result<ScenarioEditOperation, CommandError> {
    Ok(match operation {
        ScenarioEditOperationDto::Move {
            activity_id,
            start_timeslot_id,
            lock_after,
        } => ScenarioEditOperation::Move {
            activity_id: stable_id(activity_id)?.into(),
            start: stable_id(start_timeslot_id)?.into(),
            lock_after: *lock_after,
        },
        ScenarioEditOperationDto::SwapStarts {
            left_activity_id,
            right_activity_id,
        } => ScenarioEditOperation::SwapStarts {
            left: stable_id(left_activity_id)?.into(),
            right: stable_id(right_activity_id)?.into(),
        },
        ScenarioEditOperationDto::LockCurrent { activity_id } => {
            ScenarioEditOperation::LockCurrent {
                activity_id: stable_id(activity_id)?.into(),
            }
        }
        ScenarioEditOperationDto::Unlock { activity_id } => ScenarioEditOperation::Unlock {
            activity_id: stable_id(activity_id)?.into(),
        },
    })
}

fn decode_command(
    schema: u32,
    scenario_id: &str,
    scenario_revision: &str,
    timetable_revision: &str,
    operation: &ScenarioEditOperationDto,
) -> Result<ScenarioEditCommand, CommandError> {
    let identity = decode_identity(schema, scenario_id, scenario_revision, timetable_revision)?;
    Ok(ScenarioEditCommand {
        scenario_id: identity.scenario_id,
        expected_scenario_revision: identity.scenario_revision,
        expected_timetable_revision: identity.timetable_revision,
        operation: decode_operation(operation)?,
    })
}

fn decode_commit(
    request: &CommitScenarioEditRequest,
) -> Result<ScenarioEditCommitCommand, CommandError> {
    let edit = decode_command(
        request.schema_version,
        &request.scenario_id,
        &request.expected_scenario_revision,
        &request.expected_timetable_revision,
        &request.operation,
    )?;
    for hash in [
        &request.expected_scenario_payload_hash,
        &request.expected_timetable_payload_hash,
    ] {
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CommandError::new(
                "APPLICATION_SCENARIO_INVALID_HASH",
                "预览凭据摘要无效，请重新预览后再保存调课。",
            ));
        }
    }
    Ok(ScenarioEditCommitCommand {
        edit,
        expected_scenario_payload_hash: request.expected_scenario_payload_hash.clone(),
        expected_timetable_payload_hash: request.expected_timetable_payload_hash.clone(),
    })
}

fn preview_inner(
    request: &PreviewScenarioEditRequest,
    database: &Path,
) -> Result<ScenarioEditPreviewDto, CommandError> {
    let command = decode_command(
        request.schema_version,
        &request.scenario_id,
        &request.expected_scenario_revision,
        &request.expected_timetable_revision,
        &request.operation,
    )?;
    let store = open_query_store(database)?;
    class_schedule_application::preview_scenario_edit(&store, &command)
        .map(Into::into)
        .map_err(|error| edit_error(&error))
}

fn commit_inner(
    request: &CommitScenarioEditRequest,
    database: &Path,
) -> Result<ScenarioEditCommitDto, CommandError> {
    let command = decode_commit(request)?;
    validate_database_path(database)?;
    let mut store = SqliteStore::open_existing(database).map_err(|error| {
        let message = match error.code() {
            "PERSISTENCE_SCHEMA_UPGRADE_REQUIRED" => {
                "本机项目库需要升级，请先使用兼容版本打开项目。"
            }
            "PERSISTENCE_UNSUPPORTED_SCHEMA" => "本机项目库版本不受支持，请使用兼容的应用版本。",
            _ => "无法打开现有项目库，调课未提交。请重新打开项目。",
        };
        CommandError::new(error.code(), message)
    })?;
    let prepared = class_schedule_application::prepare_scenario_edit_commit(&store, &command)
        .map_err(|error| edit_error(&error))?;
    class_schedule_application::commit_prepared_scenario_edit(&mut store, prepared)
        .map(Into::into)
        .map_err(|error| edit_error(&error))
}

#[tauri::command]
pub async fn preview_scenario_edit(
    app: tauri::AppHandle,
    jobs: tauri::State<'_, ScenarioEditJobs>,
    request: Value,
) -> Result<ScenarioEditPreviewDto, CommandError> {
    let permit = jobs.try_begin()?;
    let request = parse_request(request)?;
    let database = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        preview_inner(&request, &database)
    })
    .await
    .map_err(|_| {
        CommandError::new(
            "DESKTOP_SCENARIO_EDIT_PREVIEW_FAILED",
            "调课预览未完成，原课表未修改。请重新预览。",
        )
    })?
}

#[tauri::command]
pub async fn commit_scenario_edit(
    app: tauri::AppHandle,
    jobs: tauri::State<'_, ScenarioEditJobs>,
    request: Value,
) -> Result<ScenarioEditCommitDto, CommandError> {
    let permit = jobs.try_begin()?;
    let request = parse_request(request)?;
    let database = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        commit_inner(&request, &database)
    })
    .await
    .map_err(|_| {
        CommandError::new(
            "DESKTOP_SCENARIO_EDIT_COMMIT_UNCONFIRMED",
            "调课提交结果未确认，请重新打开方案核对当前版本。",
        )
    })?
}

fn edit_error(error: &ScenarioApplicationError) -> CommandError {
    let code = error.code();
    let message = match code {
        "APPLICATION_SCENARIO_EDIT_HARD_REJECTED" => {
            "本次调课违反硬性约束，未保存。请重新预览并查看冲突。"
        }
        "APPLICATION_SCENARIO_EDIT_UNKNOWN_ACTIVITY" => {
            "所选课次不属于此方案，请重新打开方案选择课次。"
        }
        "APPLICATION_SCENARIO_EDIT_UNKNOWN_TIMESLOT" => "目标课节不属于此方案日历，请重新选择。",
        "PERSISTENCE_SCENARIO_REVISION_TIMESTAMP_INVALID" => {
            "本机时间早于上一修订或精度无效，未保存调课。请核对本机时间。"
        }
        "PERSISTENCE_INTEGER_OUT_OF_RANGE" => "方案版本已达到存储上限，无法追加调课版本。",
        "APPLICATION_SCENARIO_INVALID_HASH" => "预览凭据摘要无效，请重新预览。",
        code if code.contains("REVISION_CONFLICT")
            || code.contains("HASH_CONFLICT")
            || code.contains("HASH_MISMATCH") =>
        {
            "来源、方案或课表已变化，未保存调课。请重新打开方案并预览。"
        }
        code if code.ends_with("NOT_FOUND") => {
            "没有找到方案或其来源记录，请刷新列表并重新打开方案。"
        }
        code if code.contains("RESOURCE_LIMIT") => "方案超过当前调课处理上限，本次操作未提交。",
        _ => "方案调课未通过独立复核或保存，请保留错误代码并重新打开方案。",
    };
    CommandError::new(code, message)
}
