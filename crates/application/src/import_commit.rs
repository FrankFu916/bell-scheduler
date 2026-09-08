//! Shared import audit and revision-checked commit. Preparation cannot access a database.

use class_schedule_domain::SchoolProjectId;
use class_schedule_import::{
    CsvImporter, CsvSource, ImportBatch, ImportConfig, ImportFailure, WorkbookCsvDataset,
    WorkbookFailure, WorkbookSheetMapping, XlsxWorkbook,
};
use class_schedule_persistence::{PersistenceError, ProjectDocument, SqliteStore};
use class_schedule_validation::{ValidationReport, static_feasibility_check};
use thiserror::Error;

use crate::{
    AutoSectioningError, AutoSectioningPolicy, AutoSectioningPreparation, CalendarDefinition,
    CompileError, CompiledSchoolProblem, ImportedProjectDocument, ImportedProjectDocumentError,
    compile_import_batch, compile_import_batch_with_sectioning, prepare_auto_sectioning,
};

#[derive(Clone, Copy, Debug)]
pub enum CsvImportMode {
    ExistingSections,
    Unsectioned(AutoSectioningPolicy),
}

#[derive(Clone, Debug)]
pub struct CsvImportAuditOptions {
    pub project_stable_key: String,
    pub calendar: CalendarDefinition,
    pub exact_subject_choices: usize,
    pub mode: CsvImportMode,
}

/// Aggregate bytes for a single tabular command, before and after XLSX normalization.
pub const MAXIMUM_TABULAR_IMPORT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct WorkbookImportSource<'a> {
    pub bytes: &'a [u8],
    pub mappings: &'a [WorkbookSheetMapping],
}

#[derive(Clone, Debug)]
pub struct ImportCandidateAudit {
    pub compiled: CompiledSchoolProblem,
    pub static_validation: ValidationReport,
}

#[derive(Clone, Debug)]
pub struct CsvImportAudit {
    pub batch: ImportBatch,
    pub sectioning: Option<AutoSectioningPreparation>,
    pub candidates: Vec<ImportCandidateAudit>,
}

#[derive(Clone, Copy, Debug)]
pub enum ImportCommitIntent {
    Create,
    Replace { expected_revision: u64 },
}

#[derive(Clone, Debug)]
pub struct ImportCommitCommand {
    pub project_id: SchoolProjectId,
    pub display_name: String,
    pub intent: ImportCommitIntent,
    pub options: CsvImportAuditOptions,
}

/// Only this module can construct a prepared write; transports cannot substitute a payload.
#[derive(Debug)]
pub struct PreparedImportCommit {
    document: ProjectDocument,
    project_id: SchoolProjectId,
    intent: ImportCommitIntent,
    sectioning_required: bool,
    stable_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportCommitReceipt {
    pub project_id: SchoolProjectId,
    pub revision: u64,
    pub document_schema_version: u32,
    pub payload_hash: String,
    pub sectioning_required: bool,
}

#[derive(Debug)]
pub struct LoadedImportedProject {
    pub receipt: ImportCommitReceipt,
    pub display_name: String,
    pub document: ImportedProjectDocument,
    /// None is an unresolved Input B, never a formal empty timetable or successful solve.
    pub compiled: Option<CompiledSchoolProblem>,
}

#[derive(Debug, Error)]
pub enum ImportCommandError {
    #[error("import command field is invalid: {field}")]
    InvalidField { field: &'static str },
    #[error("import commit v1 requires exactly three subject choices per student")]
    UnsupportedChoicePolicy,
    #[error("CSV import was rejected")]
    Import(#[from] ImportFailure),
    #[error("XLSX workbook decoding was rejected")]
    Workbook(#[from] WorkbookFailure),
    #[error("tabular import exceeds the {stage} byte limit")]
    ImportResourceLimit {
        stage: &'static str,
        maximum_bytes: usize,
    },
    #[error("semantic compilation failed")]
    Compile(#[from] CompileError),
    #[error("sectioning preparation failed")]
    Sectioning(#[from] AutoSectioningError),
    #[error("no audited candidate passed static Hard pre-check")]
    PrecheckRejected { reports: Vec<ValidationReport> },
    #[error("import document encoding or schema validation failed")]
    Document(#[from] ImportedProjectDocumentError),
    #[error("project persistence failed")]
    Persistence(#[from] PersistenceError),
    #[error("replacement cannot change the project stable key")]
    StableKeyMismatch,
    #[error("stored row and payload document versions do not match")]
    DocumentVersionMismatch,
    #[error("materialized sectioning requires an independently validated selection command")]
    UnsupportedMaterialization,
}

impl ImportCommandError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidField { .. } => "APPLICATION_IMPORT_INVALID_FIELD",
            Self::UnsupportedChoicePolicy => "APPLICATION_IMPORT_CHOICE_POLICY_UNSUPPORTED",
            Self::Import(_) => "APPLICATION_IMPORT_REJECTED",
            Self::Workbook(_) => "APPLICATION_WORKBOOK_REJECTED",
            Self::ImportResourceLimit { .. } => "APPLICATION_IMPORT_RESOURCE_LIMIT",
            Self::Compile(error) => error.code(),
            Self::Sectioning(error) => error.code(),
            Self::PrecheckRejected { .. } => "APPLICATION_IMPORT_PRECHECK_REJECTED",
            Self::Document(_) => "APPLICATION_IMPORT_DOCUMENT_INVALID",
            Self::Persistence(error) => error.code(),
            Self::StableKeyMismatch => "APPLICATION_IMPORT_STABLE_KEY_MISMATCH",
            Self::DocumentVersionMismatch => "APPLICATION_IMPORT_DOCUMENT_VERSION_MISMATCH",
            Self::UnsupportedMaterialization => "APPLICATION_IMPORT_MATERIALIZATION_UNSUPPORTED",
        }
    }
}

/// Performs the complete read-only CSV audit used by all transports.
///
/// # Errors
/// Returns structured parsing, reference, sectioning or compilation errors. Static problems
/// remain in the audit so the UI can explain each candidate, including rejected candidates.
pub fn audit_csv_import<'a>(
    sources: impl IntoIterator<Item = CsvSource<'a>>,
    options: &CsvImportAuditOptions,
) -> Result<CsvImportAudit, ImportCommandError> {
    validate_options(options)?;
    let batch = CsvImporter::new(
        ImportConfig::default().with_exact_subject_choices(Some(options.exact_subject_choices)),
    )
    .import(sources)?;
    audit_batch(batch, options)
}

/// Decodes all workbook sheets and combines them with CSV before one complete shared audit.
///
/// # Errors
/// Returns bounded workbook errors, aggregate resource-limit errors, or the same strict CSV,
/// cross-table, sectioning and compilation errors as [`audit_csv_import`]. Duplicate datasets
/// across file formats are rejected by the existing importer, never merged or overwritten.
pub fn audit_tabular_import<'a>(
    sources: impl IntoIterator<Item = CsvSource<'a>>,
    workbooks: impl IntoIterator<Item = WorkbookImportSource<'a>>,
    options: &CsvImportAuditOptions,
) -> Result<CsvImportAudit, ImportCommandError> {
    let normalized = normalize_tabular_sources(sources, workbooks)?;
    audit_csv_import(normalized.sources(), options)
}

/// Fully normalizes CSV/XLSX input before preparing the existing immutable import command.
///
/// # Errors
/// Returns workbook/resource errors or any existing prepare rejection before database access.
pub fn prepare_tabular_import_commit<'a>(
    command: &ImportCommitCommand,
    sources: impl IntoIterator<Item = CsvSource<'a>>,
    workbooks: impl IntoIterator<Item = WorkbookImportSource<'a>>,
) -> Result<PreparedImportCommit, ImportCommandError> {
    let normalized = normalize_tabular_sources(sources, workbooks)?;
    prepare_csv_import_commit(command, normalized.sources())
}

#[derive(Debug)]
struct NormalizedTabularSources<'a> {
    csv: Vec<CsvSource<'a>>,
    workbook_datasets: Vec<WorkbookCsvDataset>,
}

impl NormalizedTabularSources<'_> {
    fn sources(&self) -> impl Iterator<Item = CsvSource<'_>> {
        self.csv
            .iter()
            .map(|source| CsvSource::new(source.kind(), source.bytes()))
            .chain(
                self.workbook_datasets
                    .iter()
                    .map(|dataset| CsvSource::new(dataset.dataset, &dataset.bytes)),
            )
    }
}

fn normalize_tabular_sources<'a>(
    sources: impl IntoIterator<Item = CsvSource<'a>>,
    workbooks: impl IntoIterator<Item = WorkbookImportSource<'a>>,
) -> Result<NormalizedTabularSources<'a>, ImportCommandError> {
    let csv = sources.into_iter().collect::<Vec<_>>();
    let workbooks = workbooks.into_iter().collect::<Vec<_>>();
    let mut source_bytes = 0;
    for length in csv
        .iter()
        .map(|source| source.bytes().len())
        .chain(workbooks.iter().map(|workbook| workbook.bytes.len()))
    {
        add_tabular_bytes(&mut source_bytes, length, "source")?;
    }
    let mut normalized_bytes = 0;
    for source in &csv {
        add_tabular_bytes(&mut normalized_bytes, source.bytes().len(), "normalized")?;
    }
    let mut workbook_datasets = Vec::new();
    for workbook in workbooks {
        let datasets =
            XlsxWorkbook::read(workbook.bytes, &ImportConfig::default(), workbook.mappings)?;
        for dataset in &datasets {
            add_tabular_bytes(&mut normalized_bytes, dataset.bytes.len(), "normalized")?;
        }
        workbook_datasets.extend(datasets);
    }
    Ok(NormalizedTabularSources {
        csv,
        workbook_datasets,
    })
}

fn add_tabular_bytes(
    total: &mut usize,
    length: usize,
    stage: &'static str,
) -> Result<(), ImportCommandError> {
    *total = total
        .checked_add(length)
        .filter(|total| *total <= MAXIMUM_TABULAR_IMPORT_BYTES)
        .ok_or(ImportCommandError::ImportResourceLimit {
            stage,
            maximum_bytes: MAXIMUM_TABULAR_IMPORT_BYTES,
        })?;
    Ok(())
}

fn validate_options(options: &CsvImportAuditOptions) -> Result<(), ImportCommandError> {
    if options.project_stable_key.trim().is_empty() {
        return Err(ImportCommandError::InvalidField {
            field: "project_stable_key",
        });
    }
    if options.exact_subject_choices == 0 {
        return Err(ImportCommandError::InvalidField {
            field: "exact_subject_choices",
        });
    }
    Ok(())
}

fn audit_batch(
    batch: ImportBatch,
    options: &CsvImportAuditOptions,
) -> Result<CsvImportAudit, ImportCommandError> {
    let key = options.project_stable_key.trim();
    let mut candidates = Vec::new();
    let sectioning = match options.mode {
        CsvImportMode::ExistingSections => {
            candidates.push(audit_compiled(compile_import_batch(
                &batch,
                &options.calendar,
                key,
            )?));
            None
        }
        CsvImportMode::Unsectioned(policy) => {
            // Fields are public for Rust callers; never assume a constructor was used.
            let policy = AutoSectioningPolicy::new(
                policy.minimum_size,
                policy.target_size,
                policy.maximum_size,
                policy.seed,
                policy.profile,
                policy.candidate_count,
            )?;
            let prepared = prepare_auto_sectioning(&batch, key, policy)?;
            for candidate in &prepared.candidates {
                candidates.push(audit_compiled(compile_import_batch_with_sectioning(
                    &batch,
                    &options.calendar,
                    key,
                    candidate,
                )?));
            }
            Some(prepared)
        }
    };
    Ok(CsvImportAudit {
        batch,
        sectioning,
        candidates,
    })
}

fn audit_compiled(compiled: CompiledSchoolProblem) -> ImportCandidateAudit {
    let static_validation = static_feasibility_check(&compiled.problem);
    ImportCandidateAudit {
        compiled,
        static_validation,
    }
}

/// Fully prepares and serializes a command before any database write transaction can start.
///
/// # Errors
/// Rejects invalid metadata, unsupported choice policy, any import/compile error, or an audit
/// without a candidate passing static pre-check. This does not prove timetable feasibility.
pub fn prepare_csv_import_commit<'a>(
    command: &ImportCommitCommand,
    sources: impl IntoIterator<Item = CsvSource<'a>>,
) -> Result<PreparedImportCommit, ImportCommandError> {
    if command.display_name.trim().is_empty() {
        return Err(ImportCommandError::InvalidField {
            field: "display_name",
        });
    }
    if command.options.exact_subject_choices != 3 {
        return Err(ImportCommandError::UnsupportedChoicePolicy);
    }
    let revision = match command.intent {
        ImportCommitIntent::Create => 0,
        ImportCommitIntent::Replace { expected_revision } => expected_revision
            .checked_add(1)
            .filter(|value| i64::try_from(*value).is_ok())
            .ok_or(PersistenceError::IntegerOutOfRange {
                field: "expected_revision",
                value: expected_revision,
            })?,
    };
    let audit = audit_csv_import(sources, &command.options)?;
    require_passing_candidate(&audit.candidates)?;
    let stable_key = command.options.project_stable_key.trim().to_owned();
    let sectioning_required = needs_sectioning(&audit.batch);
    let document = ImportedProjectDocument::new(
        &stable_key,
        command.options.calendar.clone(),
        audit.batch,
        None,
    )?;
    Ok(PreparedImportCommit {
        document: ProjectDocument {
            project_id: command.project_id.to_string(),
            display_name: command.display_name.trim().to_owned(),
            revision,
            document_schema_version: document.schema_version,
            payload: document.to_json_bytes()?,
        },
        project_id: command.project_id,
        intent: command.intent,
        sectioning_required,
        stable_key,
    })
}

fn require_passing_candidate(
    candidates: &[ImportCandidateAudit],
) -> Result<(), ImportCommandError> {
    if candidates
        .iter()
        .any(|candidate| candidate.static_validation.is_valid())
    {
        Ok(())
    } else {
        Err(ImportCommandError::PrecheckRejected {
            reports: candidates
                .iter()
                .map(|candidate| candidate.static_validation.clone())
                .collect(),
        })
    }
}

/// Writes one immutable prepared import through the existing `SQLite` transaction/CAS boundary.
///
/// # Errors
/// Returns stable duplicate, missing-project, revision conflict, identity or database errors.
/// A conflict is never retried and a failed transaction creates no project or revision.
// Consume the plan so callers cannot replay it after a failed or successful write.
#[allow(clippy::needless_pass_by_value)]
pub fn commit_prepared_import(
    store: &mut SqliteStore,
    prepared: PreparedImportCommit,
) -> Result<ImportCommitReceipt, ImportCommandError> {
    match prepared.intent {
        ImportCommitIntent::Create => store.create_project(&prepared.document)?,
        ImportCommitIntent::Replace { expected_revision } => {
            // Read metadata outside the write transaction. The persistence CAS below rejects
            // any intervening command, including one occurring after this read.
            let current = store.load_project(&prepared.document.project_id)?;
            if current.revision != expected_revision {
                return Err(PersistenceError::RevisionConflict {
                    project_id: current.project_id,
                    expected_revision,
                    actual_revision: current.revision,
                }
                .into());
            }
            let current_document = decode_document(&current)?;
            if current_document.project_stable_key != prepared.stable_key {
                return Err(ImportCommandError::StableKeyMismatch);
            }
            store.replace_project(expected_revision, &prepared.document)?;
        }
    }
    Ok(receipt(
        &prepared.document,
        prepared.project_id,
        prepared.sectioning_required,
    ))
}

/// Convenience use case for transports already owning a migrated store.
///
/// # Errors
/// Returns preparation or atomic commit errors without partial project changes.
pub fn commit_csv_import<'a>(
    store: &mut SqliteStore,
    command: &ImportCommitCommand,
    sources: impl IntoIterator<Item = CsvSource<'a>>,
) -> Result<ImportCommitReceipt, ImportCommandError> {
    let prepared = prepare_csv_import_commit(command, sources)?;
    commit_prepared_import(store, prepared)
}

/// Loads, revalidates and recompiles a persisted Input A; Input B remains explicitly unresolved.
///
/// # Errors
/// Rejects corrupt/unsupported payloads, deserialized row invariant failures and static Hard
/// problems. Optional materializations are reserved for a future validated selection command.
pub fn load_imported_project(
    store: &SqliteStore,
    project_id: SchoolProjectId,
) -> Result<LoadedImportedProject, ImportCommandError> {
    let stored = store.load_project(&project_id.to_string())?;
    revalidate_stored_import(stored, project_id)
}

pub(crate) fn revalidate_stored_import(
    stored: ProjectDocument,
    project_id: SchoolProjectId,
) -> Result<LoadedImportedProject, ImportCommandError> {
    let document = decode_document(&stored)?;
    document.import_batch.revalidate(&ImportConfig::default())?;
    CalendarDefinition::new(
        document.calendar.days.clone(),
        document.calendar.periods.clone(),
    )?;
    let sectioning_required = needs_sectioning(&document.import_batch);
    let compiled = if sectioning_required {
        None
    } else {
        let candidate = audit_compiled(compile_import_batch(
            &document.import_batch,
            &document.calendar,
            &document.project_stable_key,
        )?);
        require_passing_candidate(std::slice::from_ref(&candidate))?;
        Some(candidate.compiled)
    };
    Ok(LoadedImportedProject {
        receipt: receipt(&stored, project_id, sectioning_required),
        display_name: stored.display_name,
        document,
        compiled,
    })
}

fn decode_document(
    stored: &ProjectDocument,
) -> Result<ImportedProjectDocument, ImportCommandError> {
    let document = ImportedProjectDocument::from_json_bytes(&stored.payload)?;
    if document.schema_version != stored.document_schema_version {
        return Err(ImportCommandError::DocumentVersionMismatch);
    }
    if document.generated_sectioning.is_some() {
        return Err(ImportCommandError::UnsupportedMaterialization);
    }
    Ok(document)
}

fn needs_sectioning(batch: &ImportBatch) -> bool {
    batch.teaching_sections().is_empty() && !batch.student_subject_choices().is_empty()
}

fn receipt(
    stored: &ProjectDocument,
    project_id: SchoolProjectId,
    sectioning_required: bool,
) -> ImportCommitReceipt {
    ImportCommitReceipt {
        project_id,
        revision: stored.revision,
        document_schema_version: stored.document_schema_version,
        payload_hash: stored.payload_hash().to_hex().to_string(),
        sectioning_required,
    }
}
