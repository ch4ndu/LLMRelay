CREATE TABLE trip_task_profile_activations (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    role TEXT NOT NULL,
    settings_id TEXT NOT NULL REFERENCES role_settings(id),
    settings_revision INTEGER NOT NULL,
    profile_json TEXT NOT NULL,
    profile_hash TEXT NOT NULL,
    project_config_revision_id TEXT NOT NULL REFERENCES trip_config_revisions(id),
    project_configuration_hash TEXT NOT NULL,
    adapter_name TEXT NOT NULL,
    adapter_hash TEXT NOT NULL,
    capability_id TEXT NOT NULL REFERENCES capabilities(id),
    capability_key TEXT NOT NULL,
    capability_proof_hash TEXT NOT NULL,
    activated_at TEXT NOT NULL,
    UNIQUE(task_id, role, settings_revision, project_config_revision_id, adapter_hash, capability_id, capability_key, capability_proof_hash)
);

CREATE TABLE trip_attempt_profiles (
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    role TEXT NOT NULL,
    settings_revision INTEGER NOT NULL,
    activation_id TEXT REFERENCES trip_task_profile_activations(id),
    source TEXT NOT NULL,
    profile_json TEXT NOT NULL,
    profile_hash TEXT NOT NULL,
    project_config_revision_id TEXT NOT NULL REFERENCES trip_config_revisions(id),
    project_configuration_hash TEXT NOT NULL,
    adapter_name TEXT NOT NULL,
    adapter_hash TEXT NOT NULL,
    capability_id TEXT NOT NULL REFERENCES capabilities(id),
    capability_key TEXT NOT NULL,
    capability_proof_hash TEXT NOT NULL,
    bound_at TEXT NOT NULL,
    PRIMARY KEY(attempt_id, role)
);

CREATE INDEX trip_task_profile_revision
ON trip_task_profile_activations(task_id, role, settings_revision);
