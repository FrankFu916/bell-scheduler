use class_schedule_domain::{GradeId, RoomId, StudentId, SubjectId, TeacherId, TeachingSectionId};
use class_schedule_sectioning::{
    GenerationParameters, RoomCandidate, SectionTemplate, SectioningInput, SectioningProblemCode,
    SectioningProfile, SectioningStatus, StudentChoice, TeacherCandidate, generate_candidates,
    validate_candidate,
};
use proptest::prelude::*;
use uuid::Uuid;

fn student(value: u128) -> StudentId {
    StudentId::from_uuid(Uuid::from_u128(value))
}

fn subject(value: u128) -> SubjectId {
    SubjectId::from_uuid(Uuid::from_u128(value))
}

fn grade(value: u128) -> GradeId {
    GradeId::from_uuid(Uuid::from_u128(value))
}

fn section(value: u128) -> TeachingSectionId {
    TeachingSectionId::from_uuid(Uuid::from_u128(value))
}

fn template(
    section_value: u128,
    subject_id: SubjectId,
    min_size: u32,
    target_size: u32,
    max_size: u32,
) -> SectionTemplate {
    SectionTemplate {
        section_id: section(section_value),
        grade_id: grade(10),
        subject_id,
        min_size,
        target_size,
        max_size,
        candidate_teachers: vec![TeacherCandidate {
            teacher_id: TeacherId::from_uuid(Uuid::from_u128(1_000 + section_value)),
            availability_penalty: u32::try_from(section_value % 3).unwrap(),
        }],
        candidate_rooms: vec![RoomCandidate {
            room_id: RoomId::from_uuid(Uuid::from_u128(2_000 + section_value)),
            capacity: max_size,
            availability_penalty: u32::try_from(section_value % 2).unwrap(),
        }],
    }
}

fn fixture(student_count: u32) -> SectioningInput {
    let physics = subject(100);
    let chemistry = subject(101);
    SectioningInput {
        students: (0..student_count)
            .map(|index| StudentChoice {
                student_id: student(10_000 + u128::from(index)),
                grade_id: grade(10),
                selected_subject_ids: vec![physics, chemistry],
            })
            .collect(),
        sections: vec![
            template(200, physics, 1, student_count.div_ceil(2), student_count),
            template(201, physics, 1, student_count / 2, student_count),
            template(202, chemistry, 1, student_count.div_ceil(2), student_count),
            template(203, chemistry, 1, student_count / 2, student_count),
        ],
    }
}

#[test]
fn generates_reproducible_independently_validated_candidates() {
    let input = fixture(8);
    let parameters = GenerationParameters::new(42, SectioningProfile::Balanced, 3).unwrap();
    let first = generate_candidates(&input, parameters);
    let second = generate_candidates(&input, parameters);

    assert_eq!(first, second);
    assert_eq!(first.status, SectioningStatus::Generated);
    assert!(!first.candidates.is_empty());
    for candidate in &first.candidates {
        assert!(validate_candidate(&input, candidate).is_valid());
        assert_eq!(candidate.assignments.len(), 16);
    }
}

#[test]
fn distinguishes_invalid_input_from_proven_capacity_infeasibility() {
    let mut invalid = fixture(4);
    invalid.students.push(invalid.students[0].clone());
    let invalid_result = generate_candidates(
        &invalid,
        GenerationParameters::new(1, SectioningProfile::Fast, 1).unwrap(),
    );
    assert_eq!(invalid_result.status, SectioningStatus::InvalidInput);
    assert!(
        invalid_result
            .diagnostics
            .iter()
            .any(|problem| problem.code == SectioningProblemCode::DuplicateStudent)
    );

    let physics = subject(100);
    let infeasible = SectioningInput {
        students: (0_u32..5)
            .map(|index| StudentChoice {
                student_id: student(50_000 + u128::from(index)),
                grade_id: grade(10),
                selected_subject_ids: vec![physics],
            })
            .collect(),
        sections: vec![template(300, physics, 1, 2, 2)],
    };
    let result = generate_candidates(
        &infeasible,
        GenerationParameters::new(1, SectioningProfile::Fast, 1).unwrap(),
    );
    assert_eq!(result.status, SectioningStatus::ProvenInfeasible);
    assert!(result.diagnostics.iter().any(|problem| {
        problem.code == SectioningProblemCode::SubjectMaximumCapacityInsufficient
    }));
}

#[test]
fn independent_validator_detects_tampering() {
    let input = fixture(6);
    let mut candidate = generate_candidates(
        &input,
        GenerationParameters::new(7, SectioningProfile::Fast, 1).unwrap(),
    )
    .candidates
    .remove(0);
    candidate.assignments.remove(0);

    let report = validate_candidate(&input, &candidate);
    assert!(!report.is_valid());
    assert!(report.problems.iter().any(|problem| {
        problem.code == SectioningProblemCode::CandidateMissingAssignment
            || problem.code == SectioningProblemCode::CandidateSectionBelowMinimum
    }));
    assert!(report.problems.iter().any(|problem| {
        problem.code == SectioningProblemCode::CandidateOutputHashMismatch
            || problem.code == SectioningProblemCode::CandidateObjectiveMismatch
    }));
}

#[test]
fn same_subject_never_crosses_grade_boundaries() {
    let physics = subject(100);
    let grade_ten = grade(10);
    let grade_eleven = grade(11);
    let mut grade_ten_section = template(400, physics, 1, 1, 1);
    grade_ten_section.grade_id = grade_ten;
    let mut grade_eleven_section = template(401, physics, 1, 1, 1);
    grade_eleven_section.grade_id = grade_eleven;
    let input = SectioningInput {
        students: vec![
            StudentChoice {
                student_id: student(60_000),
                grade_id: grade_ten,
                selected_subject_ids: vec![physics],
            },
            StudentChoice {
                student_id: student(60_001),
                grade_id: grade_eleven,
                selected_subject_ids: vec![physics],
            },
        ],
        sections: vec![grade_ten_section, grade_eleven_section],
    };

    let mut candidate = generate_candidates(
        &input,
        GenerationParameters::new(9, SectioningProfile::Fast, 1).unwrap(),
    )
    .candidates
    .remove(0);

    assert!(validate_candidate(&input, &candidate).is_valid());
    assert!(candidate.assignments.iter().any(|assignment| {
        assignment.student_id == student(60_000) && assignment.section_id == section(400)
    }));
    assert!(candidate.assignments.iter().any(|assignment| {
        assignment.student_id == student(60_001) && assignment.section_id == section(401)
    }));

    let grade_ten_assignment = candidate
        .assignments
        .iter_mut()
        .find(|assignment| assignment.student_id == student(60_000))
        .expect("grade ten assignment");
    grade_ten_assignment.section_id = section(401);
    let report = validate_candidate(&input, &candidate);
    assert!(
        report
            .problems
            .iter()
            .any(|problem| { problem.code == SectioningProblemCode::CandidateGradeMismatch })
    );
}

proptest! {
    #[test]
    fn every_generated_small_candidate_passes_the_independent_validator(
        student_count in 4_u32..=16,
        seed in any::<u64>(),
    ) {
        let input = fixture(student_count);
        let result = generate_candidates(
            &input,
            GenerationParameters::new(seed, SectioningProfile::Fast, 1).unwrap(),
        );
        prop_assert_eq!(result.status, SectioningStatus::Generated);
        prop_assert_eq!(result.candidates.len(), 1);
        prop_assert!(validate_candidate(&input, &result.candidates[0]).is_valid());
    }
}
