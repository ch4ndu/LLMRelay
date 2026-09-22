CREATE TABLE permission_requests (
    id TEXT PRIMARY KEY,
    hook_invocation_nonce TEXT NOT NULL UNIQUE,
    connection_nonce TEXT NOT NULL,
    provider TEXT NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    task_id TEXT NOT NULL REFERENCES tasks(id),
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    role TEXT NOT NULL,
    service_boot_id TEXT NOT NULL,
    native_session_id TEXT NOT NULL,
    cwd TEXT NOT NULL,
    policy_fingerprint TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    input_json TEXT NOT NULL,
    requested_access_json TEXT,
    reason TEXT,
    command_display TEXT,
    family_json TEXT,
    family_unavailable_reason TEXT,
    created_at TEXT NOT NULL,
    deadline_at TEXT NOT NULL,
    state TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    decision_kind TEXT,
    decision_actor TEXT,
    decision_reason TEXT,
    matching_rule_id TEXT,
    decided_at TEXT,
    delivery_state TEXT NOT NULL DEFAULT 'not_reserved',
    delivery_reserved_at TEXT,
    reserved_behavior TEXT,
    delivered_at TEXT,
    delivery_unknown_at TEXT,
    delivery_reason TEXT,
    consumed_at TEXT,
    updated_at TEXT NOT NULL
);

CREATE INDEX permission_requests_pending
ON permission_requests(state, deadline_at, created_at);

CREATE INDEX permission_requests_session
ON permission_requests(session_id, role_generation_id, created_at);

CREATE INDEX permission_requests_delivery
ON permission_requests(delivery_state, service_boot_id, updated_at);

CREATE TABLE permission_rules (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    role TEXT NOT NULL,
    lifetime TEXT NOT NULL,
    session_id TEXT REFERENCES sessions(id),
    role_generation_id TEXT REFERENCES role_generations(id),
    native_session_id TEXT,
    registered_root TEXT NOT NULL,
    repository_identity TEXT NOT NULL,
    worktree_path TEXT,
    executable_kind TEXT NOT NULL,
    executable_value TEXT NOT NULL,
    display_family TEXT NOT NULL,
    policy_fingerprint TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    revoked_by TEXT,
    revoke_reason TEXT,
    last_used_at TEXT,
    use_count INTEGER NOT NULL DEFAULT 0,
    revision INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX permission_rules_match
ON permission_rules(project_id, provider, role, policy_fingerprint, revoked_at);

CREATE INDEX permission_rules_session
ON permission_rules(session_id, role_generation_id, native_session_id)
WHERE lifetime = 'session';
