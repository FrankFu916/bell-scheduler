use class_schedule_import::{ImportBatch, SectionEnrollmentImportRow, TeachingSectionImportRow};
use class_schedule_sectioning::SectioningDigest;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::CalendarDefinition;

/// Version of the durable JSON document stored inside a project revision.
///
/// The `SQLite` schema version and this payload version are intentionally separate: `SQLite`
/// migrations describe the storage tables, while this version describes the application-owned
/// import document that can be decoded after a future binary upgrade.
pub const IMPORTED_PROJECT_DOCUMENT_SCHEMA_VERSION: u32 = 1;

/// A validated, lossless import snapshot suitable for an atomic project revision.
///
/// This is an application persistence document, not a transport DTO and not the solver-native
/// scheduling snapshot. The original import is kept intact. When Input B generated sections,
/// the selected materialization is stored alongside the original student choices so the source
/// data and the derived decision remain auditable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImportedProjectDocument {
    pub schema_version: u32,
    pub project_stable_key: String,
    pub calendar: CalendarDefinition,
    pub import_batch: ImportBatch,
    pub generated_sectioning: Option<MaterializedSectioning>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MaterializedSectioning {
    pub candidate_hash: SectioningDigest,
    pub sections: Vec<TeachingSectionImportRow>,
    pub enrollments: Vec<SectionEnrollmentImportRow>,
}

#[derive(Debug, Error)]
pub enum ImportedProjectDocumentError {
    #[error("project stable key must not be blank")]
    BlankProjectStableKey,
    #[error("unsupported imported project document schema version {found}")]
    UnsupportedSchema { found: u32 },
    #[error("imported project document JSON serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl ImportedProjectDocument {
    /// Builds a durable document from a fully validated import and optional derived sectioning.
    ///
    /// # Errors
    ///
    /// Rejects a blank stable key. The importer and compiler remain responsible for validating
    /// the contents of `import_batch` and `calendar` before this constructor is called.
    pub fn new(
        project_stable_key: impl Into<String>,
        calendar: CalendarDefinition,
        import_batch: ImportBatch,
        generated_sectioning: Option<MaterializedSectioning>,
    ) -> Result<Self, ImportedProjectDocumentError> {
        let project_stable_key = project_stable_key.into();
        if project_stable_key.trim().is_empty() {
            return Err(ImportedProjectDocumentError::BlankProjectStableKey);
        }
        Ok(Self {
            schema_version: IMPORTED_PROJECT_DOCUMENT_SCHEMA_VERSION,
            project_stable_key,
            calendar,
            import_batch,
            generated_sectioning,
        })
    }

    /// Encodes the canonical application document for a `SQLite` project revision.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the document cannot be encoded as JSON.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, ImportedProjectDocumentError> {
        self.validate_schema()?;
        Ok(serde_json::to_vec(self)?)
    }

    /// Decodes and checks a persisted application document.
    ///
    /// # Errors
    ///
    /// Returns a structured error for malformed JSON, an unsupported schema version, or a blank
    /// stable key. This method is deliberately separate from `SQLite` table migration.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, ImportedProjectDocumentError> {
        let document: Self = serde_json::from_slice(bytes)?;
        document.validate_schema()?;
        Ok(document)
    }

    fn validate_schema(&self) -> Result<(), ImportedProjectDocumentError> {
        if self.schema_version != IMPORTED_PROJECT_DOCUMENT_SCHEMA_VERSION {
            return Err(ImportedProjectDocumentError::UnsupportedSchema {
                found: self.schema_version,
            });
        }
        if self.project_stable_key.trim().is_empty() {
            return Err(ImportedProjectDocumentError::BlankProjectStableKey);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use class_schedule_import::{CsvImporter, CsvSource, DatasetKind, ImportConfig};

    use super::*;

    fn fixture_batch() -> ImportBatch {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/small");
        let kinds = [
            DatasetKind::Students,
            DatasetKind::AdministrativeClasses,
            DatasetKind::StudentSubjectChoices,
            DatasetKind::Teachers,
            DatasetKind::Rooms,
            DatasetKind::CoursePlans,
        ];
        let sources = kinds
            .into_iter()
            .map(|kind| {
                let bytes = fs::read(root.join(format!("{}.csv", kind.as_str())))
                    .expect("checked-in fixture");
                (kind, bytes)
            })
            .collect::<Vec<_>>();
        CsvImporter::new(ImportConfig::default())
            .import(
                sources
                    .iter()
                    .map(|(kind, bytes)| CsvSource::new(*kind, bytes)),
            )
            .expect("fixture import")
    }

    #[test]
    fn durable_document_round_trips_without_losing_import_data() {
        let document = ImportedProjectDocument::new(
            "document-test",
            CalendarDefinition::weekday_with_break(8, 4).expect("calendar"),
            fixture_batch(),
            Some(MaterializedSectioning {
                candidate_hash: SectioningDigest([7; 32]),
                sections: Vec::new(),
                enrollments: Vec::new(),
            }),
        )
        .expect("document");

        let bytes = document.to_json_bytes().expect("encode");
        let decoded = ImportedProjectDocument::from_json_bytes(&bytes).expect("decode");
        assert_eq!(decoded, document);
        assert_eq!(
            decoded.schema_version,
            IMPORTED_PROJECT_DOCUMENT_SCHEMA_VERSION
        );
    }

    #[test]
    fn unsupported_document_schema_is_rejected_before_use() {
        let document = ImportedProjectDocument::new(
            "document-test",
            CalendarDefinition::weekday_with_break(8, 4).expect("calendar"),
            fixture_batch(),
            None,
        )
        .expect("document");
        let mut value = serde_json::to_value(document).expect("value");
        value["schema_version"] = serde_json::json!(99);
        let bytes = serde_json::to_vec(&value).expect("encode");
        assert!(matches!(
            ImportedProjectDocument::from_json_bytes(&bytes),
            Err(ImportedProjectDocumentError::UnsupportedSchema { found: 99 })
        ));
    }
}
