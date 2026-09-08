CREATE TABLE solve_artifacts (
    run_id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    project_revision INTEGER NOT NULL CHECK (project_revision >= 0),
    source_payload_hash BLOB NOT NULL CHECK (length(source_payload_hash) = 32),
    artifact_schema_version INTEGER NOT NULL CHECK (artifact_schema_version > 0),
    status_code TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL,
    payload BLOB NOT NULL CHECK (length(payload) <= 67108864),
    payload_hash BLOB NOT NULL CHECK (length(payload_hash) = 32),
    FOREIGN KEY(project_id, project_revision)
        REFERENCES project_revisions(project_id, revision) ON DELETE CASCADE
) STRICT;

CREATE TABLE solve_artifact_attempts (
    artifact_run_id TEXT NOT NULL REFERENCES solve_artifacts(run_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0 AND ordinal < 16),
    solver_run_id TEXT NOT NULL UNIQUE REFERENCES solver_runs(run_id),
    PRIMARY KEY(artifact_run_id, ordinal)
) STRICT;

CREATE INDEX solve_artifacts_project_started
    ON solve_artifacts(project_id, started_at DESC, run_id);
