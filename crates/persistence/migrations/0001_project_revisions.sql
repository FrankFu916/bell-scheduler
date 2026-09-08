CREATE TABLE projects (
    project_id TEXT PRIMARY KEY NOT NULL,
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
    current_revision INTEGER NOT NULL CHECK (current_revision >= 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE project_revisions (
    project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    document_schema_version INTEGER NOT NULL CHECK (document_schema_version > 0),
    payload BLOB NOT NULL,
    payload_hash BLOB NOT NULL CHECK (length(payload_hash) = 32),
    created_at TEXT NOT NULL,
    PRIMARY KEY (project_id, revision)
) STRICT;

CREATE INDEX project_revisions_created_at_idx
    ON project_revisions(created_at);

