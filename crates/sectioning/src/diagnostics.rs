use class_schedule_domain::{GradeId, StudentId, SubjectId, TeachingSectionId};
use serde::{Deserialize, Serialize};

/// Stable, localizable sectioning diagnostic codes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum SectioningProblemCode {
    EmptyStudents,
    EmptySections,
    DuplicateStudent,
    DuplicateStudentSubjectChoice,
    StudentHasNoSelectedSubject,
    DuplicateSection,
    InvalidSectionSizeBounds,
    EmptyTeacherCandidates,
    DuplicateTeacherCandidate,
    EmptyRoomCandidates,
    DuplicateRoomCandidate,
    ZeroRoomCapacity,
    MissingSectionsForSubject,
    SectionSubjectHasNoStudents,
    SubjectMinimumCapacityExceeded,
    SubjectMaximumCapacityInsufficient,
    SectionRoomCapacityInsufficient,
    CandidateCountLimitedByDistinctSolutions,
    CandidateInputInvalid,
    CandidateMissingAssignment,
    CandidateDuplicateAssignment,
    CandidateUnexpectedAssignment,
    CandidateUnknownSection,
    CandidateSubjectMismatch,
    CandidateGradeMismatch,
    CandidateSectionBelowMinimum,
    CandidateSectionAboveMaximum,
    CandidateMissingResourceRecommendation,
    CandidateDuplicateResourceRecommendation,
    CandidateUnexpectedResourceRecommendation,
    CandidateTeacherNotAllowed,
    CandidateRoomNotAllowed,
    CandidateRoomCapacityInsufficient,
    CandidateObjectiveMismatch,
    CandidateInputHashMismatch,
    CandidateOutputHashMismatch,
    CandidateAlgorithmVersionMismatch,
}

impl SectioningProblemCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmptyStudents => "SECTIONING_EMPTY_STUDENTS",
            Self::EmptySections => "SECTIONING_EMPTY_SECTIONS",
            Self::DuplicateStudent => "SECTIONING_DUPLICATE_STUDENT",
            Self::DuplicateStudentSubjectChoice => "SECTIONING_DUPLICATE_STUDENT_SUBJECT_CHOICE",
            Self::StudentHasNoSelectedSubject => "SECTIONING_STUDENT_HAS_NO_SELECTED_SUBJECT",
            Self::DuplicateSection => "SECTIONING_DUPLICATE_SECTION",
            Self::InvalidSectionSizeBounds => "SECTIONING_INVALID_SECTION_SIZE_BOUNDS",
            Self::EmptyTeacherCandidates => "SECTIONING_EMPTY_TEACHER_CANDIDATES",
            Self::DuplicateTeacherCandidate => "SECTIONING_DUPLICATE_TEACHER_CANDIDATE",
            Self::EmptyRoomCandidates => "SECTIONING_EMPTY_ROOM_CANDIDATES",
            Self::DuplicateRoomCandidate => "SECTIONING_DUPLICATE_ROOM_CANDIDATE",
            Self::ZeroRoomCapacity => "SECTIONING_ZERO_ROOM_CAPACITY",
            Self::MissingSectionsForSubject => "SECTIONING_MISSING_SECTIONS_FOR_SUBJECT",
            Self::SectionSubjectHasNoStudents => "SECTIONING_SECTION_SUBJECT_HAS_NO_STUDENTS",
            Self::SubjectMinimumCapacityExceeded => "SECTIONING_SUBJECT_MINIMUM_CAPACITY_EXCEEDED",
            Self::SubjectMaximumCapacityInsufficient => {
                "SECTIONING_SUBJECT_MAXIMUM_CAPACITY_INSUFFICIENT"
            }
            Self::SectionRoomCapacityInsufficient => {
                "SECTIONING_SECTION_ROOM_CAPACITY_INSUFFICIENT"
            }
            Self::CandidateCountLimitedByDistinctSolutions => {
                "SECTIONING_CANDIDATE_COUNT_LIMITED_BY_DISTINCT_SOLUTIONS"
            }
            Self::CandidateInputInvalid => "SECTIONING_CANDIDATE_INPUT_INVALID",
            Self::CandidateMissingAssignment => "SECTIONING_CANDIDATE_MISSING_ASSIGNMENT",
            Self::CandidateDuplicateAssignment => "SECTIONING_CANDIDATE_DUPLICATE_ASSIGNMENT",
            Self::CandidateUnexpectedAssignment => "SECTIONING_CANDIDATE_UNEXPECTED_ASSIGNMENT",
            Self::CandidateUnknownSection => "SECTIONING_CANDIDATE_UNKNOWN_SECTION",
            Self::CandidateSubjectMismatch => "SECTIONING_CANDIDATE_SUBJECT_MISMATCH",
            Self::CandidateGradeMismatch => "SECTIONING_CANDIDATE_GRADE_MISMATCH",
            Self::CandidateSectionBelowMinimum => "SECTIONING_CANDIDATE_SECTION_BELOW_MINIMUM",
            Self::CandidateSectionAboveMaximum => "SECTIONING_CANDIDATE_SECTION_ABOVE_MAXIMUM",
            Self::CandidateMissingResourceRecommendation => {
                "SECTIONING_CANDIDATE_MISSING_RESOURCE_RECOMMENDATION"
            }
            Self::CandidateDuplicateResourceRecommendation => {
                "SECTIONING_CANDIDATE_DUPLICATE_RESOURCE_RECOMMENDATION"
            }
            Self::CandidateUnexpectedResourceRecommendation => {
                "SECTIONING_CANDIDATE_UNEXPECTED_RESOURCE_RECOMMENDATION"
            }
            Self::CandidateTeacherNotAllowed => "SECTIONING_CANDIDATE_TEACHER_NOT_ALLOWED",
            Self::CandidateRoomNotAllowed => "SECTIONING_CANDIDATE_ROOM_NOT_ALLOWED",
            Self::CandidateRoomCapacityInsufficient => {
                "SECTIONING_CANDIDATE_ROOM_CAPACITY_INSUFFICIENT"
            }
            Self::CandidateObjectiveMismatch => "SECTIONING_CANDIDATE_OBJECTIVE_MISMATCH",
            Self::CandidateInputHashMismatch => "SECTIONING_CANDIDATE_INPUT_HASH_MISMATCH",
            Self::CandidateOutputHashMismatch => "SECTIONING_CANDIDATE_OUTPUT_HASH_MISMATCH",
            Self::CandidateAlgorithmVersionMismatch => {
                "SECTIONING_CANDIDATE_ALGORITHM_VERSION_MISMATCH"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

/// Numeric context is deliberately structured so presentation layers can localize
/// diagnostics without parsing English strings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SectioningDiagnostic {
    pub code: SectioningProblemCode,
    pub severity: DiagnosticSeverity,
    pub student_id: Option<StudentId>,
    pub grade_id: Option<GradeId>,
    pub subject_id: Option<SubjectId>,
    pub section_id: Option<TeachingSectionId>,
    pub expected_min: Option<u64>,
    pub expected_max: Option<u64>,
    pub actual: Option<u64>,
}

impl SectioningDiagnostic {
    #[must_use]
    pub const fn error(code: SectioningProblemCode) -> Self {
        Self {
            code,
            severity: DiagnosticSeverity::Error,
            student_id: None,
            grade_id: None,
            subject_id: None,
            section_id: None,
            expected_min: None,
            expected_max: None,
            actual: None,
        }
    }

    #[must_use]
    pub const fn warning(code: SectioningProblemCode) -> Self {
        Self {
            code,
            severity: DiagnosticSeverity::Warning,
            student_id: None,
            grade_id: None,
            subject_id: None,
            section_id: None,
            expected_min: None,
            expected_max: None,
            actual: None,
        }
    }

    #[must_use]
    pub const fn with_student(mut self, student_id: StudentId) -> Self {
        self.student_id = Some(student_id);
        self
    }

    #[must_use]
    pub const fn with_grade(mut self, grade_id: GradeId) -> Self {
        self.grade_id = Some(grade_id);
        self
    }

    #[must_use]
    pub const fn with_subject(mut self, subject_id: SubjectId) -> Self {
        self.subject_id = Some(subject_id);
        self
    }

    #[must_use]
    pub const fn with_section(mut self, section_id: TeachingSectionId) -> Self {
        self.section_id = Some(section_id);
        self
    }

    #[must_use]
    pub const fn with_counts(mut self, min: u64, max: u64, actual: u64) -> Self {
        self.expected_min = Some(min);
        self.expected_max = Some(max);
        self.actual = Some(actual);
        self
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CandidateValidationReport {
    pub problems: Vec<SectioningDiagnostic>,
    pub recomputed_objective: Option<crate::SectioningObjective>,
    pub recomputed_candidate_hash: Option<crate::SectioningDigest>,
}

impl CandidateValidationReport {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.problems.is_empty()
    }
}
