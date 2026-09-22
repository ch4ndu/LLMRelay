ALTER TABLE sessions ADD COLUMN desired_running INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN launch_boot_identity TEXT;
ALTER TABLE sessions ADD COLUMN recovery_anchor_json TEXT;

ALTER TABLE check_runs ADD COLUMN launch_boot_identity TEXT;
ALTER TABLE check_runs ADD COLUMN recovery_anchor_json TEXT;

ALTER TABLE resume_invocations ADD COLUMN prior_exit_json TEXT;
ALTER TABLE resume_invocations ADD COLUMN prior_transcript_epoch TEXT;
ALTER TABLE resume_invocations ADD COLUMN prior_launch_boot_identity TEXT;
ALTER TABLE resume_invocations ADD COLUMN prior_recovery_anchor_json TEXT;
ALTER TABLE resume_invocations ADD COLUMN prior_recovery_root_pid INTEGER;
ALTER TABLE resume_invocations ADD COLUMN prior_recovery_process_group_id INTEGER;

CREATE TABLE instance_settings (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    version INTEGER NOT NULL,
    auto_resume_eligible INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL
);

INSERT INTO instance_settings(singleton,version,auto_resume_eligible,updated_at)
VALUES(1,1,0,strftime('%Y-%m-%dT%H:%M:%fZ','now'));

CREATE TABLE restart_candidates (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id),
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    task_id TEXT NOT NULL REFERENCES tasks(id),
    source TEXT NOT NULL,
    state TEXT NOT NULL,
    reason TEXT NOT NULL,
    requested_by TEXT,
    result_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX restart_candidates_state ON restart_candidates(state,updated_at);
