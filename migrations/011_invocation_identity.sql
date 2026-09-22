ALTER TABLE sessions ADD COLUMN capability_key TEXT;
ALTER TABLE sessions ADD COLUMN capability_identity_json TEXT;
ALTER TABLE sessions ADD COLUMN recovery_root_pid INTEGER;
ALTER TABLE sessions ADD COLUMN recovery_process_group_id INTEGER;

ALTER TABLE check_runs ADD COLUMN recovery_root_pid INTEGER;
ALTER TABLE check_runs ADD COLUMN recovery_process_group_id INTEGER;

CREATE TABLE resume_invocations (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    resume_ordinal INTEGER NOT NULL,
    transcript_epoch TEXT NOT NULL UNIQUE,
    launch_config_json TEXT NOT NULL,
    capability_key TEXT NOT NULL,
    capability_identity_json TEXT NOT NULL,
    state TEXT NOT NULL,
    process_identity_json TEXT,
    error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(session_id, resume_ordinal)
);

CREATE INDEX resume_invocations_session
ON resume_invocations(session_id, resume_ordinal);
