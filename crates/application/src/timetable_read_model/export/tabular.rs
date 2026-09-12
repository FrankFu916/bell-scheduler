use std::io::Write;

use super::{BoundedBuffer, ExportData, ScenarioTimetableExportError, TextBudget};
use crate::{TimetableAudience, TimetableFilter, TimetableRow};

const CSV_HEADERS: [&str; 28] = [
    "project_id",
    "source_project_revision",
    "source_payload_hash",
    "scenario_id",
    "scenario_revision",
    "scenario_payload_hash",
    "timetable_id",
    "timetable_revision",
    "timetable_payload_hash",
    "source_is_current",
    "view",
    "entity_id",
    "entity_code",
    "entity_name",
    "activity_id",
    "day",
    "period",
    "duration_periods",
    "course_plan_code",
    "course_plan_name",
    "audience_kind",
    "audience_code",
    "audience_name",
    "teacher_code",
    "teacher_name",
    "room_code",
    "room_name",
    "student_count",
];

pub(super) const DETAIL_HEADERS: [&str; 14] = [
    "星期",
    "开始课节",
    "课时数",
    "课程代码",
    "课程",
    "对象类型",
    "班级代码",
    "班级",
    "教师代码",
    "教师",
    "教室代码",
    "教室",
    "人数",
    "课次 ID",
];

pub(super) fn render_csv(data: &ExportData<'_>) -> Result<Vec<u8>, ScenarioTimetableExportError> {
    let mut buffer = BoundedBuffer::default();
    buffer
        .write_all(&[0xef, 0xbb, 0xbf])
        .map_err(csv::Error::from)?;
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_writer(buffer);
    writer.write_record(CSV_HEADERS)?;
    let mut budget = TextBudget::default();
    for row in data.rows {
        let mut fields = provenance(data);
        let (kind, code, name) = audience(row);
        fields.extend([
            row.activity_id.to_string(),
            row.day_label.clone(),
            row.period_label.clone(),
            row.duration_periods.to_string(),
            row.course_plan.code.clone(),
            row.course_plan.label.clone(),
            kind.to_owned(),
            code.to_owned(),
            name.to_owned(),
            row.teacher.code.clone(),
            row.teacher.label.clone(),
            row.room.code.clone(),
            row.room.label.clone(),
            row.student_count.to_string(),
        ]);
        let fields = fields.into_iter().map(spreadsheet_safe).collect::<Vec<_>>();
        for field in &fields {
            budget.check(field)?;
        }
        writer.write_record(fields)?;
    }
    let buffer = writer
        .into_inner()
        .map_err(|error| csv::Error::from(error.into_error()))?;
    Ok(buffer.cursor.into_inner())
}

fn provenance(data: &ExportData<'_>) -> Vec<String> {
    let metadata = data.metadata;
    let receipt = &metadata.receipt;
    let (view, id) = filter_identity(metadata.selection.filter);
    vec![
        receipt.project_id.to_string(),
        receipt.source_project_revision.to_string(),
        receipt.source_payload_hash.clone(),
        receipt.scenario_id.to_string(),
        receipt.scenario_revision.to_string(),
        receipt.scenario_payload_hash.clone(),
        receipt.timetable_id.to_string(),
        receipt.timetable_revision.to_string(),
        receipt.timetable_payload_hash.clone(),
        metadata.source_is_current.to_string(),
        view.to_owned(),
        id,
        metadata.selection.code.clone(),
        metadata.selection.label.clone(),
    ]
}

pub(super) fn filter_identity(filter: TimetableFilter) -> (&'static str, String) {
    match filter {
        TimetableFilter::AdministrativeClass(id) => ("administrative_class", id.to_string()),
        TimetableFilter::TeachingSection(id) => ("teaching_section", id.to_string()),
        TimetableFilter::Teacher(id) => ("teacher", id.to_string()),
        TimetableFilter::Room(id) => ("room", id.to_string()),
        TimetableFilter::Student(id) => ("student", id.to_string()),
        TimetableFilter::Subject(id) => ("subject", id.to_string()),
        TimetableFilter::Grade(id) => ("grade", id.to_string()),
    }
}

pub(super) fn audience(row: &TimetableRow) -> (&'static str, &str, &str) {
    match &row.audience {
        TimetableAudience::AdministrativeClass(label) => {
            ("administrative_class", &label.code, &label.label)
        }
        TimetableAudience::TeachingSection(label) => {
            ("teaching_section", &label.code, &label.label)
        }
    }
}

pub(super) fn details(row: &TimetableRow) -> [String; 14] {
    let (kind, code, name) = audience(row);
    [
        row.day_label.clone(),
        row.period_label.clone(),
        row.duration_periods.to_string(),
        row.course_plan.code.clone(),
        row.course_plan.label.clone(),
        if kind == "administrative_class" {
            "行政班"
        } else {
            "教学班"
        }
        .to_owned(),
        code.to_owned(),
        name.to_owned(),
        row.teacher.code.clone(),
        row.teacher.label.clone(),
        row.room.code.clone(),
        row.room.label.clone(),
        row.student_count.to_string(),
        row.activity_id.to_string(),
    ]
}

fn spreadsheet_safe(value: String) -> String {
    let significant = value.trim_start_matches(char::is_whitespace);
    if significant.starts_with(['=', '+', '-', '@']) || value.starts_with(['\t', '\r', '\n']) {
        format!("'{value}")
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::spreadsheet_safe;

    #[test]
    fn csv_formula_prefixes_are_data_and_ordinary_text_is_unchanged() {
        for value in [
            "=1+1", "+SUM(A1)", "-1+1", "@SUM(1)", " \t=1", "\ttext", "\rtext", "\ntext",
        ] {
            assert_eq!(spreadsheet_safe(value.to_owned()), format!("'{value}"));
        }
        for value in ["0012", "中文,\"名称\"", "Student one", "'literal"] {
            assert_eq!(spreadsheet_safe(value.to_owned()), value);
        }
    }
}
