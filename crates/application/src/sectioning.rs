use std::collections::{BTreeMap, BTreeSet};

use class_schedule_domain::{RoomId, StudentId, TeacherId, TeachingSectionId};
use class_schedule_import::{
    ImportBatch, ImportedAudienceKind, ImportedRoomPolicy, ImportedTeacherAssignment,
    SectionEnrollmentImportRow, TeachingSectionImportRow,
};
use class_schedule_sectioning::{
    CandidateValidationReport, GenerationParameters, RoomCandidate, SectionTemplate,
    SectioningCandidate, SectioningRunProvenance, SectioningStatus, StudentChoice,
    TeacherCandidate, generate_candidates, validate_candidate,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::compile::stable_id;

pub use class_schedule_sectioning::{
    DiagnosticSeverity, SectioningDiagnostic, SectioningDigest, SectioningObjective,
    SectioningProfile,
};

/// Explicit school policy used only when the import contains choices but no pre-built sections.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutoSectioningPolicy {
    pub minimum_size: u16,
    pub target_size: u16,
    pub maximum_size: u16,
    pub seed: u64,
    pub profile: SectioningProfile,
    pub candidate_count: u8,
}

impl AutoSectioningPolicy {
    /// Creates a bounded auto-sectioning policy.
    ///
    /// # Errors
    ///
    /// Rejects zero/inverted class-size bounds or an unsupported candidate count.
    pub fn new(
        minimum_size: u16,
        target_size: u16,
        maximum_size: u16,
        seed: u64,
        profile: SectioningProfile,
        candidate_count: u8,
    ) -> Result<Self, AutoSectioningError> {
        if minimum_size == 0
            || minimum_size > target_size
            || target_size > maximum_size
            || maximum_size == 0
        {
            return Err(AutoSectioningError::InvalidSizePolicy {
                minimum_size,
                target_size,
                maximum_size,
            });
        }
        GenerationParameters::new(seed, profile, candidate_count)
            .map_err(|_| AutoSectioningError::InvalidCandidateCount { candidate_count })?;
        Ok(Self {
            minimum_size,
            target_size,
            maximum_size,
            seed,
            profile,
            candidate_count,
        })
    }
}

/// One independently validated sectioning candidate materialized for timetable compilation.
#[derive(Clone, Debug)]
pub struct PreparedSectioningCandidate {
    candidate: SectioningCandidate,
    pub(crate) sections: Vec<TeachingSectionImportRow>,
    pub(crate) enrollments: Vec<SectionEnrollmentImportRow>,
    project_stable_key: String,
}

impl PreparedSectioningCandidate {
    #[must_use]
    pub const fn candidate(&self) -> &SectioningCandidate {
        &self.candidate
    }

    #[must_use]
    pub fn generated_sections(&self) -> &[TeachingSectionImportRow] {
        &self.sections
    }

    #[must_use]
    pub fn generated_enrollments(&self) -> &[SectionEnrollmentImportRow] {
        &self.enrollments
    }

    pub(crate) fn matches_project_key(&self, project_stable_key: &str) -> bool {
        self.project_stable_key == project_stable_key
    }
}

/// Provenance and all materialized candidates from one deterministic sectioning run.
#[derive(Clone, Debug)]
pub struct AutoSectioningPreparation {
    pub provenance: SectioningRunProvenance,
    pub diagnostics: Vec<SectioningDiagnostic>,
    pub candidates: Vec<PreparedSectioningCandidate>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AutoSectioningError {
    #[error("auto sectioning requires an import without existing teaching sections or enrollments")]
    ExistingSectioning,
    #[error(
        "invalid auto-sectioning size policy: minimum={minimum_size}, target={target_size}, maximum={maximum_size}"
    )]
    InvalidSizePolicy {
        minimum_size: u16,
        target_size: u16,
        maximum_size: u16,
    },
    #[error("invalid requested sectioning candidate count {candidate_count}")]
    InvalidCandidateCount { candidate_count: u8 },
    #[error("student `{student_code}` has no resolvable administrative-class grade")]
    MissingStudentGrade { student_code: String },
    #[error(
        "selected subject `{subject_code}` in grade `{grade_code}` has no teaching-section course plan"
    )]
    MissingTeachingSectionPlan {
        grade_code: String,
        subject_code: String,
    },
    #[error(
        "grade `{grade_code}` subject `{subject_code}` has no teacher allowed by every course plan"
    )]
    NoCommonTeacherCandidate {
        grade_code: String,
        subject_code: String,
    },
    #[error(
        "grade `{grade_code}` subject `{subject_code}` has no room allowed by every course plan"
    )]
    NoCommonRoomCandidate {
        grade_code: String,
        subject_code: String,
    },
    #[error(
        "grade `{grade_code}` subject `{subject_code}` demand {demand} cannot satisfy section-size policy"
    )]
    NoFeasibleSectionCount {
        grade_code: String,
        subject_code: String,
        demand: u32,
    },
    #[error("auto sectioning did not produce a candidate: {status:?}")]
    GenerationFailed {
        status: SectioningStatus,
        diagnostics: Vec<SectioningDiagnostic>,
    },
    #[error("generated candidate failed independent sectioning validation")]
    CandidateValidationFailed { report: CandidateValidationReport },
    #[error("generated sectioning contains an unresolved stable identifier")]
    GeneratedReferenceMissing,
    #[error("auto-sectioning cardinality exceeded a supported integer range")]
    CardinalityOverflow,
}

impl AutoSectioningError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ExistingSectioning => "APPLICATION_SECTIONING_EXISTING_DATA_CONFLICT",
            Self::InvalidSizePolicy { .. } => "APPLICATION_SECTIONING_INVALID_SIZE_POLICY",
            Self::InvalidCandidateCount { .. } => "APPLICATION_SECTIONING_INVALID_CANDIDATE_COUNT",
            Self::MissingStudentGrade { .. } => "APPLICATION_SECTIONING_STUDENT_GRADE_NOT_FOUND",
            Self::MissingTeachingSectionPlan { .. } => {
                "APPLICATION_SECTIONING_TEACHING_PLAN_NOT_FOUND"
            }
            Self::NoCommonTeacherCandidate { .. } => "APPLICATION_SECTIONING_NO_COMMON_TEACHER",
            Self::NoCommonRoomCandidate { .. } => "APPLICATION_SECTIONING_NO_COMMON_ROOM",
            Self::NoFeasibleSectionCount { .. } => "APPLICATION_SECTIONING_SIZE_POLICY_INFEASIBLE",
            Self::GenerationFailed { .. } => "APPLICATION_SECTIONING_GENERATION_FAILED",
            Self::CandidateValidationFailed { .. } => "INTERNAL_ERROR_INVALID_SECTIONING_OUTPUT",
            Self::GeneratedReferenceMissing => "INTERNAL_ERROR_SECTIONING_REFERENCE_MISSING",
            Self::CardinalityOverflow => "APPLICATION_SECTIONING_CARDINALITY_OVERFLOW",
        }
    }
}

#[derive(Clone, Debug)]
struct SectionMetadata {
    section_id: TeachingSectionId,
    section_code: String,
    grade_code: String,
    subject_code: String,
    display_ordinal: u32,
}

/// Builds section templates from validated Input-B data, generates candidates, independently
/// validates every candidate, and materializes memberships/resources without mutating the import.
///
/// # Errors
///
/// Returns a stable application error when source resources cannot support a fixed section,
/// class-size policy is impossible, or generated output fails the independent validator.
#[allow(clippy::too_many_lines)]
pub fn prepare_auto_sectioning(
    batch: &ImportBatch,
    project_stable_key: &str,
    policy: AutoSectioningPolicy,
) -> Result<AutoSectioningPreparation, AutoSectioningError> {
    if !batch.teaching_sections().is_empty() || !batch.section_enrollments().is_empty() {
        return Err(AutoSectioningError::ExistingSectioning);
    }

    let class_grades = batch
        .administrative_classes()
        .iter()
        .map(|row| {
            (
                row.administrative_class_code.as_str(),
                row.grade_code.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let student_grades = batch
        .students()
        .iter()
        .map(|row| {
            class_grades
                .get(row.administrative_class_code.as_str())
                .copied()
                .map(|grade| (row.student_code.as_str(), grade))
                .ok_or_else(|| AutoSectioningError::MissingStudentGrade {
                    student_code: row.student_code.clone(),
                })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let choices_by_student = batch.student_subject_choices().iter().fold(
        BTreeMap::<&str, Vec<&str>>::new(),
        |mut values, row| {
            values
                .entry(row.student_code.as_str())
                .or_default()
                .push(row.subject_code.as_str());
            values
        },
    );
    let students = batch
        .students()
        .iter()
        .map(|row| {
            let grade_code = student_grades[row.student_code.as_str()];
            StudentChoice {
                student_id: stable_id(project_stable_key, "student", &row.student_code),
                grade_id: stable_id(project_stable_key, "grade", grade_code),
                selected_subject_ids: choices_by_student
                    .get(row.student_code.as_str())
                    .into_iter()
                    .flatten()
                    .map(|subject| stable_id(project_stable_key, "subject", subject))
                    .collect(),
            }
        })
        .collect::<Vec<_>>();

    let mut demand = BTreeMap::<(&str, &str), u32>::new();
    for choice in batch.student_subject_choices() {
        let grade_code = student_grades[choice.student_code.as_str()];
        let count = demand
            .entry((grade_code, choice.subject_code.as_str()))
            .or_default();
        *count = count
            .checked_add(1)
            .ok_or(AutoSectioningError::CardinalityOverflow)?;
    }
    let plans = batch.course_plans().iter().fold(
        BTreeMap::<(&str, &str), Vec<_>>::new(),
        |mut values, plan| {
            if plan.audience_kind == ImportedAudienceKind::TeachingSection {
                values
                    .entry((plan.grade_code.as_str(), plan.subject_code.as_str()))
                    .or_default()
                    .push(plan);
            }
            values
        },
    );
    let teacher_penalties = batch.teacher_unavailability().iter().fold(
        BTreeMap::<&str, u32>::new(),
        |mut values, row| {
            *values.entry(row.teacher_code.as_str()).or_default() += 1;
            values
        },
    );
    let teacher_codes = batch
        .teachers()
        .iter()
        .map(|row| {
            (
                stable_id(project_stable_key, "teacher", &row.teacher_code),
                row.teacher_code.as_str(),
            )
        })
        .collect::<BTreeMap<TeacherId, _>>();
    let rooms_by_code = batch
        .rooms()
        .iter()
        .map(|row| (row.room_code.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let room_codes = batch
        .rooms()
        .iter()
        .map(|row| {
            (
                stable_id(project_stable_key, "room", &row.room_code),
                row.room_code.as_str(),
            )
        })
        .collect::<BTreeMap<RoomId, _>>();

    let mut templates = Vec::new();
    let mut metadata = BTreeMap::new();
    for ((grade_code, subject_code), student_count) in demand {
        let subject_plans = plans.get(&(grade_code, subject_code)).ok_or_else(|| {
            AutoSectioningError::MissingTeachingSectionPlan {
                grade_code: grade_code.to_owned(),
                subject_code: subject_code.to_owned(),
            }
        })?;
        let allowed_teachers = intersect_teacher_codes(subject_plans);
        if allowed_teachers.is_empty() {
            return Err(AutoSectioningError::NoCommonTeacherCandidate {
                grade_code: grade_code.to_owned(),
                subject_code: subject_code.to_owned(),
            });
        }
        let allowed_rooms = intersect_room_codes(subject_plans, &rooms_by_code);
        if allowed_rooms.is_empty() {
            return Err(AutoSectioningError::NoCommonRoomCandidate {
                grade_code: grade_code.to_owned(),
                subject_code: subject_code.to_owned(),
            });
        }
        let effective_maximum_size = allowed_rooms
            .iter()
            .map(|code| u32::from(rooms_by_code[code.as_str()].capacity))
            .max()
            .unwrap_or_default()
            .min(u32::from(policy.maximum_size));
        let section_count = choose_section_count(student_count, policy, effective_maximum_size)
            .ok_or_else(|| AutoSectioningError::NoFeasibleSectionCount {
                grade_code: grade_code.to_owned(),
                subject_code: subject_code.to_owned(),
                demand: student_count,
            })?;
        for ordinal in 1..=section_count {
            let section_code =
                generated_section_code(project_stable_key, grade_code, subject_code, ordinal);
            let section_id = stable_id(project_stable_key, "teaching_section", &section_code);
            templates.push(SectionTemplate {
                section_id,
                grade_id: stable_id(project_stable_key, "grade", grade_code),
                subject_id: stable_id(project_stable_key, "subject", subject_code),
                min_size: u32::from(policy.minimum_size),
                target_size: u32::from(policy.target_size),
                max_size: u32::from(policy.maximum_size),
                candidate_teachers: allowed_teachers
                    .iter()
                    .map(|code| TeacherCandidate {
                        teacher_id: stable_id(project_stable_key, "teacher", code),
                        availability_penalty: teacher_penalties
                            .get(code.as_str())
                            .copied()
                            .unwrap_or(0),
                    })
                    .collect(),
                candidate_rooms: allowed_rooms
                    .iter()
                    .map(|code| RoomCandidate {
                        room_id: stable_id(project_stable_key, "room", code),
                        capacity: u32::from(rooms_by_code[code.as_str()].capacity),
                        availability_penalty: unnecessary_feature_penalty(
                            rooms_by_code[code.as_str()],
                            subject_plans,
                        ),
                    })
                    .collect(),
            });
            metadata.insert(
                section_id,
                SectionMetadata {
                    section_id,
                    section_code,
                    grade_code: grade_code.to_owned(),
                    subject_code: subject_code.to_owned(),
                    display_ordinal: ordinal,
                },
            );
        }
    }

    let input = class_schedule_sectioning::SectioningInput {
        students,
        sections: templates,
    };
    let parameters = GenerationParameters::new(policy.seed, policy.profile, policy.candidate_count)
        .map_err(|_| AutoSectioningError::InvalidCandidateCount {
            candidate_count: policy.candidate_count,
        })?;
    let result = generate_candidates(&input, parameters);
    if result.status != SectioningStatus::Generated {
        return Err(AutoSectioningError::GenerationFailed {
            status: result.status,
            diagnostics: result.diagnostics,
        });
    }
    let student_codes = batch
        .students()
        .iter()
        .map(|row| {
            (
                stable_id(project_stable_key, "student", &row.student_code),
                row.student_code.as_str(),
            )
        })
        .collect::<BTreeMap<StudentId, _>>();
    let mut prepared = Vec::with_capacity(result.candidates.len());
    for candidate in &result.candidates {
        let report = validate_candidate(&input, candidate);
        if !report.is_valid() {
            return Err(AutoSectioningError::CandidateValidationFailed { report });
        }
        prepared.push(materialize_candidate(
            candidate,
            &metadata,
            &student_codes,
            &teacher_codes,
            &room_codes,
            policy,
            project_stable_key,
        )?);
    }
    Ok(AutoSectioningPreparation {
        provenance: result.provenance,
        diagnostics: result.diagnostics,
        candidates: prepared,
    })
}

fn materialize_candidate(
    candidate: &SectioningCandidate,
    metadata: &BTreeMap<TeachingSectionId, SectionMetadata>,
    student_codes: &BTreeMap<StudentId, &str>,
    teacher_codes: &BTreeMap<TeacherId, &str>,
    room_codes: &BTreeMap<RoomId, &str>,
    policy: AutoSectioningPolicy,
    project_stable_key: &str,
) -> Result<PreparedSectioningCandidate, AutoSectioningError> {
    let resources = candidate
        .resource_recommendations
        .iter()
        .map(|resource| (resource.section_id, resource))
        .collect::<BTreeMap<_, _>>();
    let mut sections = metadata
        .values()
        .map(|section| {
            let resource = resources
                .get(&section.section_id)
                .ok_or(AutoSectioningError::GeneratedReferenceMissing)?;
            let teacher_code = teacher_codes
                .get(&resource.teacher_id)
                .ok_or(AutoSectioningError::GeneratedReferenceMissing)?;
            let room_code = room_codes
                .get(&resource.room_id)
                .ok_or(AutoSectioningError::GeneratedReferenceMissing)?;
            Ok(TeachingSectionImportRow {
                row: 0,
                section_code: section.section_code.clone(),
                name: format!(
                    "{} {} 自动教学{}班",
                    section.grade_code, section.subject_code, section.display_ordinal
                ),
                grade_code: section.grade_code.clone(),
                subject_code: section.subject_code.clone(),
                min_size: policy.minimum_size,
                target_size: policy.target_size,
                max_size: policy.maximum_size,
                room_policy: ImportedRoomPolicy::Fixed {
                    room_code: (*room_code).to_owned(),
                },
                teacher_assignment: ImportedTeacherAssignment::Fixed {
                    teacher_code: (*teacher_code).to_owned(),
                },
            })
        })
        .collect::<Result<Vec<_>, AutoSectioningError>>()?;
    sections.sort_by(|left, right| left.section_code.cmp(&right.section_code));
    let mut enrollments = candidate
        .assignments
        .iter()
        .map(|assignment| {
            let section = metadata
                .get(&assignment.section_id)
                .ok_or(AutoSectioningError::GeneratedReferenceMissing)?;
            let student_code = student_codes
                .get(&assignment.student_id)
                .ok_or(AutoSectioningError::GeneratedReferenceMissing)?;
            Ok(SectionEnrollmentImportRow {
                row: 0,
                section_code: section.section_code.clone(),
                student_code: (*student_code).to_owned(),
            })
        })
        .collect::<Result<Vec<_>, AutoSectioningError>>()?;
    enrollments.sort_by(|left, right| {
        (&left.section_code, &left.student_code).cmp(&(&right.section_code, &right.student_code))
    });
    Ok(PreparedSectioningCandidate {
        candidate: candidate.clone(),
        sections,
        enrollments,
        project_stable_key: project_stable_key.to_owned(),
    })
}

fn intersect_teacher_codes(
    plans: &[&class_schedule_import::CoursePlanImportRow],
) -> BTreeSet<String> {
    plans
        .iter()
        .map(|plan| teacher_codes(&plan.teacher_assignment))
        .reduce(|left, right| left.intersection(&right).cloned().collect())
        .unwrap_or_default()
}

fn teacher_codes(assignment: &ImportedTeacherAssignment) -> BTreeSet<String> {
    match assignment {
        ImportedTeacherAssignment::Fixed { teacher_code } => {
            std::iter::once(teacher_code.clone()).collect()
        }
        ImportedTeacherAssignment::Candidates { teacher_codes } => {
            teacher_codes.iter().cloned().collect()
        }
    }
}

fn intersect_room_codes(
    plans: &[&class_schedule_import::CoursePlanImportRow],
    rooms: &BTreeMap<&str, &class_schedule_import::RoomImportRow>,
) -> BTreeSet<String> {
    plans
        .iter()
        .map(|plan| {
            let policy_codes = room_policy_codes(plan.room_policy.as_ref(), rooms);
            policy_codes
                .into_iter()
                .filter(|code| {
                    let features = &rooms[code.as_str()].features;
                    plan.required_room_features
                        .iter()
                        .all(|required| features.contains(required))
                })
                .collect::<BTreeSet<_>>()
        })
        .reduce(|left, right| left.intersection(&right).cloned().collect())
        .unwrap_or_default()
}

fn room_policy_codes(
    policy: Option<&ImportedRoomPolicy>,
    rooms: &BTreeMap<&str, &class_schedule_import::RoomImportRow>,
) -> BTreeSet<String> {
    match policy {
        None => rooms.keys().map(|code| (*code).to_owned()).collect(),
        Some(ImportedRoomPolicy::AdminHomeRoom) => BTreeSet::new(),
        Some(ImportedRoomPolicy::Fixed { room_code }) => {
            std::iter::once(room_code.clone()).collect()
        }
        Some(
            ImportedRoomPolicy::SectionFixed {
                candidate_room_codes,
            }
            | ImportedRoomPolicy::Flexible {
                candidate_room_codes,
            },
        ) => candidate_room_codes.iter().cloned().collect(),
        Some(ImportedRoomPolicy::PreferredFixed {
            preferred_room_codes,
            fallback_room_codes,
        }) => preferred_room_codes
            .iter()
            .chain(fallback_room_codes)
            .cloned()
            .collect(),
    }
}

fn unnecessary_feature_penalty(
    room: &class_schedule_import::RoomImportRow,
    plans: &[&class_schedule_import::CoursePlanImportRow],
) -> u32 {
    let penalty = plans
        .iter()
        .map(|plan| {
            room.features
                .iter()
                .filter(|feature| !plan.required_room_features.contains(feature))
                .count()
        })
        .sum::<usize>();
    u32::try_from(penalty).unwrap_or(u32::MAX)
}

fn choose_section_count(
    demand: u32,
    policy: AutoSectioningPolicy,
    effective_maximum_size: u32,
) -> Option<u32> {
    let maximum = effective_maximum_size;
    let minimum = u32::from(policy.minimum_size);
    let target = u32::from(policy.target_size);
    if maximum < minimum {
        return None;
    }
    let minimum_sections = demand.div_ceil(maximum);
    let maximum_sections = demand / minimum;
    (minimum_sections..=maximum_sections).min_by_key(|count| {
        let target_capacity = u64::from(*count) * u64::from(target);
        (target_capacity.abs_diff(u64::from(demand)), *count)
    })
}

fn generated_section_code(
    project_stable_key: &str,
    grade_code: &str,
    subject_code: &str,
    ordinal: u32,
) -> String {
    let id: TeachingSectionId = stable_id(
        project_stable_key,
        "generated_section_code",
        &format!(
            "{}:{grade_code}:{}:{subject_code}:{ordinal}",
            grade_code.len(),
            subject_code.len()
        ),
    );
    format!("AUTO-{}-{ordinal:02}", id.as_uuid().simple())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use class_schedule_import::{CsvImporter, CsvSource, DatasetKind, ImportConfig};
    use class_schedule_sectioning::SectioningProfile;

    use super::*;
    use crate::{
        CalendarDefinition, CompileError, compile_import_batch,
        compile_import_batch_with_sectioning,
    };

    fn input_b_batch() -> ImportBatch {
        let mut files = BTreeMap::<DatasetKind, Vec<u8>>::new();
        files.insert(
            DatasetKind::Students,
            concat!(
                "student_code,name,administrative_class_code\n",
                "S10A,Student 10 A,AC10\n",
                "S10B,Student 10 B,AC10\n",
                "S11A,Student 11 A,AC11\n",
                "S11B,Student 11 B,AC11\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::AdministrativeClasses,
            concat!(
                "administrative_class_code,name,grade_code,home_room_code\n",
                "AC10,Grade 10,G10,H10\n",
                "AC11,Grade 11,G11,H11\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::StudentSubjectChoices,
            concat!(
                "student_code,subject_code\n",
                "S10A,physics\n",
                "S10B,physics\n",
                "S11A,physics\n",
                "S11B,physics\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::Teachers,
            concat!(
                "teacher_code,name\n",
                "T10A,Teacher 10 A\n",
                "T10B,Teacher 10 B\n",
                "T11A,Teacher 11 A\n",
                "T11B,Teacher 11 B\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::Rooms,
            concat!(
                "room_code,name,building_code,capacity,features\n",
                "H10,Home 10,B1,40,\n",
                "H11,Home 11,B1,40,\n",
                "L1,Lab 1,B2,2,lab\n",
                "L2,Lab 2,B2,2,lab\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::CoursePlans,
            concat!(
                "course_plan_code,name,grade_code,subject_code,audience_kind,weekly_periods,meeting_pattern,min_days_between,max_periods_per_day,may_cross_breaks,required_room_features,teacher_assignment,teacher_codes,room_policy,room_candidates,preferred_rooms,fallback_rooms\n",
                "P10,Physics 10,G10,physics,teaching_section,1,1,0,1,false,lab,candidates,T10A;T10B,section_fixed,L1;L2,,\n",
                "P11,Physics 11,G11,physics,teaching_section,1,1,0,1,false,lab,candidates,T11A;T11B,section_fixed,L1;L2,,\n",
            )
            .as_bytes()
            .to_vec(),
        );
        CsvImporter::new(ImportConfig::default().with_exact_subject_choices(Some(1)))
            .import(
                files
                    .iter()
                    .map(|(&kind, bytes)| CsvSource::new(kind, bytes)),
            )
            .unwrap_or_else(|failure| panic!("import failed: {:?}", failure.problems()))
    }

    fn policy() -> AutoSectioningPolicy {
        AutoSectioningPolicy::new(1, 1, 2, 42, SectioningProfile::Balanced, 2).unwrap()
    }

    #[test]
    fn input_b_fails_closed_until_a_validated_candidate_is_applied() {
        let batch = input_b_batch();
        let calendar = CalendarDefinition::weekday_with_break(4, 2).unwrap();

        let error = compile_import_batch(&batch, &calendar, "project-b").unwrap_err();

        assert!(matches!(error, CompileError::UnresolvedSectioning { .. }));
        assert_eq!(error.code(), "APPLICATION_SECTIONING_REQUIRED");
    }

    #[test]
    fn auto_sectioning_is_deterministic_grade_scoped_and_compilable() {
        let batch = input_b_batch();
        let first = prepare_auto_sectioning(&batch, "project-b", policy()).unwrap();
        let repeated = prepare_auto_sectioning(&batch, "project-b", policy()).unwrap();

        assert_eq!(first.candidates.len(), 2);
        assert_eq!(
            first
                .candidates
                .iter()
                .map(|value| value.candidate().provenance.candidate_hash)
                .collect::<Vec<_>>(),
            repeated
                .candidates
                .iter()
                .map(|value| value.candidate().provenance.candidate_hash)
                .collect::<Vec<_>>()
        );
        let candidate = &first.candidates[0];
        assert_eq!(candidate.generated_sections().len(), 4);
        assert_eq!(candidate.generated_enrollments().len(), 4);
        let section_grades = candidate
            .generated_sections()
            .iter()
            .map(|section| (section.section_code.as_str(), section.grade_code.as_str()))
            .collect::<BTreeMap<_, _>>();
        let student_grades = batch
            .students()
            .iter()
            .map(|student| {
                let grade = if student.administrative_class_code == "AC10" {
                    "G10"
                } else {
                    "G11"
                };
                (student.student_code.as_str(), grade)
            })
            .collect::<BTreeMap<_, _>>();
        assert!(candidate.generated_enrollments().iter().all(|enrollment| {
            section_grades[enrollment.section_code.as_str()]
                == student_grades[enrollment.student_code.as_str()]
        }));
        assert!(candidate.generated_sections().iter().all(|section| {
            matches!(
                &section.room_policy,
                ImportedRoomPolicy::Fixed { room_code } if room_code == "L1" || room_code == "L2"
            )
        }));

        let calendar = CalendarDefinition::weekday_with_break(4, 2).unwrap();
        let compiled =
            compile_import_batch_with_sectioning(&batch, &calendar, "project-b", candidate)
                .unwrap();
        assert_eq!(compiled.problem.activities().len(), 4);
        assert_eq!(compiled.problem.students().len(), 4);
        assert!(
            compiled
                .problem
                .activities()
                .iter()
                .all(|activity| activity.audience.count() == 1)
        );
    }

    #[test]
    fn section_count_uses_effective_room_capacity_not_only_policy_maximum() {
        let policy =
            AutoSectioningPolicy::new(10, 40, 50, 1, SectioningProfile::Balanced, 1).unwrap();

        assert_eq!(choose_section_count(60, policy, 25), Some(3));
        assert_eq!(choose_section_count(60, policy, 9), None);
    }
}
