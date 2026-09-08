use std::collections::{BTreeMap, BTreeSet};

use class_schedule_domain::{GradeId, StudentId, SubjectId, TeachingSectionId};

use crate::diagnostics::{DiagnosticSeverity, SectioningDiagnostic, SectioningProblemCode};
use crate::hash::{candidate_hash, input_hash, resource_tie_key, tie_key};
use crate::objective::compute_objective;
use crate::validate::{validate_candidate, validate_input};
use crate::{
    CandidateProvenance, GenerationParameters, SECTIONING_ALGORITHM_VERSION,
    SectionResourceRecommendation, SectionTemplate, SectioningCandidate, SectioningInput,
    SectioningProfile, SectioningResult, SectioningStatus, StudentSectionAssignment,
};

/// Generates independently validated, deterministic sectioning candidates.
#[must_use]
pub fn generate_candidates(
    input: &SectioningInput,
    parameters: GenerationParameters,
) -> SectioningResult {
    let digest = input_hash(input);
    let mut diagnostics = validate_input(input);
    if diagnostics
        .iter()
        .any(|problem| problem.severity == DiagnosticSeverity::Error)
    {
        let status = if diagnostics
            .iter()
            .filter(|problem| problem.severity == DiagnosticSeverity::Error)
            .all(|problem| is_infeasibility(problem.code))
        {
            SectioningStatus::ProvenInfeasible
        } else {
            SectioningStatus::InvalidInput
        };
        return SectioningResult {
            status,
            candidates: Vec::new(),
            diagnostics,
            provenance: run_provenance(parameters, digest, 0, 0),
        };
    }

    let attempt_limit = attempt_limit(parameters);
    let mut distinct = BTreeSet::new();
    let mut candidates = Vec::new();
    let mut attempts = 0_u32;
    for attempt in 0..attempt_limit {
        attempts += 1;
        let Some(mut candidate) = generate_one(input, parameters, digest, attempt) else {
            continue;
        };
        let key = candidate.provenance.candidate_hash;
        if distinct.insert(key) && validate_candidate(input, &candidate).is_valid() {
            // Recompute after validation to make the write order explicit and auditable.
            candidate.provenance.candidate_hash = candidate_hash(&candidate);
            candidates.push(candidate);
            if candidates.len() == usize::from(parameters.candidate_count()) {
                break;
            }
        }
    }
    candidates.sort_by_key(|candidate| (candidate.objective, candidate.provenance.candidate_hash));
    if candidates.len() < usize::from(parameters.candidate_count()) {
        diagnostics.push(
            SectioningDiagnostic::warning(
                SectioningProblemCode::CandidateCountLimitedByDistinctSolutions,
            )
            .with_counts(
                u64::from(parameters.candidate_count()),
                u64::from(parameters.candidate_count()),
                u64::from(u8::try_from(candidates.len()).unwrap_or(u8::MAX)),
            ),
        );
    }
    let status = if candidates.is_empty() {
        SectioningStatus::ProvenInfeasible
    } else {
        SectioningStatus::Generated
    };
    SectioningResult {
        status,
        provenance: run_provenance(
            parameters,
            digest,
            attempts,
            u8::try_from(candidates.len()).unwrap_or(u8::MAX),
        ),
        candidates,
        diagnostics,
    }
}

// Candidate construction keeps audience-overlap state beside assignment to make each tie-break
// deterministic and reviewable.
#[allow(clippy::too_many_lines)]
fn generate_one(
    input: &SectioningInput,
    parameters: GenerationParameters,
    digest: crate::SectioningDigest,
    attempt: u32,
) -> Option<SectioningCandidate> {
    let mut students_by_subject: BTreeMap<(GradeId, SubjectId), Vec<StudentId>> = BTreeMap::new();
    for student in &input.students {
        for subject in &student.selected_subject_ids {
            students_by_subject
                .entry((student.grade_id, *subject))
                .or_default()
                .push(student.student_id);
        }
    }
    let sections_by_subject = group_sections(input);
    let mut subject_order = students_by_subject.keys().copied().collect::<Vec<_>>();
    subject_order.sort_by_key(|(grade, subject)| {
        tie_key(
            parameters.seed(),
            attempt,
            *grade,
            *subject,
            StudentId::from_uuid(uuid::Uuid::nil()),
            None,
        )
    });

    let mut assignments = Vec::new();
    let mut student_prior_sections: BTreeMap<StudentId, Vec<TeachingSectionId>> = BTreeMap::new();
    let mut intersection_counts: BTreeMap<(TeachingSectionId, TeachingSectionId), u32> =
        BTreeMap::new();
    for (grade, subject) in subject_order {
        let sections = sections_by_subject.get(&(grade, subject))?;
        let students = students_by_subject.get_mut(&(grade, subject))?;
        students.sort_by_key(|student| {
            tie_key(parameters.seed(), attempt, grade, subject, *student, None)
        });
        let quotas = allocation_counts(
            sections,
            u32::try_from(students.len()).ok()?,
            parameters,
            attempt,
        )?;
        let mut used: BTreeMap<TeachingSectionId, u32> = BTreeMap::new();
        for student in students.iter().copied() {
            let prior = student_prior_sections
                .get(&student)
                .cloned()
                .unwrap_or_default();
            let selected = sections
                .iter()
                .filter(|section| {
                    used.get(&section.section_id).copied().unwrap_or_default()
                        < quotas[&section.section_id]
                })
                .min_by_key(|section| {
                    let overlap_cost = prior
                        .iter()
                        .map(|prior_section| {
                            let pair = ordered_pair(section.section_id, *prior_section);
                            2_u64
                                * u64::from(
                                    intersection_counts.get(&pair).copied().unwrap_or_default(),
                                )
                                + 1
                        })
                        .sum::<u64>();
                    (
                        overlap_cost,
                        tie_key(
                            parameters.seed(),
                            attempt,
                            grade,
                            subject,
                            student,
                            Some(section.section_id),
                        ),
                    )
                })?;
            *used.entry(selected.section_id).or_default() += 1;
            for prior_section in &prior {
                *intersection_counts
                    .entry(ordered_pair(selected.section_id, *prior_section))
                    .or_default() += 1;
            }
            student_prior_sections
                .entry(student)
                .or_default()
                .push(selected.section_id);
            assignments.push(StudentSectionAssignment {
                student_id: student,
                subject_id: subject,
                section_id: selected.section_id,
            });
        }
    }
    assignments.sort_unstable();
    let counts = assignments
        .iter()
        .fold(BTreeMap::new(), |mut values, assignment| {
            *values.entry(assignment.section_id).or_insert(0_u32) += 1;
            values
        });
    let mut resources = Vec::with_capacity(input.sections.len());
    for section in &input.sections {
        resources.push(select_resources(
            section,
            counts.get(&section.section_id).copied().unwrap_or_default(),
            parameters,
            attempt,
        )?);
    }
    resources.sort_by_key(|resource| resource.section_id);
    let objective = compute_objective(input, &assignments, &resources);
    let mut candidate = SectioningCandidate {
        assignments,
        resource_recommendations: resources,
        objective,
        provenance: CandidateProvenance {
            algorithm_version: SECTIONING_ALGORITHM_VERSION.to_owned(),
            input_hash: digest,
            candidate_hash: crate::SectioningDigest([0; 32]),
            seed: parameters.seed(),
            profile: parameters.profile(),
            attempt_index: attempt,
            accepted_local_swaps: 0,
        },
    };
    candidate.provenance.candidate_hash = candidate_hash(&candidate);
    Some(candidate)
}

fn group_sections(
    input: &SectioningInput,
) -> BTreeMap<(GradeId, SubjectId), Vec<&SectionTemplate>> {
    let mut values: BTreeMap<(GradeId, SubjectId), Vec<&SectionTemplate>> = BTreeMap::new();
    for section in &input.sections {
        values
            .entry((section.grade_id, section.subject_id))
            .or_default()
            .push(section);
    }
    values
}

fn allocation_counts(
    sections: &[&SectionTemplate],
    student_count: u32,
    parameters: GenerationParameters,
    attempt: u32,
) -> Option<BTreeMap<TeachingSectionId, u32>> {
    let effective_max = |section: &SectionTemplate| {
        section.max_size.min(
            section
                .candidate_rooms
                .iter()
                .map(|room| room.capacity)
                .max()
                .unwrap_or_default(),
        )
    };
    let mut counts = sections
        .iter()
        .map(|section| (section.section_id, section.min_size))
        .collect::<BTreeMap<_, _>>();
    let minimum: u32 = sections.iter().map(|section| section.min_size).sum();
    let mut remaining = student_count.checked_sub(minimum)?;
    while remaining > 0 {
        let section = sections
            .iter()
            .filter(|section| counts[&section.section_id] < effective_max(section))
            .min_by_key(|section| {
                let current = counts[&section.section_id];
                let next = current + 1;
                let current_deviation = current.abs_diff(section.target_size);
                let next_deviation = next.abs_diff(section.target_size);
                let marginal = i64::from(next_deviation) - i64::from(current_deviation);
                (
                    marginal,
                    next,
                    tie_key(
                        parameters.seed(),
                        attempt,
                        section.grade_id,
                        section.subject_id,
                        StudentId::from_uuid(uuid::Uuid::nil()),
                        Some(section.section_id),
                    ),
                )
            })?;
        *counts
            .get_mut(&section.section_id)
            .expect("section initialized") += 1;
        remaining -= 1;
    }
    Some(counts)
}

fn select_resources(
    section: &SectionTemplate,
    assigned_size: u32,
    parameters: GenerationParameters,
    attempt: u32,
) -> Option<SectionResourceRecommendation> {
    let teacher = section.candidate_teachers.iter().min_by_key(|teacher| {
        (
            teacher.availability_penalty,
            resource_tie_key(
                parameters.seed(),
                attempt,
                teacher.teacher_id.as_uuid().as_bytes(),
            ),
        )
    })?;
    let room = section
        .candidate_rooms
        .iter()
        .filter(|room| room.capacity >= assigned_size)
        .min_by_key(|room| {
            (
                room.availability_penalty,
                room.capacity - assigned_size,
                resource_tie_key(
                    parameters.seed(),
                    attempt,
                    room.room_id.as_uuid().as_bytes(),
                ),
            )
        })?;
    Some(SectionResourceRecommendation {
        section_id: section.section_id,
        assigned_size,
        teacher_id: teacher.teacher_id,
        room_id: room.room_id,
    })
}

fn run_provenance(
    parameters: GenerationParameters,
    input_hash: crate::SectioningDigest,
    attempts: u32,
    generated_candidates: u8,
) -> crate::SectioningRunProvenance {
    crate::SectioningRunProvenance {
        algorithm_version: SECTIONING_ALGORITHM_VERSION.to_owned(),
        input_hash,
        seed: parameters.seed(),
        profile: parameters.profile(),
        requested_candidates: parameters.candidate_count(),
        generated_candidates,
        attempts,
    }
}

const fn is_infeasibility(code: SectioningProblemCode) -> bool {
    matches!(
        code,
        SectioningProblemCode::SubjectMinimumCapacityExceeded
            | SectioningProblemCode::SubjectMaximumCapacityInsufficient
            | SectioningProblemCode::SectionRoomCapacityInsufficient
    )
}

fn ordered_pair(
    left: TeachingSectionId,
    right: TeachingSectionId,
) -> (TeachingSectionId, TeachingSectionId) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn attempt_limit(parameters: GenerationParameters) -> u32 {
    let multiplier = match parameters.profile() {
        SectioningProfile::Fast => 2,
        SectioningProfile::Balanced => 8,
        SectioningProfile::BestQuality => 24,
    };
    u32::from(parameters.candidate_count()) * multiplier
}
