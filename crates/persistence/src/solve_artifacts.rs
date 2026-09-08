//! Immutable completed runs; semantic validation belongs to application.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::{
    PersistenceError, ProjectDocument, SolverRunRecord, SqliteStore, insert_solver_run,
    sqlite_integer, timestamp,
};

pub const MAXIMUM_SOLVE_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SolveArtifactDocument {
    pub run_id: String,
    pub project_id: String,
    pub project_revision: u64,
    pub source_payload_hash: [u8; 32],
    pub artifact_schema_version: u32,
    pub status_code: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSolveArtifact {
    pub document: SolveArtifactDocument,
    pub attempts: Vec<SolverRunRecord>,
}

/// Metadata is not a claim that a run has passed application revalidation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SolveArtifactSummary {
    pub run_id: String,
    pub project_id: String,
    pub project_revision: u64,
    pub artifact_schema_version: u32,
    pub status_code: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

fn invalid(code: &'static str, detail: &str) -> PersistenceError {
    PersistenceError::InvalidDocument {
        code,
        detail: detail.to_owned(),
    }
}

impl SolveArtifactDocument {
    pub fn payload_hash(&self) -> blake3::Hash {
        blake3::hash(&self.payload)
    }

    fn validate(&self) -> Result<(), PersistenceError> {
        if self.run_id.trim().is_empty()
            || self.project_id.trim().is_empty()
            || self.status_code.trim().is_empty()
            || self.artifact_schema_version == 0
            || self.finished_at < self.started_at
        {
            return Err(invalid(
                "PERSISTENCE_INVALID_SOLVE_ARTIFACT",
                "invalid identity, schema, status or timestamps",
            ));
        }
        if self.payload.len() > MAXIMUM_SOLVE_ARTIFACT_BYTES {
            return Err(invalid(
                "PERSISTENCE_SOLVE_ARTIFACT_RESOURCE_LIMIT",
                "artifact exceeds 64 MiB",
            ));
        }
        serde_json::from_slice::<serde_json::Value>(&self.payload).map_err(|_| {
            invalid(
                "PERSISTENCE_INVALID_SOLVE_ARTIFACT_JSON",
                "artifact is not valid JSON",
            )
        })?;
        sqlite_integer(self.project_revision, "project_revision")?;
        Ok(())
    }
}

impl SqliteStore {
    /// Reads an exact historical payload and verifies its hash. The label is the current name.
    ///
    /// # Errors
    /// Rejects a missing revision, corrupt JSON/hash, or an out-of-range revision.
    pub fn load_project_revision(
        &self,
        project_id: &str,
        revision: u64,
    ) -> Result<ProjectDocument, PersistenceError> {
        let revision_value = sqlite_integer(revision, "revision")?;
        let (document, hash) = self
            .connection
            .query_row(
                "SELECT p.display_name, r.document_schema_version, r.payload, r.payload_hash
             FROM project_revisions r JOIN projects p ON p.project_id = r.project_id
             WHERE r.project_id = ?1 AND r.revision = ?2",
                params![project_id, revision_value],
                |row| {
                    Ok((
                        ProjectDocument {
                            project_id: project_id.to_owned(),
                            display_name: row.get(0)?,
                            revision,
                            document_schema_version: row.get(1)?,
                            payload: row.get(2)?,
                        },
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                invalid(
                    "PERSISTENCE_PROJECT_REVISION_NOT_FOUND",
                    "historical project revision was not found",
                )
            })?;
        if hash.as_slice() != document.payload_hash().as_bytes() {
            return Err(invalid(
                "PERSISTENCE_PAYLOAD_HASH_MISMATCH",
                "historical payload hash mismatch",
            ));
        }
        document.validate()?;
        Ok(document)
    }

    /// Saves a completed artifact and all its attempt provenance in one transaction.
    ///
    /// # Errors
    /// Duplicate IDs, source mismatch, invalid provenance, or any SQL failure roll everything back.
    pub fn record_solve_artifact(
        &mut self,
        document: &SolveArtifactDocument,
        attempts: &[SolverRunRecord],
    ) -> Result<(), PersistenceError> {
        document.validate()?;
        validate_attempts(document, attempts)?;
        let revision = sqlite_integer(document.project_revision, "project_revision")?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source: Option<(Vec<u8>, Vec<u8>)> = tx.query_row(
            "SELECT payload, payload_hash FROM project_revisions WHERE project_id = ?1 AND revision = ?2",
            params![document.project_id, revision], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let (source, hash) = source.ok_or_else(|| {
            invalid(
                "PERSISTENCE_PROJECT_REVISION_NOT_FOUND",
                "run source revision was not found",
            )
        })?;
        if hash != document.source_payload_hash
            || blake3::hash(&source).as_bytes() != &document.source_payload_hash
        {
            return Err(invalid(
                "PERSISTENCE_SOLVE_SOURCE_HASH_MISMATCH",
                "run source does not match the historical payload",
            ));
        }
        let inserted = tx.execute(
            "INSERT INTO solve_artifacts(run_id, project_id, project_revision, source_payload_hash,
             artifact_schema_version, status_code, started_at, finished_at, payload, payload_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) ON CONFLICT(run_id) DO NOTHING",
            params![
                document.run_id,
                document.project_id,
                revision,
                document.source_payload_hash.as_slice(),
                document.artifact_schema_version,
                document.status_code,
                timestamp(document.started_at),
                timestamp(document.finished_at),
                document.payload,
                document.payload_hash().as_bytes()
            ],
        )?;
        if inserted == 0 {
            return Err(invalid(
                "PERSISTENCE_SOLVE_RUN_ALREADY_EXISTS",
                "completed run ID is immutable",
            ));
        }
        for (ordinal, record) in attempts.iter().enumerate() {
            insert_solver_run(&tx, record)?;
            tx.execute("INSERT INTO solve_artifact_attempts(artifact_run_id, ordinal, solver_run_id) VALUES (?1, ?2, ?3)",
                params![document.run_id, ordinal, record.run_id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Loads a bounded completed artifact plus its ordered attempt records.
    ///
    /// # Errors
    /// Rejects missing/corrupt/oversized artifacts or inconsistent provenance. This does not run
    /// semantic compilation or the timetable validator; application must do both before use.
    pub fn load_solve_artifact(
        &self,
        run_id: &str,
    ) -> Result<StoredSolveArtifact, PersistenceError> {
        // A read transaction keeps metadata, payload and attempts on one SQLite snapshot.
        let tx = self.connection.unchecked_transaction()?;
        let size: Option<u64> = tx
            .query_row(
                "SELECT length(payload) FROM solve_artifacts WHERE run_id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        let size = size.ok_or_else(|| {
            invalid(
                "PERSISTENCE_SOLVE_RUN_NOT_FOUND",
                "completed run was not found",
            )
        })?;
        if size > MAXIMUM_SOLVE_ARTIFACT_BYTES as u64 {
            return Err(invalid(
                "PERSISTENCE_SOLVE_ARTIFACT_RESOURCE_LIMIT",
                "artifact exceeds 64 MiB",
            ));
        }
        let (document, hash) = tx.query_row(
            "SELECT run_id, project_id, project_revision, source_payload_hash, artifact_schema_version,
             status_code, started_at, finished_at, payload, payload_hash FROM solve_artifacts WHERE run_id = ?1", [run_id],
            |row| Ok((SolveArtifactDocument {
                run_id: row.get(0)?, project_id: row.get(1)?, project_revision: row.get(2)?,
                source_payload_hash: row.get(3)?, artifact_schema_version: row.get(4)?, status_code: row.get(5)?,
                started_at: row.get(6)?, finished_at: row.get(7)?, payload: row.get(8)?,
            }, row.get::<_, Vec<u8>>(9)?)),
        )?;
        if hash.as_slice() != document.payload_hash().as_bytes() {
            return Err(invalid(
                "PERSISTENCE_SOLVE_ARTIFACT_HASH_MISMATCH",
                "artifact payload hash mismatch",
            ));
        }
        document.validate()?;
        let attempts = {
            let mut statement = tx.prepare(
                "SELECT a.ordinal, s.run_id, s.project_id, s.project_revision, s.scenario_id, s.request_id,
                 s.input_snapshot_hash, s.solver_engine_version, s.protocol_version, s.seed,
                 s.parameters_json, s.worker_count, s.time_limit_ms, s.status_code, s.objective_json,
                 s.validation_code, s.output_hash, s.started_at, s.finished_at
                 FROM solve_artifact_attempts a JOIN solver_runs s ON s.run_id = a.solver_run_id
                 WHERE a.artifact_run_id = ?1 ORDER BY a.ordinal LIMIT 17")?;
            let rows = statement.query_map([run_id], |row| {
                Ok((
                    row.get::<_, usize>(0)?,
                    SolverRunRecord {
                        run_id: row.get(1)?,
                        project_id: row.get(2)?,
                        project_revision: row.get(3)?,
                        scenario_id: row.get(4)?,
                        request_id: row.get(5)?,
                        input_snapshot_hash: row.get(6)?,
                        solver_engine_version: row.get(7)?,
                        protocol_version: row.get(8)?,
                        seed: row.get(9)?,
                        parameters_json: row.get(10)?,
                        worker_count: row.get(11)?,
                        time_limit_ms: row.get(12)?,
                        status_code: row.get(13)?,
                        objective_json: row.get(14)?,
                        validation_code: row.get(15)?,
                        output_hash: row.get(16)?,
                        started_at: row.get(17)?,
                        finished_at: row.get(18)?,
                    },
                ))
            })?;
            let mut attempts = Vec::new();
            for row in rows {
                let (ordinal, record) = row?;
                if ordinal != attempts.len() {
                    return Err(invalid(
                        "PERSISTENCE_SOLVE_ATTEMPT_MISMATCH",
                        "attempt ordinal is not contiguous",
                    ));
                }
                attempts.push(record);
            }
            attempts
        };
        validate_attempts(&document, &attempts)?;
        tx.commit()?;
        Ok(StoredSolveArtifact { document, attempts })
    }

    /// Reads a deterministic metadata page, without loading artifact payloads.
    ///
    /// # Errors
    /// Rejects a limit outside 1..=101 or a database error.
    pub fn list_solve_artifacts(
        &self,
        project_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SolveArtifactSummary>, PersistenceError> {
        if !(1..=101).contains(&limit) {
            return Err(invalid(
                "PERSISTENCE_INVALID_SOLVE_LIST_LIMIT",
                "list limit must be between 1 and 101",
            ));
        }
        let mut statement = self.connection.prepare(
            "SELECT run_id, project_id, project_revision, artifact_schema_version, status_code, started_at, finished_at
             FROM solve_artifacts WHERE project_id = ?1 ORDER BY started_at DESC, run_id LIMIT ?2 OFFSET ?3")?;
        let rows = statement.query_map(params![project_id, limit, offset], |row| {
            Ok(SolveArtifactSummary {
                run_id: row.get(0)?,
                project_id: row.get(1)?,
                project_revision: row.get(2)?,
                artifact_schema_version: row.get(3)?,
                status_code: row.get(4)?,
                started_at: row.get(5)?,
                finished_at: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

fn validate_attempts(
    document: &SolveArtifactDocument,
    attempts: &[SolverRunRecord],
) -> Result<(), PersistenceError> {
    if attempts.len() > 16 {
        return Err(invalid(
            "PERSISTENCE_SOLVE_ARTIFACT_RESOURCE_LIMIT",
            "a run cannot exceed 16 attempts",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut requests = BTreeSet::new();
    for record in attempts {
        record.validate()?;
        sqlite_integer(record.seed, "seed")?;
        sqlite_integer(record.time_limit_ms, "time_limit_ms")?;
        if record.project_id != document.project_id
            || record.project_revision != document.project_revision
            || record
                .finished_at
                .is_none_or(|end| end < record.started_at || end > document.finished_at)
            || record.started_at < document.started_at
            || !ids.insert(&record.run_id)
            || !requests.insert(&record.request_id)
        {
            return Err(invalid(
                "PERSISTENCE_SOLVE_ATTEMPT_MISMATCH",
                "attempt identity or timestamps disagree with root run",
            ));
        }
    }
    Ok(())
}
