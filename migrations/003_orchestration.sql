ALTER TABLE attempts ADD COLUMN scope_hash TEXT;
ALTER TABLE attempts ADD COLUMN configuration_hash TEXT;
ALTER TABLE task_dependencies ADD COLUMN integration_manifest_hash TEXT;
ALTER TABLE task_dependencies ADD COLUMN verification_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE review_requests ADD COLUMN session_id TEXT REFERENCES sessions(id);
ALTER TABLE review_requests ADD COLUMN settings_revision INTEGER;
ALTER TABLE review_requests ADD COLUMN budget_spent_at TEXT;
ALTER TABLE review_requests ADD COLUMN ambiguity_state TEXT NOT NULL DEFAULT 'none';
ALTER TABLE review_requests ADD COLUMN resume_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE review_requests ADD COLUMN operation_key TEXT;
ALTER TABLE guidance_messages ADD COLUMN written_at TEXT;
ALTER TABLE guidance_messages ADD COLUMN submitted_at TEXT;
ALTER TABLE switch_intents ADD COLUMN operation_id TEXT;
ALTER TABLE switch_intents ADD COLUMN expected_task_version INTEGER;
ALTER TABLE switch_intents ADD COLUMN config_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE switch_intents ADD COLUMN authority_fence TEXT;
ALTER TABLE controls ADD COLUMN requested_operation_id TEXT;

CREATE TABLE check_suites (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    name TEXT NOT NULL,
    position INTEGER NOT NULL,
    executable TEXT NOT NULL,
    arguments_json TEXT NOT NULL,
    timeout_seconds INTEGER NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(project_id, name)
);

CREATE TABLE launch_permits (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    role TEXT NOT NULL,
    settings_revision INTEGER NOT NULL,
    switch_intent_id TEXT REFERENCES switch_intents(id),
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    consumed_at TEXT
);

CREATE TABLE rework_intents (
    id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL UNIQUE,
    parent_attempt_id TEXT NOT NULL REFERENCES attempts(id),
    new_attempt_id TEXT NOT NULL UNIQUE REFERENCES attempts(id),
    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),
    feedback TEXT NOT NULL,
    carry_plan_approval INTEGER NOT NULL,
    scope_hash TEXT NOT NULL,
    configuration_hash TEXT NOT NULL,
    state TEXT NOT NULL,
    result_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE recovery_records (
    id TEXT PRIMARY KEY,
    session_id TEXT REFERENCES sessions(id),
    attempt_id TEXT REFERENCES attempts(id),
    state TEXT NOT NULL,
    process_identity_json TEXT,
    detail_json TEXT NOT NULL,
    resolved_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX one_active_review_request
ON review_requests(attempt_id, review_kind)
WHERE delivery_state IN ('reserved','launching','delivered','ambiguous');

CREATE INDEX launch_permits_active ON launch_permits(attempt_id, role, state);
CREATE INDEX rework_intents_state ON rework_intents(state, created_at);
CREATE INDEX recovery_records_state ON recovery_records(state, created_at);
