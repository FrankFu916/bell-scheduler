use class_schedule_import::{
    ColumnMapping, CsvImporter, CsvSource, DatasetKind, ImportConfig, ImportProblem,
    ImportProblemCode, ImportedAudienceKind, ImportedDay, ImportedRoomPolicy,
    ImportedTeacherAssignment,
};
use std::collections::BTreeMap;

#[derive(Debug)]
struct Bundle {
    files: BTreeMap<DatasetKind, Vec<u8>>,
}

impl Bundle {
    #[allow(clippy::too_many_lines)]
    fn valid() -> Self {
        let mut files = BTreeMap::new();
        files.insert(
            DatasetKind::Students,
            concat!(
                "\u{feff}student_code,name,administrative_class_code\n",
                "S001,张同学,AC01\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::AdministrativeClasses,
            concat!(
                "administrative_class_code,name,grade_code,home_room_code\n",
                "AC01,高三1班,G12,R1\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::StudentSubjectChoices,
            concat!(
                "student_code,subject_code\n",
                "S001,physics\n",
                "S001,chemistry\n",
                "S001,biology\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::Teachers,
            concat!(
                "teacher_code,name\n",
                "T1,物理教师\n",
                "T2,化学教师\n",
                "T3,生物教师\n",
                "T4,语文教师\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::TeacherUnavailability,
            "teacher_code,day,period\nT2,星期二,3\n".as_bytes().to_vec(),
        );
        files.insert(
            DatasetKind::Rooms,
            concat!(
                "room_code,name,building_code,capacity,features\n",
                "R1,高三1班教室,B1,50,multimedia\n",
                "R2,物理实验室,B2,30,physics_lab;multimedia\n",
                "R3,理科教室,B2,30,multimedia\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::CoursePlans,
            concat!(
                "course_plan_code,name,grade_code,subject_code,audience_kind,weekly_periods,meeting_pattern,min_days_between,max_periods_per_day,may_cross_breaks,required_room_features,teacher_assignment,teacher_codes,room_policy,room_candidates,preferred_rooms,fallback_rooms\n",
                "CP-P,物理,G12,physics,teaching_section,2,1;1,1,1,false,physics_lab,fixed,T1,,,,\n",
                "CP-C,化学,G12,chemistry,teaching_section,2,2,0,2,false,,fixed,T2,fixed,R3,,\n",
                "CP-B,生物,G12,biology,teaching_section,2,1;1,1,1,否,,candidates,T3,flexible,R2;R3,,\n",
                "CP-Z,语文,G12,chinese,administrative_class,5,1;1;1;1;1,1,1,false,,fixed,T4,,,,\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::TeachingSections,
            concat!(
                "section_code,name,grade_code,subject_code,min_size,target_size,max_size,room_policy,room_candidates,preferred_rooms,fallback_rooms,teacher_assignment,teacher_codes\n",
                "SEC-P,物理1班,G12,physics,1,1,2,section_fixed,R2;R3,,,fixed,T1\n",
                "SEC-C,化学1班,G12,chemistry,1,1,2,fixed,R3,,,fixed,T2\n",
                "SEC-B,生物1班,G12,biology,1,1,2,preferred_fixed,,R2,R3,candidates,T3\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::SectionEnrollments,
            concat!(
                "section_code,student_code\n",
                "SEC-P,S001\n",
                "SEC-C,S001\n",
                "SEC-B,S001\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::CourseOfferings,
            concat!(
                "course_plan_code,audience_kind,audience_code,teacher_assignment,teacher_codes,room_policy,room_candidates,preferred_rooms,fallback_rooms\n",
                "CP-Z,administrative_class,AC01,fixed,T4,admin_home_room,,,\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::FixedActivities,
            concat!(
                "course_plan_code,audience_kind,audience_code,meeting_ordinal,day,period,duration,room_code,teacher_code\n",
                "CP-P,teaching_section,SEC-P,1,周一,1,1,R2,T1\n",
            )
            .as_bytes()
            .to_vec(),
        );
        Self { files }
    }

    fn import(
        &self,
        config: ImportConfig,
    ) -> Result<class_schedule_import::ImportBatch, Vec<ImportProblem>> {
        CsvImporter::new(config)
            .import(
                self.files
                    .iter()
                    .map(|(&kind, bytes)| CsvSource::new(kind, bytes)),
            )
            .map_err(class_schedule_import::ImportFailure::into_problems)
    }

    fn replace(&mut self, kind: DatasetKind, csv: &str) {
        self.files.insert(kind, csv.as_bytes().to_vec());
    }
}

fn isolated_config() -> ImportConfig {
    ImportConfig::default()
        .with_required_core_datasets(false)
        .with_exact_subject_choices(None)
}

fn codes(problems: &[ImportProblem]) -> Vec<ImportProblemCode> {
    problems.iter().map(ImportProblem::code).collect()
}

#[test]
fn durable_batch_revalidates_input_a_and_unmaterialized_input_b() {
    let batch = Bundle::valid().import(ImportConfig::default()).unwrap();
    let value = serde_json::to_value(&batch).unwrap();
    let decoded: class_schedule_import::ImportBatch =
        serde_json::from_value(value.clone()).unwrap();
    decoded.revalidate(&ImportConfig::default()).unwrap();
    assert_eq!(decoded, batch);

    let mut unsectioned = value;
    for key in [
        "teaching_sections",
        "section_enrollments",
        "fixed_activities",
    ] {
        unsectioned[key] = serde_json::json!([]);
    }
    let decoded: class_schedule_import::ImportBatch = serde_json::from_value(unsectioned).unwrap();
    decoded.revalidate(&ImportConfig::default()).unwrap();
    assert!(decoded.teaching_sections().is_empty());
}

#[test]
fn durable_batch_rejects_blank_required_strings_in_every_dataset() {
    let batch = Bundle::valid().import(ImportConfig::default()).unwrap();
    let original = serde_json::to_value(batch).unwrap();
    for (dataset, rows) in original.as_object().unwrap() {
        for (index, row) in rows.as_array().unwrap().iter().enumerate() {
            for (field, value) in row.as_object().unwrap() {
                if !value.is_string() || ["audience_kind", "day"].contains(&field.as_str()) {
                    continue;
                }
                let mut tampered = original.clone();
                tampered[dataset][index][field] = serde_json::json!(" \t ");
                let decoded: class_schedule_import::ImportBatch =
                    serde_json::from_value(tampered).unwrap();
                let failure = decoded.revalidate(&ImportConfig::default()).unwrap_err();
                assert!(
                    codes(failure.problems())
                        .contains(&ImportProblemCode::ImportEmptyRequiredValue),
                    "{dataset}/{index}/{field}: {failure:?}"
                );
            }
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn durable_batch_rejects_tampered_values_and_cross_table_relations_without_panicking() {
    use ImportProblemCode::{
        ImportDuplicateSubjectChoice, ImportEmptyRequiredValue, ImportInvalidList,
        ImportInvalidRoomPolicy, ImportInvalidTeacherAssignment, ImportInvalidUnsignedInteger,
        ImportMissingReference, ImportSubjectChoiceCount,
    };
    use serde_json::json;

    let batch = Bundle::valid().import(ImportConfig::default()).unwrap();
    let original = serde_json::to_value(batch).unwrap();
    let cases = [
        ("/rooms/0/capacity", json!(0), ImportInvalidUnsignedInteger),
        (
            "/teacher_unavailability/0/period",
            json!(0),
            ImportInvalidUnsignedInteger,
        ),
        (
            "/fixed_activities/0/meeting_ordinal",
            json!(0),
            ImportInvalidUnsignedInteger,
        ),
        (
            "/fixed_activities/0/period",
            json!(0),
            ImportInvalidUnsignedInteger,
        ),
        (
            "/fixed_activities/0/duration",
            json!(0),
            ImportInvalidUnsignedInteger,
        ),
        (
            "/course_plans/0/weekly_periods",
            json!(0),
            ImportInvalidUnsignedInteger,
        ),
        (
            "/course_plans/0/max_periods_per_day",
            json!(0),
            ImportInvalidUnsignedInteger,
        ),
        (
            "/teaching_sections/0/max_size",
            json!(0),
            ImportInvalidUnsignedInteger,
        ),
        (
            "/rooms/0/features",
            json!(["lab", "lab"]),
            ImportInvalidList,
        ),
        (
            "/rooms/0/features",
            json!(["lab", " lab "]),
            ImportInvalidList,
        ),
        (
            "/course_plans/0/required_room_features",
            json!([" "]),
            ImportInvalidList,
        ),
        (
            "/course_plans/0/meeting_pattern",
            json!([]),
            ImportInvalidList,
        ),
        (
            "/course_plans/0/meeting_pattern",
            json!([0, 2]),
            ImportInvalidList,
        ),
        (
            "/course_plans/0/teacher_assignment",
            json!({"kind": "candidates", "teacher_codes": []}),
            ImportInvalidTeacherAssignment,
        ),
        (
            "/course_plans/0/teacher_assignment",
            json!({"kind": "fixed", "teacher_code": " "}),
            ImportInvalidTeacherAssignment,
        ),
        (
            "/teaching_sections/0/teacher_assignment",
            json!({"kind": "candidates", "teacher_codes": ["T1", "T1"]}),
            ImportInvalidTeacherAssignment,
        ),
        (
            "/course_offerings/0/teacher_assignment",
            json!({"kind": "candidates", "teacher_codes": [" "]}),
            ImportInvalidTeacherAssignment,
        ),
        (
            "/course_plans/0/room_policy",
            json!({"kind": "fixed", "room_code": " "}),
            ImportInvalidRoomPolicy,
        ),
        (
            "/course_plans/0/room_policy",
            json!({"kind": "flexible", "candidate_room_codes": []}),
            ImportInvalidRoomPolicy,
        ),
        (
            "/teaching_sections/0/room_policy",
            json!({"kind": "section_fixed", "candidate_room_codes": ["R2", "R2"]}),
            ImportInvalidRoomPolicy,
        ),
        (
            "/teaching_sections/0/room_policy",
            json!({"kind": "section_fixed", "candidate_room_codes": []}),
            ImportInvalidRoomPolicy,
        ),
        (
            "/course_offerings/0/room_policy",
            json!({"kind": "preferred_fixed", "preferred_room_codes": [], "fallback_room_codes": ["R1"]}),
            ImportInvalidRoomPolicy,
        ),
        (
            "/teaching_sections/0/room_policy",
            json!({"kind": "preferred_fixed", "preferred_room_codes": ["R2"], "fallback_room_codes": ["R2"]}),
            ImportInvalidRoomPolicy,
        ),
        ("/students/0/name", json!(""), ImportEmptyRequiredValue),
        (
            "/students/0/administrative_class_code",
            json!("missing"),
            ImportMissingReference,
        ),
        (
            "/student_subject_choices/1",
            original["student_subject_choices"][0].clone(),
            ImportDuplicateSubjectChoice,
        ),
        (
            "/student_subject_choices",
            json!([]),
            ImportSubjectChoiceCount,
        ),
    ];
    for (pointer, replacement, expected) in cases {
        let mut tampered = original.clone();
        *tampered.pointer_mut(pointer).unwrap() = replacement;
        let decoded: class_schedule_import::ImportBatch = serde_json::from_value(tampered).unwrap();
        let failure = decoded.revalidate(&ImportConfig::default()).unwrap_err();
        assert!(
            codes(failure.problems()).contains(&expected),
            "{pointer}: {failure:?}"
        );
    }
}

#[test]
fn imports_complete_bundle_with_bom_and_all_supported_datasets() {
    let batch = Bundle::valid().import(ImportConfig::default()).unwrap();

    assert_eq!(batch.students().len(), 1);
    assert_eq!(batch.administrative_classes().len(), 1);
    assert_eq!(batch.student_subject_choices().len(), 3);
    assert_eq!(batch.teachers().len(), 4);
    assert_eq!(batch.teacher_unavailability().len(), 1);
    assert_eq!(batch.rooms().len(), 3);
    assert_eq!(batch.course_plans().len(), 4);
    assert_eq!(batch.teaching_sections().len(), 3);
    assert_eq!(batch.section_enrollments().len(), 3);
    assert_eq!(batch.course_offerings().len(), 1);
    assert_eq!(batch.fixed_activities().len(), 1);

    assert_eq!(batch.students()[0].row, 2);
    assert_eq!(batch.teacher_unavailability()[0].day, ImportedDay::Tuesday);
    assert_eq!(batch.rooms()[1].capacity, 30);
    assert_eq!(
        batch.rooms()[1].features,
        ["physics_lab".to_owned(), "multimedia".to_owned()]
    );
    assert_eq!(batch.course_plans()[0].meeting_pattern, [1, 1]);
    assert_eq!(
        batch.course_plans()[0].required_room_features,
        ["physics_lab".to_owned()]
    );
    assert!(matches!(
        batch.course_plans()[0].teacher_assignment,
        ImportedTeacherAssignment::Fixed { .. }
    ));
    assert!(!batch.course_plans()[0].may_cross_breaks);
    assert!(matches!(
        batch.teaching_sections()[0].room_policy,
        ImportedRoomPolicy::SectionFixed { .. }
    ));
    assert!(matches!(
        batch.teaching_sections()[0].teacher_assignment,
        ImportedTeacherAssignment::Fixed { .. }
    ));
    assert_eq!(
        batch.fixed_activities()[0].audience_kind,
        ImportedAudienceKind::TeachingSection
    );
}

#[test]
fn applies_explicit_header_mapping_but_keeps_schema_strict() {
    let mapping = ColumnMapping::new([("工号", "teacher_code"), ("教师姓名", "name")]);
    let config = isolated_config().with_mapping(DatasetKind::Teachers, mapping);
    let sources = [CsvSource::new(
        DatasetKind::Teachers,
        "工号,教师姓名\nT1,王老师\n".as_bytes(),
    )];
    let batch = CsvImporter::new(config).import(sources).unwrap();
    assert_eq!(batch.teachers()[0].teacher_code, "T1");

    let failure = CsvImporter::new(isolated_config())
        .import([CsvSource::new(
            DatasetKind::Teachers,
            "teacher_code,name,unexpected\nT1,王老师,value\n".as_bytes(),
        )])
        .unwrap_err();
    let problem = failure
        .problems()
        .iter()
        .find(|problem| problem.code() == ImportProblemCode::ImportUnknownHeader)
        .unwrap();
    assert_eq!(problem.location().row_number(), Some(1));
    assert_eq!(problem.location().column(), Some("unexpected"));
}

#[test]
fn rejects_invalid_and_ambiguous_header_mappings() {
    let invalid = isolated_config().with_mapping(
        DatasetKind::Teachers,
        ColumnMapping::new([("missing_source", "teacher_code")]),
    );
    let failure = CsvImporter::new(invalid)
        .import([CsvSource::new(
            DatasetKind::Teachers,
            "teacher_code,name\nT1,王老师\n".as_bytes(),
        )])
        .unwrap_err();
    assert!(codes(failure.problems()).contains(&ImportProblemCode::ImportInvalidColumnMapping));

    let ambiguous = isolated_config().with_mapping(
        DatasetKind::Teachers,
        ColumnMapping::new([("工号", "teacher_code")]),
    );
    let failure = CsvImporter::new(ambiguous)
        .import([CsvSource::new(
            DatasetKind::Teachers,
            "teacher_code,工号,name\nT1,T2,王老师\n".as_bytes(),
        )])
        .unwrap_err();
    let problem = failure
        .problems()
        .iter()
        .find(|problem| problem.code() == ImportProblemCode::ImportDuplicateMappedField)
        .unwrap();
    assert_eq!(problem.location().row_number(), Some(1));
    assert_eq!(problem.location().column(), Some("teacher_code"));
}

#[test]
fn reports_malformed_csv_and_invalid_utf8_without_partial_success() {
    let malformed = CsvImporter::new(isolated_config())
        .import([CsvSource::new(
            DatasetKind::Teachers,
            "teacher_code,name\nT1\nT2,李老师\n".as_bytes(),
        )])
        .unwrap_err();
    let syntax = malformed
        .problems()
        .iter()
        .find(|problem| problem.code() == ImportProblemCode::ImportCsvSyntax)
        .unwrap();
    assert_eq!(syntax.location().dataset_kind(), DatasetKind::Teachers);
    assert_eq!(syntax.location().row_number(), Some(2));

    let invalid_utf8 = CsvImporter::new(isolated_config())
        .import([CsvSource::new(DatasetKind::Teachers, b"\xff\xfe")])
        .unwrap_err();
    assert_eq!(
        invalid_utf8.problems()[0].code(),
        ImportProblemCode::ImportInvalidUtf8
    );
    assert_eq!(invalid_utf8.problems()[0].location().row_number(), None);
}

#[test]
fn aggregates_duplicate_dataset_code_and_relation_problems() {
    let teachers = "teacher_code,name\nT1,王老师\nT1,另一位老师\n";
    let unavailable = "teacher_code,day,period\nT1,周一,1\nT1,周一,1\n";
    let failure = CsvImporter::new(isolated_config())
        .import([
            CsvSource::new(DatasetKind::Teachers, teachers.as_bytes()),
            CsvSource::new(DatasetKind::Teachers, teachers.as_bytes()),
            CsvSource::new(DatasetKind::TeacherUnavailability, unavailable.as_bytes()),
        ])
        .unwrap_err();
    let found = codes(failure.problems());
    assert!(found.contains(&ImportProblemCode::ImportDuplicateDataset));
    assert!(found.contains(&ImportProblemCode::ImportDuplicateExternalCode));
    assert!(found.contains(&ImportProblemCode::ImportDuplicateRelation));
}

#[test]
fn reports_duplicate_choice_and_exact_choice_count() {
    let mut bundle = Bundle::valid();
    bundle.replace(
        DatasetKind::StudentSubjectChoices,
        "student_code,subject_code\nS001,physics\nS001,physics\nS001,chemistry\n",
    );
    let problems = bundle.import(ImportConfig::default()).unwrap_err();
    let duplicate = problems
        .iter()
        .find(|problem| problem.code() == ImportProblemCode::ImportDuplicateSubjectChoice)
        .unwrap();
    assert_eq!(duplicate.location().row_number(), Some(3));
    assert_eq!(duplicate.related_row(), Some(2));
    let count = problems
        .iter()
        .find(|problem| problem.code() == ImportProblemCode::ImportSubjectChoiceCount)
        .unwrap();
    assert_eq!(count.expected(), Some(3));
    assert_eq!(count.actual(), Some(2));
}

#[test]
fn missing_references_are_reported_at_the_referencing_cell() {
    let failure = CsvImporter::new(isolated_config())
        .import([
            CsvSource::new(DatasetKind::Teachers, b"teacher_code,name\nT1,Teacher\n"),
            CsvSource::new(
                DatasetKind::TeacherUnavailability,
                b"teacher_code,day,period\nUNKNOWN,monday,1\n",
            ),
        ])
        .unwrap_err();
    let problem = failure
        .problems()
        .iter()
        .find(|problem| problem.code() == ImportProblemCode::ImportMissingReference)
        .unwrap();
    assert_eq!(
        problem.location().dataset_kind(),
        DatasetKind::TeacherUnavailability
    );
    assert_eq!(problem.location().row_number(), Some(2));
    assert_eq!(problem.location().column(), Some("teacher_code"));
}

#[test]
fn rejects_invalid_capacity_features_pattern_and_fixed_activity() {
    let mut bundle = Bundle::valid();
    bundle.replace(
        DatasetKind::Rooms,
        concat!(
            "room_code,name,building_code,capacity,features\n",
            "R1,高三1班教室,B1,0,multimedia\n",
            "R2,物理实验室,B2,30,physics_lab;physics_lab\n",
            "R3,理科教室,B2,30,multimedia\n",
        ),
    );
    bundle.replace(
        DatasetKind::CoursePlans,
        concat!(
            "course_plan_code,name,grade_code,subject_code,audience_kind,weekly_periods,meeting_pattern,min_days_between,max_periods_per_day,may_cross_breaks,required_room_features,teacher_assignment,teacher_codes,room_policy,room_candidates,preferred_rooms,fallback_rooms\n",
            "CP-P,物理,G12,physics,teaching_section,2,2;1,1,2,false,physics_lab,fixed,T1,,,,\n",
            "CP-C,化学,G12,chemistry,teaching_section,2,2,0,2,false,,fixed,T2,fixed,R3,,\n",
            "CP-B,生物,G12,biology,teaching_section,2,1;1,1,1,false,,candidates,T3,flexible,R2;R3,,\n",
            "CP-Z,语文,G12,chinese,administrative_class,5,1;1;1;1;1,1,1,false,,fixed,T4,,,,\n",
        ),
    );
    bundle.replace(
        DatasetKind::FixedActivities,
        concat!(
            "course_plan_code,audience_kind,audience_code,meeting_ordinal,day,period,duration,room_code,teacher_code\n",
            "CP-P,administrative_class,AC01,1,周一,1,1,R2,T1\n",
        ),
    );
    let problems = bundle.import(ImportConfig::default()).unwrap_err();
    let found = codes(&problems);
    assert!(found.contains(&ImportProblemCode::ImportInvalidUnsignedInteger));
    assert!(found.contains(&ImportProblemCode::ImportInvalidList));
    assert!(found.contains(&ImportProblemCode::ImportInvalidMeetingPattern));
    assert!(found.contains(&ImportProblemCode::ImportFixedActivityMismatch));
}

#[test]
fn optional_dataset_is_not_confused_with_a_required_core_dataset() {
    let mut bundle = Bundle::valid();
    bundle.files.remove(&DatasetKind::TeacherUnavailability);
    let batch = bundle.import(ImportConfig::default()).unwrap();
    assert!(batch.teacher_unavailability().is_empty());

    bundle.files.remove(&DatasetKind::Students);
    let problems = bundle.import(ImportConfig::default()).unwrap_err();
    assert!(codes(&problems).contains(&ImportProblemCode::ImportMissingDataset));
}

#[test]
fn validates_course_offering_audience_and_uniqueness() {
    let mut bundle = Bundle::valid();
    bundle.replace(
        DatasetKind::CourseOfferings,
        concat!(
            "course_plan_code,audience_kind,audience_code,teacher_assignment,teacher_codes\n",
            "CP-Z,teaching_section,SEC-P,fixed,T4\n",
            "CP-Z,teaching_section,SEC-P,fixed,T4\n",
        ),
    );
    let problems = bundle.import(ImportConfig::default()).unwrap_err();
    let found = codes(&problems);
    assert!(found.contains(&ImportProblemCode::ImportCourseOfferingMismatch));
    assert!(found.contains(&ImportProblemCode::ImportDuplicateRelation));
}
