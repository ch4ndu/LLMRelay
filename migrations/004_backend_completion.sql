ALTER TABLE review_requests ADD COLUMN prompt_text TEXT;
ALTER TABLE review_requests ADD COLUMN handoff_json TEXT;
ALTER TABLE sessions ADD COLUMN invocation_input_json TEXT;
ALTER TABLE role_results ADD COLUMN consumed_at TEXT;
ALTER TABLE attempts ADD COLUMN step_budget INTEGER NOT NULL DEFAULT -1;
ALTER TABLE attempts ADD COLUMN last_coordinator_at TEXT;

CREATE TABLE session_processes (
    session_id TEXT NOT NULL REFERENCES sessions(id),
    pid INTEGER NOT NULL,
    native_start_marker TEXT NOT NULL,
    process_group_id INTEGER NOT NULL,
    parent_pid INTEGER,
    last_seen_at TEXT NOT NULL,
    PRIMARY KEY(session_id, pid, native_start_marker)
);

CREATE INDEX session_processes_session ON session_processes(session_id, last_seen_at);

CREATE TABLE check_processes (
    check_id TEXT NOT NULL REFERENCES check_runs(id),
    pid INTEGER NOT NULL,
    native_start_marker TEXT NOT NULL,
    process_group_id INTEGER NOT NULL,
    parent_pid INTEGER,
    last_seen_at TEXT NOT NULL,
    PRIMARY KEY(check_id, pid, native_start_marker)
);
