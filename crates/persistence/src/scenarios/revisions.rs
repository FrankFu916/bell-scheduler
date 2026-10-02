use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::{
    ScenarioDocument, StoredScenario, check_references, invalid, read_scenario,
    read_scenario_revision,
};
use crate::{PersistenceError, SqliteStore, sqlite_integer, timestamp};

/// Exact current identities and digests frozen by the application's edit preparation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioRevisionExpectation {
    pub scenario_id: String,
    pub expected_scenario_revision: u64,
    pub expected_scenario_payload_hash: [u8; 32],
    pub expected_timetable_id: String,
    pub expected_timetable_revision: u64,
    pub expected_timetable_payload_hash: [u8; 32],
}

impl SqliteStore {
    /// Reads the exact historical scenario and linked timetable revisions consistently.
    ///
    /// Source and origin payloads are checked, but the source need not remain current. This
    /// does not load the current head's payload or recursively traverse revision history.
    /// Application code must independently revalidate the returned business documents.
    ///
    /// # Errors
    /// Returns a stable missing revision, relationship, integer, size, JSON or hash error.
    pub fn load_scenario_revision(
        &self,
        scenario_id: &str,
        scenario_revision: u64,
        timetable_revision: u64,
    ) -> Result<StoredScenario, PersistenceError> {
        sqlite_integer(scenario_revision, "scenario_revision")?;
        sqlite_integer(timetable_revision, "timetable_revision")?;
        let tx = self.connection.unchecked_transaction()?;
        let actual_timetable_revision: u64 = tx
            .query_row(
                "SELECT timetable_revision FROM scenario_revisions
                 WHERE scenario_id = ?1 AND scenario_revision = ?2",
                params![scenario_id, scenario_revision],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                invalid(
                    "PERSISTENCE_SCENARIO_REVISION_NOT_FOUND",
                    "the requested scenario revision was not found",
                )
            })?;
        if actual_timetable_revision != timetable_revision {
            return Err(invalid(
                "PERSISTENCE_TIMETABLE_REVISION_CONFLICT",
                "the exact scenario revision links to a different timetable revision",
            ));
        }
        let stored = read_scenario_revision(&tx, scenario_id, scenario_revision)?;
        check_references(&tx, &stored.document, false)?;
        tx.commit()?;
        Ok(stored)
    }

    /// Appends one complete scenario/timetable pair and advances the visible head atomically.
    ///
    /// The application must finish its edit, independent Hard validation and scoring before
    /// calling this method. Both revisions advance exactly once; existing payloads are untouched.
    /// The current source, origin, prior identities, hashes and immutable metadata are checked
    /// inside `BEGIN IMMEDIATE`. No rows or head change survive any insertion or CAS failure.
    ///
    /// # Errors
    /// Returns stable revision/hash/source/metadata conflicts, invalid timestamps, integer
    /// overflow or database errors. Revision time may equal, but cannot precede, its predecessor.
    pub fn append_scenario_revision(
        &mut self,
        expected: &ScenarioRevisionExpectation,
        document: &ScenarioDocument,
        revision_created_at: DateTime<Utc>,
    ) -> Result<(), PersistenceError> {
        validate_append(expected, document, revision_created_at)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_scenario(&tx, &expected.scenario_id)?;
        check_expected(expected, &current)?;
        check_immutable(&current.document, document)?;
        if revision_created_at < current.revision_created_at {
            return Err(invalid(
                "PERSISTENCE_SCENARIO_REVISION_TIMESTAMP_INVALID",
                "revision timestamp cannot precede its predecessor",
            ));
        }
        check_references(&tx, document, true)?;
        insert_revision_pair(&tx, document, revision_created_at)?;
        let changed = tx.execute(
            "UPDATE scenarios SET current_revision = ?1, updated_at = ?2
             WHERE scenario_id = ?3 AND current_revision = ?4
              AND timetable_id = ?5 AND project_id = ?6",
            params![
                document.scenario_revision,
                timestamp(revision_created_at),
                document.scenario_id,
                expected.expected_scenario_revision,
                document.timetable_id,
                document.project_id,
            ],
        )?;
        if changed != 1 {
            return Err(invalid(
                "PERSISTENCE_SCENARIO_REVISION_CONFLICT",
                "scenario head changed before the revision could be published",
            ));
        }
        tx.commit()?;
        Ok(())
    }
}

fn next_revision(value: u64, field: &'static str) -> Result<u64, PersistenceError> {
    sqlite_integer(value, field)?;
    let next = value
        .checked_add(1)
        .ok_or(PersistenceError::IntegerOutOfRange { field, value })?;
    sqlite_integer(next, field)?;
    Ok(next)
}

fn validate_append(
    expected: &ScenarioRevisionExpectation,
    document: &ScenarioDocument,
    revision_created_at: DateTime<Utc>,
) -> Result<(), PersistenceError> {
    document.validate()?;
    let scenario_revision =
        next_revision(expected.expected_scenario_revision, "scenario_revision")?;
    let timetable_revision =
        next_revision(expected.expected_timetable_revision, "timetable_revision")?;
    if document.scenario_revision != scenario_revision
        || document.timetable_revision != timetable_revision
    {
        return Err(invalid(
            "PERSISTENCE_SCENARIO_REVISION_NOT_NEXT",
            "scenario and timetable revisions must each advance exactly once",
        ));
    }
    if expected.scenario_id != document.scenario_id
        || expected.expected_timetable_id != document.timetable_id
    {
        return Err(invalid(
            "PERSISTENCE_INVALID_SCENARIO",
            "an append must preserve scenario and timetable identities",
        ));
    }
    if revision_created_at.timestamp_subsec_nanos() % 1_000_000 != 0 {
        return Err(invalid(
            "PERSISTENCE_SCENARIO_REVISION_TIMESTAMP_INVALID",
            "revision timestamp must have millisecond precision",
        ));
    }
    Ok(())
}

fn check_expected(
    expected: &ScenarioRevisionExpectation,
    current: &StoredScenario,
) -> Result<(), PersistenceError> {
    let document = &current.document;
    for (matches, code) in [
        (
            document.scenario_revision == expected.expected_scenario_revision,
            "PERSISTENCE_SCENARIO_REVISION_CONFLICT",
        ),
        (
            current.scenario_payload_hash == expected.expected_scenario_payload_hash,
            "PERSISTENCE_SCENARIO_HASH_MISMATCH",
        ),
        (
            document.timetable_id == expected.expected_timetable_id
                && document.timetable_revision == expected.expected_timetable_revision,
            "PERSISTENCE_TIMETABLE_REVISION_CONFLICT",
        ),
        (
            current.timetable_payload_hash == expected.expected_timetable_payload_hash,
            "PERSISTENCE_TIMETABLE_HASH_MISMATCH",
        ),
    ] {
        if !matches {
            return Err(invalid(
                code,
                "the prepared scenario revision no longer matches the current head",
            ));
        }
    }
    Ok(())
}

fn check_immutable(
    current: &ScenarioDocument,
    next: &ScenarioDocument,
) -> Result<(), PersistenceError> {
    if current.scenario_id != next.scenario_id
        || current.timetable_id != next.timetable_id
        || current.project_id != next.project_id
        || current.display_name != next.display_name
        || current.created_at != next.created_at
        || current.source_project_revision != next.source_project_revision
        || current.source_payload_hash != next.source_payload_hash
        || current.origin_run_id != next.origin_run_id
        || current.origin_artifact_hash != next.origin_artifact_hash
    {
        return Err(invalid(
            "PERSISTENCE_INVALID_SCENARIO",
            "an append must preserve original identities, metadata, source and origin provenance",
        ));
    }
    Ok(())
}

fn insert_revision_pair(
    tx: &Transaction<'_>,
    document: &ScenarioDocument,
    revision_created_at: DateTime<Utc>,
) -> Result<(), PersistenceError> {
    let now = timestamp(revision_created_at);
    tx.execute(
        "INSERT INTO scenario_revisions(scenario_id, scenario_revision, timetable_id, timetable_revision,
         project_id, source_project_revision, source_payload_hash, origin_run_id, origin_artifact_hash,
         scenario_schema_version, payload, payload_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![document.scenario_id, document.scenario_revision, document.timetable_id,
            document.timetable_revision, document.project_id, document.source_project_revision,
            document.source_payload_hash.as_slice(), document.origin_run_id,
            document.origin_artifact_hash.as_slice(), document.scenario_schema_version,
            document.scenario_payload, blake3::hash(&document.scenario_payload).as_bytes(), now],
    )?;
    tx.execute(
        "INSERT INTO timetable_revisions(timetable_id, timetable_revision, scenario_id, scenario_revision,
         timetable_schema_version, payload, payload_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![document.timetable_id, document.timetable_revision, document.scenario_id,
            document.scenario_revision, document.timetable_schema_version, document.timetable_payload,
            blake3::hash(&document.timetable_payload).as_bytes(), now],
    )?;
    Ok(())
}
