#![forbid(unsafe_code)]

//! `SQLite` persistence primitives for revisioned project documents and solver provenance.
//!
//! The persistence representation is deliberately separate from the domain model. The
//! application layer is responsible for converting a validated domain aggregate into a
//! [`ProjectDocument`] and back. Every write is transactional and uses optimistic revision
//! checks; a failed write never advances the visible project revision.

use std::path::Path;

use blake3::Hash;
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod scenarios;
mod solve_artifacts;
pub use scenarios::{
    CopyScenarioSource, MAXIMUM_SCENARIO_PAYLOAD_BYTES, ScenarioDocument, ScenarioSummary,
    StoredScenario,
};
pub use solve_artifacts::{
    MAXIMUM_SOLVE_ARTIFACT_BYTES, SolveArtifactDocument, SolveArtifactSummary, StoredSolveArtifact,
};

const MIGRATIONS: &[(u32, &str)] = &[
    (1, include_str!("../migrations/0001_project_revisions.sql")),
    (
        2,
        include_str!("../migrations/0002_solver_run_provenance.sql"),
    ),
    (3, include_str!("../migrations/0003_solve_artifacts.sql")),
    (4, include_str!("../migrations/0004_scenarios.sql")),
];

pub const DATABASE_SCHEMA_VERSION: u32 = 4;

/// Project-table metadata only; this does not attest to document payload integrity or validity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSummary {
    pub project_id: String,
    pub display_name: String,
    pub current_revision: u64,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDocument {
    pub project_id: String,
    pub display_name: String,
    pub revision: u64,
    pub document_schema_version: u32,
    pub payload: Vec<u8>,
}

impl ProjectDocument {
    pub fn payload_hash(&self) -> Hash {
        blake3::hash(&self.payload)
    }

    fn validate(&self) -> Result<(), PersistenceError> {
        if self.project_id.trim().is_empty() {
            return Err(PersistenceError::InvalidDocument {
                code: "PERSISTENCE_EMPTY_PROJECT_ID",
                detail: "project_id cannot be blank".to_owned(),
            });
        }
        if self.display_name.trim().is_empty() {
            return Err(PersistenceError::InvalidDocument {
                code: "PERSISTENCE_EMPTY_PROJECT_NAME",
                detail: "display_name cannot be blank".to_owned(),
            });
        }
        if self.document_schema_version == 0 {
            return Err(PersistenceError::InvalidDocument {
                code: "PERSISTENCE_INVALID_DOCUMENT_VERSION",
                detail: "document_schema_version must be positive".to_owned(),
            });
        }
        serde_json::from_slice::<serde_json::Value>(&self.payload).map_err(|error| {
            PersistenceError::InvalidDocument {
                code: "PERSISTENCE_INVALID_JSON_PAYLOAD",
                detail: error.to_string(),
            }
        })?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SolverRunRecord {
    pub run_id: String,
    pub project_id: String,
    pub project_revision: u64,
    pub scenario_id: Option<String>,
    pub request_id: String,
    pub input_snapshot_hash: [u8; 32],
    pub solver_engine_version: String,
    pub protocol_version: u32,
    pub seed: u64,
    pub parameters_json: String,
    pub worker_count: u32,
    pub time_limit_ms: u64,
    pub status_code: String,
    pub objective_json: String,
    pub validation_code: String,
    pub output_hash: Option<[u8; 32]>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl SolverRunRecord {
    fn validate(&self) -> Result<(), PersistenceError> {
        for (code, value) in [
            ("PERSISTENCE_EMPTY_RUN_ID", self.run_id.as_str()),
            ("PERSISTENCE_EMPTY_REQUEST_ID", self.request_id.as_str()),
            (
                "PERSISTENCE_EMPTY_SOLVER_VERSION",
                self.solver_engine_version.as_str(),
            ),
            ("PERSISTENCE_EMPTY_STATUS", self.status_code.as_str()),
            (
                "PERSISTENCE_EMPTY_VALIDATION",
                self.validation_code.as_str(),
            ),
        ] {
            if value.trim().is_empty() {
                return Err(PersistenceError::InvalidDocument {
                    code,
                    detail: "required provenance field cannot be blank".to_owned(),
                });
            }
        }
        if self.protocol_version == 0 || self.worker_count == 0 || self.time_limit_ms == 0 {
            return Err(PersistenceError::InvalidDocument {
                code: "PERSISTENCE_INVALID_SOLVER_PARAMETERS",
                detail: "protocol version, worker count, and time limit must be positive"
                    .to_owned(),
            });
        }
        serde_json::from_str::<serde_json::Value>(&self.parameters_json).map_err(|error| {
            PersistenceError::InvalidDocument {
                code: "PERSISTENCE_INVALID_PARAMETERS_JSON",
                detail: error.to_string(),
            }
        })?;
        serde_json::from_str::<serde_json::Value>(&self.objective_json).map_err(|error| {
            PersistenceError::InvalidDocument {
                code: "PERSISTENCE_INVALID_OBJECTIVE_JSON",
                detail: error.to_string(),
            }
        })?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("invalid persisted document ({code}): {detail}")]
    InvalidDocument { code: &'static str, detail: String },
    #[error("project {project_id} was not found")]
    ProjectNotFound { project_id: String },
    #[error("project {project_id} already exists")]
    ProjectAlreadyExists { project_id: String },
    #[error(
        "project {project_id} revision conflict: expected {expected_revision}, actual {actual_revision}"
    )]
    RevisionConflict {
        project_id: String,
        expected_revision: u64,
        actual_revision: u64,
    },
    #[error("integer {field} value {value} cannot be represented by SQLite")]
    IntegerOutOfRange { field: &'static str, value: u64 },
    #[error("database schema version {found} is newer than supported version {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
}

impl PersistenceError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Database(_) => "PERSISTENCE_DATABASE_ERROR",
            Self::InvalidDocument { code, .. } => code,
            Self::ProjectNotFound { .. } => "PERSISTENCE_PROJECT_NOT_FOUND",
            Self::ProjectAlreadyExists { .. } => "PERSISTENCE_PROJECT_ALREADY_EXISTS",
            Self::RevisionConflict { .. } => "PERSISTENCE_REVISION_CONFLICT",
            Self::IntegerOutOfRange { .. } => "PERSISTENCE_INTEGER_OUT_OF_RANGE",
            Self::UnsupportedSchema { .. } => "PERSISTENCE_UNSUPPORTED_SCHEMA",
        }
    }
}

#[derive(Debug)]
pub struct SqliteStore {
    connection: Connection,
}

impl SqliteStore {
    /// Opens, configures, and migrates a file-backed database.
    ///
    /// # Errors
    ///
    /// Returns a database or migration error when the file cannot be opened or upgraded.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PersistenceError> {
        let connection = Connection::open(path)?;
        Self::from_connection(connection)
    }

    /// Creates and migrates an isolated in-memory database.
    ///
    /// # Errors
    ///
    /// Returns a database or migration error when `SQLite` cannot initialize the schema.
    pub fn in_memory() -> Result<Self, PersistenceError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(connection: Connection) -> Result<Self, PersistenceError> {
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    /// Reads the installed database schema version.
    ///
    /// # Errors
    ///
    /// Returns a database error if schema metadata cannot be read.
    pub fn schema_version(&self) -> Result<u32, PersistenceError> {
        Ok(self.connection.query_row(
            "SELECT version FROM schema_metadata WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?)
    }

    /// Reads a bounded metadata page without loading revision payloads or opening a write transaction.
    ///
    /// The maximum of 101 lets an application page of 100 determine whether another page exists
    /// with one query. Equal timestamps use the raw project ID as a deterministic tie-breaker.
    ///
    /// # Errors
    ///
    /// Returns `PERSISTENCE_INVALID_PROJECT_LIST_LIMIT` unless `limit` is between 1 and 101,
    /// inclusive, or a database error if metadata cannot be read.
    pub fn list_projects(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<ProjectSummary>, PersistenceError> {
        if !(1..=101).contains(&limit) {
            return Err(PersistenceError::InvalidDocument {
                code: "PERSISTENCE_INVALID_PROJECT_LIST_LIMIT",
                detail: "project list limit must be between 1 and 101".to_owned(),
            });
        }
        let mut statement = self.connection.prepare(
            "SELECT project_id, display_name, current_revision, updated_at
             FROM projects
             ORDER BY updated_at DESC, project_id ASC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement.query_map(params![i64::from(limit), i64::from(offset)], |row| {
            Ok(ProjectSummary {
                project_id: row.get(0)?,
                display_name: row.get(1)?,
                current_revision: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Atomically creates a project and its initial revision.
    ///
    /// # Errors
    ///
    /// Returns a validation error, duplicate-project error, or database error. The transaction
    /// is rolled back on every error.
    pub fn create_project(&mut self, document: &ProjectDocument) -> Result<(), PersistenceError> {
        document.validate()?;
        if document.revision != 0 {
            return Err(PersistenceError::InvalidDocument {
                code: "PERSISTENCE_INITIAL_REVISION_NOT_ZERO",
                detail: format!("initial revision was {}", document.revision),
            });
        }
        let now = timestamp(Utc::now());
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = tx.execute(
            "INSERT INTO projects(project_id, display_name, current_revision, created_at, updated_at)
             VALUES (?1, ?2, 0, ?3, ?3)
             ON CONFLICT(project_id) DO NOTHING",
            params![document.project_id, document.display_name, now],
        )?;
        if inserted == 0 {
            return Err(PersistenceError::ProjectAlreadyExists {
                project_id: document.project_id.clone(),
            });
        }
        insert_revision(&tx, document, &now)?;
        tx.commit()?;
        Ok(())
    }

    /// Loads the current revision of a project.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::ProjectNotFound`] or a database error.
    pub fn load_project(&self, project_id: &str) -> Result<ProjectDocument, PersistenceError> {
        let (document, stored_hash) = self
            .connection
            .query_row(
                "SELECT p.display_name, p.current_revision, r.document_schema_version, r.payload,
                        r.payload_hash
                 FROM projects p
                 JOIN project_revisions r
                   ON r.project_id = p.project_id AND r.revision = p.current_revision
                 WHERE p.project_id = ?1",
                [project_id],
                |row| {
                    Ok((
                        ProjectDocument {
                            project_id: project_id.to_owned(),
                            display_name: row.get(0)?,
                            revision: row.get(1)?,
                            document_schema_version: row.get(2)?,
                            payload: row.get(3)?,
                        },
                        row.get::<_, Vec<u8>>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| PersistenceError::ProjectNotFound {
                project_id: project_id.to_owned(),
            })?;
        if stored_hash.as_slice() != document.payload_hash().as_bytes() {
            return Err(PersistenceError::InvalidDocument {
                code: "PERSISTENCE_PAYLOAD_HASH_MISMATCH",
                detail: "stored payload does not match its recorded BLAKE3 digest".to_owned(),
            });
        }
        document.validate()?;
        Ok(document)
    }

    /// Atomically appends the next document revision using optimistic concurrency control.
    ///
    /// # Errors
    ///
    /// Returns a revision conflict, validation error, missing-project error, or database error.
    /// No partial revision is committed on failure.
    pub fn replace_project(
        &mut self,
        expected_revision: u64,
        document: &ProjectDocument,
    ) -> Result<(), PersistenceError> {
        document.validate()?;
        let next_revision =
            expected_revision
                .checked_add(1)
                .ok_or(PersistenceError::IntegerOutOfRange {
                    field: "revision",
                    value: expected_revision,
                })?;
        if document.revision != next_revision {
            return Err(PersistenceError::InvalidDocument {
                code: "PERSISTENCE_NON_SEQUENTIAL_REVISION",
                detail: format!(
                    "document revision {} must be expected revision {} plus one",
                    document.revision, expected_revision
                ),
            });
        }
        let expected = sqlite_integer(expected_revision, "expected_revision")?;
        let next = sqlite_integer(next_revision, "revision")?;
        let now = timestamp(Utc::now());
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actual: Option<u64> = tx
            .query_row(
                "SELECT current_revision FROM projects WHERE project_id = ?1",
                [&document.project_id],
                |row| row.get(0),
            )
            .optional()?;
        let actual = actual.ok_or_else(|| PersistenceError::ProjectNotFound {
            project_id: document.project_id.clone(),
        })?;
        if actual != expected_revision {
            return Err(PersistenceError::RevisionConflict {
                project_id: document.project_id.clone(),
                expected_revision,
                actual_revision: actual,
            });
        }
        insert_revision(&tx, document, &now)?;
        let changed = tx.execute(
            "UPDATE projects
             SET display_name = ?1, current_revision = ?2, updated_at = ?3
             WHERE project_id = ?4 AND current_revision = ?5",
            params![
                document.display_name,
                next,
                now,
                document.project_id,
                expected
            ],
        )?;
        if changed != 1 {
            return Err(PersistenceError::RevisionConflict {
                project_id: document.project_id.clone(),
                expected_revision,
                actual_revision: actual,
            });
        }
        tx.commit()?;
        Ok(())
    }

    /// Persists a complete, privacy-safe solver provenance record.
    ///
    /// # Errors
    ///
    /// Returns a validation, foreign-key, uniqueness, or other database error. The record is
    /// never partially inserted.
    pub fn record_solver_run(&mut self, record: &SolverRunRecord) -> Result<(), PersistenceError> {
        record.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_solver_run(&tx, record)?;
        tx.commit()?;
        Ok(())
    }

    fn migrate(&mut self) -> Result<(), PersistenceError> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_metadata (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                version INTEGER NOT NULL CHECK (version >= 0)
             ) STRICT;
             INSERT INTO schema_metadata(singleton, version)
             VALUES (1, 0)
             ON CONFLICT(singleton) DO NOTHING;",
        )?;
        let found: u32 = self.connection.query_row(
            "SELECT version FROM schema_metadata WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        if found > DATABASE_SCHEMA_VERSION {
            return Err(PersistenceError::UnsupportedSchema {
                found,
                supported: DATABASE_SCHEMA_VERSION,
            });
        }
        for &(version, sql) in MIGRATIONS.iter().filter(|(version, _)| *version > found) {
            let tx = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(sql)?;
            tx.execute(
                "UPDATE schema_metadata SET version = ?1 WHERE singleton = 1",
                [version],
            )?;
            tx.commit()?;
        }
        Ok(())
    }
}

fn insert_solver_run(
    tx: &rusqlite::Transaction<'_>,
    record: &SolverRunRecord,
) -> Result<(), PersistenceError> {
    let project_revision = sqlite_integer(record.project_revision, "project_revision")?;
    let seed = sqlite_integer(record.seed, "seed")?;
    let worker_count = i64::from(record.worker_count);
    let time_limit_ms = sqlite_integer(record.time_limit_ms, "time_limit_ms")?;
    let protocol_version = i64::from(record.protocol_version);
    tx.execute(
        "INSERT INTO solver_runs(
                run_id, project_id, project_revision, scenario_id, request_id,
                input_snapshot_hash, solver_engine_version, protocol_version, seed,
                parameters_json, worker_count, time_limit_ms, status_code, objective_json,
                validation_code, output_hash, started_at, finished_at
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18
             )",
        params![
            record.run_id,
            record.project_id,
            project_revision,
            record.scenario_id,
            record.request_id,
            record.input_snapshot_hash.as_slice(),
            record.solver_engine_version,
            protocol_version,
            seed,
            record.parameters_json,
            worker_count,
            time_limit_ms,
            record.status_code,
            record.objective_json,
            record.validation_code,
            record.output_hash.as_ref().map(<[u8; 32]>::as_slice),
            timestamp(record.started_at),
            record.finished_at.map(timestamp),
        ],
    )?;
    Ok(())
}

fn insert_revision(
    tx: &rusqlite::Transaction<'_>,
    document: &ProjectDocument,
    now: &str,
) -> Result<(), PersistenceError> {
    let revision = sqlite_integer(document.revision, "revision")?;
    tx.execute(
        "INSERT INTO project_revisions(
            project_id, revision, document_schema_version, payload, payload_hash, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            document.project_id,
            revision,
            document.document_schema_version,
            document.payload,
            document.payload_hash().as_bytes(),
            now,
        ],
    )?;
    Ok(())
}

fn sqlite_integer(value: u64, field: &'static str) -> Result<i64, PersistenceError> {
    i64::try_from(value).map_err(|_| PersistenceError::IntegerOutOfRange { field, value })
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(revision: u64, value: &str) -> ProjectDocument {
        ProjectDocument {
            project_id: "project-1".to_owned(),
            display_name: "测试高中 2026 秋季".to_owned(),
            revision,
            document_schema_version: 1,
            payload: serde_json::to_vec(&serde_json::json!({ "value": value })).unwrap(),
        }
    }

    #[test]
    fn create_load_and_replace_are_atomic() {
        let mut store = SqliteStore::in_memory().unwrap();
        store.create_project(&document(0, "initial")).unwrap();
        assert_eq!(
            store.load_project("project-1").unwrap(),
            document(0, "initial")
        );

        store.replace_project(0, &document(1, "updated")).unwrap();
        assert_eq!(
            store.load_project("project-1").unwrap(),
            document(1, "updated")
        );
    }

    #[test]
    fn stale_revision_is_rejected_without_partial_write() {
        let mut store = SqliteStore::in_memory().unwrap();
        store.create_project(&document(0, "initial")).unwrap();
        store.replace_project(0, &document(1, "winner")).unwrap();

        let error = store.replace_project(0, &document(1, "stale")).unwrap_err();
        assert_eq!(error.code(), "PERSISTENCE_REVISION_CONFLICT");
        assert_eq!(
            store.load_project("project-1").unwrap(),
            document(1, "winner")
        );
    }

    #[test]
    fn invalid_json_does_not_create_project() {
        let mut store = SqliteStore::in_memory().unwrap();
        let mut invalid = document(0, "ignored");
        invalid.payload = b"{not-json".to_vec();
        let error = store.create_project(&invalid).unwrap_err();
        assert_eq!(error.code(), "PERSISTENCE_INVALID_JSON_PAYLOAD");
        assert!(matches!(
            store.load_project("project-1"),
            Err(PersistenceError::ProjectNotFound { .. })
        ));
    }

    #[test]
    fn load_rejects_payload_corruption_instead_of_recomputing_a_trusted_receipt() {
        let mut store = SqliteStore::in_memory().unwrap();
        store.create_project(&document(0, "original")).unwrap();
        store
            .connection
            .execute(
                "UPDATE project_revisions SET payload = ?1 WHERE project_id = ?2",
                params![b"{\"changed\":true}".as_slice(), "project-1"],
            )
            .unwrap();
        assert_eq!(
            store.load_project("project-1").unwrap_err().code(),
            "PERSISTENCE_PAYLOAD_HASH_MISMATCH"
        );
    }

    #[test]
    fn foreign_keys_reject_orphan_solver_runs() {
        let mut store = SqliteStore::in_memory().unwrap();
        let run = SolverRunRecord {
            run_id: "run-1".to_owned(),
            project_id: "missing".to_owned(),
            project_revision: 0,
            scenario_id: None,
            request_id: "request-1".to_owned(),
            input_snapshot_hash: [1; 32],
            solver_engine_version: "or-tools-9.15.6755".to_owned(),
            protocol_version: 1,
            seed: 42,
            parameters_json: "{}".to_owned(),
            worker_count: 1,
            time_limit_ms: 1_000,
            status_code: "PROVEN_INFEASIBLE".to_owned(),
            objective_json: "{}".to_owned(),
            validation_code: "NOT_APPLICABLE".to_owned(),
            output_hash: None,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
        };
        assert!(matches!(
            store.record_solver_run(&run),
            Err(PersistenceError::Database(_))
        ));
    }

    #[test]
    fn migration_is_idempotent() {
        let mut store = SqliteStore::in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), DATABASE_SCHEMA_VERSION);
        store.migrate().unwrap();
        assert_eq!(store.schema_version().unwrap(), DATABASE_SCHEMA_VERSION);
    }

    #[test]
    fn upgrades_v1_database_without_losing_project_data() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schema_metadata (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    version INTEGER NOT NULL CHECK (version >= 0)
                 ) STRICT;
                 INSERT INTO schema_metadata(singleton, version) VALUES (1, 1);",
            )
            .unwrap();
        connection.execute_batch(MIGRATIONS[0].1).unwrap();
        let now = timestamp(Utc::now());
        let initial = document(0, "before-upgrade");
        connection
            .execute(
                "INSERT INTO projects(project_id, display_name, current_revision, created_at, updated_at)
                 VALUES (?1, ?2, 0, ?3, ?3)",
                params![initial.project_id, initial.display_name, now],
            )
            .unwrap();
        let tx = connection.unchecked_transaction().unwrap();
        insert_revision(&tx, &initial, &now).unwrap();
        tx.commit().unwrap();

        let store = SqliteStore::from_connection(connection).unwrap();
        assert_eq!(store.schema_version().unwrap(), DATABASE_SCHEMA_VERSION);
        assert_eq!(store.load_project("project-1").unwrap(), initial);
    }
}
