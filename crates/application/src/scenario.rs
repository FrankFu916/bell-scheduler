//! Explicit adoption and independent copies of revalidated saved runs.

mod document;

use chrono::{DateTime, Utc};
use class_schedule_domain::{ScenarioId, SchoolProjectId, SolverRunId, Timetable, TimetableId};
use class_schedule_persistence::{
    CopyScenarioSource, PersistenceError, ScenarioDocument, SqliteStore,
};
use class_schedule_scheduling::Assignment;
use class_schedule_scoring::ObjectiveVector;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    CompiledSchoolProblem, ImportedProjectDocument, MaterializedSectioning, SolveArtifactError,
    load_solve_artifact,
};
use document::{
    ScenarioPayload, TimetablePayload, build_documents, decode_documents, replay_timetable,
};

pub const SCENARIO_DOCUMENT_SCHEMA_VERSION: u32 = 1;
pub const TIMETABLE_DOCUMENT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub struct AdoptRunCommand {
    pub project_id: SchoolProjectId,
    pub expected_source_revision: u64,
    pub run_id: SolverRunId,
    pub scenario_id: ScenarioId,
    pub display_name: String,
}

#[derive(Clone, Debug)]
pub struct CloneScenarioCommand {
    pub project_id: SchoolProjectId,
    pub expected_source_revision: u64,
    pub parent_scenario_id: ScenarioId,
    pub expected_scenario_revision: u64,
    pub expected_timetable_revision: u64,
    pub scenario_id: ScenarioId,
    pub display_name: String,
}

/// Exact immutable parent identity; loading a child does not read its parent's current state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioCloneLineage {
    pub scenario_id: ScenarioId,
    pub scenario_revision: u64,
    pub scenario_payload_hash: [u8; 32],
    pub timetable_id: TimetableId,
    pub timetable_revision: u64,
    pub timetable_payload_hash: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioReceipt {
    pub project_id: SchoolProjectId,
    pub source_project_revision: u64,
    pub source_payload_hash: String,
    pub scenario_id: ScenarioId,
    pub scenario_revision: u64,
    pub scenario_payload_hash: String,
    pub timetable_id: TimetableId,
    pub timetable_revision: u64,
    pub timetable_payload_hash: String,
    pub origin_run_id: SolverRunId,
    pub origin_artifact_hash: String,
    pub created_at: DateTime<Utc>,
}

/// Cannot be constructed or altered by a transport. All business validation precedes its write.
#[derive(Debug)]
pub struct PreparedScenarioCreation {
    document: ScenarioDocument,
    parent: Option<CopyScenarioSource>,
    receipt: ScenarioReceipt,
}

impl PreparedScenarioCreation {
    pub const fn receipt(&self) -> &ScenarioReceipt {
        &self.receipt
    }
}

/// Full historical revalidation has completed. The source may be historical but is never rebased.
#[derive(Debug)]
pub struct LoadedScenario {
    receipt: ScenarioReceipt,
    display_name: String,
    lineage: Option<ScenarioCloneLineage>,
    source_is_current: bool,
    source_document: ImportedProjectDocument,
    materialized_sectioning: Option<MaterializedSectioning>,
    compiled: CompiledSchoolProblem,
    assignments: Vec<Assignment>,
    timetable: Timetable,
    quality: ObjectiveVector,
}

impl LoadedScenario {
    pub const fn receipt(&self) -> &ScenarioReceipt {
        &self.receipt
    }
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
    pub const fn lineage(&self) -> Option<&ScenarioCloneLineage> {
        self.lineage.as_ref()
    }
    pub const fn source_is_current(&self) -> bool {
        self.source_is_current
    }
    pub const fn source_document(&self) -> &ImportedProjectDocument {
        &self.source_document
    }
    pub const fn materialized_sectioning(&self) -> Option<&MaterializedSectioning> {
        self.materialized_sectioning.as_ref()
    }
    pub const fn compiled(&self) -> &CompiledSchoolProblem {
        &self.compiled
    }
    pub fn assignments(&self) -> &[Assignment] {
        &self.assignments
    }
    pub const fn timetable(&self) -> &Timetable {
        &self.timetable
    }
    pub const fn quality(&self) -> &ObjectiveVector {
        &self.quality
    }
}

#[derive(Debug, Error)]
pub enum ScenarioApplicationError {
    #[error("scenario persistence failed")]
    Persistence(#[from] PersistenceError),
    #[error("origin run failed independent revalidation")]
    Artifact(#[from] SolveArtifactError),
    #[error("scenario document encoding failed")]
    Encoding(#[from] serde_json::Error),
    #[error("scenario command or document failed validation: {code}")]
    Invalid { code: &'static str },
}

impl ScenarioApplicationError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Persistence(error) => error.code(),
            Self::Artifact(error) => error.code(),
            Self::Encoding(_) => "APPLICATION_SCENARIO_DOCUMENT_ENCODING",
            Self::Invalid { code } => code,
        }
    }
}

/// Revalidates the run and prepares a new, explicitly adopted scenario without writing.
///
/// # Errors
/// Rejects stale sources, unrelated or non-successful runs, invalid names, and replay failures.
pub fn prepare_adopt_run(
    store: &SqliteStore,
    command: &AdoptRunCommand,
) -> Result<PreparedScenarioCreation, ScenarioApplicationError> {
    let loaded = load_solve_artifact(store, &command.run_id.to_string())?;
    if loaded.receipt.source.project_id != command.project_id
        || loaded.receipt.source.revision != command.expected_source_revision
    {
        return Err(invalid("APPLICATION_SCENARIO_RUN_SOURCE_MISMATCH"));
    }
    ensure_current_source(
        store,
        command.project_id,
        command.expected_source_revision,
        parse_hash(&loaded.receipt.source.payload_hash)?,
    )?;
    let (scenario, timetable) =
        build_documents(&loaded, command.scenario_id, &command.display_name, None)?;
    prepare_documents(&scenario, &timetable, None)
}

/// Prepares an independent copy, retaining exact parent revision/hash lineage.
///
/// # Errors
/// Rejects stale source/parent revisions, self-copy, invalid names, and historical replay failures.
pub fn prepare_clone_scenario(
    store: &SqliteStore,
    command: &CloneScenarioCommand,
) -> Result<PreparedScenarioCreation, ScenarioApplicationError> {
    let parent = load_scenario(store, command.parent_scenario_id)?;
    let receipt = parent.receipt();
    if receipt.project_id != command.project_id
        || receipt.source_project_revision != command.expected_source_revision
    {
        return Err(invalid("APPLICATION_SCENARIO_PARENT_SOURCE_MISMATCH"));
    }
    if command.scenario_id == command.parent_scenario_id {
        return Err(invalid("APPLICATION_SCENARIO_SELF_COPY"));
    }
    if receipt.scenario_revision != command.expected_scenario_revision {
        return Err(invalid("APPLICATION_SCENARIO_REVISION_CONFLICT"));
    }
    if receipt.timetable_revision != command.expected_timetable_revision {
        return Err(invalid("APPLICATION_TIMETABLE_REVISION_CONFLICT"));
    }
    ensure_current_source(
        store,
        command.project_id,
        command.expected_source_revision,
        parse_hash(&receipt.source_payload_hash)?,
    )?;
    let lineage = ScenarioCloneLineage {
        scenario_id: receipt.scenario_id,
        scenario_revision: receipt.scenario_revision,
        scenario_payload_hash: parse_hash(&receipt.scenario_payload_hash)?,
        timetable_id: receipt.timetable_id,
        timetable_revision: receipt.timetable_revision,
        timetable_payload_hash: parse_hash(&receipt.timetable_payload_hash)?,
    };
    let expected = CopyScenarioSource {
        scenario_id: lineage.scenario_id.to_string(),
        expected_scenario_revision: lineage.scenario_revision,
        expected_scenario_payload_hash: lineage.scenario_payload_hash,
        expected_timetable_id: lineage.timetable_id.to_string(),
        expected_timetable_revision: lineage.timetable_revision,
        expected_timetable_payload_hash: lineage.timetable_payload_hash,
    };
    let loaded = load_solve_artifact(store, &receipt.origin_run_id.to_string())?;
    let (scenario, timetable) = build_documents(
        &loaded,
        command.scenario_id,
        &command.display_name,
        Some(lineage),
    )?;
    prepare_documents(&scenario, &timetable, Some(expected))
}

/// Commits the privately prepared documents with source/run/parent CAS in one `SQLite` transaction.
///
/// # Errors
/// Returns revision/hash conflicts, duplicate IDs, or transactional storage errors. No partial
/// scenario or timetable is retained on failure and the import revision never changes.
pub fn commit_prepared_scenario_creation(
    store: &mut SqliteStore,
    prepared: PreparedScenarioCreation,
) -> Result<ScenarioReceipt, ScenarioApplicationError> {
    match &prepared.parent {
        Some(parent) => store.copy_scenario(parent, &prepared.document)?,
        None => store.adopt_scenario(&prepared.document)?,
    }
    Ok(prepared.receipt)
}

/// Opens a scenario from exact historical input and revalidates every assignment and its score.
///
/// # Errors
/// Rejects unsupported documents, inconsistent provenance, unknown stable IDs, and Hard failures.
pub fn load_scenario(
    store: &SqliteStore,
    scenario_id: ScenarioId,
) -> Result<LoadedScenario, ScenarioApplicationError> {
    let stored = store.load_scenario(&scenario_id.to_string())?;
    let (scenario, timetable) = decode_documents(&stored)?;
    let loaded = load_solve_artifact(store, &scenario.origin_run_id.to_string())?;
    let (compiled, assignments, domain_timetable, quality) =
        replay_timetable(&loaded, &scenario, &timetable)?;
    let current = store.load_project(&scenario.project_id.to_string())?;
    let source_is_current = current.revision == scenario.source_project_revision
        && blake3::hash(&current.payload).as_bytes() == &scenario.source_payload_hash;
    Ok(LoadedScenario {
        receipt: make_receipt(
            &scenario,
            &timetable,
            stored.scenario_payload_hash,
            stored.timetable_payload_hash,
        ),
        display_name: scenario.display_name,
        lineage: scenario.lineage,
        source_is_current,
        source_document: loaded.source_document().clone(),
        materialized_sectioning: scenario.materialized_sectioning,
        compiled,
        assignments,
        timetable: domain_timetable,
        quality,
    })
}

fn prepare_documents(
    scenario: &ScenarioPayload,
    timetable: &TimetablePayload,
    parent: Option<CopyScenarioSource>,
) -> Result<PreparedScenarioCreation, ScenarioApplicationError> {
    let scenario_payload = serde_json::to_vec(scenario)?;
    let timetable_payload = serde_json::to_vec(timetable)?;
    let receipt = make_receipt(
        scenario,
        timetable,
        *blake3::hash(&scenario_payload).as_bytes(),
        *blake3::hash(&timetable_payload).as_bytes(),
    );
    let document = ScenarioDocument {
        scenario_id: scenario.scenario_id.to_string(),
        project_id: scenario.project_id.to_string(),
        display_name: scenario.display_name.clone(),
        scenario_revision: scenario.scenario_revision,
        timetable_id: timetable.timetable_id.to_string(),
        timetable_revision: timetable.timetable_revision,
        source_project_revision: scenario.source_project_revision,
        source_payload_hash: scenario.source_payload_hash,
        origin_run_id: scenario.origin_run_id.to_string(),
        origin_artifact_hash: scenario.origin_artifact_hash,
        scenario_schema_version: SCENARIO_DOCUMENT_SCHEMA_VERSION,
        scenario_payload,
        timetable_schema_version: TIMETABLE_DOCUMENT_SCHEMA_VERSION,
        timetable_payload,
        created_at: scenario.created_at,
    };
    Ok(PreparedScenarioCreation {
        document,
        parent,
        receipt,
    })
}

fn make_receipt(
    scenario: &ScenarioPayload,
    timetable: &TimetablePayload,
    scenario_hash: [u8; 32],
    timetable_hash: [u8; 32],
) -> ScenarioReceipt {
    ScenarioReceipt {
        project_id: scenario.project_id,
        source_project_revision: scenario.source_project_revision,
        source_payload_hash: hash_text(scenario.source_payload_hash),
        scenario_id: scenario.scenario_id,
        scenario_revision: scenario.scenario_revision,
        scenario_payload_hash: hash_text(scenario_hash),
        timetable_id: timetable.timetable_id,
        timetable_revision: timetable.timetable_revision,
        timetable_payload_hash: hash_text(timetable_hash),
        origin_run_id: scenario.origin_run_id,
        origin_artifact_hash: hash_text(scenario.origin_artifact_hash),
        created_at: scenario.created_at,
    }
}

fn ensure_current_source(
    store: &SqliteStore,
    project_id: SchoolProjectId,
    revision: u64,
    hash: [u8; 32],
) -> Result<(), ScenarioApplicationError> {
    let current = store.load_project(&project_id.to_string())?;
    if current.revision != revision {
        return Err(PersistenceError::RevisionConflict {
            project_id: project_id.to_string(),
            expected_revision: revision,
            actual_revision: current.revision,
        }
        .into());
    }
    if blake3::hash(&current.payload).as_bytes() != &hash {
        return Err(invalid("APPLICATION_SCENARIO_SOURCE_HASH_MISMATCH"));
    }
    Ok(())
}

fn parse_hash(value: &str) -> Result<[u8; 32], ScenarioApplicationError> {
    blake3::Hash::from_hex(value)
        .map(|hash| *hash.as_bytes())
        .map_err(|_| invalid("APPLICATION_SCENARIO_INVALID_HASH"))
}

fn hash_text(hash: [u8; 32]) -> String {
    blake3::Hash::from(hash).to_hex().to_string()
}

fn invalid(code: &'static str) -> ScenarioApplicationError {
    ScenarioApplicationError::Invalid { code }
}
