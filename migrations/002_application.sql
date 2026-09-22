ALTER TABLE projects ADD COLUMN version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE projects ADD COLUMN settings_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE tasks ADD COLUMN role_overrides_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE tasks ADD COLUMN ready_at TEXT;
ALTER TABLE attempts ADD COLUMN parent_attempt_id TEXT REFERENCES attempts(id);
ALTER TABLE attempts ADD COLUMN plan_hash TEXT;
ALTER TABLE attempts ADD COLUMN plan_approved_at TEXT;
ALTER TABLE attempts ADD COLUMN candidate_hash TEXT;
ALTER TABLE attempts ADD COLUMN accepted_snapshot_id TEXT;
ALTER TABLE sessions ADD COLUMN native_identity_verified_at TEXT;
ALTER TABLE sessions ADD COLUMN readiness_state TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE capabilities ADD COLUMN hook_hash TEXT;
ALTER TABLE capabilities ADD COLUMN proof_json TEXT NOT NULL DEFAULT '{}';

CREATE TABLE task_dependencies (
    task_id TEXT NOT NULL REFERENCES tasks(id),
    depends_on_task_id TEXT NOT NULL REFERENCES tasks(id),
    integration_ref TEXT,
    integration_commit TEXT,
    verified_at TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY(task_id, depends_on_task_id),
    CHECK(task_id != depends_on_task_id)
);

CREATE TABLE workspaces (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL UNIQUE REFERENCES attempts(id),
    repository_identity TEXT NOT NULL,
    path TEXT NOT NULL,
    base_revision TEXT NOT NULL,
    worktree_head TEXT NOT NULL,
    policy_json TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE snapshots (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    kind TEXT NOT NULL,
    snapshot_base TEXT NOT NULL,
    manifest_hash TEXT NOT NULL,
    manifest_json TEXT NOT NULL,
    complete INTEGER NOT NULL,
    incomplete_reason TEXT,
    created_at TEXT NOT NULL,
    UNIQUE(attempt_id, kind, manifest_hash)
);

CREATE TABLE role_settings (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    role TEXT NOT NULL,
    revision INTEGER NOT NULL,
    config_json TEXT NOT NULL,
    effective_generation_id TEXT REFERENCES role_generations(id),
    created_at TEXT NOT NULL,
    UNIQUE(task_id, role, revision)
);

CREATE TABLE review_requests (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    review_kind TEXT NOT NULL,
    candidate_hash TEXT NOT NULL,
    role_generation_id TEXT REFERENCES role_generations(id),
    prompt_hash TEXT NOT NULL,
    handoff_hash TEXT NOT NULL,
    delivery_state TEXT NOT NULL,
    verdict TEXT,
    feedback TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE guidance_messages (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    body TEXT NOT NULL,
    state TEXT NOT NULL,
    reason TEXT,
    created_at TEXT NOT NULL,
    acknowledged_at TEXT
);

CREATE TABLE switch_intents (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    role TEXT NOT NULL,
    old_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    requested_settings_revision INTEGER NOT NULL,
    checkpoint_snapshot_id TEXT REFERENCES snapshots(id),
    handoff_json TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE check_runs (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    candidate_hash TEXT NOT NULL,
    executable TEXT NOT NULL,
    arguments_json TEXT NOT NULL,
    cwd TEXT NOT NULL,
    status TEXT NOT NULL,
    exit_code INTEGER,
    evidence_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    finished_at TEXT
);

CREATE TABLE import_records (
    source_identity TEXT PRIMARY KEY,
    source_hash TEXT NOT NULL,
    result_json TEXT NOT NULL,
    imported_at TEXT NOT NULL
);

CREATE TABLE scheduler_projects (
    project_id TEXT PRIMARY KEY REFERENCES projects(id),
    last_claimed_at TEXT
);

CREATE INDEX tasks_project_schedule ON tasks(project_id, lifecycle, priority DESC, manual_order, created_at);
CREATE INDEX reviews_attempt_kind ON review_requests(attempt_id, review_kind, created_at);
CREATE INDEX guidance_generation_state ON guidance_messages(role_generation_id, state, created_at);
