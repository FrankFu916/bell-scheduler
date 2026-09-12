//! Native save dialog transport over the shared, frozen scenario export use case.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use class_schedule_application::{
    PreparedScenarioTimetableExport, ScenarioTimetableExportCommand, ScenarioTimetableExportError,
    ScenarioTimetableExportFormat, ScenarioTimetableExportMetadata,
    prepare_scenario_timetable_export, publish_scenario_timetable_export,
};
use serde::{Deserialize, Serialize};
use tauri_plugin_dialog::DialogExt;

use crate::import_commit::database_path;
use crate::scenario_commands::ScenarioReceiptDto;
use crate::scenario_timetable_queries::{decode_identity, open_query_store};
use crate::timetable_queries::{TimetableEntityDto, TimetableViewDto, decode_filter};
use crate::{COMMAND_SCHEMA_VERSION, CommandError};

#[cfg(test)]
mod tests;

#[derive(Debug, Default)]
pub(super) struct ScenarioExportJobs {
    active: Arc<AtomicBool>,
}

impl ScenarioExportJobs {
    fn try_begin(&self) -> Result<ExportPermit, CommandError> {
        self.active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                CommandError::new(
                    "DESKTOP_SCENARIO_EXPORT_BUSY",
                    "已有课表正在导出，请先完成或取消当前保存窗口。",
                )
            })?;
        Ok(ExportPermit(Arc::clone(&self.active)))
    }
}

#[derive(Debug)]
struct ExportPermit(Arc<AtomicBool>);

impl Drop for ExportPermit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScenarioExportFormatDto {
    Csv,
    Xlsx,
}

impl From<ScenarioExportFormatDto> for ScenarioTimetableExportFormat {
    fn from(format: ScenarioExportFormatDto) -> Self {
        match format {
            ScenarioExportFormatDto::Csv => Self::Csv,
            ScenarioExportFormatDto::Xlsx => Self::Xlsx,
        }
    }
}

impl From<ScenarioTimetableExportFormat> for ScenarioExportFormatDto {
    fn from(format: ScenarioTimetableExportFormat) -> Self {
        match format {
            ScenarioTimetableExportFormat::Csv => Self::Csv,
            ScenarioTimetableExportFormat::Xlsx => Self::Xlsx,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportScenarioTimetableRequest {
    pub schema_version: u32,
    pub scenario_id: String,
    pub expected_scenario_revision: String,
    pub expected_timetable_revision: String,
    pub view: TimetableViewDto,
    pub entity_id: String,
    pub format: ScenarioExportFormatDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedScenarioExportDto {
    pub receipt: ScenarioReceiptDto,
    pub source_is_current: bool,
    pub scenario_display_name: String,
    pub view: TimetableViewDto,
    pub selection: TimetableEntityDto,
    pub format: ScenarioExportFormatDto,
    pub exported_activity_count: u32,
    pub file_name: String,
    pub generated_at: String,
    pub byte_length: String,
    pub payload_hash: String,
    pub payload_hash_algorithm: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum ExportScenarioTimetableResponse {
    Cancelled {
        #[serde(rename = "schemaVersion")]
        schema_version: u32,
    },
    Saved {
        #[serde(rename = "schemaVersion")]
        schema_version: u32,
        #[serde(flatten)]
        export: Box<SavedScenarioExportDto>,
    },
}

fn decode_request(
    request: &ExportScenarioTimetableRequest,
) -> Result<ScenarioTimetableExportCommand, CommandError> {
    let identity = decode_identity(
        request.schema_version,
        &request.scenario_id,
        &request.expected_scenario_revision,
        &request.expected_timetable_revision,
    )?;
    Ok(ScenarioTimetableExportCommand {
        scenario_id: identity.scenario_id,
        expected_scenario_revision: identity.scenario_revision,
        expected_timetable_revision: identity.timetable_revision,
        filter: decode_filter(request.view, &request.entity_id)?,
        format: request.format.into(),
    })
}

fn prepare_export(
    request: &ExportScenarioTimetableRequest,
    database: &Path,
) -> Result<PreparedScenarioTimetableExport, CommandError> {
    let command = decode_request(request)?;
    let store = open_query_store(database)?;
    prepare_scenario_timetable_export(&store, &command).map_err(|error| export_error(&error))
}

fn saved_response(
    metadata: &ScenarioTimetableExportMetadata,
    file_name: String,
) -> ExportScenarioTimetableResponse {
    ExportScenarioTimetableResponse::Saved {
        schema_version: metadata.schema_version,
        export: Box::new(SavedScenarioExportDto {
            receipt: (&metadata.receipt).into(),
            source_is_current: metadata.source_is_current,
            scenario_display_name: metadata.scenario_display_name.clone(),
            view: metadata.selection.filter.view().into(),
            selection: metadata.selection.clone().into(),
            format: metadata.format.into(),
            exported_activity_count: metadata.meeting_count,
            file_name,
            generated_at: metadata.generated_at.to_rfc3339(),
            byte_length: metadata.byte_length.to_string(),
            payload_hash: metadata.payload_hash.clone(),
            payload_hash_algorithm: "blake3",
        }),
    }
}

fn export_with_picker(
    request: &ExportScenarioTimetableRequest,
    database: &Path,
    pick: impl FnOnce(&ScenarioTimetableExportMetadata) -> Result<Option<PathBuf>, CommandError>,
) -> Result<ExportScenarioTimetableResponse, CommandError> {
    // The connection is dropped before entering the native modal dialog. Publication uses
    // only this immutable artifact, even if another connection subsequently changes the source.
    let prepared = prepare_export(request, database)?;
    let Some(path) = pick(prepared.metadata())? else {
        return Ok(ExportScenarioTimetableResponse::Cancelled {
            schema_version: COMMAND_SCHEMA_VERSION,
        });
    };
    // Validate the DTO representation before publication so no fallible transport step follows
    // a successful atomic save. The application separately enforces filename/extension rules.
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            CommandError::new(
                "DESKTOP_SCENARIO_EXPORT_PATH_INVALID",
                "所选文件名无法用于导出，请选择有效的新文件名。",
            )
        })?
        .to_owned();
    let response = saved_response(prepared.metadata(), file_name);
    publish_scenario_timetable_export(&prepared, &path).map_err(|error| export_error(&error))?;
    Ok(response)
}

fn suggested_file_name(metadata: &ScenarioTimetableExportMetadata) -> String {
    fn component(value: &str) -> String {
        let value: String = value
            .chars()
            .map(|character| {
                if character.is_control() || "/\\:*?\"<>|".contains(character) {
                    '_'
                } else {
                    character
                }
            })
            .take(20)
            .collect();
        let value = value.trim_matches([' ', '.']);
        if value.is_empty() {
            "课表".to_owned()
        } else {
            value.to_owned()
        }
    }
    let extension = match metadata.format {
        ScenarioTimetableExportFormat::Csv => "csv",
        ScenarioTimetableExportFormat::Xlsx => "xlsx",
    };
    format!(
        "Bell-{}-{}-s{}-t{}.{}",
        component(&metadata.scenario_display_name),
        component(&metadata.selection.label),
        metadata.receipt.scenario_revision,
        metadata.receipt.timetable_revision,
        extension,
    )
}

#[tauri::command]
pub async fn export_scenario_timetable(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    jobs: tauri::State<'_, ScenarioExportJobs>,
    request: ExportScenarioTimetableRequest,
) -> Result<ExportScenarioTimetableResponse, CommandError> {
    let permit = jobs.try_begin()?;
    let database = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        export_with_picker(&request, &database, |metadata| {
            let (label, extension) = match metadata.format {
                ScenarioTimetableExportFormat::Csv => ("CSV 课程清单", "csv"),
                ScenarioTimetableExportFormat::Xlsx => ("Excel 课表", "xlsx"),
            };
            app.dialog()
                .file()
                .set_parent(&window)
                .set_title("导出完整课表（请选择新文件名）")
                .set_file_name(suggested_file_name(metadata))
                .add_filter(label, &[extension])
                .blocking_save_file()
                .map(|path| {
                    path.into_path().map_err(|_| {
                        CommandError::new(
                            "DESKTOP_SCENARIO_EXPORT_PATH_INVALID",
                            "请选择本机有效的保存位置。",
                        )
                    })
                })
                .transpose()
        })
    })
    .await
    .map_err(|_| {
        CommandError::new(
            "DESKTOP_SCENARIO_EXPORT_FAILED",
            "无法确认导出结果，请先检查保存位置。",
        )
    })?
}

fn export_error(error: &ScenarioTimetableExportError) -> CommandError {
    let code = error.code();
    let message = match code {
        "APPLICATION_SCENARIO_REVISION_CONFLICT" | "APPLICATION_TIMETABLE_REVISION_CONFLICT" => {
            "方案或课表版本已变化，请重新打开方案后再导出。"
        }
        "APPLICATION_TIMETABLE_ENTITY_NOT_FOUND" => "所选对象不在此方案中，请重新选择。",
        "APPLICATION_TIMETABLE_EXPORT_PRINT_LAYOUT_LIMIT" => {
            "课程文字超过 Excel 周课表的排版容量，可改用 CSV 导出或缩小所选对象。"
        }
        code if code.contains("ALREADY_EXISTS") || code.ends_with("TARGET_EXISTS") => {
            "所选文件已存在，请选择新文件名。现有文件未被覆盖。"
        }
        code if code.contains("EXTENSION") => "文件扩展名与导出格式不符，请重新选择文件名。",
        code if code.contains("FILE_NAME") || code.contains("FILENAME") => {
            "所选文件名不适合导出，请选择有效的新文件名。"
        }
        code if code.contains("PATH") || code.contains("PARENT") => {
            "保存位置不可用，请选择本机已存在的文件夹和新文件名。"
        }
        code if code.contains("RESOURCE_LIMIT") || code.contains("CELL_LIMIT") => {
            "所选课表超过当前导出的容量限制，请缩小所选对象范围。"
        }
        code if code.ends_with("NOT_FOUND") => "没有找到此方案或其来源记录，请刷新方案列表。",
        _ => "课表复核、生成或保存未完成，请检查方案与保存位置后重新导出。",
    };
    CommandError::new(code, message)
}
