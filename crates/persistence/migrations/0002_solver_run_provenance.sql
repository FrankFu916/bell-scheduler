CREATE TABLE solver_runs (
    run_id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
    project_revision INTEGER NOT NULL,
    scenario_id TEXT,
    request_id TEXT NOT NULL UNIQUE,
    input_snapshot_hash BLOB NOT NULL CHECK (length(input_snapshot_hash) = 32),
    solver_engine_version TEXT NOT NULL,
    protocol_version INTEGER NOT NULL CHECK (protocol_version > 0),
    seed INTEGER NOT NULL CHECK (seed >= 0),
    parameters_json TEXT NOT NULL,
    worker_count INTEGER NOT NULL CHECK (worker_count > 0),
    time_limit_ms INTEGER NOT NULL CHECK (time_limit_ms > 0),
    status_code TEXT NOT NULL,
    objective_json TEXT NOT NULL,
    validation_code TEXT NOT NULL,
    output_hash BLOB CHECK (output_hash IS NULL OR length(output_hash) = 32),
    started_at TEXT NOT NULL,
    finished_at TEXT,
    FOREIGN KEY (project_id, project_revision)
        REFERENCES project_revisions(project_id, revision)
) STRICT;

CREATE INDEX solver_runs_project_revision_idx
    ON solver_runs(project_id, project_revision, started_at);

