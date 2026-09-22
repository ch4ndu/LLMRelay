CREATE TABLE trip_runtime_admissions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    task_id TEXT REFERENCES tasks(id),
    scope_hash TEXT NOT NULL,
    state TEXT NOT NULL,
    fresh_call_count INTEGER NOT NULL,
    authorized_at TEXT,
    failure_reason TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE trip_runtime_probes (
    admission_id TEXT NOT NULL REFERENCES trip_runtime_admissions(id),
    role TEXT NOT NULL,
    settings_revision INTEGER,
    launch_config_json TEXT NOT NULL,
    profile_json TEXT NOT NULL,
    profile_hash TEXT NOT NULL,
    project_config_revision_id TEXT NOT NULL REFERENCES trip_config_revisions(id),
    project_configuration_hash TEXT NOT NULL,
    adapter_name TEXT NOT NULL,
    adapter_hash TEXT NOT NULL,
    capability_key TEXT NOT NULL,
    capability_identity_json TEXT NOT NULL,
    fixture_project_id TEXT NOT NULL REFERENCES projects(id),
    fixture_repository_identity TEXT NOT NULL,
    fixture_root TEXT NOT NULL,
    attempt_id TEXT REFERENCES attempts(id),
    workspace_path TEXT,
    service_sentinel_path TEXT,
    control_socket_path TEXT,
    nonce TEXT NOT NULL,
    state TEXT NOT NULL,
    session_id TEXT REFERENCES sessions(id),
    capability_id TEXT REFERENCES capabilities(id),
    failure_reason TEXT,
    published_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(admission_id, role)
);

CREATE INDEX trip_runtime_admission_project
ON trip_runtime_admissions(project_id, updated_at);
