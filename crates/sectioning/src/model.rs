use class_schedule_domain::{GradeId, RoomId, StudentId, SubjectId, TeacherId, TeachingSectionId};
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

/// A student's elective subject choices at the sectioning boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StudentChoice {
    pub student_id: StudentId,
    pub grade_id: GradeId,
    pub selected_subject_ids: Vec<SubjectId>,
}

/// A candidate teacher and a lower-is-better coarse availability penalty.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TeacherCandidate {
    pub teacher_id: TeacherId,
    pub availability_penalty: u32,
}

/// A candidate fixed room for a section.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoomCandidate {
    pub room_id: RoomId,
    pub capacity: u32,
    /// Lower is better. This is only a sectioning feasibility proxy.
    pub availability_penalty: u32,
}

/// One teaching-section slot supplied by the school or section-template builder.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SectionTemplate {
    pub section_id: TeachingSectionId,
    pub grade_id: GradeId,
    pub subject_id: SubjectId,
    pub min_size: u32,
    pub target_size: u32,
    pub max_size: u32,
    pub candidate_teachers: Vec<TeacherCandidate>,
    pub candidate_rooms: Vec<RoomCandidate>,
}

/// Semantic input to the decomposition stage. Domain IDs remain stable while the
/// sectioning implementation is free to compact them internally.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SectioningInput {
    pub students: Vec<StudentChoice>,
    pub sections: Vec<SectionTemplate>,
}

/// Search effort. Profiles never change hard rules or objective ordering.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectioningProfile {
    Fast,
    Balanced,
    BestQuality,
}

/// Validated controls for reproducible candidate generation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GenerationParameters {
    seed: u64,
    profile: SectioningProfile,
    candidate_count: u8,
}

impl GenerationParameters {
    pub const MAX_CANDIDATES: u8 = 16;

    /// Creates bounded, reproducible generation controls.
    ///
    /// # Errors
    ///
    /// Returns an error when `candidate_count` is zero or exceeds [`Self::MAX_CANDIDATES`].
    pub fn new(
        seed: u64,
        profile: SectioningProfile,
        candidate_count: u8,
    ) -> Result<Self, SectioningParameterError> {
        if !(1..=Self::MAX_CANDIDATES).contains(&candidate_count) {
            return Err(SectioningParameterError { candidate_count });
        }
        Ok(Self {
            seed,
            profile,
            candidate_count,
        })
    }

    #[must_use]
    pub const fn seed(self) -> u64 {
        self.seed
    }

    #[must_use]
    pub const fn profile(self) -> SectioningProfile {
        self.profile
    }

    #[must_use]
    pub const fn candidate_count(self) -> u8 {
        self.candidate_count
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SectioningParameterError {
    pub candidate_count: u8,
}

impl fmt::Display for SectioningParameterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "candidate_count must be between 1 and {}, got {}",
            GenerationParameters::MAX_CANDIDATES,
            self.candidate_count
        )
    }
}

impl Error for SectioningParameterError {}

/// A fixed-width digest used for input and candidate provenance.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SectioningDigest(pub [u8; 32]);

impl SectioningDigest {
    #[must_use]
    pub fn to_hex(self) -> String {
        let mut text = String::with_capacity(64);
        for byte in self.0 {
            use fmt::Write as _;
            write!(&mut text, "{byte:02x}").expect("writing to String cannot fail");
        }
        text
    }
}

/// Exact membership assignment for one selected subject.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct StudentSectionAssignment {
    pub student_id: StudentId,
    pub subject_id: SubjectId,
    pub section_id: TeachingSectionId,
}

/// Coarse recommendation passed forward to timetable problem construction. It is
/// not a timetable reservation and may be replaced by a later explicit workflow.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SectionResourceRecommendation {
    pub section_id: TeachingSectionId,
    pub assigned_size: u32,
    pub teacher_id: TeacherId,
    pub room_id: RoomId,
}

/// Third-tier feasibility proxy. Fields are compared in declaration order, not
/// collapsed into an opaque large weighted sum.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TimetableFeasibilityProxy {
    /// Sum of squared cross-subject section intersections. Lower concentration is
    /// usually easier to timetable, but is not a proof of feasibility.
    pub concentrated_audience_overlap: u64,
    pub resource_availability_penalty: u64,
    pub room_capacity_tightness: u64,
}

/// Lexicographic sectioning objective. Hard validity is always evaluated first.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SectioningObjective {
    pub target_size_deviation: u64,
    pub size_imbalance: u64,
    pub timetable_feasibility: TimetableFeasibilityProxy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CandidateProvenance {
    pub algorithm_version: String,
    pub input_hash: SectioningDigest,
    pub candidate_hash: SectioningDigest,
    pub seed: u64,
    pub profile: SectioningProfile,
    pub attempt_index: u32,
    pub accepted_local_swaps: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SectioningCandidate {
    pub assignments: Vec<StudentSectionAssignment>,
    pub resource_recommendations: Vec<SectionResourceRecommendation>,
    pub objective: SectioningObjective,
    pub provenance: CandidateProvenance,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectioningStatus {
    Generated,
    InvalidInput,
    ProvenInfeasible,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SectioningRunProvenance {
    pub algorithm_version: String,
    pub input_hash: SectioningDigest,
    pub seed: u64,
    pub profile: SectioningProfile,
    pub requested_candidates: u8,
    pub generated_candidates: u8,
    pub attempts: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SectioningResult {
    pub status: SectioningStatus,
    pub candidates: Vec<SectioningCandidate>,
    pub diagnostics: Vec<crate::SectioningDiagnostic>,
    pub provenance: SectioningRunProvenance,
}
