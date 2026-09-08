use std::collections::{BTreeMap, BTreeSet};

use class_schedule_domain::{GradeId, SubjectId, TeachingSectionId};

use crate::diagnostics::{
    CandidateValidationReport, DiagnosticSeverity, SectioningDiagnostic, SectioningProblemCode,
};
use crate::hash::{candidate_hash, input_hash};
use crate::objective::compute_objective;
use crate::{SECTIONING_ALGORITHM_VERSION, SectioningCandidate, SectioningInput};

// Input validation is intentionally one audit gate over students, templates and aggregate
// capacity; splitting it would obscure which feasibility checks ran before generation.
#[allow(clippy::too_many_lines)]
pub(crate) fn validate_input(input: &SectioningInput) -> Vec<SectioningDiagnostic> {
    let mut problems = Vec::new();
    if input.students.is_empty() {
        problems.push(SectioningDiagnostic::error(
            SectioningProblemCode::EmptyStudents,
        ));
    }
    if input.sections.is_empty() {
        problems.push(SectioningDiagnostic::error(
            SectioningProblemCode::EmptySections,
        ));
    }

    let mut students = BTreeSet::new();
    let mut demand_by_subject: BTreeMap<(GradeId, SubjectId), u64> = BTreeMap::new();
    for student in &input.students {
        if !students.insert(student.student_id) {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::DuplicateStudent)
                    .with_student(student.student_id),
            );
        }
        if student.selected_subject_ids.is_empty() {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::StudentHasNoSelectedSubject)
                    .with_student(student.student_id),
            );
        }
        let mut choices = BTreeSet::new();
        for subject in &student.selected_subject_ids {
            if choices.insert(*subject) {
                *demand_by_subject
                    .entry((student.grade_id, *subject))
                    .or_default() += 1;
            } else {
                problems.push(
                    SectioningDiagnostic::error(
                        SectioningProblemCode::DuplicateStudentSubjectChoice,
                    )
                    .with_student(student.student_id)
                    .with_subject(*subject),
                );
            }
        }
    }

    let mut section_ids = BTreeSet::new();
    let mut capacity_by_subject: BTreeMap<(GradeId, SubjectId), (u64, u64)> = BTreeMap::new();
    for section in &input.sections {
        if !section_ids.insert(section.section_id) {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::DuplicateSection)
                    .with_section(section.section_id),
            );
        }
        if section.min_size > section.target_size || section.target_size > section.max_size {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::InvalidSectionSizeBounds)
                    .with_section(section.section_id)
                    .with_counts(
                        u64::from(section.min_size),
                        u64::from(section.max_size),
                        u64::from(section.target_size),
                    ),
            );
        }
        validate_teacher_candidates(section, &mut problems);
        let effective_max = validate_room_candidates(section, &mut problems);
        let capacity = capacity_by_subject
            .entry((section.grade_id, section.subject_id))
            .or_default();
        capacity.0 += u64::from(section.min_size);
        capacity.1 += u64::from(effective_max.min(section.max_size));
    }

    for ((grade, subject), demand) in &demand_by_subject {
        let Some((minimum, maximum)) = capacity_by_subject.get(&(*grade, *subject)).copied() else {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::MissingSectionsForSubject)
                    .with_subject(*subject)
                    .with_counts(*demand, *demand, 0),
            );
            continue;
        };
        if minimum > *demand {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::SubjectMinimumCapacityExceeded)
                    .with_subject(*subject)
                    .with_counts(minimum, maximum, *demand),
            );
        }
        if maximum < *demand {
            problems.push(
                SectioningDiagnostic::error(
                    SectioningProblemCode::SubjectMaximumCapacityInsufficient,
                )
                .with_subject(*subject)
                .with_counts(minimum, maximum, *demand),
            );
        }
    }
    for (grade, subject) in capacity_by_subject.keys() {
        if !demand_by_subject.contains_key(&(*grade, *subject)) {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::SectionSubjectHasNoStudents)
                    .with_subject(*subject),
            );
        }
    }
    normalize(&mut problems);
    problems
}

fn validate_teacher_candidates(
    section: &crate::SectionTemplate,
    problems: &mut Vec<SectioningDiagnostic>,
) {
    if section.candidate_teachers.is_empty() {
        problems.push(
            SectioningDiagnostic::error(SectioningProblemCode::EmptyTeacherCandidates)
                .with_section(section.section_id),
        );
    }
    let mut teacher_ids = BTreeSet::new();
    for teacher in &section.candidate_teachers {
        if !teacher_ids.insert(teacher.teacher_id) {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::DuplicateTeacherCandidate)
                    .with_section(section.section_id),
            );
        }
    }
}

fn validate_room_candidates(
    section: &crate::SectionTemplate,
    problems: &mut Vec<SectioningDiagnostic>,
) -> u32 {
    if section.candidate_rooms.is_empty() {
        problems.push(
            SectioningDiagnostic::error(SectioningProblemCode::EmptyRoomCandidates)
                .with_section(section.section_id),
        );
        return 0;
    }
    let mut room_ids = BTreeSet::new();
    let mut maximum_capacity = 0_u32;
    for room in &section.candidate_rooms {
        if !room_ids.insert(room.room_id) {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::DuplicateRoomCandidate)
                    .with_section(section.section_id),
            );
        }
        if room.capacity == 0 {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::ZeroRoomCapacity)
                    .with_section(section.section_id),
            );
        }
        maximum_capacity = maximum_capacity.max(room.capacity);
    }
    if maximum_capacity < section.min_size {
        problems.push(
            SectioningDiagnostic::error(SectioningProblemCode::SectionRoomCapacityInsufficient)
                .with_section(section.section_id)
                .with_counts(
                    u64::from(section.min_size),
                    u64::from(section.max_size),
                    u64::from(maximum_capacity),
                ),
        );
    }
    maximum_capacity
}

/// Independently validates section membership, resources, objective, and provenance.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn validate_candidate(
    input: &SectioningInput,
    candidate: &SectioningCandidate,
) -> CandidateValidationReport {
    let input_problems = validate_input(input);
    if input_problems
        .iter()
        .any(|problem| problem.severity == DiagnosticSeverity::Error)
    {
        return CandidateValidationReport {
            problems: vec![SectioningDiagnostic::error(
                SectioningProblemCode::CandidateInputInvalid,
            )],
            recomputed_objective: None,
            recomputed_candidate_hash: None,
        };
    }

    let expected = input
        .students
        .iter()
        .flat_map(|student| {
            student
                .selected_subject_ids
                .iter()
                .map(move |subject| (student.student_id, *subject))
        })
        .collect::<BTreeSet<_>>();
    let student_grades = input
        .students
        .iter()
        .map(|student| (student.student_id, student.grade_id))
        .collect::<BTreeMap<_, _>>();
    let sections = input
        .sections
        .iter()
        .map(|section| (section.section_id, section))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let mut counts: BTreeMap<TeachingSectionId, u32> = BTreeMap::new();
    let mut problems = Vec::new();
    for assignment in &candidate.assignments {
        let key = (assignment.student_id, assignment.subject_id);
        if !expected.contains(&key) {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::CandidateUnexpectedAssignment)
                    .with_student(assignment.student_id)
                    .with_subject(assignment.subject_id),
            );
        }
        if !seen.insert(key) {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::CandidateDuplicateAssignment)
                    .with_student(assignment.student_id)
                    .with_subject(assignment.subject_id),
            );
        }
        match sections.get(&assignment.section_id) {
            None => problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::CandidateUnknownSection)
                    .with_section(assignment.section_id),
            ),
            Some(section) => {
                let subject_matches = section.subject_id == assignment.subject_id;
                if !subject_matches {
                    problems.push(
                        SectioningDiagnostic::error(
                            SectioningProblemCode::CandidateSubjectMismatch,
                        )
                        .with_student(assignment.student_id)
                        .with_subject(assignment.subject_id)
                        .with_section(assignment.section_id),
                    );
                }
                let grade_matches = student_grades
                    .get(&assignment.student_id)
                    .is_some_and(|grade| *grade == section.grade_id);
                if !grade_matches {
                    problems.push(
                        SectioningDiagnostic::error(SectioningProblemCode::CandidateGradeMismatch)
                            .with_student(assignment.student_id)
                            .with_grade(section.grade_id)
                            .with_section(assignment.section_id),
                    );
                }
                if subject_matches && grade_matches && expected.contains(&key) {
                    *counts.entry(assignment.section_id).or_default() += 1;
                }
            }
        }
    }
    for (student, subject) in expected.difference(&seen) {
        problems.push(
            SectioningDiagnostic::error(SectioningProblemCode::CandidateMissingAssignment)
                .with_student(*student)
                .with_subject(*subject),
        );
    }

    for section in &input.sections {
        let count = counts.get(&section.section_id).copied().unwrap_or_default();
        if count < section.min_size {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::CandidateSectionBelowMinimum)
                    .with_section(section.section_id)
                    .with_counts(
                        u64::from(section.min_size),
                        u64::from(section.max_size),
                        u64::from(count),
                    ),
            );
        }
        if count > section.max_size {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::CandidateSectionAboveMaximum)
                    .with_section(section.section_id)
                    .with_counts(
                        u64::from(section.min_size),
                        u64::from(section.max_size),
                        u64::from(count),
                    ),
            );
        }
    }
    validate_resources(input, candidate, &counts, &mut problems);

    let recomputed_objective = compute_objective(
        input,
        &candidate.assignments,
        &candidate.resource_recommendations,
    );
    if recomputed_objective != candidate.objective {
        problems.push(SectioningDiagnostic::error(
            SectioningProblemCode::CandidateObjectiveMismatch,
        ));
    }
    let recomputed_hash = candidate_hash(candidate);
    if candidate.provenance.input_hash != input_hash(input) {
        problems.push(SectioningDiagnostic::error(
            SectioningProblemCode::CandidateInputHashMismatch,
        ));
    }
    if candidate.provenance.candidate_hash != recomputed_hash {
        problems.push(SectioningDiagnostic::error(
            SectioningProblemCode::CandidateOutputHashMismatch,
        ));
    }
    if candidate.provenance.algorithm_version != SECTIONING_ALGORITHM_VERSION {
        problems.push(SectioningDiagnostic::error(
            SectioningProblemCode::CandidateAlgorithmVersionMismatch,
        ));
    }
    normalize(&mut problems);
    CandidateValidationReport {
        problems,
        recomputed_objective: Some(recomputed_objective),
        recomputed_candidate_hash: Some(recomputed_hash),
    }
}

fn validate_resources(
    input: &SectioningInput,
    candidate: &SectioningCandidate,
    counts: &BTreeMap<TeachingSectionId, u32>,
    problems: &mut Vec<SectioningDiagnostic>,
) {
    let sections = input
        .sections
        .iter()
        .map(|section| (section.section_id, section))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for resource in &candidate.resource_recommendations {
        let Some(section) = sections.get(&resource.section_id) else {
            problems.push(
                SectioningDiagnostic::error(
                    SectioningProblemCode::CandidateUnexpectedResourceRecommendation,
                )
                .with_section(resource.section_id),
            );
            continue;
        };
        if !seen.insert(resource.section_id) {
            problems.push(
                SectioningDiagnostic::error(
                    SectioningProblemCode::CandidateDuplicateResourceRecommendation,
                )
                .with_section(resource.section_id),
            );
        }
        let actual = counts
            .get(&resource.section_id)
            .copied()
            .unwrap_or_default();
        if resource.assigned_size != actual {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::CandidateObjectiveMismatch)
                    .with_section(resource.section_id)
                    .with_counts(
                        u64::from(actual),
                        u64::from(actual),
                        u64::from(resource.assigned_size),
                    ),
            );
        }
        if !section
            .candidate_teachers
            .iter()
            .any(|candidate| candidate.teacher_id == resource.teacher_id)
        {
            problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::CandidateTeacherNotAllowed)
                    .with_section(resource.section_id),
            );
        }
        match section
            .candidate_rooms
            .iter()
            .find(|candidate| candidate.room_id == resource.room_id)
        {
            None => problems.push(
                SectioningDiagnostic::error(SectioningProblemCode::CandidateRoomNotAllowed)
                    .with_section(resource.section_id),
            ),
            Some(room) if room.capacity < actual => problems.push(
                SectioningDiagnostic::error(
                    SectioningProblemCode::CandidateRoomCapacityInsufficient,
                )
                .with_section(resource.section_id)
                .with_counts(
                    u64::from(actual),
                    u64::from(actual),
                    u64::from(room.capacity),
                ),
            ),
            Some(_) => {}
        }
    }
    for section in &input.sections {
        if !seen.contains(&section.section_id) {
            problems.push(
                SectioningDiagnostic::error(
                    SectioningProblemCode::CandidateMissingResourceRecommendation,
                )
                .with_section(section.section_id),
            );
        }
    }
}

fn normalize(problems: &mut Vec<SectioningDiagnostic>) {
    problems.sort_by_key(|problem| {
        (
            problem.code,
            problem.student_id,
            problem.subject_id,
            problem.section_id,
            problem.expected_min,
            problem.expected_max,
            problem.actual,
        )
    });
    problems.dedup();
}
