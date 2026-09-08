use crate::{
    AdministrativeClassImportRow, CoursePlanImportRow, DatasetKind, ImportConfig, ImportLocation,
    ImportProblem, ImportProblemCode, ImportedAudienceKind, ImportedRoomPolicy,
    ImportedTeacherAssignment, ParsedBatch, RoomImportRow, StudentImportRow,
    TeachingSectionImportRow,
};
use class_schedule_domain::{
    Capacity, ClassSizeRange, MeetingDuration, MeetingPattern, WeeklyPeriods,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn validate_batch(
    batch: &ParsedBatch,
    config: &ImportConfig,
    problems: &mut Vec<ImportProblem>,
) {
    crate::values::validate_row_values(batch, problems);
    validate_unique_codes(batch, problems);

    let students = index_rows_by_code(&batch.students, |row| row.student_code.as_str());
    let classes = index_rows_by_code(&batch.administrative_classes, |row| {
        row.administrative_class_code.as_str()
    });
    let teachers = index_by_code(
        &batch.teachers,
        |row| row.teacher_code.as_str(),
        |row| row.row,
    );
    let rooms = index_rows_by_code(&batch.rooms, |row| row.room_code.as_str());
    let plans = index_rows_by_code(&batch.course_plans, |row| row.course_plan_code.as_str());
    let sections = index_rows_by_code(&batch.teaching_sections, |row| row.section_code.as_str());
    let grades: BTreeSet<_> = batch
        .administrative_classes
        .iter()
        .map(|row| row.grade_code.as_str())
        .collect();
    // Course plans are the import bundle's subject catalogue. A section must not make a typo
    // look like a newly defined subject merely by repeating it in its own row.
    let subjects: BTreeSet<_> = batch
        .course_plans
        .iter()
        .map(|row| row.subject_code.as_str())
        .collect();

    for row in &batch.students {
        require_reference(
            classes.contains_key(row.administrative_class_code.as_str()),
            DatasetKind::Students,
            row.row,
            "administrative_class_code",
            problems,
        );
    }
    for row in &batch.administrative_classes {
        require_reference(
            rooms.contains_key(row.home_room_code.as_str()),
            DatasetKind::AdministrativeClasses,
            row.row,
            "home_room_code",
            problems,
        );
    }

    validate_choices(batch, config, &students, &subjects, problems);
    validate_teacher_unavailability(batch, &teachers, problems);
    validate_course_plans(batch, &grades, &teachers, &rooms, problems);
    validate_sections(batch, &grades, &subjects, &teachers, &rooms, problems);
    validate_enrollments(batch, &students, &classes, &sections, problems);
    validate_course_offerings(
        batch, &plans, &classes, &sections, &teachers, &rooms, problems,
    );
    validate_fixed_activities(
        batch, &plans, &classes, &sections, &teachers, &rooms, problems,
    );
}

fn validate_course_offerings(
    batch: &ParsedBatch,
    plans: &BTreeMap<&str, &CoursePlanImportRow>,
    classes: &BTreeMap<&str, &AdministrativeClassImportRow>,
    sections: &BTreeMap<&str, &TeachingSectionImportRow>,
    teachers: &BTreeMap<&str, u64>,
    rooms: &BTreeMap<&str, &RoomImportRow>,
    problems: &mut Vec<ImportProblem>,
) {
    let mut relations = BTreeMap::new();
    for row in &batch.course_offerings {
        let plan = plans.get(row.course_plan_code.as_str()).copied();
        require_reference(
            plan.is_some(),
            DatasetKind::CourseOfferings,
            row.row,
            "course_plan_code",
            problems,
        );
        for _teacher_code in row
            .teacher_assignment
            .teacher_codes()
            .filter(|code| !teachers.contains_key(code))
        {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportMissingReference,
                ImportLocation::cell(DatasetKind::CourseOfferings, row.row, "teacher_codes"),
            ));
        }
        if let Some(policy) = &row.room_policy {
            validate_room_policy_refs(
                policy,
                DatasetKind::CourseOfferings,
                row.row,
                rooms,
                problems,
            );
            if row.audience_kind == ImportedAudienceKind::TeachingSection
                && matches!(policy, ImportedRoomPolicy::AdminHomeRoom)
            {
                problems.push(ImportProblem::new(
                    ImportProblemCode::ImportInvalidRoomPolicy,
                    ImportLocation::cell(DatasetKind::CourseOfferings, row.row, "room_policy"),
                ));
            }
        }

        let audience_matches = match row.audience_kind {
            ImportedAudienceKind::AdministrativeClass => classes
                .get(row.audience_code.as_str())
                .is_some_and(|class| {
                    plan.is_some_and(|plan| {
                        plan.audience_kind == row.audience_kind
                            && plan.grade_code == class.grade_code
                    })
                }),
            ImportedAudienceKind::TeachingSection => sections
                .get(row.audience_code.as_str())
                .is_some_and(|section| {
                    plan.is_some_and(|plan| {
                        plan.audience_kind == row.audience_kind
                            && plan.grade_code == section.grade_code
                            && plan.subject_code == section.subject_code
                    })
                }),
        };
        let audience_exists = match row.audience_kind {
            ImportedAudienceKind::AdministrativeClass => {
                classes.contains_key(row.audience_code.as_str())
            }
            ImportedAudienceKind::TeachingSection => {
                sections.contains_key(row.audience_code.as_str())
            }
        };
        require_reference(
            audience_exists,
            DatasetKind::CourseOfferings,
            row.row,
            "audience_code",
            problems,
        );
        if plan.is_some() && audience_exists && !audience_matches {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportCourseOfferingMismatch,
                ImportLocation::cell(DatasetKind::CourseOfferings, row.row, "audience_code"),
            ));
        }

        let key = (
            row.course_plan_code.as_str(),
            row.audience_kind,
            row.audience_code.as_str(),
        );
        if let Some(first_row) = relations.get(&key).copied() {
            problems.push(
                ImportProblem::new(
                    ImportProblemCode::ImportDuplicateRelation,
                    ImportLocation::cell(DatasetKind::CourseOfferings, row.row, "audience_code"),
                )
                .with_related_row(first_row),
            );
        } else {
            relations.insert(key, row.row);
        }
    }
}

fn validate_unique_codes(batch: &ParsedBatch, problems: &mut Vec<ImportProblem>) {
    validate_unique(
        &batch.students,
        DatasetKind::Students,
        "student_code",
        |row| row.student_code.as_str(),
        |row| row.row,
        problems,
    );
    validate_unique(
        &batch.administrative_classes,
        DatasetKind::AdministrativeClasses,
        "administrative_class_code",
        |row| row.administrative_class_code.as_str(),
        |row| row.row,
        problems,
    );
    validate_unique(
        &batch.teachers,
        DatasetKind::Teachers,
        "teacher_code",
        |row| row.teacher_code.as_str(),
        |row| row.row,
        problems,
    );
    validate_unique(
        &batch.rooms,
        DatasetKind::Rooms,
        "room_code",
        |row| row.room_code.as_str(),
        |row| row.row,
        problems,
    );
    validate_unique(
        &batch.course_plans,
        DatasetKind::CoursePlans,
        "course_plan_code",
        |row| row.course_plan_code.as_str(),
        |row| row.row,
        problems,
    );
    validate_unique(
        &batch.teaching_sections,
        DatasetKind::TeachingSections,
        "section_code",
        |row| row.section_code.as_str(),
        |row| row.row,
        problems,
    );
}

fn validate_choices(
    batch: &ParsedBatch,
    config: &ImportConfig,
    students: &BTreeMap<&str, &StudentImportRow>,
    subjects: &BTreeSet<&str>,
    problems: &mut Vec<ImportProblem>,
) {
    let mut first_relation = BTreeMap::new();
    let mut choices_by_student: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for row in &batch.student_subject_choices {
        require_reference(
            students.contains_key(row.student_code.as_str()),
            DatasetKind::StudentSubjectChoices,
            row.row,
            "student_code",
            problems,
        );
        require_reference(
            subjects.contains(row.subject_code.as_str()),
            DatasetKind::StudentSubjectChoices,
            row.row,
            "subject_code",
            problems,
        );
        let key = (row.student_code.as_str(), row.subject_code.as_str());
        if let Some(first_row) = first_relation.insert(key, row.row) {
            problems.push(
                ImportProblem::new(
                    ImportProblemCode::ImportDuplicateSubjectChoice,
                    ImportLocation::cell(
                        DatasetKind::StudentSubjectChoices,
                        row.row,
                        "subject_code",
                    ),
                )
                .with_related_row(first_row),
            );
        }
        choices_by_student
            .entry(row.student_code.as_str())
            .or_default()
            .insert(row.subject_code.as_str());
    }

    if let Some(expected) = config.exact_subject_choices() {
        for row in &batch.students {
            let actual = choices_by_student
                .get(row.student_code.as_str())
                .map_or(0, BTreeSet::len);
            if actual != expected {
                problems.push(
                    ImportProblem::new(
                        ImportProblemCode::ImportSubjectChoiceCount,
                        ImportLocation::cell(DatasetKind::Students, row.row, "student_code"),
                    )
                    .with_counts(expected as u64, actual as u64),
                );
            }
        }
    }
}

fn validate_teacher_unavailability(
    batch: &ParsedBatch,
    teachers: &BTreeMap<&str, u64>,
    problems: &mut Vec<ImportProblem>,
) {
    let mut relations = BTreeMap::new();
    for row in &batch.teacher_unavailability {
        require_reference(
            teachers.contains_key(row.teacher_code.as_str()),
            DatasetKind::TeacherUnavailability,
            row.row,
            "teacher_code",
            problems,
        );
        let key = (row.teacher_code.as_str(), row.day, row.period);
        if let Some(first_row) = relations.insert(key, row.row) {
            problems.push(
                ImportProblem::new(
                    ImportProblemCode::ImportDuplicateRelation,
                    ImportLocation::row(DatasetKind::TeacherUnavailability, row.row),
                )
                .with_related_row(first_row),
            );
        }
    }
}

fn validate_course_plans(
    batch: &ParsedBatch,
    grades: &BTreeSet<&str>,
    teachers: &BTreeMap<&str, u64>,
    rooms: &BTreeMap<&str, &RoomImportRow>,
    problems: &mut Vec<ImportProblem>,
) {
    for row in &batch.course_plans {
        require_reference(
            grades.contains(row.grade_code.as_str()),
            DatasetKind::CoursePlans,
            row.row,
            "grade_code",
            problems,
        );
        if build_meeting_pattern(row).is_none() {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportInvalidMeetingPattern,
                ImportLocation::cell(DatasetKind::CoursePlans, row.row, "meeting_pattern"),
            ));
        }
        if let Some(policy) = &row.room_policy {
            validate_room_policy_refs(policy, DatasetKind::CoursePlans, row.row, rooms, problems);
            if row.audience_kind == ImportedAudienceKind::TeachingSection
                && matches!(policy, ImportedRoomPolicy::AdminHomeRoom)
            {
                problems.push(ImportProblem::new(
                    ImportProblemCode::ImportInvalidRoomPolicy,
                    ImportLocation::cell(DatasetKind::CoursePlans, row.row, "room_policy"),
                ));
            }
        }
        for _teacher_code in row
            .teacher_assignment
            .teacher_codes()
            .filter(|code| !teachers.contains_key(code))
        {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportMissingReference,
                ImportLocation::cell(DatasetKind::CoursePlans, row.row, "teacher_codes"),
            ));
        }
    }
}

fn validate_sections(
    batch: &ParsedBatch,
    grades: &BTreeSet<&str>,
    subjects: &BTreeSet<&str>,
    teachers: &BTreeMap<&str, u64>,
    rooms: &BTreeMap<&str, &RoomImportRow>,
    problems: &mut Vec<ImportProblem>,
) {
    for row in &batch.teaching_sections {
        require_reference(
            grades.contains(row.grade_code.as_str()),
            DatasetKind::TeachingSections,
            row.row,
            "grade_code",
            problems,
        );
        require_reference(
            subjects.contains(row.subject_code.as_str()),
            DatasetKind::TeachingSections,
            row.row,
            "subject_code",
            problems,
        );
        if subjects.contains(row.subject_code.as_str()) {
            require_reference(
                batch.course_plans.iter().any(|plan| {
                    plan.grade_code == row.grade_code
                        && plan.subject_code == row.subject_code
                        && plan.audience_kind == ImportedAudienceKind::TeachingSection
                }),
                DatasetKind::TeachingSections,
                row.row,
                "subject_code",
                problems,
            );
        }
        let size_valid = Capacity::new(row.max_size)
            .and_then(|max| ClassSizeRange::new(row.min_size, row.target_size, max))
            .is_ok();
        if !size_valid {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportInvalidClassSizeRange,
                ImportLocation::row(DatasetKind::TeachingSections, row.row),
            ));
        }
        if matches!(row.room_policy, ImportedRoomPolicy::AdminHomeRoom) {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportInvalidRoomPolicy,
                ImportLocation::cell(DatasetKind::TeachingSections, row.row, "room_policy"),
            ));
        }
        validate_room_policy_refs(
            &row.room_policy,
            DatasetKind::TeachingSections,
            row.row,
            rooms,
            problems,
        );
        for _teacher_code in row
            .teacher_assignment
            .teacher_codes()
            .filter(|code| !teachers.contains_key(code))
        {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportMissingReference,
                ImportLocation::cell(DatasetKind::TeachingSections, row.row, "teacher_codes"),
            ));
        }
    }
}

// The relation is validated together so exact-one membership and section capacity share source
// provenance and cannot diverge across separate passes.
#[allow(clippy::too_many_lines)]
fn validate_enrollments(
    batch: &ParsedBatch,
    students: &BTreeMap<&str, &StudentImportRow>,
    classes: &BTreeMap<&str, &AdministrativeClassImportRow>,
    sections: &BTreeMap<&str, &TeachingSectionImportRow>,
    problems: &mut Vec<ImportProblem>,
) {
    let choices: BTreeMap<_, _> = batch
        .student_subject_choices
        .iter()
        .map(|row| {
            (
                (row.student_code.as_str(), row.subject_code.as_str()),
                row.row,
            )
        })
        .collect();
    let mut exact_relations = BTreeMap::new();
    let mut section_by_student_subject = BTreeMap::new();
    let mut member_counts: BTreeMap<&str, usize> = BTreeMap::new();

    for row in &batch.section_enrollments {
        let student = students.get(row.student_code.as_str()).copied();
        let section = sections.get(row.section_code.as_str()).copied();
        require_reference(
            student.is_some(),
            DatasetKind::SectionEnrollments,
            row.row,
            "student_code",
            problems,
        );
        require_reference(
            section.is_some(),
            DatasetKind::SectionEnrollments,
            row.row,
            "section_code",
            problems,
        );
        let relation = (row.section_code.as_str(), row.student_code.as_str());
        if let Some(first_row) = exact_relations.get(&relation).copied() {
            problems.push(
                ImportProblem::new(
                    ImportProblemCode::ImportDuplicateRelation,
                    ImportLocation::cell(DatasetKind::SectionEnrollments, row.row, "student_code"),
                )
                .with_related_row(first_row),
            );
            continue;
        }
        exact_relations.insert(relation, row.row);
        let (Some(student), Some(section)) = (student, section) else {
            continue;
        };
        *member_counts
            .entry(section.section_code.as_str())
            .or_default() += 1;
        let selected =
            choices.contains_key(&(row.student_code.as_str(), section.subject_code.as_str()));
        if !selected {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportSectionSubjectNotSelected,
                ImportLocation::cell(DatasetKind::SectionEnrollments, row.row, "section_code"),
            ));
        }
        let grade_matches = classes
            .get(student.administrative_class_code.as_str())
            .is_some_and(|class| class.grade_code == section.grade_code);
        if !grade_matches {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportSectionGradeMismatch,
                ImportLocation::cell(DatasetKind::SectionEnrollments, row.row, "section_code"),
            ));
        }
        if !selected || !grade_matches {
            continue;
        }
        let subject_key = (row.student_code.as_str(), section.subject_code.as_str());
        if let Some(first_row) = section_by_student_subject.get(&subject_key).copied() {
            problems.push(
                ImportProblem::new(
                    ImportProblemCode::ImportMultipleSectionsForSubject,
                    ImportLocation::cell(DatasetKind::SectionEnrollments, row.row, "section_code"),
                )
                .with_related_row(first_row),
            );
        } else {
            section_by_student_subject.insert(subject_key, row.row);
        }
    }

    // Supplying sections selects input mode A. In that mode every selected subject must already
    // have exactly one valid section assignment. An empty section dataset is input mode B and is
    // deliberately left for the sectioning engine.
    if !batch.teaching_sections.is_empty() {
        for row in &batch.student_subject_choices {
            if students.contains_key(row.student_code.as_str())
                && !section_by_student_subject
                    .contains_key(&(row.student_code.as_str(), row.subject_code.as_str()))
            {
                problems.push(ImportProblem::new(
                    ImportProblemCode::ImportMissingSectionForSubject,
                    ImportLocation::cell(
                        DatasetKind::StudentSubjectChoices,
                        row.row,
                        "subject_code",
                    ),
                ));
            }
        }
    }

    for section in &batch.teaching_sections {
        let count = member_counts
            .get(section.section_code.as_str())
            .copied()
            .unwrap_or_default();
        if count < usize::from(section.min_size) {
            problems.push(
                ImportProblem::new(
                    ImportProblemCode::ImportSectionBelowMinimum,
                    ImportLocation::cell(DatasetKind::TeachingSections, section.row, "min_size"),
                )
                .with_counts(u64::from(section.min_size), count as u64),
            );
        }
        if count > usize::from(section.max_size) {
            problems.push(
                ImportProblem::new(
                    ImportProblemCode::ImportSectionAboveMaximum,
                    ImportLocation::cell(DatasetKind::TeachingSections, section.row, "max_size"),
                )
                .with_counts(u64::from(section.max_size), count as u64),
            );
        }
    }
}

// Consistency spans plan, offering, audience, teacher, room, feature and ordinal in one audit pass.
#[allow(clippy::too_many_lines)]
fn validate_fixed_activities(
    batch: &ParsedBatch,
    plans: &BTreeMap<&str, &CoursePlanImportRow>,
    classes: &BTreeMap<&str, &AdministrativeClassImportRow>,
    sections: &BTreeMap<&str, &TeachingSectionImportRow>,
    teachers: &BTreeMap<&str, u64>,
    rooms: &BTreeMap<&str, &RoomImportRow>,
    problems: &mut Vec<ImportProblem>,
) {
    let mut assignments = BTreeMap::new();
    for row in &batch.fixed_activities {
        let plan = plans.get(row.course_plan_code.as_str()).copied();
        let room = rooms.get(row.room_code.as_str()).copied();
        require_reference(
            plan.is_some(),
            DatasetKind::FixedActivities,
            row.row,
            "course_plan_code",
            problems,
        );
        require_reference(
            room.is_some(),
            DatasetKind::FixedActivities,
            row.row,
            "room_code",
            problems,
        );
        require_reference(
            teachers.contains_key(row.teacher_code.as_str()),
            DatasetKind::FixedActivities,
            row.row,
            "teacher_code",
            problems,
        );
        let administrative_class = (row.audience_kind == ImportedAudienceKind::AdministrativeClass)
            .then(|| classes.get(row.audience_code.as_str()).copied())
            .flatten();
        let section = (row.audience_kind == ImportedAudienceKind::TeachingSection)
            .then(|| sections.get(row.audience_code.as_str()).copied())
            .flatten();
        let audience_exists = administrative_class.is_some() || section.is_some();
        require_reference(
            audience_exists,
            DatasetKind::FixedActivities,
            row.row,
            "audience_code",
            problems,
        );
        if let Some(plan) = plan {
            let ordinal_index = row.meeting_ordinal.checked_sub(1).map(usize::from);
            let audience_matches = match row.audience_kind {
                ImportedAudienceKind::AdministrativeClass => {
                    administrative_class.is_some_and(|class| {
                        plan.audience_kind == row.audience_kind
                            && plan.grade_code == class.grade_code
                    })
                }
                ImportedAudienceKind::TeachingSection => section.is_some_and(|section| {
                    plan.audience_kind == row.audience_kind
                        && plan.grade_code == section.grade_code
                        && plan.subject_code == section.subject_code
                }),
            };
            let offering = batch.course_offerings.iter().find(|offering| {
                offering.course_plan_code == row.course_plan_code
                    && offering.audience_kind == row.audience_kind
                    && offering.audience_code == row.audience_code
            });
            let teacher_assignment = offering
                .map(|offering| &offering.teacher_assignment)
                .or_else(|| section.map(|section| &section.teacher_assignment))
                .unwrap_or(&plan.teacher_assignment);
            let teacher_matches = !teachers.contains_key(row.teacher_code.as_str())
                || teacher_assignment_allows(teacher_assignment, &row.teacher_code);

            let room_policy = offering
                .and_then(|offering| offering.room_policy.as_ref())
                .or(plan.room_policy.as_ref())
                .or_else(|| section.map(|section| &section.room_policy));
            let room_matches = room.is_none_or(|_| {
                room_policy.map_or_else(
                    || {
                        administrative_class
                            .is_some_and(|class| class.home_room_code == row.room_code)
                    },
                    |policy| {
                        room_policy_allows(
                            policy,
                            &row.room_code,
                            administrative_class.map(|class| class.home_room_code.as_str()),
                        )
                    },
                )
            });
            let features_match = room.is_none_or(|room| {
                plan.required_room_features
                    .iter()
                    .all(|feature| room.features.contains(feature))
            });
            if !audience_matches
                || ordinal_index
                    .and_then(|index| plan.meeting_pattern.get(index))
                    .copied()
                    != Some(row.duration)
                || !teacher_matches
                || !room_matches
                || !features_match
            {
                problems.push(ImportProblem::new(
                    ImportProblemCode::ImportFixedActivityMismatch,
                    ImportLocation::row(DatasetKind::FixedActivities, row.row),
                ));
            }
        }
        let key = (
            row.course_plan_code.as_str(),
            row.audience_kind,
            row.audience_code.as_str(),
            row.meeting_ordinal,
        );
        if let Some(first_row) = assignments.get(&key).copied() {
            problems.push(
                ImportProblem::new(
                    ImportProblemCode::ImportDuplicateRelation,
                    ImportLocation::row(DatasetKind::FixedActivities, row.row),
                )
                .with_related_row(first_row),
            );
        } else {
            assignments.insert(key, row.row);
        }
    }
}

fn teacher_assignment_allows(assignment: &ImportedTeacherAssignment, teacher_code: &str) -> bool {
    match assignment {
        ImportedTeacherAssignment::Fixed {
            teacher_code: fixed,
        } => fixed == teacher_code,
        ImportedTeacherAssignment::Candidates { teacher_codes } => teacher_codes
            .iter()
            .any(|candidate| candidate == teacher_code),
    }
}

fn room_policy_allows(
    policy: &ImportedRoomPolicy,
    room_code: &str,
    administrative_home_room: Option<&str>,
) -> bool {
    match policy {
        ImportedRoomPolicy::AdminHomeRoom => administrative_home_room == Some(room_code),
        ImportedRoomPolicy::Fixed { room_code: fixed } => fixed == room_code,
        ImportedRoomPolicy::SectionFixed {
            candidate_room_codes,
        }
        | ImportedRoomPolicy::Flexible {
            candidate_room_codes,
        } => candidate_room_codes
            .iter()
            .any(|candidate| candidate == room_code),
        ImportedRoomPolicy::PreferredFixed {
            preferred_room_codes,
            fallback_room_codes,
        } => preferred_room_codes
            .iter()
            .chain(fallback_room_codes)
            .any(|candidate| candidate == room_code),
    }
}

fn validate_room_policy_refs(
    policy: &ImportedRoomPolicy,
    kind: DatasetKind,
    row: u64,
    rooms: &BTreeMap<&str, &RoomImportRow>,
    problems: &mut Vec<ImportProblem>,
) {
    for _room_code in policy.room_codes().filter(|code| !rooms.contains_key(code)) {
        problems.push(ImportProblem::new(
            ImportProblemCode::ImportMissingReference,
            ImportLocation::cell(kind, row, "room_policy"),
        ));
    }
}

fn build_meeting_pattern(row: &CoursePlanImportRow) -> Option<MeetingPattern> {
    let weekly = WeeklyPeriods::new(row.weekly_periods).ok()?;
    let daily = WeeklyPeriods::new(row.max_periods_per_day).ok()?;
    let durations = row
        .meeting_pattern
        .iter()
        .copied()
        .map(MeetingDuration::new)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    MeetingPattern::new(
        weekly,
        durations,
        row.min_days_between,
        daily,
        row.may_cross_breaks,
    )
    .ok()
}

fn require_reference(
    exists: bool,
    kind: DatasetKind,
    row: u64,
    column: &'static str,
    problems: &mut Vec<ImportProblem>,
) {
    if !exists {
        problems.push(ImportProblem::new(
            ImportProblemCode::ImportMissingReference,
            ImportLocation::cell(kind, row, column),
        ));
    }
}

fn validate_unique<T>(
    values: &[T],
    kind: DatasetKind,
    column: &'static str,
    key: impl Fn(&T) -> &str,
    row: impl Fn(&T) -> u64,
    problems: &mut Vec<ImportProblem>,
) {
    let mut seen = BTreeMap::new();
    for value in values {
        if let Some(first_row) = seen.insert(key(value), row(value)) {
            problems.push(
                ImportProblem::new(
                    ImportProblemCode::ImportDuplicateExternalCode,
                    ImportLocation::cell(kind, row(value), column),
                )
                .with_related_row(first_row),
            );
        }
    }
}

fn index_by_code<'a, T>(
    values: &'a [T],
    key: impl Fn(&'a T) -> &'a str,
    row: impl Fn(&T) -> u64,
) -> BTreeMap<&'a str, u64> {
    values
        .iter()
        .map(|value| (key(value), row(value)))
        .collect()
}

fn index_rows_by_code<'a, T>(
    values: &'a [T],
    key: impl Fn(&'a T) -> &'a str,
) -> BTreeMap<&'a str, &'a T> {
    values.iter().map(|value| (key(value), value)).collect()
}
