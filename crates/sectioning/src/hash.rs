use crate::{
    SectionResourceRecommendation, SectioningCandidate, SectioningDigest, SectioningInput,
    SectioningObjective,
};
use class_schedule_domain::{GradeId, StudentId, SubjectId, TeachingSectionId};

fn put_u32(hasher: &mut blake3::Hasher, value: u32) {
    hasher.update(&value.to_le_bytes());
}

fn put_u64(hasher: &mut blake3::Hasher, value: u64) {
    hasher.update(&value.to_le_bytes());
}

pub(crate) fn input_hash(input: &SectioningInput) -> SectioningDigest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"class-schedule-sectioning-input-v2\0");

    let mut students = input.students.clone();
    students.sort_by_key(|student| student.student_id);
    put_u64(&mut hasher, students.len() as u64);
    for student in students {
        hasher.update(student.student_id.as_uuid().as_bytes());
        hasher.update(student.grade_id.as_uuid().as_bytes());
        let mut subjects = student.selected_subject_ids;
        subjects.sort_unstable();
        put_u64(&mut hasher, subjects.len() as u64);
        for subject in subjects {
            hasher.update(subject.as_uuid().as_bytes());
        }
    }

    let mut sections = input.sections.clone();
    sections.sort_by_key(|section| section.section_id);
    put_u64(&mut hasher, sections.len() as u64);
    for section in sections {
        hasher.update(section.section_id.as_uuid().as_bytes());
        hasher.update(section.grade_id.as_uuid().as_bytes());
        hasher.update(section.subject_id.as_uuid().as_bytes());
        put_u32(&mut hasher, section.min_size);
        put_u32(&mut hasher, section.target_size);
        put_u32(&mut hasher, section.max_size);

        let mut teachers = section.candidate_teachers;
        teachers.sort_by_key(|candidate| candidate.teacher_id);
        put_u64(&mut hasher, teachers.len() as u64);
        for teacher in teachers {
            hasher.update(teacher.teacher_id.as_uuid().as_bytes());
            put_u32(&mut hasher, teacher.availability_penalty);
        }

        let mut rooms = section.candidate_rooms;
        rooms.sort_by_key(|candidate| candidate.room_id);
        put_u64(&mut hasher, rooms.len() as u64);
        for room in rooms {
            hasher.update(room.room_id.as_uuid().as_bytes());
            put_u32(&mut hasher, room.capacity);
            put_u32(&mut hasher, room.availability_penalty);
        }
    }
    SectioningDigest(*hasher.finalize().as_bytes())
}

pub(crate) fn candidate_hash(candidate: &SectioningCandidate) -> SectioningDigest {
    hash_candidate_parts(
        &candidate.assignments,
        &candidate.resource_recommendations,
        candidate.objective,
    )
}

pub(crate) fn hash_candidate_parts(
    assignments: &[crate::StudentSectionAssignment],
    resources: &[SectionResourceRecommendation],
    objective: SectioningObjective,
) -> SectioningDigest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"class-schedule-sectioning-candidate-v1\0");
    let mut assignments = assignments.to_vec();
    assignments.sort_unstable();
    put_u64(&mut hasher, assignments.len() as u64);
    for assignment in assignments {
        hasher.update(assignment.student_id.as_uuid().as_bytes());
        hasher.update(assignment.subject_id.as_uuid().as_bytes());
        hasher.update(assignment.section_id.as_uuid().as_bytes());
    }
    let mut resources = resources.to_vec();
    resources.sort_by_key(|resource| resource.section_id);
    put_u64(&mut hasher, resources.len() as u64);
    for resource in resources {
        hasher.update(resource.section_id.as_uuid().as_bytes());
        put_u32(&mut hasher, resource.assigned_size);
        hasher.update(resource.teacher_id.as_uuid().as_bytes());
        hasher.update(resource.room_id.as_uuid().as_bytes());
    }
    put_u64(&mut hasher, objective.target_size_deviation);
    put_u64(&mut hasher, objective.size_imbalance);
    put_u64(
        &mut hasher,
        objective
            .timetable_feasibility
            .concentrated_audience_overlap,
    );
    put_u64(
        &mut hasher,
        objective
            .timetable_feasibility
            .resource_availability_penalty,
    );
    put_u64(
        &mut hasher,
        objective.timetable_feasibility.room_capacity_tightness,
    );
    SectioningDigest(*hasher.finalize().as_bytes())
}

pub(crate) fn tie_key(
    seed: u64,
    attempt: u32,
    grade_id: GradeId,
    subject_id: SubjectId,
    student_id: StudentId,
    section_id: Option<TeachingSectionId>,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"class-schedule-sectioning-tie-v2\0");
    hasher.update(&seed.to_le_bytes());
    hasher.update(&attempt.to_le_bytes());
    hasher.update(grade_id.as_uuid().as_bytes());
    hasher.update(subject_id.as_uuid().as_bytes());
    hasher.update(student_id.as_uuid().as_bytes());
    if let Some(section_id) = section_id {
        hasher.update(section_id.as_uuid().as_bytes());
    }
    *hasher.finalize().as_bytes()
}

pub(crate) fn resource_tie_key(seed: u64, attempt: u32, id: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"class-schedule-sectioning-resource-tie-v1\0");
    hasher.update(&seed.to_le_bytes());
    hasher.update(&attempt.to_le_bytes());
    hasher.update(id);
    *hasher.finalize().as_bytes()
}
