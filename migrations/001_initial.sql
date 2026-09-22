PRAGMA foreign_keys = ON;

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    repository_path TEXT NOT NULL,
    repository_identity TEXT NOT NULL UNIQUE,
    base_revision TEXT NOT NULL,
    queue_paused INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    acceptance_criteria_json TEXT NOT NULL DEFAULT '[]',
    priority INTEGER NOT NULL DEFAULT 0,
    manual_order INTEGER NOT NULL DEFAULT 0,
    lifecycle TEXT NOT NULL,
    attention TEXT NOT NULL DEFAULT 'none',
    version INTEGER NOT NULL DEFAULT 1,
    archived_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE attempts (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    context_id TEXT NOT NULL,
    phase TEXT NOT NULL,
    base_revision TEXT NOT NULL,
    configuration_revision INTEGER NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE config_revisions (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    revision INTEGER NOT NULL,
    config_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(attempt_id, revision)
);

CREATE TABLE role_generations (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    role TEXT NOT NULL,
    provider TEXT NOT NULL,
    generation INTEGER NOT NULL,
    config_revision INTEGER NOT NULL,
    status TEXT NOT NULL,
    authority_generation TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(attempt_id, role, generation)
);

CREATE TABLE role_credentials (
    id TEXT PRIMARY KEY,
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    token_hash TEXT NOT NULL UNIQUE,
    permissions_json TEXT NOT NULL,
    revoked_at TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE claims (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    repository_identity TEXT NOT NULL,
    state TEXT NOT NULL,
    process_identity_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX one_active_claim_per_repository
ON claims(repository_identity)
WHERE state IN ('reserved', 'launching', 'running', 'unknown', 'stopping');

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    provider TEXT NOT NULL,
    validation_cell TEXT,
    status TEXT NOT NULL,
    launch_config_json TEXT NOT NULL,
    executable_version TEXT NOT NULL,
    native_session_id TEXT,
    native_identity_source TEXT,
    process_identity_json TEXT,
    transcript_epoch TEXT NOT NULL,
    transcript_last_sequence INTEGER NOT NULL DEFAULT 0,
    capture_state TEXT NOT NULL DEFAULT 'capturing',
    capture_error TEXT,
    hook_trust_state TEXT NOT NULL DEFAULT 'pending_observation',
    exit_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE hook_events (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    provider TEXT NOT NULL,
    event_name TEXT NOT NULL,
    native_session_id TEXT,
    payload_json TEXT NOT NULL,
    peer_pid INTEGER NOT NULL,
    peer_process_group_id INTEGER NOT NULL,
    peer_start_marker TEXT NOT NULL,
    provenance_state TEXT NOT NULL,
    received_at TEXT NOT NULL
);

CREATE INDEX hook_events_session_order ON hook_events(session_id, received_at);

CREATE TABLE role_results (
    id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    outcome TEXT NOT NULL,
    summary TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(operation_id, role_generation_id)
);

CREATE TABLE capabilities (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    executable_version TEXT NOT NULL,
    role TEXT NOT NULL,
    mode TEXT NOT NULL,
    config_hash TEXT NOT NULL,
    status TEXT NOT NULL,
    evidence_reference TEXT,
    gaps_json TEXT NOT NULL DEFAULT '[]',
    checked_at TEXT NOT NULL,
    UNIQUE(provider, executable_version, role, mode, config_hash)
);

CREATE TABLE reviews (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    candidate_hash TEXT NOT NULL,
    review_kind TEXT NOT NULL,
    status TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE review_budgets (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    review_kind TEXT NOT NULL,
    initial_allowance INTEGER NOT NULL,
    extension_allowance INTEGER NOT NULL DEFAULT 0,
    spent INTEGER NOT NULL DEFAULT 0,
    version INTEGER NOT NULL DEFAULT 1,
    UNIQUE(attempt_id, review_kind)
);

CREATE TABLE controls (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    role_generation_id TEXT REFERENCES role_generations(id),
    kind TEXT NOT NULL,
    state TEXT NOT NULL,
    expected_version INTEGER NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE audit_events (
    id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL,
    actor_kind TEXT NOT NULL,
    actor_id TEXT,
    event_code TEXT NOT NULL,
    entity_kind TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    old_version INTEGER,
    new_version INTEGER,
    detail_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX audit_events_operation ON audit_events(operation_id);
CREATE INDEX audit_events_entity ON audit_events(entity_kind, entity_id, created_at);

CREATE TABLE operation_receipts (
    operation_id TEXT NOT NULL,
    actor_key TEXT NOT NULL,
    operation_kind TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    result_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(operation_id, actor_key, operation_kind)
);

CREATE TABLE input_leases (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id),
    lease_id_hash TEXT NOT NULL,
    owner_kind TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    role_generation_id TEXT NOT NULL,
    process_identity_json TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    revoked_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
