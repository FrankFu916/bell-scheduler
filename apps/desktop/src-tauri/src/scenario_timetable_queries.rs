//! Read-only scenario timetable transport with exact scenario and timetable revisions.

use std::path::Path;

use class_schedule_application::{
    ScenarioTimetableEntityPage, ScenarioTimetablePage, ScenarioTimetableQueryError, TimetableQuery,
};
use class_schedule_domain::{Revision, ScenarioId};
use class_schedule_persistence::SqliteStore;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::CommandError;
use crate::import_commit::{database_path, parse_revision, validate_schema};
use crate::scenario_commands::ScenarioReceiptDto;
use crate::timetable_queries::{
    TimetableEntityDto, TimetableGridCellDto, TimetableMetricDto, TimetableQualityTierDto,
    TimetableRowDto, TimetableViewDto, decode_filter, validate_page,
};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioTimetableEntitiesRequest {
    pub schema_version: u32,
    pub scenario_id: String,
    pub expected_scenario_revision: String,
    pub expected_timetable_revision: String,
    pub view: TimetableViewDto,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioTimetableRequest {
    pub schema_version: u32,
    pub scenario_id: String,
    pub expected_scenario_revision: String,
    pub expected_timetable_revision: String,
    pub view: TimetableViewDto,
    pub entity_id: String,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioTimetableEntitiesResponse {
    pub schema_version: u32,
    pub receipt: ScenarioReceiptDto,
    pub source_is_current: bool,
    pub scenario_display_name: String,
    pub view: TimetableViewDto,
    pub entities: Vec<TimetableEntityDto>,
    pub total_entities: u32,
    pub offset: u32,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

impl From<ScenarioTimetableEntityPage> for ScenarioTimetableEntitiesResponse {
    fn from(page: ScenarioTimetableEntityPage) -> Self {
        Self {
            schema_version: page.schema_version,
            receipt: (&page.receipt).into(),
            source_is_current: page.source_is_current,
            scenario_display_name: page.scenario_display_name,
            view: page.view.into(),
            entities: page.entities.into_iter().map(Into::into).collect(),
            total_entities: page.total_entities,
            offset: page.offset,
            has_more: page.has_more,
            next_offset: page.next_offset,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioTimetableResponse {
    pub schema_version: u32,
    pub receipt: ScenarioReceiptDto,
    pub source_is_current: bool,
    pub scenario_display_name: String,
    pub selection: TimetableEntityDto,
    pub rows: Vec<TimetableRowDto>,
    pub calendar: Vec<TimetableGridCellDto>,
    pub total_rows: u32,
    pub offset: u32,
    pub has_more: bool,
    pub next_offset: Option<u32>,
    pub quality: Vec<TimetableQualityTierDto>,
}

impl From<ScenarioTimetablePage> for ScenarioTimetableResponse {
    fn from(page: ScenarioTimetablePage) -> Self {
        Self {
            schema_version: page.schema_version,
            receipt: (&page.receipt).into(),
            source_is_current: page.source_is_current,
            scenario_display_name: page.scenario_display_name,
            selection: page.selection.into(),
            rows: page.rows.into_iter().map(Into::into).collect(),
            calendar: page.calendar.into_iter().map(Into::into).collect(),
            total_rows: page.total_rows,
            offset: page.offset,
            has_more: page.has_more,
            next_offset: page.next_offset,
            quality: page
                .quality
                .tiers
                .into_iter()
                .map(|tier| TimetableQualityTierDto {
                    id: tier.id,
                    priority: tier.priority,
                    value: tier.value.to_string(),
                    metrics: tier
                        .metrics
                        .into_iter()
                        .map(|metric| TimetableMetricDto {
                            code: metric.kind.code(),
                            raw_value: metric.raw_value.to_string(),
                            weight_within_tier: metric.weight_within_tier,
                            weighted_value: metric.weighted_value.to_string(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

#[derive(Debug)]
pub(super) struct ScenarioQueryIdentity {
    pub(super) scenario_id: ScenarioId,
    pub(super) scenario_revision: Revision,
    pub(super) timetable_revision: Revision,
}

pub(super) fn decode_identity(
    schema: u32,
    scenario_id: &str,
    scenario_revision: &str,
    timetable_revision: &str,
) -> Result<ScenarioQueryIdentity, CommandError> {
    validate_schema(schema)?;
    let scenario_id = Uuid::parse_str(scenario_id)
        .ok()
        .filter(|id| id.to_string() == scenario_id)
        .ok_or_else(|| {
            CommandError::new("DESKTOP_SCENARIO_INVALID_ID", "请选择有效的已保存方案。")
        })?;
    let revision = |value| {
        parse_revision(value)
            .map(Revision::from_u64)
            .map_err(|error| {
                CommandError::new(
                    error.code,
                    "方案版本和课表版本必须是有效的非负整数字符串，请重新打开方案。",
                )
            })
    };
    Ok(ScenarioQueryIdentity {
        scenario_id: scenario_id.into(),
        scenario_revision: revision(scenario_revision)?,
        timetable_revision: revision(timetable_revision)?,
    })
}

fn query_entities_inner(
    request: &ScenarioTimetableEntitiesRequest,
    database: &Path,
) -> Result<ScenarioTimetableEntitiesResponse, CommandError> {
    let identity = decode_identity(
        request.schema_version,
        &request.scenario_id,
        &request.expected_scenario_revision,
        &request.expected_timetable_revision,
    )?;
    validate_page(request.offset, request.limit)?;
    let store = open_query_store(database)?;
    class_schedule_application::query_scenario_timetable_entities(
        &store,
        identity.scenario_id,
        identity.scenario_revision,
        identity.timetable_revision,
        request.view.into(),
        request.offset,
        request.limit,
    )
    .map(Into::into)
    .map_err(|error| query_error(&error))
}

fn query_timetable_inner(
    request: &ScenarioTimetableRequest,
    database: &Path,
) -> Result<ScenarioTimetableResponse, CommandError> {
    let identity = decode_identity(
        request.schema_version,
        &request.scenario_id,
        &request.expected_scenario_revision,
        &request.expected_timetable_revision,
    )?;
    validate_page(request.offset, request.limit)?;
    let query = TimetableQuery {
        filter: decode_filter(request.view, &request.entity_id)?,
        offset: request.offset,
        limit: request.limit,
    };
    let store = open_query_store(database)?;
    class_schedule_application::query_scenario_timetable(
        &store,
        identity.scenario_id,
        identity.scenario_revision,
        identity.timetable_revision,
        &query,
    )
    .map(Into::into)
    .map_err(|error| query_error(&error))
}

pub(super) fn open_query_store(database: &Path) -> Result<SqliteStore, CommandError> {
    let metadata = std::fs::symlink_metadata(database).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CommandError::new(
                "DESKTOP_DATABASE_NOT_FOUND",
                "尚未找到本机项目库，请先导入并保存项目。",
            )
        } else {
            CommandError::new("DESKTOP_DATABASE_UNAVAILABLE", "无法读取本机项目库。")
        }
    })?;
    if !metadata.is_file() {
        return Err(CommandError::new(
            "DESKTOP_DATABASE_PATH_INVALID",
            "本机项目库不是普通文件，无法读取方案课表。",
        ));
    }
    SqliteStore::open_readonly(database).map_err(|error| {
        let code = error.code();
        let message = match code {
            "PERSISTENCE_SCHEMA_UPGRADE_REQUIRED" => {
                "本机项目库需要升级，请先使用兼容版本打开项目。"
            }
            "PERSISTENCE_UNSUPPORTED_SCHEMA" => {
                "本机项目库版本不受支持，请使用与该项目库兼容的应用版本。"
            }
            _ => "无法以只读方式打开本机项目库。",
        };
        CommandError::new(code, message)
    })
}

#[tauri::command]
pub async fn query_scenario_timetable_entities(
    app: tauri::AppHandle,
    request: ScenarioTimetableEntitiesRequest,
) -> Result<ScenarioTimetableEntitiesResponse, CommandError> {
    let path = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || query_entities_inner(&request, &path))
        .await
        .map_err(|_| task_error())?
}

#[tauri::command]
pub async fn query_scenario_timetable(
    app: tauri::AppHandle,
    request: ScenarioTimetableRequest,
) -> Result<ScenarioTimetableResponse, CommandError> {
    let path = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || query_timetable_inner(&request, &path))
        .await
        .map_err(|_| task_error())?
}

fn query_error(error: &ScenarioTimetableQueryError) -> CommandError {
    let code = error.code();
    let message = match code {
        "APPLICATION_SCENARIO_REVISION_CONFLICT" | "APPLICATION_TIMETABLE_REVISION_CONFLICT" => {
            "方案或课表版本已变化，请重新打开方案后再查询。"
        }
        "APPLICATION_TIMETABLE_ENTITY_NOT_FOUND" => "所选对象不在此方案中，请重新选择。",
        "APPLICATION_TIMETABLE_INVALID_PAGE" => "课表分页范围无效，请重新选择视图。",
        "APPLICATION_TIMETABLE_RESOURCE_LIMIT" => "此方案课表超过当前视图的资源上限。",
        code if code.ends_with("NOT_FOUND") => "没有找到此方案或其来源记录，请刷新方案列表。",
        _ => "方案课表重载或独立复核未通过，不能展示为可用课表。",
    };
    CommandError::new(code, message)
}

fn task_error() -> CommandError {
    CommandError::new(
        "DESKTOP_SCENARIO_TIMETABLE_QUERY_FAILED",
        "本机方案课表查询未完成，请重新打开方案。",
    )
}
