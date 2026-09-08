CREATE TABLE scenarios (
    scenario_id TEXT PRIMARY KEY CHECK (length(trim(scenario_id)) > 0),
    timetable_id TEXT NOT NULL UNIQUE CHECK (length(trim(timetable_id)) > 0),
    project_id TEXT NOT NULL REFERENCES projects(project_id),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
    current_revision INTEGER NOT NULL CHECK (current_revision >= 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (scenario_id, timetable_id, project_id),
    FOREIGN KEY (scenario_id, current_revision)
        REFERENCES scenario_revisions(scenario_id, scenario_revision)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE TABLE scenario_revisions (
    scenario_id TEXT NOT NULL,
    scenario_revision INTEGER NOT NULL CHECK (scenario_revision >= 0),
    timetable_id TEXT NOT NULL,
    timetable_revision INTEGER NOT NULL CHECK (timetable_revision >= 0),
    project_id TEXT NOT NULL,
    source_project_revision INTEGER NOT NULL CHECK (source_project_revision >= 0),
    source_payload_hash BLOB NOT NULL CHECK (length(source_payload_hash) = 32),
    origin_run_id TEXT NOT NULL REFERENCES solve_artifacts(run_id),
    origin_artifact_hash BLOB NOT NULL CHECK (length(origin_artifact_hash) = 32),
    scenario_schema_version INTEGER NOT NULL CHECK (scenario_schema_version > 0),
    payload BLOB NOT NULL CHECK (length(payload) <= 67108864),
    payload_hash BLOB NOT NULL CHECK (length(payload_hash) = 32),
    created_at TEXT NOT NULL,
    PRIMARY KEY (scenario_id, scenario_revision),
    FOREIGN KEY (scenario_id, timetable_id, project_id)
        REFERENCES scenarios(scenario_id, timetable_id, project_id),
    FOREIGN KEY (project_id, source_project_revision)
        REFERENCES project_revisions(project_id, revision),
    FOREIGN KEY (scenario_id, scenario_revision, timetable_id, timetable_revision)
        REFERENCES timetable_revisions(scenario_id, scenario_revision, timetable_id, timetable_revision)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE TABLE timetable_revisions (
    timetable_id TEXT NOT NULL,
    timetable_revision INTEGER NOT NULL CHECK (timetable_revision >= 0),
    scenario_id TEXT NOT NULL,
    scenario_revision INTEGER NOT NULL CHECK (scenario_revision >= 0),
    timetable_schema_version INTEGER NOT NULL CHECK (timetable_schema_version > 0),
    payload BLOB NOT NULL CHECK (length(payload) <= 67108864),
    payload_hash BLOB NOT NULL CHECK (length(payload_hash) = 32),
    created_at TEXT NOT NULL,
    PRIMARY KEY (timetable_id, timetable_revision),
    UNIQUE (scenario_id, scenario_revision, timetable_id, timetable_revision),
    FOREIGN KEY (scenario_id, scenario_revision)
        REFERENCES scenario_revisions(scenario_id, scenario_revision)
) STRICT;

CREATE INDEX scenarios_by_project ON scenarios(project_id, created_at DESC, scenario_id);
