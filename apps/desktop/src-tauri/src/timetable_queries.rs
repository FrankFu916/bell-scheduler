//! Thin, versioned desktop DTOs over independently revalidated application timetable queries.

use std::fmt::Display;

use class_schedule_application::{
    MAXIMUM_TIMETABLE_PAGE_SIZE, SavedTimetablePage, TimetableAudience, TimetableEntityOption,
    TimetableFilter, TimetableLabel, TimetableQuery, TimetableQueryError, TimetableRow,
    TimetableView,
};
use class_schedule_domain::Day;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::import_commit::{database_path, open_store, validate_schema};
use crate::{COMMAND_SCHEMA_VERSION, CommandError};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimetableViewDto {
    AdministrativeClass,
    TeachingSection,
    Teacher,
    Room,
    Student,
    Subject,
    Grade,
}

impl From<TimetableViewDto> for TimetableView {
    fn from(view: TimetableViewDto) -> Self {
        match view {
            TimetableViewDto::AdministrativeClass => Self::AdministrativeClass,
            TimetableViewDto::TeachingSection => Self::TeachingSection,
            TimetableViewDto::Teacher => Self::Teacher,
            TimetableViewDto::Room => Self::Room,
            TimetableViewDto::Student => Self::Student,
            TimetableViewDto::Subject => Self::Subject,
            TimetableViewDto::Grade => Self::Grade,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimetableEntitiesRequest {
    pub schema_version: u32,
    pub run_id: String,
    pub view: TimetableViewDto,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedTimetableRequest {
    pub schema_version: u32,
    pub run_id: String,
    pub view: TimetableViewDto,
    pub entity_id: String,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimetableEntityDto {
    pub id: String,
    pub code: String,
    pub label: String,
}

impl<Id: Display> From<TimetableLabel<Id>> for TimetableEntityDto {
    fn from(value: TimetableLabel<Id>) -> Self {
        Self {
            id: value.id.to_string(),
            code: value.code,
            label: value.label,
        }
    }
}

impl From<TimetableEntityOption> for TimetableEntityDto {
    fn from(value: TimetableEntityOption) -> Self {
        let id = match value.filter {
            TimetableFilter::AdministrativeClass(id) => id.to_string(),
            TimetableFilter::TeachingSection(id) => id.to_string(),
            TimetableFilter::Teacher(id) => id.to_string(),
            TimetableFilter::Room(id) => id.to_string(),
            TimetableFilter::Student(id) => id.to_string(),
            TimetableFilter::Subject(id) => id.to_string(),
            TimetableFilter::Grade(id) => id.to_string(),
        };
        Self {
            id,
            code: value.code,
            label: value.label,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimetableEntitiesResponse {
    pub schema_version: u32,
    pub run_id: String,
    pub view: TimetableViewDto,
    pub entities: Vec<TimetableEntityDto>,
    pub total_entities: u32,
    pub offset: u32,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimetableAudienceDto {
    pub kind: &'static str,
    pub entity: TimetableEntityDto,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimetableRowDto {
    pub activity_id: String,
    pub activity_index: u32,
    pub course_offering_id: String,
    pub meeting_ordinal: u16,
    pub start_timeslot_index: u32,
    pub day: &'static str,
    pub day_label: String,
    pub period_index: u16,
    pub period_label: String,
    pub duration_periods: u8,
    pub occupied_timeslot_indices: Vec<u32>,
    pub grade: TimetableEntityDto,
    pub subject: TimetableEntityDto,
    pub course_plan: TimetableEntityDto,
    pub audience: TimetableAudienceDto,
    pub teacher: TimetableEntityDto,
    pub room: TimetableEntityDto,
    pub student_count: u32,
}

impl From<TimetableRow> for TimetableRowDto {
    fn from(row: TimetableRow) -> Self {
        let audience = match row.audience {
            TimetableAudience::AdministrativeClass(entity) => TimetableAudienceDto {
                kind: "administrative_class",
                entity: entity.into(),
            },
            TimetableAudience::TeachingSection(entity) => TimetableAudienceDto {
                kind: "teaching_section",
                entity: entity.into(),
            },
        };
        Self {
            activity_id: row.activity_id.to_string(),
            activity_index: row.activity_index,
            course_offering_id: row.course_offering_id.to_string(),
            meeting_ordinal: row.meeting_ordinal,
            start_timeslot_index: row.start_timeslot_index,
            day: day_code(row.day),
            day_label: row.day_label,
            period_index: row.period_index,
            period_label: row.period_label,
            duration_periods: row.duration_periods,
            occupied_timeslot_indices: row.occupied_timeslot_indices,
            grade: row.grade.into(),
            subject: row.subject.into(),
            course_plan: row.course_plan.into(),
            audience,
            teacher: row.teacher.into(),
            room: row.room.into(),
            student_count: row.student_count,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimetableGridCellDto {
    pub timeslot_index: u32,
    pub day: &'static str,
    pub day_label: String,
    pub period_index: u16,
    pub period_label: String,
    pub instructional_block: u8,
    pub occupied_count: u32,
    pub page_activity_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimetableMetricDto {
    pub code: &'static str,
    pub raw_value: String,
    pub weight_within_tier: u32,
    pub weighted_value: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimetableQualityTierDto {
    pub id: String,
    pub priority: u32,
    pub value: String,
    pub metrics: Vec<TimetableMetricDto>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedTimetableResponse {
    pub schema_version: u32,
    pub run_id: String,
    pub project_id: String,
    pub project_revision: String,
    pub project_display_name: String,
    pub source_payload_hash: String,
    pub artifact_payload_hash: String,
    pub input_snapshot_hash: String,
    pub output_hash: String,
    pub adopted: bool,
    pub selected_attempt_index: Option<u32>,
    pub selection: TimetableEntityDto,
    pub rows: Vec<TimetableRowDto>,
    pub calendar: Vec<TimetableGridCellDto>,
    pub total_rows: u32,
    pub offset: u32,
    pub has_more: bool,
    pub next_offset: Option<u32>,
    pub quality: Vec<TimetableQualityTierDto>,
}

impl From<SavedTimetablePage> for SavedTimetableResponse {
    fn from(page: SavedTimetablePage) -> Self {
        Self {
            schema_version: page.schema_version,
            run_id: page.run_id,
            project_id: page.project_id.to_string(),
            project_revision: page.project_revision,
            project_display_name: page.project_display_name,
            source_payload_hash: page.source_payload_hash,
            artifact_payload_hash: page.artifact_payload_hash,
            input_snapshot_hash: page.input_snapshot_hash,
            output_hash: page.output_hash,
            adopted: page.adopted,
            selected_attempt_index: page.selected_attempt_index,
            selection: page.selection.into(),
            rows: page.rows.into_iter().map(Into::into).collect(),
            calendar: page
                .calendar
                .into_iter()
                .map(|cell| TimetableGridCellDto {
                    timeslot_index: cell.timeslot_index,
                    day: day_code(cell.day),
                    day_label: cell.day_label,
                    period_index: cell.period_index,
                    period_label: cell.period_label,
                    instructional_block: cell.instructional_block,
                    occupied_count: cell.occupied_count,
                    page_activity_ids: cell
                        .page_activity_ids
                        .into_iter()
                        .map(|id| id.to_string())
                        .collect(),
                })
                .collect(),
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

#[tauri::command]
pub async fn query_saved_timetable_entities(
    app: tauri::AppHandle,
    request: TimetableEntitiesRequest,
) -> Result<TimetableEntitiesResponse, CommandError> {
    validate_request(
        request.schema_version,
        &request.run_id,
        request.offset,
        request.limit,
    )?;
    let path = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let store = open_store(&path)?;
        let page = class_schedule_application::query_saved_timetable_entities(
            &store,
            &request.run_id,
            request.view.into(),
            request.offset,
            request.limit,
        )
        .map_err(query_error)?;
        Ok(TimetableEntitiesResponse {
            schema_version: COMMAND_SCHEMA_VERSION,
            run_id: page.run_id,
            view: request.view,
            entities: page.entities.into_iter().map(Into::into).collect(),
            total_entities: page.total_entities,
            offset: page.offset,
            has_more: page.has_more,
            next_offset: page.next_offset,
        })
    })
    .await
    .map_err(|_| task_error())?
}

#[tauri::command]
pub async fn query_saved_timetable(
    app: tauri::AppHandle,
    request: SavedTimetableRequest,
) -> Result<SavedTimetableResponse, CommandError> {
    let query = decode_query(&request)?;
    let path = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let store = open_store(&path)?;
        class_schedule_application::query_saved_timetable(&store, &request.run_id, &query)
            .map(Into::into)
            .map_err(query_error)
    })
    .await
    .map_err(|_| task_error())?
}

fn validate_request(
    schema: u32,
    run_id: &str,
    offset: u32,
    limit: u32,
) -> Result<(), CommandError> {
    validate_schema(schema)?;
    canonical_uuid(run_id).map_err(|()| {
        CommandError::new(
            "DESKTOP_TIMETABLE_RUN_ID_INVALID",
            "请选择有效的已保存运行。",
        )
    })?;
    if !(1..=MAXIMUM_TIMETABLE_PAGE_SIZE).contains(&limit) || offset.checked_add(limit).is_none() {
        return Err(CommandError::new(
            "APPLICATION_TIMETABLE_INVALID_PAGE",
            "课表分页范围无效，请重新选择视图。",
        ));
    }
    Ok(())
}

fn decode_query(request: &SavedTimetableRequest) -> Result<TimetableQuery, CommandError> {
    validate_request(
        request.schema_version,
        &request.run_id,
        request.offset,
        request.limit,
    )?;
    let id = canonical_uuid(&request.entity_id).map_err(|()| {
        CommandError::new(
            "DESKTOP_TIMETABLE_ENTITY_ID_INVALID",
            "请先选择该视图中的班级、教师、教室、学生、学科或年级。",
        )
    })?;
    let filter = match request.view {
        TimetableViewDto::AdministrativeClass => TimetableFilter::AdministrativeClass(id.into()),
        TimetableViewDto::TeachingSection => TimetableFilter::TeachingSection(id.into()),
        TimetableViewDto::Teacher => TimetableFilter::Teacher(id.into()),
        TimetableViewDto::Room => TimetableFilter::Room(id.into()),
        TimetableViewDto::Student => TimetableFilter::Student(id.into()),
        TimetableViewDto::Subject => TimetableFilter::Subject(id.into()),
        TimetableViewDto::Grade => TimetableFilter::Grade(id.into()),
    };
    Ok(TimetableQuery {
        filter,
        offset: request.offset,
        limit: request.limit,
    })
}

fn canonical_uuid(value: &str) -> Result<Uuid, ()> {
    let id = Uuid::parse_str(value).map_err(|_| ())?;
    if id.to_string() != value {
        return Err(());
    }
    Ok(id)
}

fn query_error(error: TimetableQueryError) -> CommandError {
    if let TimetableQueryError::Artifact(error) = error {
        return crate::solve_commands::artifact_error(error);
    }
    let message = match error {
        TimetableQueryError::EntityNotFound { .. } => "所选对象不在此运行中，请重新选择。",
        TimetableQueryError::Unavailable => "此运行没有独立校验通过的已选课表，不能打开课表视图。",
        TimetableQueryError::InvalidPage => "课表分页范围无效，请重新选择视图。",
        TimetableQueryError::ResourceLimit => "此课表超过当前视图的资源上限。",
        _ => "课表重载校验未通过，不能展示为可用课表。",
    };
    CommandError::new(error.code(), message)
}

fn task_error() -> CommandError {
    CommandError::new(
        "DESKTOP_TIMETABLE_QUERY_FAILED",
        "本机课表查询未完成，请重新打开已保存运行。",
    )
}

const fn day_code(day: Day) -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::{SavedTimetableRequest, TimetableViewDto, decode_query};
    use class_schedule_application::TimetableFilter;

    #[test]
    fn timetable_transport_requires_explicit_canonical_selection_and_bounded_page() {
        let mut request = SavedTimetableRequest {
            schema_version: 1,
            run_id: "11111111-1111-4111-8111-111111111111".to_owned(),
            view: TimetableViewDto::Student,
            entity_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            offset: 0,
            limit: 20,
        };
        assert!(matches!(
            decode_query(&request).unwrap().filter,
            TimetableFilter::Student(_)
        ));
        request.entity_id.clear();
        assert_eq!(
            decode_query(&request).unwrap_err().code,
            "DESKTOP_TIMETABLE_ENTITY_ID_INVALID"
        );
        request.limit = 0;
        assert_eq!(
            decode_query(&request).unwrap_err().code,
            "APPLICATION_TIMETABLE_INVALID_PAGE"
        );
        request.schema_version = 2;
        assert!(decode_query(&request).is_err());
    }

    #[test]
    fn timetable_transport_rejects_missing_selection_and_untrusted_paths() {
        let missing = r#"{"schemaVersion":1,"runId":"11111111-1111-4111-8111-111111111111","view":"student","offset":0,"limit":20}"#;
        assert!(serde_json::from_str::<SavedTimetableRequest>(missing).is_err());
        let injected = missing.replace("\"offset\"", "\"entityId\":\"22222222-2222-4222-8222-222222222222\",\"databasePath\":\"/private/data\",\"offset\"");
        assert!(serde_json::from_str::<SavedTimetableRequest>(&injected).is_err());
    }
}
