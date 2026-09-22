ALTER TABLE projects ADD COLUMN internal_purpose TEXT;
ALTER TABLE attempts ADD COLUMN setup_operation_id TEXT;
ALTER TABLE attempts ADD COLUMN upstream_source_hash TEXT;
ALTER TABLE attempts ADD COLUMN overlay_hash TEXT;
ALTER TABLE attempts ADD COLUMN structured_plan_id TEXT;
ALTER TABLE attempts ADD COLUMN final_repair_round INTEGER NOT NULL DEFAULT 0;
ALTER TABLE attempts ADD COLUMN legacy_migration_required INTEGER NOT NULL DEFAULT 1;
ALTER TABLE attempts ADD COLUMN manager_conformance_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE attempts ADD COLUMN selected_checks_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE attempts ADD COLUMN human_acceptance_at TEXT;
ALTER TABLE role_generations ADD COLUMN lane_id TEXT NOT NULL DEFAULT 'default';
ALTER TABLE launch_permits ADD COLUMN lane_id TEXT NOT NULL DEFAULT 'default';
ALTER TABLE launch_permits ADD COLUMN setup_permit_id TEXT;
ALTER TABLE sessions ADD COLUMN lane_id TEXT NOT NULL DEFAULT 'default';
ALTER TABLE sessions ADD COLUMN setup_permit_id TEXT;
ALTER TABLE check_runs ADD COLUMN check_id TEXT;
ALTER TABLE check_runs ADD COLUMN selected_check_revision INTEGER;
ALTER TABLE check_runs ADD COLUMN inputs_hash TEXT;
ALTER TABLE check_runs ADD COLUMN acceptance_coverage_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE check_runs ADD COLUMN elapsed_millis INTEGER;
ALTER TABLE check_runs ADD COLUMN freshness_state TEXT NOT NULL DEFAULT 'unknown';

CREATE TABLE trip_project_state (
    project_id TEXT PRIMARY KEY REFERENCES projects(id),
    readiness TEXT NOT NULL,
    reason TEXT NOT NULL,
    detected_installation TEXT NOT NULL,
    detected_json TEXT NOT NULL DEFAULT '{}',
    setup_operation_id TEXT,
    active_config_revision_id TEXT,
    workflow_id TEXT,
    package_version TEXT,
    upstream_source_hash TEXT,
    overlay_hash TEXT,
    manifest_hash TEXT,
    activated_at TEXT,
    updated_at TEXT NOT NULL
);

CREATE TABLE trip_config_revisions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    revision INTEGER NOT NULL,
    state TEXT NOT NULL,
    config_json TEXT NOT NULL,
    adapters_json TEXT NOT NULL,
    preflight_json TEXT NOT NULL,
    verification_json TEXT NOT NULL,
    source_hash TEXT NOT NULL,
    overlay_hash TEXT NOT NULL,
    configuration_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    activated_at TEXT,
    UNIQUE(project_id, revision)
);

CREATE TABLE trip_setup_operations (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    fixture_project_id TEXT REFERENCES projects(id),
    validation_task_id TEXT REFERENCES tasks(id),
    discovery_attempt_id TEXT REFERENCES attempts(id),
    probe_attempt_id TEXT REFERENCES attempts(id),
    state TEXT NOT NULL,
    target_inventory_json TEXT NOT NULL,
    proposal_json TEXT NOT NULL,
    proposal_hash TEXT,
    selected_profiles_hash TEXT,
    probe_authorized_at TEXT,
    install_authorized_at TEXT,
    approved_preimages_hash TEXT,
    error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX one_active_trip_setup
ON trip_setup_operations(project_id)
WHERE state NOT IN ('activated','aborted','superseded');

CREATE TABLE trip_setup_permits (
    id TEXT PRIMARY KEY,
    setup_operation_id TEXT NOT NULL REFERENCES trip_setup_operations(id),
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    fixture_project_id TEXT NOT NULL REFERENCES projects(id),
    fixture_repository_identity TEXT NOT NULL,
    role TEXT NOT NULL,
    profile_hash TEXT NOT NULL,
    settings_revision INTEGER NOT NULL,
    purpose TEXT NOT NULL,
    approved_action TEXT NOT NULL,
    nonce TEXT,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    consumed_at TEXT
);

CREATE TABLE trip_setup_profile_selections (
    setup_operation_id TEXT NOT NULL REFERENCES trip_setup_operations(id),
    role TEXT NOT NULL,
    selection_state TEXT NOT NULL,
    profile_json TEXT,
    profile_hash TEXT,
    selected_at TEXT,
    PRIMARY KEY(setup_operation_id, role)
);

CREATE TABLE trip_setup_reads (
    id TEXT PRIMARY KEY,
    setup_operation_id TEXT NOT NULL REFERENCES trip_setup_operations(id),
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    relative_path TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(setup_operation_id, role_generation_id, relative_path)
);

CREATE TABLE trip_apply_journal (
    id TEXT PRIMARY KEY,
    setup_operation_id TEXT NOT NULL REFERENCES trip_setup_operations(id),
    relative_path TEXT NOT NULL,
    source_hash TEXT NOT NULL,
    expected_preimage_hash TEXT,
    observed_preimage_hash TEXT,
    staged_path TEXT NOT NULL,
    state TEXT NOT NULL,
    created_by_operation INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(setup_operation_id, relative_path)
);

CREATE TABLE trip_preflight_receipts (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    setup_operation_id TEXT NOT NULL REFERENCES trip_setup_operations(id),
    profile_id TEXT NOT NULL,
    profile_hash TEXT NOT NULL,
    provider TEXT NOT NULL,
    role TEXT NOT NULL,
    authority TEXT NOT NULL,
    session_mode TEXT NOT NULL,
    generation_id TEXT NOT NULL,
    result TEXT NOT NULL,
    model_evidence TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(setup_operation_id, profile_id, generation_id)
);

CREATE TABLE trip_structured_plans (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    plan_hash TEXT NOT NULL,
    plan_json TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    profile_revision_id TEXT NOT NULL REFERENCES trip_config_revisions(id),
    criteria_hash TEXT NOT NULL,
    verification_hash TEXT NOT NULL,
    ownership_hash TEXT NOT NULL,
    conformance_hash TEXT NOT NULL,
    explorer_decision_id TEXT,
    review_request_id TEXT,
    reviewed_at TEXT,
    approved_at TEXT,
    implementation_authorized_at TEXT,
    created_at TEXT NOT NULL,
    UNIQUE(attempt_id, plan_hash)
);

CREATE TABLE trip_explorer_decisions (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    stage TEXT NOT NULL,
    census_json TEXT NOT NULL,
    trigger TEXT NOT NULL,
    activated INTEGER NOT NULL,
    limits_json TEXT NOT NULL,
    role_generation_id TEXT REFERENCES role_generations(id),
    candidate_hash TEXT,
    outcome_json TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE trip_explorer_extensions (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL UNIQUE REFERENCES attempts(id),
    stage TEXT NOT NULL,
    justification TEXT NOT NULL,
    authorized_at TEXT NOT NULL,
    consumed_at TEXT
);

CREATE TABLE implementation_lanes (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    lane_key TEXT NOT NULL,
    owned_paths_json TEXT NOT NULL,
    shared_paths_json TEXT NOT NULL,
    protected_paths_json TEXT NOT NULL,
    dependencies_json TEXT NOT NULL,
    source_hashes_json TEXT NOT NULL,
    frozen_seams_hash TEXT NOT NULL,
    required INTEGER NOT NULL DEFAULT 1,
    state TEXT NOT NULL,
    admitted_by_generation_id TEXT REFERENCES role_generations(id),
    yielded_at TEXT,
    receipt_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(attempt_id, lane_key)
);

CREATE TABLE lane_generations (
    lane_id TEXT PRIMARY KEY REFERENCES implementation_lanes(id),
    effective_generation_id TEXT REFERENCES role_generations(id),
    pending_settings_revision INTEGER,
    updated_at TEXT NOT NULL
);

CREATE TABLE trip_integration_requests (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL UNIQUE REFERENCES attempts(id),
    capsule_json TEXT NOT NULL,
    requested_by_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    dispatched_at TEXT
);

CREATE TABLE trip_verification_checks (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    config_revision_id TEXT NOT NULL REFERENCES trip_config_revisions(id),
    check_key TEXT NOT NULL,
    category TEXT NOT NULL,
    command_kind TEXT NOT NULL,
    executable TEXT,
    arguments_json TEXT,
    shell_command TEXT,
    cwd TEXT NOT NULL,
    timeout_seconds INTEGER NOT NULL,
    acceptance_rows_json TEXT NOT NULL,
    relevant_inputs_json TEXT NOT NULL,
    invalidation_json TEXT NOT NULL,
    original_text TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    UNIQUE(config_revision_id, check_key)
);

CREATE TABLE trip_selected_checks (
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    revision INTEGER NOT NULL,
    check_id TEXT NOT NULL REFERENCES trip_verification_checks(id),
    required INTEGER NOT NULL DEFAULT 1,
    selected_by_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    created_at TEXT NOT NULL,
    PRIMARY KEY(attempt_id, revision, check_id)
);

CREATE TABLE trip_check_authorizations (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    check_id TEXT NOT NULL REFERENCES trip_verification_checks(id),
    selected_revision INTEGER NOT NULL,
    exact_command_hash TEXT NOT NULL,
    scope_hash TEXT NOT NULL,
    decision TEXT NOT NULL,
    lifetime TEXT NOT NULL,
    created_at TEXT NOT NULL,
    consumed_at TEXT
);

CREATE TABLE trip_conformance_receipts (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    revision INTEGER NOT NULL,
    candidate_hash TEXT NOT NULL,
    config_hash TEXT NOT NULL,
    acceptance_json TEXT NOT NULL,
    ownership_json TEXT NOT NULL,
    documentation_json TEXT NOT NULL,
    test_policy_json TEXT NOT NULL,
    readability_json TEXT NOT NULL,
    submitted_by_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    created_at TEXT NOT NULL,
    UNIQUE(attempt_id, revision)
);

CREATE TABLE trip_legacy_migrations (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL UNIQUE REFERENCES attempts(id),
    from_workflow_id TEXT NOT NULL,
    to_workflow_id TEXT NOT NULL,
    preserved_json TEXT NOT NULL,
    reviewed_plan_hash TEXT NOT NULL,
    config_revision_id TEXT NOT NULL REFERENCES trip_config_revisions(id),
    authorized_at TEXT NOT NULL
);

INSERT INTO trip_project_state(
    project_id,readiness,reason,detected_installation,detected_json,updated_at
)
SELECT id,'not_initialized',
       'TRIP Explorer 0.9.0 has not been inspected and activated for this project',
       'unknown','{}',strftime('%Y-%m-%dT%H:%M:%fZ','now')
FROM projects;

UPDATE role_settings SET role='final_verifier' WHERE role='final_reviewer';
UPDATE role_generations SET role='final_verifier' WHERE role='final_reviewer';
UPDATE capabilities SET role='final_verifier' WHERE role='final_reviewer';
UPDATE launch_permits SET role='final_verifier' WHERE role='final_reviewer';
UPDATE switch_intents SET role='final_verifier' WHERE role='final_reviewer';
UPDATE permission_requests SET role='final_verifier' WHERE role='final_reviewer';
UPDATE permission_rules SET role='final_verifier' WHERE role='final_reviewer';

CREATE INDEX trip_setup_state ON trip_setup_operations(state, updated_at);
CREATE INDEX trip_journal_state ON trip_apply_journal(setup_operation_id, state);
CREATE INDEX trip_plan_attempt ON trip_structured_plans(attempt_id, created_at);
CREATE INDEX trip_lane_attempt_state ON implementation_lanes(attempt_id, state);
CREATE INDEX trip_integration_attempt_state ON trip_integration_requests(attempt_id, state);
CREATE INDEX trip_checks_project ON trip_verification_checks(project_id, config_revision_id);
CREATE INDEX trip_selected_attempt ON trip_selected_checks(attempt_id, revision);
