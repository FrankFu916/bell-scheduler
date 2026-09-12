use std::collections::BTreeMap;

use crate::{TimetableGridCell, TimetableRow};
use class_schedule_domain::MeetingDemandId;
use rust_xlsxwriter::{Format, FormatAlign, FormatBorder, Workbook, Worksheet};

use super::{
    BoundedBuffer, ExportData, ScenarioTimetableExportError, TextBudget, resource_limit, tabular,
};

pub(super) fn render(data: &ExportData<'_>) -> Result<Vec<u8>, ScenarioTimetableExportError> {
    let mut workbook = Workbook::new();
    let mut budget = TextBudget::default();
    source_sheet(workbook.add_worksheet(), data, &mut budget)?;
    grid_sheet(workbook.add_worksheet(), data, &mut budget)?;
    details_sheet(workbook.add_worksheet(), data, &mut budget)?;
    let mut buffer = BoundedBuffer::default();
    let result = workbook.save_to_writer(&mut buffer);
    if buffer.exceeded {
        return Err(resource_limit());
    }
    result?;
    Ok(buffer.cursor.into_inner())
}

fn source_sheet(
    sheet: &mut Worksheet,
    data: &ExportData<'_>,
    budget: &mut TextBudget,
) -> Result<(), ScenarioTimetableExportError> {
    sheet
        .set_name("来源说明")?
        .set_column_width(0, 25)?
        .set_column_width(1, 75)?;
    let metadata = data.metadata;
    let receipt = &metadata.receipt;
    let (view, entity_id) = tabular::filter_identity(metadata.selection.filter);
    let rows = [
        ("方案名称", metadata.scenario_display_name.clone()),
        ("查看对象", metadata.selection.label.clone()),
        ("对象代码", metadata.selection.code.clone()),
        ("视图", view.to_owned()),
        ("对象 ID", entity_id),
        ("导出时间（UTC）", metadata.generated_at.to_rfc3339()),
        (
            "来源状态（校验时）",
            if metadata.source_is_current {
                "当前来源"
            } else {
                "历史来源：项目已有更新"
            }
            .to_owned(),
        ),
        ("课次数", metadata.meeting_count.to_string()),
        ("项目 ID", receipt.project_id.to_string()),
        ("来源版本", receipt.source_project_revision.to_string()),
        ("来源内容 BLAKE3", receipt.source_payload_hash.clone()),
        ("方案 ID", receipt.scenario_id.to_string()),
        ("方案版本", receipt.scenario_revision.to_string()),
        ("方案内容 BLAKE3", receipt.scenario_payload_hash.clone()),
        ("课表 ID", receipt.timetable_id.to_string()),
        ("课表版本", receipt.timetable_revision.to_string()),
        ("课表内容 BLAKE3", receipt.timetable_payload_hash.clone()),
        ("来源运行 ID", receipt.origin_run_id.to_string()),
        ("来源运行 BLAKE3", receipt.origin_artifact_hash.clone()),
        (
            "独立校验",
            "已按历史输入、实际选课与此方案课表重新校验".to_owned(),
        ),
        ("导出格式版本", metadata.schema_version.to_string()),
    ];
    let body = Format::new()
        .set_text_wrap()
        .set_align(FormatAlign::VerticalCenter);
    for (index, (key, value)) in (0_u32..).zip(&rows) {
        write_cell(sheet, index, 0, key, &body, budget)?;
        write_cell(sheet, index, 1, value, &body, budget)?;
        sheet.set_row_height(index, height_for(value, 70, 16)?.max(24.0))?;
    }
    Ok(())
}

fn grid_sheet(
    sheet: &mut Worksheet,
    data: &ExportData<'_>,
    budget: &mut TextBudget,
) -> Result<(), ScenarioTimetableExportError> {
    sheet.set_name("周课表")?.set_column_width(0, 15)?;
    sheet
        .set_landscape()
        .set_paper_size(9)
        .set_print_fit_to_pages(1, 0)
        .set_print_center_horizontally(true);
    let title = Format::new().set_bold().set_font_size(16).set_text_wrap();
    let heading = Format::new()
        .set_bold()
        .set_background_color("#E9F0EC")
        .set_border(FormatBorder::Thin);
    let body = Format::new()
        .set_font_size(11)
        .set_text_wrap()
        .set_align(FormatAlign::Top)
        .set_border(FormatBorder::Thin);
    let label = format!(
        "{} · {}",
        data.metadata.scenario_display_name, data.metadata.selection.label
    );
    let version = format!(
        "来源 {} / 方案 {} / 课表 {} · {}",
        data.metadata.receipt.source_project_revision,
        data.metadata.receipt.scenario_revision,
        data.metadata.receipt.timetable_revision,
        if data.metadata.source_is_current {
            "校验时来源为当前版本"
        } else {
            "历史来源，项目已有更新"
        }
    );
    let mut days = Vec::new();
    let mut periods = BTreeMap::new();
    for cell in data.calendar {
        if !days.iter().any(|(day, _)| *day == cell.day) {
            days.push((cell.day, cell.day_label.as_str()));
        }
        periods
            .entry(cell.period_index)
            .or_insert(cell.period_label.as_str());
    }
    let last_column = u16::try_from(days.len()).map_err(|_| resource_limit())?;
    sheet.merge_range(0, 0, 0, last_column, "", &title)?;
    write_cell(sheet, 0, 0, &label, &title, budget)?;
    sheet.set_row_height(0, height_for(&label, 12 + days.len() * 22, 22)?)?;
    let subtitle = Format::new().set_text_wrap();
    sheet.merge_range(1, 0, 1, last_column, "", &subtitle)?;
    write_cell(sheet, 1, 0, &version, &subtitle, budget)?;
    sheet.set_row_height(1, height_for(&version, 12 + days.len() * 22, 16)?)?;
    let rows: BTreeMap<_, _> = data.rows.iter().map(|row| (row.activity_id, row)).collect();
    write_cell(sheet, 3, 0, "课节", &heading, budget)?;
    for (column, (_, label)) in (1_u16..).zip(&days) {
        sheet.set_column_width(column, 27)?;
        write_cell(sheet, 3, column, label, &heading, budget)?;
    }
    let mut row_index = 4;
    for (period, label) in &periods {
        let cells = days
            .iter()
            .map(|(day, _)| {
                data.calendar
                    .iter()
                    .find(|cell| cell.day == *day && cell.period_index == *period)
                    .ok_or_else(resource_limit)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let parallel = cells
            .iter()
            .map(|cell| cell.page_activity_ids.len())
            .max()
            .unwrap_or(0)
            .max(1);
        for position in 0..parallel {
            if row_index > 100_000 {
                return Err(resource_limit());
            }
            write_cell(sheet, row_index, 0, label, &heading, budget)?;
            let mut height = 34_f64;
            for (column, cell) in (1_u16..).zip(&cells) {
                let text = lesson_text(cell, position, &rows)?;
                height = height.max(print_height(&text)?);
                write_cell(sheet, row_index, column, &text, &body, budget)?;
            }
            sheet.set_row_height(row_index, height)?;
            row_index += 1;
        }
    }
    sheet.set_freeze_panes(4, 1)?.set_repeat_rows(0, 3)?;
    sheet.set_print_area(0, 0, row_index - 1, last_column)?;
    Ok(())
}

fn lesson_text(
    cell: &TimetableGridCell,
    position: usize,
    rows: &BTreeMap<MeetingDemandId, &TimetableRow>,
) -> Result<String, ScenarioTimetableExportError> {
    let Some(id) = cell.page_activity_ids.get(position) else {
        return Ok(if position == 0 {
            "无课程".to_owned()
        } else {
            String::new()
        });
    };
    let row = rows.get(id).ok_or_else(resource_limit)?;
    let (_, _, audience) = tabular::audience(row);
    let continuation = if cell.timeslot_index == row.start_timeslot_index {
        ""
    } else {
        " · 续"
    };
    Ok(format!(
        "{}{continuation}\n{audience}\n{} · {}",
        row.course_plan.label, row.teacher.label, row.room.label
    ))
}

fn print_height(text: &str) -> Result<f64, ScenarioTimetableExportError> {
    // At an explicit 11pt font and 27-character column, allow 24 width units after padding.
    // Non-ASCII glyphs conservatively occupy two units. Never silently clip an oversized lesson.
    height_for(text, 24, 16)
}

fn height_for(
    text: &str,
    width: usize,
    line_height: usize,
) -> Result<f64, ScenarioTimetableExportError> {
    let lines: usize = text
        .split('\n')
        .map(|line| {
            line.chars()
                .map(|character| if character.is_ascii() { 1_usize } else { 2 })
                .sum::<usize>()
                .div_ceil(width)
                .max(1)
        })
        .sum();
    let height = lines.saturating_mul(line_height).saturating_add(10);
    if height > 409 {
        return Err(ScenarioTimetableExportError::Invalid {
            code: "APPLICATION_TIMETABLE_EXPORT_PRINT_LAYOUT_LIMIT",
        });
    }
    Ok(f64::from(
        u32::try_from(height).map_err(|_| resource_limit())?,
    ))
}

fn details_sheet(
    sheet: &mut Worksheet,
    data: &ExportData<'_>,
    budget: &mut TextBudget,
) -> Result<(), ScenarioTimetableExportError> {
    sheet.set_name("课程清单")?.set_freeze_panes(1, 0)?;
    let heading = Format::new().set_bold().set_background_color("#E9F0EC");
    let body = Format::new().set_text_wrap();
    for (column, header) in (0_u16..).zip(&tabular::DETAIL_HEADERS) {
        sheet.set_column_width(column, if column == 13 { 38 } else { 18 })?;
        write_cell(sheet, 0, column, header, &heading, budget)?;
    }
    for (index, row) in (1_u32..).zip(data.rows) {
        let mut height = 26_f64;
        for (column, value) in (0_u16..).zip(&tabular::details(row)) {
            write_cell(sheet, index, column, value, &body, budget)?;
            height = height.max(height_for(value, if column == 13 { 35 } else { 16 }, 16)?);
        }
        sheet.set_row_height(index, height)?;
    }
    sheet.autofilter(0, 0, data.metadata.meeting_count, 13)?;
    Ok(())
}

fn write_cell(
    sheet: &mut Worksheet,
    row: u32,
    column: u16,
    text: &str,
    format: &Format,
    budget: &mut TextBudget,
) -> Result<(), ScenarioTimetableExportError> {
    budget.check(text)?;
    sheet.write_string_with_format(row, column, text, format)?;
    Ok(())
}
