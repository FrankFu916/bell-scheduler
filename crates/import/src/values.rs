//! Parsed-row invariants shared by CSV import and durable-document revalidation.

use std::collections::BTreeSet;

use crate::{
    DatasetKind, ImportLocation, ImportProblem, ImportProblemCode, ImportedRoomPolicy,
    ImportedTeacherAssignment, ParsedBatch,
};

pub(crate) fn required_text_is_valid(value: &str) -> bool {
    !value.trim().is_empty()
}

pub(crate) fn list_is_valid(values: &[String]) -> bool {
    values.iter().all(|value| required_text_is_valid(value))
        && values
            .iter()
            .map(|value| value.trim())
            .collect::<BTreeSet<_>>()
            .len()
            == values.len()
}

// Every dataset's required text and positive numeric fields are listed explicitly. Enum/range
// decoding is enforced by the typed representation; relationship checks remain in validate.rs.
#[allow(clippy::too_many_lines)]
pub(crate) fn validate_row_values(batch: &ParsedBatch, problems: &mut Vec<ImportProblem>) {
    macro_rules! required_texts {
        ($rows:expr, $kind:ident, $($field:ident),+ $(,)?) => {
            for row in $rows {
                $(if !required_text_is_valid(&row.$field) {
                    problem(problems, DatasetKind::$kind, row.row, stringify!($field),
                        ImportProblemCode::ImportEmptyRequiredValue);
                })+
            }
        };
    }
    macro_rules! positive_fields {
        ($rows:expr, $kind:ident, $($field:ident),+ $(,)?) => {
            for row in $rows {
                $(if row.$field == 0 {
                    problem(problems, DatasetKind::$kind, row.row, stringify!($field),
                        ImportProblemCode::ImportInvalidUnsignedInteger);
                })+
            }
        };
    }

    required_texts!(
        &batch.students,
        Students,
        student_code,
        name,
        administrative_class_code
    );
    required_texts!(
        &batch.administrative_classes,
        AdministrativeClasses,
        administrative_class_code,
        name,
        grade_code,
        home_room_code
    );
    required_texts!(
        &batch.student_subject_choices,
        StudentSubjectChoices,
        student_code,
        subject_code
    );
    required_texts!(&batch.teachers, Teachers, teacher_code, name);
    required_texts!(
        &batch.teacher_unavailability,
        TeacherUnavailability,
        teacher_code
    );
    required_texts!(&batch.rooms, Rooms, room_code, name, building_code);
    required_texts!(
        &batch.course_plans,
        CoursePlans,
        course_plan_code,
        name,
        grade_code,
        subject_code
    );
    required_texts!(
        &batch.teaching_sections,
        TeachingSections,
        section_code,
        name,
        grade_code,
        subject_code
    );
    required_texts!(
        &batch.section_enrollments,
        SectionEnrollments,
        section_code,
        student_code
    );
    required_texts!(
        &batch.course_offerings,
        CourseOfferings,
        course_plan_code,
        audience_code
    );
    required_texts!(
        &batch.fixed_activities,
        FixedActivities,
        course_plan_code,
        audience_code,
        room_code,
        teacher_code
    );
    positive_fields!(&batch.teacher_unavailability, TeacherUnavailability, period);
    positive_fields!(&batch.rooms, Rooms, capacity);
    positive_fields!(
        &batch.course_plans,
        CoursePlans,
        weekly_periods,
        max_periods_per_day
    );
    positive_fields!(&batch.teaching_sections, TeachingSections, max_size);
    positive_fields!(
        &batch.fixed_activities,
        FixedActivities,
        meeting_ordinal,
        period,
        duration
    );

    for row in &batch.rooms {
        validate_list(
            &row.features,
            DatasetKind::Rooms,
            row.row,
            "features",
            problems,
        );
    }
    for row in &batch.course_plans {
        let kind = DatasetKind::CoursePlans;
        validate_list(
            &row.required_room_features,
            kind,
            row.row,
            "required_room_features",
            problems,
        );
        if row.meeting_pattern.is_empty() || row.meeting_pattern.contains(&0) {
            problem(
                problems,
                kind,
                row.row,
                "meeting_pattern",
                ImportProblemCode::ImportInvalidList,
            );
        }
        validate_teacher(&row.teacher_assignment, kind, row.row, problems);
        if let Some(policy) = &row.room_policy {
            validate_room_policy(policy, kind, row.row, problems);
        }
    }
    for row in &batch.teaching_sections {
        validate_teacher(
            &row.teacher_assignment,
            DatasetKind::TeachingSections,
            row.row,
            problems,
        );
        validate_room_policy(
            &row.room_policy,
            DatasetKind::TeachingSections,
            row.row,
            problems,
        );
    }
    for row in &batch.course_offerings {
        validate_teacher(
            &row.teacher_assignment,
            DatasetKind::CourseOfferings,
            row.row,
            problems,
        );
        if let Some(policy) = &row.room_policy {
            validate_room_policy(policy, DatasetKind::CourseOfferings, row.row, problems);
        }
    }
}

fn validate_list(
    values: &[String],
    kind: DatasetKind,
    row: u64,
    field: &'static str,
    problems: &mut Vec<ImportProblem>,
) {
    if !list_is_valid(values) {
        problem(
            problems,
            kind,
            row,
            field,
            ImportProblemCode::ImportInvalidList,
        );
    }
}

fn validate_teacher(
    assignment: &ImportedTeacherAssignment,
    kind: DatasetKind,
    row: u64,
    problems: &mut Vec<ImportProblem>,
) {
    let valid = match assignment {
        ImportedTeacherAssignment::Fixed { teacher_code } => required_text_is_valid(teacher_code),
        ImportedTeacherAssignment::Candidates { teacher_codes } => {
            !teacher_codes.is_empty() && list_is_valid(teacher_codes)
        }
    };
    if !valid {
        problem(
            problems,
            kind,
            row,
            "teacher_assignment",
            ImportProblemCode::ImportInvalidTeacherAssignment,
        );
    }
}

fn validate_room_policy(
    policy: &ImportedRoomPolicy,
    kind: DatasetKind,
    row: u64,
    problems: &mut Vec<ImportProblem>,
) {
    let valid = match policy {
        ImportedRoomPolicy::AdminHomeRoom => true,
        ImportedRoomPolicy::Fixed { room_code } => required_text_is_valid(room_code),
        ImportedRoomPolicy::SectionFixed {
            candidate_room_codes,
        }
        | ImportedRoomPolicy::Flexible {
            candidate_room_codes,
        } => !candidate_room_codes.is_empty() && list_is_valid(candidate_room_codes),
        ImportedRoomPolicy::PreferredFixed {
            preferred_room_codes,
            fallback_room_codes,
        } => {
            !preferred_room_codes.is_empty()
                && !fallback_room_codes.is_empty()
                && list_is_valid(preferred_room_codes)
                && list_is_valid(fallback_room_codes)
                && preferred_room_codes
                    .iter()
                    .all(|room| !fallback_room_codes.contains(room))
        }
    };
    if !valid {
        problem(
            problems,
            kind,
            row,
            "room_policy",
            ImportProblemCode::ImportInvalidRoomPolicy,
        );
    }
}

fn problem(
    problems: &mut Vec<ImportProblem>,
    kind: DatasetKind,
    row: u64,
    field: &'static str,
    code: ImportProblemCode,
) {
    problems.push(ImportProblem::new(
        code,
        ImportLocation::cell(kind, row, field),
    ));
}
