use std::collections::{BTreeMap, BTreeSet};

use class_schedule_import::ImportedAudienceKind;
use class_schedule_scheduling::{Activity, Assignment, DenseBitSet};

use super::{
    AdministrativeClassId, Day, GradeId, MAXIMUM_TIMETABLE_GRID_CELLS, StudentId,
    TimetableAudience, TimetableEntityOption, TimetableFilter, TimetableGridCell, TimetableLabel,
    TimetableProjectionInput, TimetableQuery, TimetableQueryError, TimetableRow, TimetableView,
    count, required,
};
use crate::compile::stable_id;

pub(super) fn entities(
    input: &TimetableProjectionInput<'_>,
    view: TimetableView,
) -> Vec<TimetableEntityOption> {
    let document = input.source;
    let batch = &document.import_batch;
    let key = &document.project_stable_key;
    let mut options: Vec<_> = match view {
        TimetableView::AdministrativeClass => batch
            .administrative_classes()
            .iter()
            .map(|row| {
                option(
                    TimetableFilter::AdministrativeClass(stable_id(
                        key,
                        "administrative_class",
                        &row.administrative_class_code,
                    )),
                    &row.administrative_class_code,
                    &row.name,
                )
            })
            .collect(),
        TimetableView::TeachingSection => input
            .sections
            .iter()
            .map(|row| {
                option(
                    TimetableFilter::TeachingSection(stable_id(
                        key,
                        "teaching_section",
                        &row.section_code,
                    )),
                    &row.section_code,
                    &row.name,
                )
            })
            .collect(),
        TimetableView::Teacher => batch
            .teachers()
            .iter()
            .map(|row| {
                option(
                    TimetableFilter::Teacher(stable_id(key, "teacher", &row.teacher_code)),
                    &row.teacher_code,
                    &row.name,
                )
            })
            .collect(),
        TimetableView::Room => batch
            .rooms()
            .iter()
            .map(|row| {
                option(
                    TimetableFilter::Room(stable_id(key, "room", &row.room_code)),
                    &row.room_code,
                    &row.name,
                )
            })
            .collect(),
        TimetableView::Student => batch
            .students()
            .iter()
            .map(|row| {
                option(
                    TimetableFilter::Student(stable_id(key, "student", &row.student_code)),
                    &row.student_code,
                    &row.name,
                )
            })
            .collect(),
        TimetableView::Subject => batch
            .course_plans()
            .iter()
            .map(|row| row.subject_code.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|code| {
                option(
                    TimetableFilter::Subject(stable_id(key, "subject", code)),
                    code,
                    code,
                )
            })
            .collect(),
        TimetableView::Grade => batch
            .administrative_classes()
            .iter()
            .map(|row| row.grade_code.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|code| {
                option(
                    TimetableFilter::Grade(stable_id(key, "grade", code)),
                    code,
                    code,
                )
            })
            .collect(),
    };
    options.sort_by(|left, right| left.code.cmp(&right.code));
    options
}

fn option(filter: TimetableFilter, code: &str, name: &str) -> TimetableEntityOption {
    TimetableEntityOption {
        filter,
        code: code.to_owned(),
        label: name.to_owned(),
    }
}

fn student_mask(
    input: &TimetableProjectionInput<'_>,
    filter: TimetableFilter,
) -> Result<DenseBitSet, TimetableQueryError> {
    let document = input.source;
    let batch = &document.import_batch;
    let key = &document.project_stable_key;
    let classes: BTreeMap<_, _> = batch
        .administrative_classes()
        .iter()
        .map(|row| (row.administrative_class_code.as_str(), row))
        .collect();
    let codes: BTreeSet<_> = batch
        .students()
        .iter()
        .filter(|student| match filter {
            TimetableFilter::Student(id) => {
                stable_id::<StudentId>(key, "student", &student.student_code) == id
            }
            TimetableFilter::AdministrativeClass(id) => {
                stable_id::<AdministrativeClassId>(
                    key,
                    "administrative_class",
                    &student.administrative_class_code,
                ) == id
            }
            TimetableFilter::Grade(id) => classes
                .get(student.administrative_class_code.as_str())
                .is_some_and(|class| stable_id::<GradeId>(key, "grade", &class.grade_code) == id),
            _ => false,
        })
        .map(|student| student.student_code.as_str())
        .collect();
    DenseBitSet::from_indices(
        input.compiled.problem.students().len(),
        input
            .compiled
            .catalog
            .student_codes
            .iter()
            .enumerate()
            .filter_map(|(index, code)| codes.contains(code.as_str()).then_some(index)),
    )
    .map_err(|_| TimetableQueryError::Inconsistent {
        field: "student_catalog",
    })
}

fn matches_filter(
    input: &TimetableProjectionInput<'_>,
    assignment: &Assignment,
    activity: &Activity,
    filter: TimetableFilter,
    students: &DenseBitSet,
) -> Result<bool, TimetableQueryError> {
    Ok(match filter {
        TimetableFilter::AdministrativeClass(_)
        | TimetableFilter::Student(_)
        | TimetableFilter::Grade(_) => activity.audience.intersects(students),
        TimetableFilter::TeachingSection(id) => activity.teaching_section_id == Some(id),
        TimetableFilter::Subject(id) => activity.subject_id == id,
        TimetableFilter::Teacher(id) => {
            required(
                input
                    .compiled
                    .problem
                    .teachers()
                    .get(assignment.teacher.as_usize()),
                "teacher",
            )?
            .stable_id
                == id
        }
        TimetableFilter::Room(id) => {
            required(
                input
                    .compiled
                    .problem
                    .rooms()
                    .get(assignment.room.as_usize()),
                "room",
            )?
            .stable_id
                == id
        }
    })
}

pub(super) fn project(
    input: &TimetableProjectionInput<'_>,
    query: &TimetableQuery,
) -> Result<(Vec<TimetableRow>, Vec<TimetableGridCell>, u32), TimetableQueryError> {
    let problem = &input.compiled.problem;
    let students = student_mask(input, query.filter)?;
    let mut matching = Vec::new();
    for assignment in input.assignments {
        let activity = required(
            problem.activities().get(assignment.activity.as_usize()),
            "activity",
        )?;
        if matches_filter(input, assignment, activity, query.filter, &students)? {
            matching.push((assignment, activity));
        }
    }
    matching.sort_by_key(|(assignment, activity)| (assignment.start.0, activity.stable_id));
    let total_rows = count(matching.len())?;
    let mut calendar = calendar(input)?;
    let mut rows = Vec::new();
    for (position, (assignment, activity)) in matching.into_iter().enumerate() {
        let occupied = problem
            .occupied_slots(assignment.activity, assignment.start)
            .map_err(|_| TimetableQueryError::Inconsistent { field: "duration" })?;
        let visible =
            position >= query.offset as usize && position < (query.offset + query.limit) as usize;
        for slot in &occupied {
            let cell = required(calendar.get_mut(slot.as_usize()), "calendar")?;
            cell.occupied_count = cell
                .occupied_count
                .checked_add(1)
                .ok_or(TimetableQueryError::ResourceLimit)?;
            if visible {
                cell.page_activity_ids.push(activity.stable_id);
            }
        }
        if visible {
            rows.push(row(
                input,
                assignment,
                activity,
                occupied.into_iter().map(|slot| slot.0).collect(),
            )?);
        }
    }
    Ok((rows, calendar, total_rows))
}

fn calendar(
    input: &TimetableProjectionInput<'_>,
) -> Result<Vec<TimetableGridCell>, TimetableQueryError> {
    let slots = input.compiled.problem.timeslots();
    if slots.len() > MAXIMUM_TIMETABLE_GRID_CELLS {
        return Err(TimetableQueryError::ResourceLimit);
    }
    let periods: BTreeMap<_, _> = input
        .source
        .calendar
        .periods
        .iter()
        .map(|period| (period.index, period))
        .collect();
    slots
        .iter()
        .enumerate()
        .map(|(index, slot)| {
            let period = required(periods.get(&slot.period_index), "period")?;
            Ok(TimetableGridCell {
                timeslot_index: count(index)?,
                day: slot.day,
                day_label: day_label(slot.day).to_owned(),
                period_index: slot.period_index,
                period_label: period.label.clone(),
                instructional_block: slot.instructional_block,
                occupied_count: 0,
                page_activity_ids: Vec::new(),
            })
        })
        .collect()
}

fn label<Id>(id: Id, code: &str, name: &str) -> TimetableLabel<Id> {
    TimetableLabel {
        id,
        code: code.to_owned(),
        label: name.to_owned(),
    }
}

fn row(
    input: &TimetableProjectionInput<'_>,
    assignment: &Assignment,
    activity: &Activity,
    occupied_timeslot_indices: Vec<u32>,
) -> Result<TimetableRow, TimetableQueryError> {
    let document = input.source;
    let batch = &document.import_batch;
    let key = &document.project_stable_key;
    let catalog = &input.compiled.catalog;
    let activity_label = required(
        catalog.activities.get(assignment.activity.as_usize()),
        "activity_label",
    )?;
    let plan = required(
        batch
            .course_plans()
            .iter()
            .find(|row| row.course_plan_code == activity_label.course_plan_code),
        "course_plan_label",
    )?;
    let teacher_code = required(
        catalog.teacher_codes.get(assignment.teacher.as_usize()),
        "teacher_code",
    )?;
    let teacher = required(
        batch
            .teachers()
            .iter()
            .find(|row| &row.teacher_code == teacher_code),
        "teacher_label",
    )?;
    let room_code = required(
        catalog.room_codes.get(assignment.room.as_usize()),
        "room_code",
    )?;
    let room = required(
        batch.rooms().iter().find(|row| &row.room_code == room_code),
        "room_label",
    )?;
    let slot = required(
        input
            .compiled
            .problem
            .timeslots()
            .get(assignment.start.as_usize()),
        "start",
    )?;
    let period = required(
        document
            .calendar
            .periods
            .iter()
            .find(|period| period.index == slot.period_index),
        "period",
    )?;
    let audience = audience(input, activity, activity_label)?;
    Ok(TimetableRow {
        activity_id: activity.stable_id,
        activity_index: assignment.activity.0,
        course_offering_id: activity.course_offering_id,
        meeting_ordinal: activity_label.meeting_ordinal,
        start_timeslot_index: assignment.start.0,
        day: slot.day,
        day_label: day_label(slot.day).to_owned(),
        period_index: slot.period_index,
        period_label: period.label.clone(),
        duration_periods: activity.duration_periods,
        occupied_timeslot_indices,
        grade: label(
            stable_id(key, "grade", &plan.grade_code),
            &plan.grade_code,
            &plan.grade_code,
        ),
        subject: label(activity.subject_id, &plan.subject_code, &plan.subject_code),
        course_plan: label(activity.course_plan_id, &plan.course_plan_code, &plan.name),
        audience,
        teacher: label(
            stable_id(key, "teacher", teacher_code),
            teacher_code,
            &teacher.name,
        ),
        room: label(stable_id(key, "room", room_code), room_code, &room.name),
        student_count: count(activity.audience.count())?,
    })
}

fn audience(
    input: &TimetableProjectionInput<'_>,
    activity: &Activity,
    activity_label: &crate::CompiledActivityLabel,
) -> Result<TimetableAudience, TimetableQueryError> {
    let batch = &input.source.import_batch;
    Ok(match activity_label.audience_kind {
        ImportedAudienceKind::AdministrativeClass => {
            let class = required(
                batch
                    .administrative_classes()
                    .iter()
                    .find(|row| row.administrative_class_code == activity_label.audience_code),
                "administrative_class_label",
            )?;
            TimetableAudience::AdministrativeClass(label(
                required(activity.administrative_class_id, "administrative_class_id")?,
                &class.administrative_class_code,
                &class.name,
            ))
        }
        ImportedAudienceKind::TeachingSection => {
            let section = required(
                input
                    .sections
                    .iter()
                    .find(|row| row.section_code == activity_label.audience_code),
                "teaching_section_label",
            )?;
            TimetableAudience::TeachingSection(label(
                required(activity.teaching_section_id, "teaching_section_id")?,
                &section.section_code,
                &section.name,
            ))
        }
    })
}

const fn day_label(day: Day) -> &'static str {
    match day {
        Day::Monday => "星期一",
        Day::Tuesday => "星期二",
        Day::Wednesday => "星期三",
        Day::Thursday => "星期四",
        Day::Friday => "星期五",
        Day::Saturday => "星期六",
        Day::Sunday => "星期日",
    }
}
