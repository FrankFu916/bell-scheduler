//! Thin desktop transport for explicit adoption, independent copies, and revalidated loads.

use class_schedule_application::{
    AdoptRunCommand, CloneScenarioCommand, LoadedScenario, ScenarioApplicationError,
    ScenarioReceipt, commit_prepared_scenario_creation, list_project_scenarios, load_scenario,
    prepare_adopt_run, prepare_clone_scenario,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::import_commit::{database_path, open_store, parse_revision, validate_schema};
use crate::timetable_queries::{TimetableMetricDto, TimetableQualityTierDto};
use crate::{COMMAND_SCHEMA_VERSION, CommandError};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdoptScenarioRequest {
    pub schema_version: u32,
    pub project_id: String,
    pub expected_source_revision: String,
    pub run_id: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopyScenarioRequest {
    pub schema_version: u32,
    pub project_id: String,
    pub expected_source_revision: String,
    pub parent_scenario_id: String,
    pub expected_scenario_revision: String,
    pub expected_timetable_revision: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadScenarioRequest {
    pub schema_version: u32,
    pub scenario_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListScenariosRequest {
    pub schema_version: u32,
    pub project_id: String,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioReceiptDto {
    pub schema_version: u32,
    pub project_id: String,
    pub source_project_revision: String,
    pub source_payload_hash: String,
    pub scenario_id: String,
    pub scenario_revision: String,
    pub scenario_payload_hash: String,
    pub timetable_id: String,
    pub timetable_revision: String,
    pub timetable_payload_hash: String,
    pub origin_run_id: String,
    pub origin_artifact_hash: String,
    pub created_at: String,
}

impl From<&ScenarioReceipt> for ScenarioReceiptDto {
    fn from(receipt: &ScenarioReceipt) -> Self {
        Self {
            schema_version: COMMAND_SCHEMA_VERSION,
            project_id: receipt.project_id.to_string(),
            source_project_revision: receipt.source_project_revision.to_string(),
            source_payload_hash: receipt.source_payload_hash.clone(),
            scenario_id: receipt.scenario_id.to_string(),
            scenario_revision: receipt.scenario_revision.to_string(),
            scenario_payload_hash: receipt.scenario_payload_hash.clone(),
            timetable_id: receipt.timetable_id.to_string(),
            timetable_revision: receipt.timetable_revision.to_string(),
            timetable_payload_hash: receipt.timetable_payload_hash.clone(),
            origin_run_id: receipt.origin_run_id.to_string(),
            origin_artifact_hash: receipt.origin_artifact_hash.clone(),
            created_at: receipt.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioLineageDto {
    pub scenario_id: String,
    pub scenario_revision: String,
    pub timetable_id: String,
    pub timetable_revision: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedScenarioDto {
    pub schema_version: u32,
    pub receipt: ScenarioReceiptDto,
    pub display_name: String,
    pub source_is_current: bool,
    pub activity_count: u32,
    pub materialized_section_count: u32,
    pub materialized_enrollment_count: u32,
    pub quality: Vec<TimetableQualityTierDto>,
    pub cloned_from: Option<ScenarioLineageDto>,
}

fn loaded_dto(loaded: &LoadedScenario) -> Result<LoadedScenarioDto, CommandError> {
    let count = |value| {
        u32::try_from(value).map_err(|_| {
            CommandError::new(
                "DESKTOP_SCENARIO_RESOURCE_LIMIT",
                "方案统计超过当前桌面显示上限。",
            )
        })
    };
    Ok(LoadedScenarioDto {
        schema_version: COMMAND_SCHEMA_VERSION,
        receipt: loaded.receipt().into(),
        display_name: loaded.display_name().to_owned(),
        source_is_current: loaded.source_is_current(),
        activity_count: count(loaded.assignments().len())?,
        materialized_section_count: count(
            loaded
                .materialized_sectioning()
                .map_or(0, |value| value.sections.len()),
        )?,
        materialized_enrollment_count: count(
            loaded
                .materialized_sectioning()
                .map_or(0, |value| value.enrollments.len()),
        )?,
        quality: loaded
            .quality()
            .tiers
            .iter()
            .map(|tier| TimetableQualityTierDto {
                id: tier.id.clone(),
                priority: tier.priority,
                value: tier.value.to_string(),
                metrics: tier
                    .metrics
                    .iter()
                    .map(|metric| TimetableMetricDto {
                        code: metric.kind.code(),
                        raw_value: metric.raw_value.to_string(),
                        weight_within_tier: metric.weight_within_tier,
                        weighted_value: metric.weighted_value.to_string(),
                    })
                    .collect(),
            })
            .collect(),
        cloned_from: loaded.lineage().map(|lineage| ScenarioLineageDto {
            scenario_id: lineage.scenario_id.to_string(),
            scenario_revision: lineage.scenario_revision.to_string(),
            timetable_id: lineage.timetable_id.to_string(),
            timetable_revision: lineage.timetable_revision.to_string(),
        }),
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioSummaryDto {
    pub scenario_id: String,
    pub openable: bool,
    pub project_id: String,
    pub display_name: String,
    pub scenario_revision: String,
    pub timetable_id: String,
    pub timetable_revision: String,
    pub source_project_revision: String,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioListDto {
    pub schema_version: u32,
    pub project_id: String,
    pub scenarios: Vec<ScenarioSummaryDto>,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

#[tauri::command]
pub async fn adopt_run_as_scenario(
    app: tauri::AppHandle,
    request: AdoptScenarioRequest,
) -> Result<ScenarioReceiptDto, CommandError> {
    let command = decode_adopt(request)?;
    let path = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut store = open_store(&path)?;
        let prepared =
            prepare_adopt_run(&store, &command).map_err(|error| scenario_error(&error))?;
        let receipt = commit_prepared_scenario_creation(&mut store, prepared)
            .map_err(|error| scenario_error(&error))?;
        Ok((&receipt).into())
    })
    .await
    .map_err(|_| task_error())?
}

#[tauri::command]
pub async fn copy_saved_scenario(
    app: tauri::AppHandle,
    request: CopyScenarioRequest,
) -> Result<ScenarioReceiptDto, CommandError> {
    let command = decode_copy(request)?;
    let path = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut store = open_store(&path)?;
        let prepared =
            prepare_clone_scenario(&store, &command).map_err(|error| scenario_error(&error))?;
        let receipt = commit_prepared_scenario_creation(&mut store, prepared)
            .map_err(|error| scenario_error(&error))?;
        Ok((&receipt).into())
    })
    .await
    .map_err(|_| task_error())?
}

#[tauri::command]
pub async fn load_saved_scenario(
    app: tauri::AppHandle,
    request: LoadScenarioRequest,
) -> Result<LoadedScenarioDto, CommandError> {
    validate_schema(request.schema_version)?;
    let id = canonical_uuid(&request.scenario_id)?.into();
    let path = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let store = open_store(&path)?;
        let loaded = load_scenario(&store, id).map_err(|error| scenario_error(&error))?;
        loaded_dto(&loaded)
    })
    .await
    .map_err(|_| task_error())?
}

#[tauri::command]
pub async fn list_saved_scenarios(
    app: tauri::AppHandle,
    request: ListScenariosRequest,
) -> Result<ScenarioListDto, CommandError> {
    validate_schema(request.schema_version)?;
    let project_id = canonical_uuid(&request.project_id)?.into();
    let path = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let store = open_store(&path)?;
        let page = list_project_scenarios(&store, project_id, request.limit, request.offset)
            .map_err(|error| scenario_error(&error))?;
        Ok(ScenarioListDto {
            schema_version: COMMAND_SCHEMA_VERSION,
            project_id: request.project_id,
            scenarios: page
                .scenarios
                .into_iter()
                .map(|entry| ScenarioSummaryDto {
                    openable: entry.openable_scenario_id.is_some(),
                    scenario_id: entry.metadata.scenario_id,
                    project_id: entry.metadata.project_id,
                    display_name: entry.metadata.display_name,
                    scenario_revision: entry.metadata.scenario_revision.to_string(),
                    timetable_id: entry.metadata.timetable_id,
                    timetable_revision: entry.metadata.timetable_revision.to_string(),
                    source_project_revision: entry.metadata.source_project_revision.to_string(),
                    created_at: entry.metadata.created_at,
                })
                .collect(),
            has_more: page.has_more,
            next_offset: page.next_offset,
        })
    })
    .await
    .map_err(|_| task_error())?
}

fn decode_adopt(request: AdoptScenarioRequest) -> Result<AdoptRunCommand, CommandError> {
    validate_schema(request.schema_version)?;
    Ok(AdoptRunCommand {
        project_id: canonical_uuid(&request.project_id)?.into(),
        expected_source_revision: parse_revision(&request.expected_source_revision)?,
        run_id: canonical_uuid(&request.run_id)?.into(),
        scenario_id: Uuid::new_v4().into(),
        display_name: request.display_name,
    })
}

fn decode_copy(request: CopyScenarioRequest) -> Result<CloneScenarioCommand, CommandError> {
    validate_schema(request.schema_version)?;
    Ok(CloneScenarioCommand {
        project_id: canonical_uuid(&request.project_id)?.into(),
        expected_source_revision: parse_revision(&request.expected_source_revision)?,
        parent_scenario_id: canonical_uuid(&request.parent_scenario_id)?.into(),
        expected_scenario_revision: parse_revision(&request.expected_scenario_revision)?,
        expected_timetable_revision: parse_revision(&request.expected_timetable_revision)?,
        scenario_id: Uuid::new_v4().into(),
        display_name: request.display_name,
    })
}

fn canonical_uuid(value: &str) -> Result<Uuid, CommandError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|id| id.to_string() == value)
        .ok_or_else(|| {
            CommandError::new(
                "DESKTOP_SCENARIO_INVALID_ID",
                "请选择有效的项目、运行或方案。",
            )
        })
}

fn scenario_error(error: &ScenarioApplicationError) -> CommandError {
    let code = error.code();
    let message = if code.contains("REVISION_CONFLICT") || code.contains("HASH_MISMATCH") {
        "来源或方案已变化，请重新打开项目与方案后再操作。"
    } else if code == "APPLICATION_SCENARIO_RUN_NOT_ADOPTABLE" {
        "此运行没有独立复核通过的可用课表，不能采用。"
    } else if code == "APPLICATION_SCENARIO_INVALID_NAME" {
        "请输入 1–200 字的方案名称，首尾不要留空格。"
    } else if code.ends_with("NOT_FOUND") {
        "没有找到此方案或来源运行，请刷新列表。"
    } else if code.contains("LIST_INVALID") {
        "方案分页范围无效，请刷新列表。"
    } else {
        "方案操作未通过保存或独立复核，请保留错误代码并重新打开来源。"
    };
    CommandError::new(code, message)
}

fn task_error() -> CommandError {
    CommandError::new(
        "DESKTOP_SCENARIO_TASK_FAILED",
        "本机方案操作未确认完成，请先刷新列表核对保存结果。",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        AdoptScenarioRequest, CopyScenarioRequest, decode_adopt, decode_copy, scenario_error,
    };
    use class_schedule_application::ScenarioApplicationError;

    #[test]
    fn scenario_transport_preserves_three_exact_revisions_and_allocates_independent_ids() {
        let input = r#"{"schemaVersion":1,"projectId":"11111111-1111-4111-8111-111111111111","expectedSourceRevision":"9007199254740993","parentScenarioId":"22222222-2222-4222-8222-222222222222","expectedScenarioRevision":"0","expectedTimetableRevision":"9007199254740995","displayName":"独立副本"}"#;
        let a = decode_copy(serde_json::from_str::<CopyScenarioRequest>(input).unwrap()).unwrap();
        let b = decode_copy(serde_json::from_str::<CopyScenarioRequest>(input).unwrap()).unwrap();
        assert_eq!(a.expected_source_revision, 9_007_199_254_740_993);
        assert_eq!(a.expected_scenario_revision, 0);
        assert_eq!(a.expected_timetable_revision, 9_007_199_254_740_995);
        assert_ne!(a.scenario_id, a.parent_scenario_id);
        assert_ne!(a.scenario_id, b.scenario_id);
    }

    #[test]
    fn scenario_transport_rejects_assignment_path_injection_numeric_revision_and_bad_identity() {
        let input = r#"{"schemaVersion":1,"projectId":"11111111-1111-4111-8111-111111111111","expectedSourceRevision":"0","runId":"22222222-2222-4222-8222-222222222222","displayName":"正式方案"}"#;
        assert!(decode_adopt(serde_json::from_str::<AdoptScenarioRequest>(input).unwrap()).is_ok());
        for extra in ["assignments", "workerPath", "databasePath", "scenarioId"] {
            let injected = input.replace(
                "\"schemaVersion\"",
                &format!("\"{extra}\":\"untrusted\",\"schemaVersion\""),
            );
            assert!(serde_json::from_str::<AdoptScenarioRequest>(&injected).is_err());
        }
        assert!(
            serde_json::from_str::<AdoptScenarioRequest>(&input.replace("\"0\"", "0")).is_err()
        );
        for revision in ["00", "-1", "9223372036854775808"] {
            let value = input.replace("\"0\"", &format!("\"{revision}\""));
            assert!(
                decode_adopt(serde_json::from_str::<AdoptScenarioRequest>(&value).unwrap())
                    .is_err()
            );
        }
        let bad_id = input.replace("22222222-2222-4222-8222-222222222222", "invalid");
        assert!(
            decode_adopt(serde_json::from_str::<AdoptScenarioRequest>(&bad_id).unwrap()).is_err()
        );
    }

    #[test]
    fn scenario_errors_keep_stable_codes_with_readable_private_messages() {
        let error = scenario_error(&ScenarioApplicationError::Invalid {
            code: "APPLICATION_SCENARIO_RUN_NOT_ADOPTABLE",
        });
        assert_eq!(error.code, "APPLICATION_SCENARIO_RUN_NOT_ADOPTABLE");
        assert!(error.message.contains("不能采用"));
        assert!(!error.message.contains("{}"));
    }
}
