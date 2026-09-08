use crate::{
    AcademicTermId, AdministrativeClassId, ClassSizeRange, CourseOfferingId, CoursePlanId,
    DomainError, GradeId, MeetingDemandId, MeetingDuration, Name, RoomId, SectionEnrollmentId,
    StudentId, SubjectId, TeacherId, TeachingSectionId, WeekPatternId, WeeklyPeriods,
};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RoomCandidates(Vec<RoomId>);

impl RoomCandidates {
    pub fn new(values: impl IntoIterator<Item = RoomId>) -> Result<Self, DomainError> {
        unique_nonempty(values, "room_candidates").map(Self)
    }

    #[must_use]
    pub fn as_slice(&self) -> &[RoomId] {
        &self.0
    }

    #[must_use]
    pub fn contains(&self, room_id: RoomId) -> bool {
        self.0.contains(&room_id)
    }
}

impl<'de> Deserialize<'de> for RoomCandidates {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<RoomId>::deserialize(deserializer)
            .and_then(|values| Self::new(values).map_err(D::Error::custom))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TeacherCandidates(Vec<TeacherId>);

impl TeacherCandidates {
    pub fn new(values: impl IntoIterator<Item = TeacherId>) -> Result<Self, DomainError> {
        unique_nonempty(values, "teacher_candidates").map(Self)
    }

    #[must_use]
    pub fn as_slice(&self) -> &[TeacherId] {
        &self.0
    }
}

impl<'de> Deserialize<'de> for TeacherCandidates {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<TeacherId>::deserialize(deserializer)
            .and_then(|values| Self::new(values).map_err(D::Error::custom))
    }
}

/// Room binding semantics are explicit; "fixed" never means exclusive for the whole week.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum RoomPolicy {
    AdminHomeRoom,
    Fixed(RoomId),
    SectionFixed(RoomCandidates),
    PreferredFixed {
        preferred: RoomCandidates,
        fallback: RoomCandidates,
    },
    Flexible(RoomCandidates),
}

impl RoomPolicy {
    pub fn preferred_fixed(
        preferred: RoomCandidates,
        fallback: RoomCandidates,
    ) -> Result<Self, DomainError> {
        for room_id in preferred.as_slice() {
            if fallback.contains(*room_id) {
                return Err(DomainError::OverlappingCollections {
                    first: "preferred_rooms",
                    second: "fallback_rooms",
                    value: room_id.to_string(),
                });
            }
        }
        Ok(Self::PreferredFixed {
            preferred,
            fallback,
        })
    }

    #[must_use]
    pub const fn requires_fixed_binding(&self) -> bool {
        !matches!(self, Self::AdminHomeRoom | Self::Flexible(_))
    }

    #[must_use]
    pub const fn permits_per_meeting_room_choice(&self) -> bool {
        matches!(self, Self::Flexible(_))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum TeacherAssignment {
    FixedTeacher(TeacherId),
    CandidateTeachers(TeacherCandidates),
}

impl TeacherAssignment {
    #[must_use]
    pub const fn is_fixed(&self) -> bool {
        matches!(self, Self::FixedTeacher(_))
    }
}

/// Legal weekly grouping of periods into meetings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MeetingPattern {
    weekly_periods: WeeklyPeriods,
    durations: Vec<MeetingDuration>,
    min_days_between: u8,
    max_periods_per_day: WeeklyPeriods,
    may_cross_breaks: bool,
}

impl MeetingPattern {
    pub fn new(
        weekly_periods: WeeklyPeriods,
        durations: Vec<MeetingDuration>,
        min_days_between: u8,
        max_periods_per_day: WeeklyPeriods,
        may_cross_breaks: bool,
    ) -> Result<Self, DomainError> {
        if durations.is_empty() {
            return Err(DomainError::EmptyCollection {
                field: "meeting_pattern.durations",
            });
        }
        let actual: u64 = durations
            .iter()
            .map(|duration| u64::from(duration.get()))
            .sum();
        let expected = u64::from(weekly_periods.get());
        if actual != expected {
            return Err(DomainError::InconsistentTotal {
                field: "meeting_pattern.durations",
                expected,
                actual,
            });
        }
        if min_days_between > 6 {
            return Err(DomainError::ValueOutOfRange {
                field: "meeting_pattern.min_days_between",
                min: 0,
                max: 6,
                actual: u64::from(min_days_between),
            });
        }
        if max_periods_per_day.get() > weekly_periods.get() {
            return Err(DomainError::ValueOutOfRange {
                field: "meeting_pattern.max_periods_per_day",
                min: 1,
                max: u64::from(weekly_periods.get()),
                actual: u64::from(max_periods_per_day.get()),
            });
        }
        if let Some(duration) = durations
            .iter()
            .find(|duration| u16::from(duration.get()) > max_periods_per_day.get())
        {
            return Err(DomainError::ValueOutOfRange {
                field: "meeting_pattern.duration",
                min: 1,
                max: u64::from(max_periods_per_day.get()),
                actual: u64::from(duration.get()),
            });
        }
        Ok(Self {
            weekly_periods,
            durations,
            min_days_between,
            max_periods_per_day,
            may_cross_breaks,
        })
    }

    #[must_use]
    pub const fn weekly_periods(&self) -> WeeklyPeriods {
        self.weekly_periods
    }

    #[must_use]
    pub fn durations(&self) -> &[MeetingDuration] {
        &self.durations
    }

    #[must_use]
    pub const fn min_days_between(&self) -> u8 {
        self.min_days_between
    }

    #[must_use]
    pub const fn max_periods_per_day(&self) -> WeeklyPeriods {
        self.max_periods_per_day
    }

    #[must_use]
    pub const fn may_cross_breaks(&self) -> bool {
        self.may_cross_breaks
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CourseAudienceKind {
    AdministrativeClass,
    TeachingSection,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CoursePlan {
    id: CoursePlanId,
    grade_id: GradeId,
    subject_id: SubjectId,
    name: Name,
    audience_kind: CourseAudienceKind,
    meeting_pattern: MeetingPattern,
    room_policy_override: Option<RoomPolicy>,
}

impl CoursePlan {
    #[must_use]
    pub const fn new(
        id: CoursePlanId,
        grade_id: GradeId,
        subject_id: SubjectId,
        name: Name,
        audience_kind: CourseAudienceKind,
        meeting_pattern: MeetingPattern,
        room_policy_override: Option<RoomPolicy>,
    ) -> Self {
        Self {
            id,
            grade_id,
            subject_id,
            name,
            audience_kind,
            meeting_pattern,
            room_policy_override,
        }
    }

    #[must_use]
    pub const fn id(&self) -> CoursePlanId {
        self.id
    }
    #[must_use]
    pub const fn grade_id(&self) -> GradeId {
        self.grade_id
    }
    #[must_use]
    pub const fn subject_id(&self) -> SubjectId {
        self.subject_id
    }
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }
    #[must_use]
    pub const fn audience_kind(&self) -> CourseAudienceKind {
        self.audience_kind
    }
    #[must_use]
    pub const fn meeting_pattern(&self) -> &MeetingPattern {
        &self.meeting_pattern
    }
    #[must_use]
    pub const fn room_policy_override(&self) -> Option<&RoomPolicy> {
        self.room_policy_override.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TeachingSection {
    id: TeachingSectionId,
    grade_id: GradeId,
    subject_id: SubjectId,
    name: Name,
    size: ClassSizeRange,
    room_policy: RoomPolicy,
    teacher_assignment: TeacherAssignment,
}

impl TeachingSection {
    pub fn new(
        id: TeachingSectionId,
        grade_id: GradeId,
        subject_id: SubjectId,
        name: Name,
        size: ClassSizeRange,
        room_policy: RoomPolicy,
        teacher_assignment: TeacherAssignment,
    ) -> Result<Self, DomainError> {
        if matches!(room_policy, RoomPolicy::AdminHomeRoom) {
            return Err(DomainError::InvalidReference {
                field: "teaching_section.room_policy",
                target: "section-compatible room policy",
                value: "admin_home_room".to_owned(),
            });
        }
        Ok(Self {
            id,
            grade_id,
            subject_id,
            name,
            size,
            room_policy,
            teacher_assignment,
        })
    }

    /// Default high-school section semantics: one room is selected from candidates and
    /// then used by every meeting of the section.
    pub fn with_default_room_policy(
        id: TeachingSectionId,
        grade_id: GradeId,
        subject_id: SubjectId,
        name: Name,
        size: ClassSizeRange,
        candidate_rooms: RoomCandidates,
        teacher_assignment: TeacherAssignment,
    ) -> Result<Self, DomainError> {
        Self::new(
            id,
            grade_id,
            subject_id,
            name,
            size,
            RoomPolicy::SectionFixed(candidate_rooms),
            teacher_assignment,
        )
    }

    #[must_use]
    pub const fn id(&self) -> TeachingSectionId {
        self.id
    }
    #[must_use]
    pub const fn grade_id(&self) -> GradeId {
        self.grade_id
    }
    #[must_use]
    pub const fn subject_id(&self) -> SubjectId {
        self.subject_id
    }
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }
    #[must_use]
    pub const fn size(&self) -> ClassSizeRange {
        self.size
    }
    #[must_use]
    pub const fn room_policy(&self) -> &RoomPolicy {
        &self.room_policy
    }
    #[must_use]
    pub const fn teacher_assignment(&self) -> &TeacherAssignment {
        &self.teacher_assignment
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct SectionEnrollment {
    id: SectionEnrollmentId,
    section_id: TeachingSectionId,
    student_id: StudentId,
}

impl SectionEnrollment {
    #[must_use]
    pub const fn new(
        id: SectionEnrollmentId,
        section_id: TeachingSectionId,
        student_id: StudentId,
    ) -> Self {
        Self {
            id,
            section_id,
            student_id,
        }
    }

    #[must_use]
    pub const fn id(self) -> SectionEnrollmentId {
        self.id
    }
    #[must_use]
    pub const fn section_id(self) -> TeachingSectionId {
        self.section_id
    }
    #[must_use]
    pub const fn student_id(self) -> StudentId {
        self.student_id
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum Audience {
    AdministrativeClass(AdministrativeClassId),
    TeachingSection(TeachingSectionId),
}

/// A term-scoped instance of a course plan for one concrete audience.
///
/// `CoursePlan` owns reusable meeting-pattern rules. This object owns the facts that can differ
/// between audiences, most importantly who teaches the course and which room policy applies.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CourseOffering {
    id: CourseOfferingId,
    academic_term_id: AcademicTermId,
    course_plan_id: CoursePlanId,
    audience: Audience,
    room_policy: RoomPolicy,
    teacher_assignment: TeacherAssignment,
}

impl CourseOffering {
    pub fn new(
        id: CourseOfferingId,
        academic_term_id: AcademicTermId,
        course_plan_id: CoursePlanId,
        audience: Audience,
        room_policy: RoomPolicy,
        teacher_assignment: TeacherAssignment,
    ) -> Result<Self, DomainError> {
        if matches!(audience, Audience::TeachingSection(_))
            && matches!(room_policy, RoomPolicy::AdminHomeRoom)
        {
            return Err(DomainError::InvalidReference {
                field: "course_offering.room_policy",
                target: "section-compatible room policy",
                value: "admin_home_room".to_owned(),
            });
        }
        Ok(Self {
            id,
            academic_term_id,
            course_plan_id,
            audience,
            room_policy,
            teacher_assignment,
        })
    }

    #[must_use]
    pub const fn id(&self) -> CourseOfferingId {
        self.id
    }

    #[must_use]
    pub const fn academic_term_id(&self) -> AcademicTermId {
        self.academic_term_id
    }

    #[must_use]
    pub const fn course_plan_id(&self) -> CoursePlanId {
        self.course_plan_id
    }

    #[must_use]
    pub const fn audience(&self) -> Audience {
        self.audience
    }

    #[must_use]
    pub const fn room_policy(&self) -> &RoomPolicy {
        &self.room_policy
    }

    #[must_use]
    pub const fn teacher_assignment(&self) -> &TeacherAssignment {
        &self.teacher_assignment
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MeetingDemand {
    id: MeetingDemandId,
    course_plan_id: CoursePlanId,
    audience: Audience,
    ordinal: u16,
    duration: MeetingDuration,
    week_pattern_id: WeekPatternId,
    room_policy: RoomPolicy,
    teacher_assignment: TeacherAssignment,
}

impl MeetingDemand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: MeetingDemandId,
        course_plan_id: CoursePlanId,
        audience: Audience,
        ordinal: u16,
        duration: MeetingDuration,
        week_pattern_id: WeekPatternId,
        room_policy: RoomPolicy,
        teacher_assignment: TeacherAssignment,
    ) -> Result<Self, DomainError> {
        if ordinal == 0 {
            return Err(DomainError::ZeroValue {
                field: "meeting_demand.ordinal",
            });
        }
        if matches!(audience, Audience::TeachingSection(_))
            && matches!(room_policy, RoomPolicy::AdminHomeRoom)
        {
            return Err(DomainError::InvalidReference {
                field: "meeting_demand.room_policy",
                target: "section-compatible room policy",
                value: "admin_home_room".to_owned(),
            });
        }
        Ok(Self {
            id,
            course_plan_id,
            audience,
            ordinal,
            duration,
            week_pattern_id,
            room_policy,
            teacher_assignment,
        })
    }

    #[must_use]
    pub const fn id(&self) -> MeetingDemandId {
        self.id
    }
    #[must_use]
    pub const fn course_plan_id(&self) -> CoursePlanId {
        self.course_plan_id
    }
    #[must_use]
    pub const fn audience(&self) -> Audience {
        self.audience
    }
    #[must_use]
    pub const fn ordinal(&self) -> u16 {
        self.ordinal
    }
    #[must_use]
    pub const fn duration(&self) -> MeetingDuration {
        self.duration
    }
    #[must_use]
    pub const fn week_pattern_id(&self) -> WeekPatternId {
        self.week_pattern_id
    }
    #[must_use]
    pub const fn room_policy(&self) -> &RoomPolicy {
        &self.room_policy
    }
    #[must_use]
    pub const fn teacher_assignment(&self) -> &TeacherAssignment {
        &self.teacher_assignment
    }
}

fn unique_nonempty<T>(
    values: impl IntoIterator<Item = T>,
    field: &'static str,
) -> Result<Vec<T>, DomainError>
where
    T: Copy + Ord + ToString,
{
    let values: Vec<_> = values.into_iter().collect();
    if values.is_empty() {
        return Err(DomainError::EmptyCollection { field });
    }
    let mut seen = BTreeSet::new();
    for value in &values {
        if !seen.insert(*value) {
            return Err(DomainError::DuplicateValue {
                field,
                value: value.to_string(),
            });
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Capacity, ProblemCode};

    #[test]
    fn meeting_pattern_total_must_match_weekly_periods() {
        let result = MeetingPattern::new(
            WeeklyPeriods::new(5).unwrap(),
            vec![
                MeetingDuration::new(2).unwrap(),
                MeetingDuration::new(2).unwrap(),
            ],
            0,
            WeeklyPeriods::new(2).unwrap(),
            false,
        );
        assert_eq!(
            result.unwrap_err().code(),
            ProblemCode::DomainInconsistentTotal
        );
    }

    #[test]
    fn preferred_and_fallback_rooms_must_not_overlap() {
        let room = RoomId::new_v4();
        let result = RoomPolicy::preferred_fixed(
            RoomCandidates::new([room]).unwrap(),
            RoomCandidates::new([room]).unwrap(),
        );
        assert_eq!(
            result.unwrap_err().code(),
            ProblemCode::DomainOverlappingCollections
        );
    }

    #[test]
    fn teaching_section_defaults_to_a_single_fixed_binding() {
        let section = TeachingSection::with_default_room_policy(
            TeachingSectionId::new_v4(),
            GradeId::new_v4(),
            SubjectId::new_v4(),
            Name::new("物理 2 班").unwrap(),
            ClassSizeRange::new(20, 35, Capacity::new(45).unwrap()).unwrap(),
            RoomCandidates::new([RoomId::new_v4()]).unwrap(),
            TeacherAssignment::FixedTeacher(TeacherId::new_v4()),
        )
        .unwrap();
        assert!(section.room_policy().requires_fixed_binding());
    }

    #[test]
    fn course_offering_keeps_teacher_assignment_at_the_audience_boundary() {
        let teacher = TeacherId::new_v4();
        let administrative_class = AdministrativeClassId::new_v4();
        let offering = CourseOffering::new(
            CourseOfferingId::new_v4(),
            AcademicTermId::new_v4(),
            CoursePlanId::new_v4(),
            Audience::AdministrativeClass(administrative_class),
            RoomPolicy::AdminHomeRoom,
            TeacherAssignment::FixedTeacher(teacher),
        )
        .unwrap();

        assert_eq!(
            offering.audience(),
            Audience::AdministrativeClass(administrative_class)
        );
        assert_eq!(
            offering.teacher_assignment(),
            &TeacherAssignment::FixedTeacher(teacher)
        );
    }

    #[test]
    fn teaching_section_offering_cannot_use_an_administrative_home_room() {
        let error = CourseOffering::new(
            CourseOfferingId::new_v4(),
            AcademicTermId::new_v4(),
            CoursePlanId::new_v4(),
            Audience::TeachingSection(TeachingSectionId::new_v4()),
            RoomPolicy::AdminHomeRoom,
            TeacherAssignment::FixedTeacher(TeacherId::new_v4()),
        )
        .unwrap_err();

        assert_eq!(error.code(), ProblemCode::DomainInvalidReference);
    }
}
