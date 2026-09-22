CREATE TABLE trip_check_permission_rules (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    source TEXT NOT NULL DEFAULT 'service_check',
    registered_root TEXT NOT NULL,
    repository_identity TEXT NOT NULL,
    executable_kind TEXT NOT NULL,
    executable_value TEXT NOT NULL,
    display_family TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    revoked_by TEXT,
    revoke_reason TEXT,
    last_used_at TEXT,
    use_count INTEGER NOT NULL DEFAULT 0,
    revision INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX trip_check_permission_rules_match
ON trip_check_permission_rules(project_id, repository_identity, executable_kind, executable_value, revoked_at);

ALTER TABLE trip_check_authorizations ADD COLUMN matching_rule_id TEXT REFERENCES trip_check_permission_rules(id);
