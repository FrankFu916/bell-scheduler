//! Atomic scenario creation with independent immutable scenario and timetable payloads.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{PersistenceError, SqliteStore, sqlite_integer, timestamp};

/// Maximum bytes in each complete scenario, timetable, or referenced JSON payload.
pub const MAXIMUM_SCENARIO_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioDocument {
    pub scenario_id: String,
    pub timetable_id: String,
    pub project_id: String,
    pub display_name: String,
    pub scenario_revision: u64,
    pub timetable_revision: u64,
    pub source_project_revision: u64,
    pub source_payload_hash: [u8; 32],
    pub origin_run_id: String,
    pub origin_artifact_hash: [u8; 32],
    pub scenario_schema_version: u32,
    pub scenario_payload: Vec<u8>,
    pub timetable_schema_version: u32,
    pub timetable_payload: Vec<u8>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyScenarioSource {
    pub scenario_id: String,
    pub expected_scenario_revision: u64,
    pub expected_scenario_payload_hash: [u8; 32],
    pub expected_timetable_id: String,
    pub expected_timetable_revision: u64,
    pub expected_timetable_payload_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredScenario {
    pub document: ScenarioDocument,
    pub scenario_payload_hash: [u8; 32],
    pub timetable_payload_hash: [u8; 32],
}

/// Metadata only. Payload integrity and business validity require a separate scenario load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioSummary {
    pub scenario_id: String,
    pub project_id: String,
    pub display_name: String,
    pub scenario_revision: u64,
    pub timetable_id: String,
    pub timetable_revision: u64,
    pub source_project_revision: u64,
    pub created_at: String,
}

impl ScenarioDocument {
    fn validate(&self) -> Result<(), PersistenceError> {
        if [
            &self.scenario_id,
            &self.timetable_id,
            &self.project_id,
            &self.display_name,
            &self.origin_run_id,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
            || self.scenario_schema_version == 0
            || self.timetable_schema_version == 0
            || self.created_at.timestamp_subsec_nanos() % 1_000_000 != 0
        {
            return Err(invalid(
                "PERSISTENCE_INVALID_SCENARIO",
                "scenario identifiers, name, schema versions, and millisecond timestamp are required",
            ));
        }
        for (value, field) in [
            (self.scenario_revision, "scenario_revision"),
            (self.timetable_revision, "timetable_revision"),
            (self.source_project_revision, "source_project_revision"),
        ] {
            sqlite_integer(value, field)?;
        }
        validate_json(&self.scenario_payload)?;
        validate_json(&self.timetable_payload)
    }
}

impl SqliteStore {
    /// Lists at most 101 metadata records without loading scenario or timetable payloads.
    ///
    /// # Errors
    /// Returns a stable limit error or a database error, including broken metadata relationships.
    pub fn list_scenarios(
        &self,
        project_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<ScenarioSummary>, PersistenceError> {
        if !(1..=101).contains(&limit) {
            return Err(invalid(
                "PERSISTENCE_INVALID_SCENARIO_LIST_LIMIT",
                "scenario list limit must be between 1 and 101",
            ));
        }
        let mut statement = self.connection.prepare(
            "SELECT s.scenario_id, s.project_id, s.display_name, s.current_revision,
             s.timetable_id, r.timetable_revision, r.source_project_revision, s.created_at
             FROM scenarios s LEFT JOIN scenario_revisions r
              ON r.scenario_id = s.scenario_id AND r.scenario_revision = s.current_revision
             WHERE s.project_id = ?1 ORDER BY s.created_at DESC, s.scenario_id ASC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = statement.query_map(params![project_id, limit, offset], |row| {
            Ok(ScenarioSummary {
                scenario_id: row.get(0)?,
                project_id: row.get(1)?,
                display_name: row.get(2)?,
                scenario_revision: row.get(3)?,
                timetable_id: row.get(4)?,
                timetable_revision: row.get(5)?,
                source_project_revision: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Creates an adopted scenario and its initial timetable from a validated run.
    ///
    /// Application validation must finish before this call. The write transaction compares
    /// the current source revision and the actual source and origin artifact payload hashes.
    ///
    /// # Errors
    /// Returns a stable conflict or validation code, or a database error; no rows survive failure.
    pub fn adopt_scenario(&mut self, document: &ScenarioDocument) -> Result<(), PersistenceError> {
        self.create_scenario(document, None)
    }

    /// Copies a validated scenario into independent scenario and timetable identities.
    ///
    /// # Errors
    /// Returns a conflict if either parent revision or payload changed, the current source changed,
    /// or the origin artifact changed. All three new rows roll back together on any error.
    pub fn copy_scenario(
        &mut self,
        source: &CopyScenarioSource,
        document: &ScenarioDocument,
    ) -> Result<(), PersistenceError> {
        sqlite_integer(
            source.expected_scenario_revision,
            "expected_scenario_revision",
        )?;
        sqlite_integer(
            source.expected_timetable_revision,
            "expected_timetable_revision",
        )?;
        self.create_scenario(document, Some(source))
    }

    /// Reads one consistent scenario, its complete timetable, and checked source/origin links.
    ///
    /// This does not require the imported project to remain at its historical source revision.
    /// The application must independently revalidate the returned business payloads.
    ///
    /// # Errors
    /// Returns a missing scenario, invalid relationship, size, JSON, or hash error, or database error.
    pub fn load_scenario(&self, scenario_id: &str) -> Result<StoredScenario, PersistenceError> {
        let tx = self.connection.unchecked_transaction()?;
        let stored = read_scenario(&tx, scenario_id)?;
        check_references(&tx, &stored.document, false)?;
        tx.commit()?;
        Ok(stored)
    }

    fn create_scenario(
        &mut self,
        document: &ScenarioDocument,
        source: Option<&CopyScenarioSource>,
    ) -> Result<(), PersistenceError> {
        document.validate()?;
        if document.scenario_revision != 0 || document.timetable_revision != 0 {
            return Err(invalid(
                "PERSISTENCE_INITIAL_REVISION_NOT_ZERO",
                "new scenario and timetable revisions must both be zero",
            ));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_references(&tx, document, true)?;
        if let Some(source) = source {
            check_parent(&tx, source, document)?;
        }
        insert_scenario(&tx, document)?;
        tx.commit()?;
        Ok(())
    }
}

fn invalid(code: &'static str, detail: &str) -> PersistenceError {
    PersistenceError::InvalidDocument {
        code,
        detail: detail.to_owned(),
    }
}

fn check_length(length: u64) -> Result<(), PersistenceError> {
    if length > MAXIMUM_SCENARIO_PAYLOAD_BYTES as u64 {
        return Err(invalid(
            "PERSISTENCE_SCENARIO_RESOURCE_LIMIT",
            "scenario-related JSON payload exceeds 64 MiB",
        ));
    }
    Ok(())
}

fn validate_json(payload: &[u8]) -> Result<(), PersistenceError> {
    check_length(payload.len() as u64)?;
    serde_json::from_slice::<serde_json::Value>(payload).map_err(|_| {
        invalid(
            "PERSISTENCE_INVALID_JSON_PAYLOAD",
            "scenario-related payload must contain valid JSON",
        )
    })?;
    Ok(())
}

fn checked_hash(
    payload: &[u8],
    recorded: &[u8],
    expected: &[u8; 32],
    code: &'static str,
) -> Result<(), PersistenceError> {
    if recorded != expected || blake3::hash(payload).as_bytes() != expected {
        return Err(invalid(
            code,
            "payload does not match its recorded and expected BLAKE3 digest",
        ));
    }
    validate_json(payload)
}

fn check_references(
    tx: &Transaction<'_>,
    document: &ScenarioDocument,
    current: bool,
) -> Result<(), PersistenceError> {
    let actual: u64 = tx
        .query_row(
            "SELECT current_revision FROM projects WHERE project_id = ?1",
            [&document.project_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| PersistenceError::ProjectNotFound {
            project_id: document.project_id.clone(),
        })?;
    if current && actual != document.source_project_revision {
        return Err(PersistenceError::RevisionConflict {
            project_id: document.project_id.clone(),
            expected_revision: document.source_project_revision,
            actual_revision: actual,
        });
    }
    let length: u64 = tx
        .query_row(
            "SELECT length(payload) FROM project_revisions WHERE project_id = ?1 AND revision = ?2",
            params![document.project_id, document.source_project_revision],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(relation_error)?;
    check_length(length)?;
    let (payload, hash): (Vec<u8>, Vec<u8>) = tx.query_row(
        "SELECT payload, payload_hash FROM project_revisions WHERE project_id = ?1 AND revision = ?2",
        params![document.project_id, document.source_project_revision],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    checked_hash(
        &payload,
        &hash,
        &document.source_payload_hash,
        "PERSISTENCE_SCENARIO_SOURCE_HASH_MISMATCH",
    )?;
    check_origin(tx, document)
}

fn relation_error() -> PersistenceError {
    invalid(
        "PERSISTENCE_SCENARIO_RELATION_MISMATCH",
        "scenario references a missing or inconsistent persisted record",
    )
}

fn check_origin(tx: &Transaction<'_>, document: &ScenarioDocument) -> Result<(), PersistenceError> {
    let (project_id, revision, source_hash, length): (String, u64, Vec<u8>, u64) = tx
        .query_row(
            "SELECT project_id, project_revision, source_payload_hash, length(payload)
             FROM solve_artifacts WHERE run_id = ?1",
            [&document.origin_run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or_else(relation_error)?;
    if project_id != document.project_id
        || revision != document.source_project_revision
        || source_hash != document.source_payload_hash
    {
        return Err(invalid(
            "PERSISTENCE_SCENARIO_ORIGIN_MISMATCH",
            "origin run does not belong to the exact scenario source",
        ));
    }
    check_length(length)?;
    let (payload, hash): (Vec<u8>, Vec<u8>) = tx.query_row(
        "SELECT payload, payload_hash FROM solve_artifacts WHERE run_id = ?1",
        [&document.origin_run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    checked_hash(
        &payload,
        &hash,
        &document.origin_artifact_hash,
        "PERSISTENCE_SCENARIO_ORIGIN_MISMATCH",
    )
}

fn check_parent(
    tx: &Transaction<'_>,
    source: &CopyScenarioSource,
    target: &ScenarioDocument,
) -> Result<(), PersistenceError> {
    let parent = read_scenario(tx, &source.scenario_id)?;
    let document = &parent.document;
    if document.scenario_revision != source.expected_scenario_revision {
        return Err(invalid(
            "PERSISTENCE_SCENARIO_REVISION_CONFLICT",
            "parent scenario revision changed",
        ));
    }
    if parent.scenario_payload_hash != source.expected_scenario_payload_hash {
        return Err(invalid(
            "PERSISTENCE_SCENARIO_HASH_MISMATCH",
            "parent scenario payload changed",
        ));
    }
    if document.timetable_id != source.expected_timetable_id
        || document.timetable_revision != source.expected_timetable_revision
    {
        return Err(invalid(
            "PERSISTENCE_TIMETABLE_REVISION_CONFLICT",
            "parent timetable identity or revision changed",
        ));
    }
    if parent.timetable_payload_hash != source.expected_timetable_payload_hash {
        return Err(invalid(
            "PERSISTENCE_TIMETABLE_HASH_MISMATCH",
            "parent timetable payload changed",
        ));
    }
    if document.scenario_id == target.scenario_id
        || document.timetable_id == target.timetable_id
        || document.project_id != target.project_id
        || document.source_project_revision != target.source_project_revision
        || document.source_payload_hash != target.source_payload_hash
        || document.origin_run_id != target.origin_run_id
        || document.origin_artifact_hash != target.origin_artifact_hash
    {
        return Err(invalid(
            "PERSISTENCE_INVALID_SCENARIO",
            "copy requires independent identities and identical source and origin provenance",
        ));
    }
    Ok(())
}

fn insert_scenario(
    tx: &Transaction<'_>,
    document: &ScenarioDocument,
) -> Result<(), PersistenceError> {
    for (sql, id, code) in [
        (
            "SELECT 1 FROM scenarios WHERE scenario_id = ?1",
            &document.scenario_id,
            "PERSISTENCE_SCENARIO_ALREADY_EXISTS",
        ),
        (
            "SELECT 1 FROM scenarios WHERE timetable_id = ?1",
            &document.timetable_id,
            "PERSISTENCE_TIMETABLE_ALREADY_EXISTS",
        ),
    ] {
        if tx.query_row(sql, [id], |_| Ok(())).optional()?.is_some() {
            return Err(invalid(
                code,
                "new scenario or timetable identity already exists",
            ));
        }
    }
    let now = timestamp(document.created_at);
    tx.execute(
        "INSERT INTO scenarios(scenario_id, timetable_id, project_id, display_name, current_revision,
         created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 0, ?5, ?5)",
        params![document.scenario_id, document.timetable_id, document.project_id, document.display_name, now],
    )?;
    tx.execute(
        "INSERT INTO scenario_revisions(scenario_id, scenario_revision, timetable_id, timetable_revision,
         project_id, source_project_revision, source_payload_hash, origin_run_id, origin_artifact_hash,
         scenario_schema_version, payload, payload_hash, created_at)
         VALUES (?1, 0, ?2, 0, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![document.scenario_id, document.timetable_id, document.project_id,
            document.source_project_revision, document.source_payload_hash.as_slice(), document.origin_run_id,
            document.origin_artifact_hash.as_slice(), document.scenario_schema_version, document.scenario_payload,
            blake3::hash(&document.scenario_payload).as_bytes(), now],
    )?;
    tx.execute(
        "INSERT INTO timetable_revisions(timetable_id, timetable_revision, scenario_id, scenario_revision,
         timetable_schema_version, payload, payload_hash, created_at)
         VALUES (?1, 0, ?2, 0, ?3, ?4, ?5, ?6)",
        params![document.timetable_id, document.scenario_id, document.timetable_schema_version,
            document.timetable_payload, blake3::hash(&document.timetable_payload).as_bytes(), now],
    )?;
    Ok(())
}

fn read_scenario(
    tx: &Transaction<'_>,
    scenario_id: &str,
) -> Result<StoredScenario, PersistenceError> {
    let revision: u64 = tx
        .query_row(
            "SELECT current_revision FROM scenarios WHERE scenario_id = ?1",
            [scenario_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| invalid("PERSISTENCE_SCENARIO_NOT_FOUND", "scenario was not found"))?;
    let lengths: (u64, u64) = tx.query_row(
        "SELECT length(r.payload), length(t.payload) FROM scenario_revisions r
         JOIN timetable_revisions t ON t.scenario_id = r.scenario_id AND t.scenario_revision = r.scenario_revision
          AND t.timetable_id = r.timetable_id AND t.timetable_revision = r.timetable_revision
         WHERE r.scenario_id = ?1 AND r.scenario_revision = ?2",
        params![scenario_id, revision], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?.ok_or_else(relation_error)?;
    check_length(lengths.0)?;
    check_length(lengths.1)?;
    let (document, scenario_hash, timetable_hash) = read_payloads(tx, scenario_id, revision)?;
    let scenario_payload_hash = *blake3::hash(&document.scenario_payload).as_bytes();
    let timetable_payload_hash = *blake3::hash(&document.timetable_payload).as_bytes();
    checked_hash(
        &document.scenario_payload,
        &scenario_hash,
        &scenario_payload_hash,
        "PERSISTENCE_SCENARIO_HASH_MISMATCH",
    )?;
    checked_hash(
        &document.timetable_payload,
        &timetable_hash,
        &timetable_payload_hash,
        "PERSISTENCE_TIMETABLE_HASH_MISMATCH",
    )?;
    document.validate()?;
    Ok(StoredScenario {
        document,
        scenario_payload_hash,
        timetable_payload_hash,
    })
}

fn read_payloads(
    tx: &Transaction<'_>,
    scenario_id: &str,
    revision: u64,
) -> Result<(ScenarioDocument, Vec<u8>, Vec<u8>), PersistenceError> {
    tx.query_row(
        "SELECT s.timetable_id, s.project_id, s.display_name, r.timetable_revision,
         r.source_project_revision, r.source_payload_hash, r.origin_run_id, r.origin_artifact_hash,
         r.scenario_schema_version, r.payload, t.timetable_schema_version, t.payload,
         r.created_at, r.payload_hash, t.payload_hash
         FROM scenarios s JOIN scenario_revisions r ON s.scenario_id = r.scenario_id
          AND s.project_id = r.project_id AND s.timetable_id = r.timetable_id
         JOIN timetable_revisions t ON t.scenario_id = r.scenario_id AND t.scenario_revision = r.scenario_revision
          AND t.timetable_id = r.timetable_id AND t.timetable_revision = r.timetable_revision
          AND t.created_at = r.created_at
         WHERE s.scenario_id = ?1 AND r.scenario_revision = ?2",
        params![scenario_id, revision],
        |row| Ok((ScenarioDocument {
            scenario_id: scenario_id.to_owned(), timetable_id: row.get(0)?, project_id: row.get(1)?,
            display_name: row.get(2)?, scenario_revision: revision, timetable_revision: row.get(3)?,
            source_project_revision: row.get(4)?, source_payload_hash: row.get(5)?, origin_run_id: row.get(6)?,
            origin_artifact_hash: row.get(7)?, scenario_schema_version: row.get(8)?, scenario_payload: row.get(9)?,
            timetable_schema_version: row.get(10)?, timetable_payload: row.get(11)?, created_at: row.get(12)?,
        }, row.get(13)?, row.get(14)?)),
    ).optional()?.ok_or_else(relation_error)
}
