use std::fs;
use std::path::PathBuf;

use class_schedule_application::{CalendarDefinition, compile_import_batch};
use class_schedule_import::{CsvImporter, CsvSource, DatasetKind, ImportConfig};
use class_schedule_validation::static_feasibility_check;

const DATASETS: [DatasetKind; 11] = [
    DatasetKind::Students,
    DatasetKind::AdministrativeClasses,
    DatasetKind::StudentSubjectChoices,
    DatasetKind::Teachers,
    DatasetKind::TeacherUnavailability,
    DatasetKind::Rooms,
    DatasetKind::CoursePlans,
    DatasetKind::TeachingSections,
    DatasetKind::SectionEnrollments,
    DatasetKind::CourseOfferings,
    DatasetKind::FixedActivities,
];

#[test]
fn checked_in_small_fixture_compiles_and_passes_static_feasibility() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/small");
    let files = DATASETS
        .iter()
        .map(|kind| {
            let path = fixture.join(format!("{}.csv", kind.as_str()));
            (
                *kind,
                fs::read(&path).unwrap_or_else(|error| panic!("{path:?}: {error}")),
            )
        })
        .collect::<Vec<_>>();
    let batch = CsvImporter::new(ImportConfig::default())
        .import(
            files
                .iter()
                .map(|(kind, bytes)| CsvSource::new(*kind, bytes)),
        )
        .unwrap_or_else(|failure| panic!("fixture import failed: {:?}", failure.problems()));
    let calendar = CalendarDefinition::weekday_with_break(8, 4).unwrap();
    let compiled = compile_import_batch(&batch, &calendar, "fixture-small").unwrap();

    assert_eq!(compiled.problem.students().len(), 24);
    assert_eq!(compiled.problem.teachers().len(), 16);
    assert_eq!(compiled.problem.rooms().len(), 10);
    assert_eq!(compiled.problem.timeslots().len(), 40);
    assert_eq!(compiled.problem.activities().len(), 34);
    assert_eq!(compiled.problem.meeting_patterns().len(), 12);
    assert_eq!(compiled.problem.locks().len(), 3);
    assert!(static_feasibility_check(&compiled.problem).is_valid());

    let representative_sections = [
        "SEC-PHY-1",
        "SEC-CHEM-1",
        "SEC-BIO-1",
        "SEC-HIS-1",
        "SEC-GEO-1",
        "SEC-POL-1",
    ]
    .map(|section| {
        compiled
            .catalog
            .activities
            .iter()
            .position(|activity| activity.audience_code == section && activity.meeting_ordinal == 1)
            .unwrap_or_else(|| panic!("missing section {section}"))
    });
    let conflicts = compiled.problem.student_conflict_edges();
    for (left_position, left) in representative_sections.iter().enumerate() {
        for right in representative_sections.iter().skip(left_position + 1) {
            assert!(conflicts.iter().any(|(first, second)| {
                let pair = (first.as_usize(), second.as_usize());
                pair == (*left, *right) || pair == (*right, *left)
            }));
        }
    }
}
