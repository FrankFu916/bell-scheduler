use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use class_schedule_application::{
    AutoSectioningPolicy, CalendarDefinition, SectioningProfile,
    compile_import_batch_with_sectioning, prepare_auto_sectioning,
};
use class_schedule_import::{
    CsvImporter, CsvSource, DatasetKind, ImportBatch, ImportConfig, ImportedAudienceKind,
    ImportedRoomPolicy,
};
use class_schedule_validation::static_feasibility_check;

const CORE_DATASETS: [DatasetKind; 7] = [
    DatasetKind::Students,
    DatasetKind::AdministrativeClasses,
    DatasetKind::StudentSubjectChoices,
    DatasetKind::Teachers,
    DatasetKind::TeacherUnavailability,
    DatasetKind::Rooms,
    DatasetKind::CoursePlans,
];

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/small")
}

fn load_unsectioned_fixture() -> ImportBatch {
    let root = fixture_root();
    let bytes = CORE_DATASETS
        .iter()
        .map(|kind| {
            (
                *kind,
                fs::read(root.join(format!("{}.csv", kind.as_str())))
                    .expect("checked-in fixture file"),
            )
        })
        .collect::<Vec<_>>();
    CsvImporter::new(ImportConfig::default().with_exact_subject_choices(Some(3)))
        .import(
            bytes
                .iter()
                .map(|(kind, bytes)| CsvSource::new(*kind, bytes)),
        )
        .unwrap_or_else(|failure| panic!("input-B fixture import: {:?}", failure.problems()))
}

#[test]
fn checked_in_small_core_fixture_sections_compiles_and_passes_precheck() {
    let batch = load_unsectioned_fixture();
    let policy = AutoSectioningPolicy::new(10, 12, 16, 20_260_904, SectioningProfile::Balanced, 3)
        .expect("valid policy");

    let prepared = prepare_auto_sectioning(&batch, "input-b-small", policy)
        .expect("sectioning must generate candidates");

    assert_eq!(prepared.candidates.len(), 3);
    let selected = &prepared.candidates[0];
    assert_eq!(selected.generated_sections().len(), 6);
    assert_eq!(selected.generated_enrollments().len(), 72);
    for section in selected.generated_sections() {
        let ImportedRoomPolicy::Fixed { room_code } = &section.room_policy else {
            panic!("materialized Input-B section must have a fixed room");
        };
        match section.subject_code.as_str() {
            "physics" => assert!(room_code.starts_with("R-PHY-")),
            "chemistry" => assert!(room_code.starts_with("R-CHEM-")),
            "biology" => assert!(room_code.starts_with("R-BIO-")),
            "history" | "politics" | "geography" => {
                assert!(room_code.starts_with("R-HUM-"));
            }
            subject => panic!("unexpected elective subject {subject}"),
        }
    }

    let section_subjects = selected
        .generated_sections()
        .iter()
        .map(|section| (section.section_code.as_str(), section.subject_code.as_str()))
        .collect::<BTreeMap<_, _>>();
    let expected = batch
        .student_subject_choices()
        .iter()
        .map(|choice| (choice.student_code.as_str(), choice.subject_code.as_str()))
        .collect::<BTreeSet<_>>();
    let actual = selected
        .generated_enrollments()
        .iter()
        .map(|enrollment| {
            (
                enrollment.student_code.as_str(),
                section_subjects[enrollment.section_code.as_str()],
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actual.iter().copied().collect::<BTreeSet<_>>(), expected);
    assert_eq!(actual.len(), expected.len());

    let compiled = compile_import_batch_with_sectioning(
        &batch,
        &CalendarDefinition::weekday_with_break(8, 4).expect("calendar"),
        "input-b-small",
        selected,
    )
    .expect("candidate compilation");
    assert_eq!(compiled.problem.activities().len(), 34);
    assert_eq!(
        compiled
            .catalog
            .activities
            .iter()
            .filter(|activity| { activity.audience_kind == ImportedAudienceKind::TeachingSection })
            .count(),
        12
    );
    assert!(static_feasibility_check(&compiled.problem).is_valid());
}
