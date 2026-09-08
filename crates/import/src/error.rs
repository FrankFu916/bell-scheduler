use crate::DatasetKind;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum ImportProblemCode {
    ImportDuplicateDataset,
    ImportMissingDataset,
    ImportInvalidUtf8,
    ImportCsvSyntax,
    ImportMissingHeader,
    ImportEmptyHeader,
    ImportDuplicateHeader,
    ImportUnknownHeader,
    ImportMissingRequiredHeader,
    ImportInvalidColumnMapping,
    ImportDuplicateMappedField,
    ImportEmptyRequiredValue,
    ImportInvalidUnsignedInteger,
    ImportInvalidBoolean,
    ImportInvalidEnumValue,
    ImportInvalidList,
    ImportDuplicateExternalCode,
    ImportDuplicateRelation,
    ImportMissingReference,
    ImportSubjectChoiceCount,
    ImportDuplicateSubjectChoice,
    ImportSectionSubjectNotSelected,
    ImportMultipleSectionsForSubject,
    ImportMissingSectionForSubject,
    ImportSectionGradeMismatch,
    ImportSectionBelowMinimum,
    ImportSectionAboveMaximum,
    ImportInvalidClassSizeRange,
    ImportInvalidMeetingPattern,
    ImportInvalidRoomPolicy,
    ImportInvalidTeacherAssignment,
    ImportCourseOfferingMismatch,
    ImportFixedActivityMismatch,
}

impl ImportProblemCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ImportDuplicateDataset => "IMPORT_DUPLICATE_DATASET",
            Self::ImportMissingDataset => "IMPORT_MISSING_DATASET",
            Self::ImportInvalidUtf8 => "IMPORT_INVALID_UTF8",
            Self::ImportCsvSyntax => "IMPORT_CSV_SYNTAX",
            Self::ImportMissingHeader => "IMPORT_MISSING_HEADER",
            Self::ImportEmptyHeader => "IMPORT_EMPTY_HEADER",
            Self::ImportDuplicateHeader => "IMPORT_DUPLICATE_HEADER",
            Self::ImportUnknownHeader => "IMPORT_UNKNOWN_HEADER",
            Self::ImportMissingRequiredHeader => "IMPORT_MISSING_REQUIRED_HEADER",
            Self::ImportInvalidColumnMapping => "IMPORT_INVALID_COLUMN_MAPPING",
            Self::ImportDuplicateMappedField => "IMPORT_DUPLICATE_MAPPED_FIELD",
            Self::ImportEmptyRequiredValue => "IMPORT_EMPTY_REQUIRED_VALUE",
            Self::ImportInvalidUnsignedInteger => "IMPORT_INVALID_UNSIGNED_INTEGER",
            Self::ImportInvalidBoolean => "IMPORT_INVALID_BOOLEAN",
            Self::ImportInvalidEnumValue => "IMPORT_INVALID_ENUM_VALUE",
            Self::ImportInvalidList => "IMPORT_INVALID_LIST",
            Self::ImportDuplicateExternalCode => "IMPORT_DUPLICATE_EXTERNAL_CODE",
            Self::ImportDuplicateRelation => "IMPORT_DUPLICATE_RELATION",
            Self::ImportMissingReference => "IMPORT_MISSING_REFERENCE",
            Self::ImportSubjectChoiceCount => "IMPORT_SUBJECT_CHOICE_COUNT",
            Self::ImportDuplicateSubjectChoice => "IMPORT_DUPLICATE_SUBJECT_CHOICE",
            Self::ImportSectionSubjectNotSelected => "IMPORT_SECTION_SUBJECT_NOT_SELECTED",
            Self::ImportMultipleSectionsForSubject => "IMPORT_MULTIPLE_SECTIONS_FOR_SUBJECT",
            Self::ImportMissingSectionForSubject => "IMPORT_MISSING_SECTION_FOR_SUBJECT",
            Self::ImportSectionGradeMismatch => "IMPORT_SECTION_GRADE_MISMATCH",
            Self::ImportSectionBelowMinimum => "IMPORT_SECTION_BELOW_MINIMUM",
            Self::ImportSectionAboveMaximum => "IMPORT_SECTION_ABOVE_MAXIMUM",
            Self::ImportInvalidClassSizeRange => "IMPORT_INVALID_CLASS_SIZE_RANGE",
            Self::ImportInvalidMeetingPattern => "IMPORT_INVALID_MEETING_PATTERN",
            Self::ImportInvalidRoomPolicy => "IMPORT_INVALID_ROOM_POLICY",
            Self::ImportInvalidTeacherAssignment => "IMPORT_INVALID_TEACHER_ASSIGNMENT",
            Self::ImportCourseOfferingMismatch => "IMPORT_COURSE_OFFERING_MISMATCH",
            Self::ImportFixedActivityMismatch => "IMPORT_FIXED_ACTIVITY_MISMATCH",
        }
    }
}

impl fmt::Display for ImportProblemCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImportLocation {
    dataset: DatasetKind,
    row: Option<u64>,
    column: Option<String>,
}

impl ImportLocation {
    #[must_use]
    pub fn dataset(dataset: DatasetKind) -> Self {
        Self {
            dataset,
            row: None,
            column: None,
        }
    }

    #[must_use]
    pub fn row(dataset: DatasetKind, row: u64) -> Self {
        Self {
            dataset,
            row: Some(row),
            column: None,
        }
    }

    #[must_use]
    pub fn cell(dataset: DatasetKind, row: u64, column: impl Into<String>) -> Self {
        Self {
            dataset,
            row: Some(row),
            column: Some(column.into()),
        }
    }

    #[must_use]
    pub const fn dataset_kind(&self) -> DatasetKind {
        self.dataset
    }

    #[must_use]
    pub const fn row_number(&self) -> Option<u64> {
        self.row
    }

    #[must_use]
    pub fn column(&self) -> Option<&str> {
        self.column.as_deref()
    }
}

/// A localizable import problem. Parameters are deliberately numeric/non-sensitive; raw cell
/// values and student names are never retained in the default error representation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImportProblem {
    code: ImportProblemCode,
    location: ImportLocation,
    related_row: Option<u64>,
    expected: Option<u64>,
    actual: Option<u64>,
}

impl ImportProblem {
    #[must_use]
    pub const fn new(code: ImportProblemCode, location: ImportLocation) -> Self {
        Self {
            code,
            location,
            related_row: None,
            expected: None,
            actual: None,
        }
    }

    #[must_use]
    pub const fn with_related_row(mut self, row: u64) -> Self {
        self.related_row = Some(row);
        self
    }

    #[must_use]
    pub const fn with_counts(mut self, expected: u64, actual: u64) -> Self {
        self.expected = Some(expected);
        self.actual = Some(actual);
        self
    }

    #[must_use]
    pub const fn code(&self) -> ImportProblemCode {
        self.code
    }

    #[must_use]
    pub const fn location(&self) -> &ImportLocation {
        &self.location
    }

    #[must_use]
    pub const fn related_row(&self) -> Option<u64> {
        self.related_row
    }

    #[must_use]
    pub const fn expected(&self) -> Option<u64> {
        self.expected
    }

    #[must_use]
    pub const fn actual(&self) -> Option<u64> {
        self.actual
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("CSV import failed with {count} problem(s)", count = .problems.len())]
pub struct ImportFailure {
    problems: Vec<ImportProblem>,
}

impl ImportFailure {
    #[must_use]
    pub(crate) fn new(problems: Vec<ImportProblem>) -> Self {
        debug_assert!(!problems.is_empty());
        Self { problems }
    }

    #[must_use]
    pub fn problems(&self) -> &[ImportProblem] {
        &self.problems
    }

    #[must_use]
    pub fn into_problems(self) -> Vec<ImportProblem> {
        self.problems
    }
}
