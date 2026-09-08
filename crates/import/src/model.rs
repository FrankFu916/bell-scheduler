use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetKind {
    Students,
    AdministrativeClasses,
    StudentSubjectChoices,
    Teachers,
    TeacherUnavailability,
    Rooms,
    CoursePlans,
    TeachingSections,
    SectionEnrollments,
    CourseOfferings,
    FixedActivities,
}

impl DatasetKind {
    pub const ALL: [Self; 11] = [
        Self::Students,
        Self::AdministrativeClasses,
        Self::StudentSubjectChoices,
        Self::Teachers,
        Self::TeacherUnavailability,
        Self::Rooms,
        Self::CoursePlans,
        Self::TeachingSections,
        Self::SectionEnrollments,
        Self::CourseOfferings,
        Self::FixedActivities,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Students => "students",
            Self::AdministrativeClasses => "administrative_classes",
            Self::StudentSubjectChoices => "student_subject_choices",
            Self::Teachers => "teachers",
            Self::TeacherUnavailability => "teacher_unavailability",
            Self::Rooms => "rooms",
            Self::CoursePlans => "course_plans",
            Self::TeachingSections => "teaching_sections",
            Self::SectionEnrollments => "section_enrollments",
            Self::CourseOfferings => "course_offerings",
            Self::FixedActivities => "fixed_activities",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CsvSource<'a> {
    kind: DatasetKind,
    bytes: &'a [u8],
}

impl<'a> CsvSource<'a> {
    #[must_use]
    pub const fn new(kind: DatasetKind, bytes: &'a [u8]) -> Self {
        Self { kind, bytes }
    }

    #[must_use]
    pub const fn kind(self) -> DatasetKind {
        self.kind
    }

    #[must_use]
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportedDay {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportedAudienceKind {
    AdministrativeClass,
    TeachingSection,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ImportedRoomPolicy {
    AdminHomeRoom,
    Fixed {
        room_code: String,
    },
    SectionFixed {
        candidate_room_codes: Vec<String>,
    },
    PreferredFixed {
        preferred_room_codes: Vec<String>,
        fallback_room_codes: Vec<String>,
    },
    Flexible {
        candidate_room_codes: Vec<String>,
    },
}

impl ImportedRoomPolicy {
    pub(crate) fn room_codes(&self) -> impl Iterator<Item = &str> {
        let (first, second): (&[String], &[String]) = match self {
            Self::AdminHomeRoom => (&[], &[]),
            Self::Fixed { room_code } => (std::slice::from_ref(room_code), &[]),
            Self::SectionFixed {
                candidate_room_codes,
            }
            | Self::Flexible {
                candidate_room_codes,
            } => (candidate_room_codes, &[]),
            Self::PreferredFixed {
                preferred_room_codes,
                fallback_room_codes,
            } => (preferred_room_codes, fallback_room_codes),
        };
        first.iter().chain(second).map(String::as_str)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ImportedTeacherAssignment {
    Fixed { teacher_code: String },
    Candidates { teacher_codes: Vec<String> },
}

impl ImportedTeacherAssignment {
    pub(crate) fn teacher_codes(&self) -> impl Iterator<Item = &str> {
        match self {
            Self::Fixed { teacher_code } => std::slice::from_ref(teacher_code).iter(),
            Self::Candidates { teacher_codes } => teacher_codes.iter(),
        }
        .map(String::as_str)
    }
}

macro_rules! row_struct {
    ($name:ident { $($field:ident : $ty:ty),+ $(,)? }) => {
        #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
        pub struct $name {
            pub row: u64,
            $(pub $field: $ty),+
        }
    };
}

row_struct!(StudentImportRow {
    student_code: String,
    name: String,
    administrative_class_code: String,
});
row_struct!(AdministrativeClassImportRow {
    administrative_class_code: String,
    name: String,
    grade_code: String,
    home_room_code: String,
});
row_struct!(StudentSubjectChoiceImportRow {
    student_code: String,
    subject_code: String,
});
row_struct!(TeacherImportRow {
    teacher_code: String,
    name: String,
});
row_struct!(TeacherUnavailabilityImportRow {
    teacher_code: String,
    day: ImportedDay,
    period: u16,
});
row_struct!(RoomImportRow {
    room_code: String,
    name: String,
    building_code: String,
    capacity: u16,
    features: Vec<String>,
});
row_struct!(CoursePlanImportRow {
    course_plan_code: String,
    name: String,
    grade_code: String,
    subject_code: String,
    audience_kind: ImportedAudienceKind,
    weekly_periods: u16,
    meeting_pattern: Vec<u8>,
    min_days_between: u8,
    max_periods_per_day: u16,
    may_cross_breaks: bool,
    room_policy: Option<ImportedRoomPolicy>,
    required_room_features: Vec<String>,
    teacher_assignment: ImportedTeacherAssignment,
});
row_struct!(TeachingSectionImportRow {
    section_code: String,
    name: String,
    grade_code: String,
    subject_code: String,
    min_size: u16,
    target_size: u16,
    max_size: u16,
    room_policy: ImportedRoomPolicy,
    teacher_assignment: ImportedTeacherAssignment,
});
row_struct!(SectionEnrollmentImportRow {
    section_code: String,
    student_code: String,
});
row_struct!(CourseOfferingImportRow {
    course_plan_code: String,
    audience_kind: ImportedAudienceKind,
    audience_code: String,
    teacher_assignment: ImportedTeacherAssignment,
    room_policy: Option<ImportedRoomPolicy>,
});
row_struct!(FixedActivityImportRow {
    course_plan_code: String,
    audience_kind: ImportedAudienceKind,
    audience_code: String,
    meeting_ordinal: u16,
    day: ImportedDay,
    period: u16,
    duration: u8,
    room_code: String,
    teacher_code: String,
});

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ParsedBatch {
    pub students: Vec<StudentImportRow>,
    pub administrative_classes: Vec<AdministrativeClassImportRow>,
    pub student_subject_choices: Vec<StudentSubjectChoiceImportRow>,
    pub teachers: Vec<TeacherImportRow>,
    pub teacher_unavailability: Vec<TeacherUnavailabilityImportRow>,
    pub rooms: Vec<RoomImportRow>,
    pub course_plans: Vec<CoursePlanImportRow>,
    pub teaching_sections: Vec<TeachingSectionImportRow>,
    pub section_enrollments: Vec<SectionEnrollmentImportRow>,
    pub course_offerings: Vec<CourseOfferingImportRow>,
    pub fixed_activities: Vec<FixedActivityImportRow>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImportBatch(ParsedBatch);

impl ImportBatch {
    /// Revalidates typed row values and the complete cross-table graph after deserialization.
    ///
    /// Deserialization is not evidence that a batch passed the importer. The supplied config
    /// must use the same subject-choice rule as the original import. CSV headers, mappings and
    /// dataset presence are source properties checked by `CsvImporter::import`; they cannot be
    /// reconstructed from the parsed rows and are not checked here.
    ///
    /// # Errors
    ///
    /// Returns all detected value and relationship problems without changing this batch.
    pub fn revalidate(&self, config: &crate::ImportConfig) -> Result<(), crate::ImportFailure> {
        let mut problems = Vec::new();
        crate::validate::validate_batch(&self.0, config, &mut problems);
        if problems.is_empty() {
            Ok(())
        } else {
            Err(crate::ImportFailure::new(problems))
        }
    }

    pub(crate) const fn from_validated(parsed: ParsedBatch) -> Self {
        Self(parsed)
    }

    #[must_use]
    pub fn students(&self) -> &[StudentImportRow] {
        &self.0.students
    }
    #[must_use]
    pub fn administrative_classes(&self) -> &[AdministrativeClassImportRow] {
        &self.0.administrative_classes
    }
    #[must_use]
    pub fn student_subject_choices(&self) -> &[StudentSubjectChoiceImportRow] {
        &self.0.student_subject_choices
    }
    #[must_use]
    pub fn teachers(&self) -> &[TeacherImportRow] {
        &self.0.teachers
    }
    #[must_use]
    pub fn teacher_unavailability(&self) -> &[TeacherUnavailabilityImportRow] {
        &self.0.teacher_unavailability
    }
    #[must_use]
    pub fn rooms(&self) -> &[RoomImportRow] {
        &self.0.rooms
    }
    #[must_use]
    pub fn course_plans(&self) -> &[CoursePlanImportRow] {
        &self.0.course_plans
    }
    #[must_use]
    pub fn teaching_sections(&self) -> &[TeachingSectionImportRow] {
        &self.0.teaching_sections
    }
    #[must_use]
    pub fn section_enrollments(&self) -> &[SectionEnrollmentImportRow] {
        &self.0.section_enrollments
    }
    #[must_use]
    pub fn course_offerings(&self) -> &[CourseOfferingImportRow] {
        &self.0.course_offerings
    }
    #[must_use]
    pub fn fixed_activities(&self) -> &[FixedActivityImportRow] {
        &self.0.fixed_activities
    }
}
