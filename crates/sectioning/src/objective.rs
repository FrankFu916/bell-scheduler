use std::collections::{BTreeMap, BTreeSet};

use class_schedule_domain::{GradeId, StudentId, SubjectId, TeachingSectionId};

use crate::{
    SectionResourceRecommendation, SectioningInput, SectioningObjective, StudentSectionAssignment,
    TimetableFeasibilityProxy,
};

pub(crate) fn compute_objective(
    input: &SectioningInput,
    assignments: &[StudentSectionAssignment],
    resources: &[SectionResourceRecommendation],
) -> SectioningObjective {
    let sections = input
        .sections
        .iter()
        .map(|section| (section.section_id, section))
        .collect::<BTreeMap<_, _>>();
    let mut counts = input
        .sections
        .iter()
        .map(|section| (section.section_id, 0_u32))
        .collect::<BTreeMap<_, _>>();
    let mut members: BTreeMap<TeachingSectionId, BTreeSet<StudentId>> = BTreeMap::new();
    for assignment in assignments {
        *counts.entry(assignment.section_id).or_default() += 1;
        members
            .entry(assignment.section_id)
            .or_default()
            .insert(assignment.student_id);
    }

    let target_size_deviation = input
        .sections
        .iter()
        .map(|section| u64::from(counts[&section.section_id].abs_diff(section.target_size)))
        .sum();

    let mut sizes_by_subject: BTreeMap<(GradeId, SubjectId), Vec<u32>> = BTreeMap::new();
    for section in &input.sections {
        sizes_by_subject
            .entry((section.grade_id, section.subject_id))
            .or_default()
            .push(counts[&section.section_id]);
    }
    let size_imbalance = sizes_by_subject
        .values()
        .map(|sizes| {
            let minimum = sizes.iter().copied().min().unwrap_or_default();
            let maximum = sizes.iter().copied().max().unwrap_or_default();
            u64::from(maximum - minimum)
        })
        .sum();

    let concentrated_audience_overlap = input
        .sections
        .iter()
        .enumerate()
        .flat_map(|(left_index, left)| {
            input
                .sections
                .iter()
                .skip(left_index + 1)
                .map(move |right| (left, right))
        })
        .filter(|(left, right)| {
            left.grade_id == right.grade_id && left.subject_id != right.subject_id
        })
        .map(|(left, right)| {
            let overlap = members.get(&left.section_id).map_or(0, |left_members| {
                members.get(&right.section_id).map_or(0, |right_members| {
                    left_members.intersection(right_members).count()
                })
            });
            let overlap = u64::try_from(overlap).expect("materialized student count fits u64");
            overlap * overlap
        })
        .sum();

    let mut resource_availability_penalty = 0_u64;
    let mut room_capacity_tightness = 0_u64;
    for resource in resources {
        let Some(section) = sections.get(&resource.section_id) else {
            continue;
        };
        if let Some(teacher) = section
            .candidate_teachers
            .iter()
            .find(|candidate| candidate.teacher_id == resource.teacher_id)
        {
            resource_availability_penalty += u64::from(teacher.availability_penalty);
        }
        if let Some(room) = section
            .candidate_rooms
            .iter()
            .find(|candidate| candidate.room_id == resource.room_id)
        {
            resource_availability_penalty += u64::from(room.availability_penalty);
            room_capacity_tightness +=
                u64::from(room.capacity.saturating_sub(resource.assigned_size));
        }
    }

    SectioningObjective {
        target_size_deviation,
        size_imbalance,
        timetable_feasibility: TimetableFeasibilityProxy {
            concentrated_audience_overlap,
            resource_availability_penalty,
            room_capacity_tightness,
        },
    }
}
