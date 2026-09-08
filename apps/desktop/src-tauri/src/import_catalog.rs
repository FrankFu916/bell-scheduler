//! Presentation of the importer's actual schema; no second set of parsing rules.

use class_schedule_import::{DatasetKind, csv_dataset_schema};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCatalog {
    schema_version: u32,
    datasets: Vec<DatasetTemplate>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DatasetTemplate {
    dataset: &'static str,
    label: &'static str,
    required: bool,
    required_headers: &'static [&'static str],
    optional_headers: &'static [&'static str],
}

#[tauri::command]
pub fn get_import_catalog() -> ImportCatalog {
    ImportCatalog {
        schema_version: super::COMMAND_SCHEMA_VERSION,
        datasets: DatasetKind::ALL
            .into_iter()
            .map(|kind| {
                let schema = csv_dataset_schema(kind);
                DatasetTemplate {
                    dataset: kind.as_str(),
                    label: match kind {
                        DatasetKind::Students => "学生",
                        DatasetKind::AdministrativeClasses => "行政班",
                        DatasetKind::StudentSubjectChoices => "学生选科关系",
                        DatasetKind::Teachers => "教师",
                        DatasetKind::TeacherUnavailability => "教师不可用时间",
                        DatasetKind::Rooms => "教室",
                        DatasetKind::CoursePlans => "课程计划",
                        DatasetKind::TeachingSections => "教学班",
                        DatasetKind::SectionEnrollments => "教学班成员关系",
                        DatasetKind::CourseOfferings => "实际开课资源配置",
                        DatasetKind::FixedActivities => "固定活动",
                    },
                    required: schema.required_dataset,
                    required_headers: schema.required_headers,
                    optional_headers: schema.optional_headers,
                }
            })
            .collect(),
    }
}
