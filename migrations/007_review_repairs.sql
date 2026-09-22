ALTER TABLE attempts ADD COLUMN workflow_version TEXT NOT NULL DEFAULT 'trip-v1';
ALTER TABLE attempts ADD COLUMN workflow_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN workflow_version TEXT NOT NULL DEFAULT 'trip-v1';
ALTER TABLE sessions ADD COLUMN workflow_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN prompt_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE snapshots ADD COLUMN original_base TEXT;
ALTER TABLE snapshots ADD COLUMN candidate_head TEXT;
ALTER TABLE snapshots ADD COLUMN source_role_generation_id TEXT REFERENCES role_generations(id);
ALTER TABLE snapshots ADD COLUMN source_settings_revision INTEGER;
ALTER TABLE snapshots ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);
ALTER TABLE snapshots ADD COLUMN workspace_hash TEXT;
ALTER TABLE check_runs ADD COLUMN launch_state TEXT NOT NULL DEFAULT 'not_started';
ALTER TABLE check_runs ADD COLUMN launch_error TEXT;

CREATE TABLE freeze_intents (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    kind TEXT NOT NULL,
    source_role_generation_id TEXT REFERENCES role_generations(id),
    state TEXT NOT NULL,
    result_snapshot_id TEXT REFERENCES snapshots(id),
    error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX one_active_freeze_per_attempt
ON freeze_intents(attempt_id)
WHERE state IN ('reserved','capturing','recovery_required');

CREATE INDEX freeze_intents_state ON freeze_intents(state, updated_at);
