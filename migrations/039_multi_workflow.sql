-- Single agent workflow records, app-wide provider validation and recorded
-- history views. Absent selection/binding rows mean the legacy TRIP branch;
-- no historical attempt receives fabricated Single agent provenance.
--
-- The sessions rebuild is the one change to an existing table. The runner
-- turns foreign-key enforcement off before this transaction so dropping the
-- old table cannot cascade or retarget children, then checks every foreign
-- key before commit and restores enforcement on success and failure.

CREATE TABLE workflow_definitions (
    id TEXT PRIMARY KEY,
    family TEXT NOT NULL CHECK(family IN ('trip','single_agent')),
    version TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    definition_text TEXT NOT NULL,
    evaluator_id TEXT NOT NULL,
    evaluator_revision INTEGER NOT NULL CHECK(evaluator_revision > 0),
    result_schema_revision INTEGER NOT NULL CHECK(result_schema_revision > 0),
    check_requirement TEXT NOT NULL CHECK(check_requirement IN ('none','selected')),
    provenance TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(family, version)
);

CREATE TABLE execution_config_revisions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    config_json TEXT NOT NULL CHECK(json_valid(config_json)),
    content_hash TEXT NOT NULL,
    approved_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(project_id, revision)
);

CREATE TABLE execution_verification_checks (
    id TEXT PRIMARY KEY,
    execution_config_revision_id TEXT NOT NULL REFERENCES execution_config_revisions(id),
    check_key TEXT NOT NULL,
    command_kind TEXT NOT NULL CHECK(command_kind IN ('exact_argv','exact_shell')),
    executable TEXT,
    arguments_json TEXT,
    shell_command TEXT,
    cwd TEXT NOT NULL,
    timeout_seconds INTEGER NOT NULL CHECK(timeout_seconds > 0),
    acceptance_rows_json TEXT NOT NULL,
    relevant_inputs_json TEXT NOT NULL,
    invalidation_json TEXT NOT NULL,
    original_text TEXT NOT NULL,
    UNIQUE(execution_config_revision_id, check_key)
);

CREATE TABLE agent_profiles (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    display_name TEXT NOT NULL,
    current_revision_id TEXT REFERENCES agent_profile_revisions(id),
    version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE agent_profile_revisions (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL REFERENCES agent_profiles(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    provider TEXT NOT NULL CHECK(provider IN ('codex','claude')),
    adapter_id TEXT NOT NULL,
    adapter_hash TEXT NOT NULL,
    model TEXT NOT NULL,
    effort TEXT NOT NULL,
    instructions_text TEXT NOT NULL,
    instructions_hash TEXT NOT NULL,
    policy_id TEXT NOT NULL,
    policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
    policy_hash TEXT NOT NULL,
    authority TEXT NOT NULL CHECK(authority IN ('workspace_write','read_only')),
    session_contract TEXT NOT NULL CHECK(session_contract IN ('retained','fresh')),
    profile_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(profile_id, revision)
);

CREATE TABLE profile_admissions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    profile_revision_id TEXT NOT NULL REFERENCES agent_profile_revisions(id),
    execution_config_revision_id TEXT NOT NULL REFERENCES execution_config_revisions(id),
    policy_hash TEXT NOT NULL,
    adapter_hash TEXT NOT NULL,
    session_contract TEXT NOT NULL CHECK(session_contract IN ('retained','fresh')),
    capability_key TEXT,
    proof_hash TEXT,
    prepared_scope_hash TEXT NOT NULL,
    -- The disposable task whose run proves this pending admission natively.
    probe_task_id TEXT REFERENCES tasks(id),
    state TEXT NOT NULL CHECK(state IN ('pending','activated','invalidated')),
    state_reason TEXT,
    activated_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK(state != 'activated' OR (capability_key IS NOT NULL AND proof_hash IS NOT NULL
        AND activated_at IS NOT NULL))
);

CREATE UNIQUE INDEX profile_admissions_one_activated
ON profile_admissions(profile_revision_id, execution_config_revision_id)
WHERE state = 'activated';

CREATE TABLE project_workflow_defaults (
    project_id TEXT PRIMARY KEY REFERENCES projects(id),
    family TEXT NOT NULL CHECK(family IN ('trip','single_agent')),
    definition_id TEXT REFERENCES workflow_definitions(id),
    version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
    updated_at TEXT NOT NULL,
    CHECK((family = 'trip') = (definition_id IS NULL))
);

CREATE TABLE task_workflow_selections (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id),
    family TEXT NOT NULL CHECK(family IN ('trip','single_agent')),
    definition_id TEXT REFERENCES workflow_definitions(id),
    execution_config_revision_id TEXT REFERENCES execution_config_revisions(id),
    profile_revision_id TEXT REFERENCES agent_profile_revisions(id),
    check_ids_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(check_ids_json)),
    selection_revision INTEGER NOT NULL CHECK(selection_revision > 0),
    selection_hash TEXT NOT NULL,
    admission_state TEXT NOT NULL CHECK(admission_state IN ('draft','admitted','invalidated')),
    admitted_task_version INTEGER,
    admission_reason TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK((family = 'trip') = (definition_id IS NULL)),
    CHECK(family = 'single_agent' OR (execution_config_revision_id IS NULL
        AND profile_revision_id IS NULL AND check_ids_json = '[]')),
    CHECK(admission_state != 'admitted' OR admitted_task_version IS NOT NULL)
);

CREATE TABLE workflow_run_bindings (
    attempt_id TEXT PRIMARY KEY REFERENCES attempts(id),
    family TEXT NOT NULL CHECK(family IN ('trip','single_agent')),
    definition_id TEXT REFERENCES workflow_definitions(id),
    definition_hash TEXT,
    definition_state TEXT NOT NULL CHECK(definition_state IN ('available','unavailable')),
    evaluator_revision INTEGER NOT NULL CHECK(evaluator_revision > 0),
    execution_config_revision_id TEXT REFERENCES execution_config_revisions(id),
    execution_config_hash TEXT,
    selection_hash TEXT NOT NULL,
    scope_hash TEXT NOT NULL,
    parent_attempt_id TEXT REFERENCES attempts(id),
    source_snapshot_id TEXT REFERENCES snapshots(id),
    created_at TEXT NOT NULL,
    CHECK(definition_state = 'unavailable' OR (definition_id IS NOT NULL AND definition_hash IS NOT NULL)),
    CHECK((parent_attempt_id IS NULL) = (source_snapshot_id IS NULL))
);

CREATE TABLE workflow_run_profiles (
    attempt_id TEXT NOT NULL REFERENCES workflow_run_bindings(attempt_id),
    slot_key TEXT NOT NULL CHECK(slot_key = 'single_agent'),
    profile_revision_id TEXT NOT NULL REFERENCES agent_profile_revisions(id),
    admission_id TEXT NOT NULL REFERENCES profile_admissions(id),
    policy_hash TEXT NOT NULL,
    session_contract TEXT NOT NULL CHECK(session_contract IN ('retained','fresh')),
    profile_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(attempt_id, slot_key)
);

CREATE TABLE execution_bindings (
    role_generation_id TEXT PRIMARY KEY REFERENCES role_generations(id),
    attempt_id TEXT NOT NULL,
    slot_key TEXT NOT NULL,
    actor TEXT NOT NULL CHECK(actor = 'single_agent'),
    policy_hash TEXT NOT NULL,
    session_contract TEXT NOT NULL CHECK(session_contract IN ('retained','fresh')),
    channel_operations_json TEXT NOT NULL CHECK(json_valid(channel_operations_json)),
    created_at TEXT NOT NULL,
    FOREIGN KEY(attempt_id, slot_key) REFERENCES workflow_run_profiles(attempt_id, slot_key)
);

CREATE TABLE workflow_check_selections (
    attempt_id TEXT NOT NULL REFERENCES workflow_run_bindings(attempt_id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    check_id TEXT NOT NULL REFERENCES execution_verification_checks(id),
    selected_by TEXT NOT NULL CHECK(selected_by = 'human'),
    created_at TEXT NOT NULL,
    PRIMARY KEY(attempt_id, revision, check_id)
);

CREATE TABLE workflow_check_authorizations (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES workflow_run_bindings(attempt_id),
    check_id TEXT NOT NULL REFERENCES execution_verification_checks(id),
    selected_revision INTEGER NOT NULL,
    exact_command_hash TEXT NOT NULL,
    scope_hash TEXT NOT NULL,
    decision TEXT NOT NULL CHECK(decision IN ('authorized','denied')),
    lifetime TEXT NOT NULL CHECK(lifetime IN ('once','run')),
    created_at TEXT NOT NULL,
    consumed_at TEXT
);

CREATE TABLE workflow_result_bindings (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES workflow_run_bindings(attempt_id),
    role_result_id TEXT NOT NULL UNIQUE REFERENCES role_results(id),
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    transcript_epoch TEXT NOT NULL,
    criteria_hash TEXT NOT NULL,
    evidence_hash TEXT NOT NULL,
    profile_binding_hash TEXT NOT NULL,
    selected_check_revision INTEGER NOT NULL CHECK(selected_check_revision >= 0),
    snapshot_id TEXT REFERENCES snapshots(id),
    candidate_hash TEXT,
    state TEXT NOT NULL CHECK(state IN ('pending_quiescence','awaiting_checks','awaiting_human',
        'accepted','changes_requested','superseded')),
    state_reason TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK(state = 'pending_quiescence' OR state = 'superseded'
        OR (snapshot_id IS NOT NULL AND candidate_hash IS NOT NULL))
);

CREATE UNIQUE INDEX workflow_result_bindings_one_current
ON workflow_result_bindings(attempt_id)
WHERE state IN ('pending_quiescence','awaiting_checks','awaiting_human');

CREATE TABLE workflow_result_acceptances (
    id TEXT PRIMARY KEY,
    result_binding_id TEXT NOT NULL UNIQUE REFERENCES workflow_result_bindings(id),
    operation_id TEXT NOT NULL UNIQUE,
    request_hash TEXT NOT NULL,
    decision TEXT NOT NULL CHECK(decision IN ('accepted','changes_requested')),
    expected_task_version INTEGER NOT NULL,
    candidate_hash TEXT NOT NULL,
    definition_hash TEXT NOT NULL,
    profile_binding_hash TEXT NOT NULL,
    criteria_hash TEXT NOT NULL,
    check_receipts_json TEXT NOT NULL CHECK(json_valid(check_receipts_json)),
    feedback TEXT,
    child_attempt_id TEXT REFERENCES attempts(id),
    created_at TEXT NOT NULL,
    CHECK(decision = 'accepted' OR (length(trim(COALESCE(feedback,''))) > 0))
);

CREATE TABLE provider_validation_batches (
    id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL UNIQUE,
    request_hash TEXT NOT NULL,
    preview_hash TEXT NOT NULL,
    selection_json TEXT NOT NULL CHECK(json_valid(selection_json)),
    max_fresh_calls INTEGER NOT NULL CHECK(max_fresh_calls >= 0),
    native_calls_authorized INTEGER NOT NULL CHECK(native_calls_authorized IN (0,1)),
    state TEXT NOT NULL CHECK(state IN ('running','cancel_requested','finished','cancelled')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    finished_at TEXT
);

CREATE TABLE provider_validation_invocations (
    id TEXT PRIMARY KEY,
    batch_id TEXT NOT NULL REFERENCES provider_validation_batches(id),
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    provider TEXT NOT NULL CHECK(provider IN ('codex','claude')),
    model TEXT NOT NULL,
    effort TEXT NOT NULL,
    evidence_key TEXT NOT NULL,
    scope_root TEXT,
    nonce_hash TEXT,
    state TEXT NOT NULL CHECK(state IN ('pending','reserved','running','succeeded','failed',
        'delivery_unknown','cancel_requested','cancelled','skipped')),
    failure_reason TEXT,
    result_hash TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    finished_at TEXT,
    UNIQUE(batch_id, ordinal)
);

CREATE TABLE provider_observations (
    id TEXT PRIMARY KEY,
    batch_id TEXT REFERENCES provider_validation_batches(id),
    invocation_id TEXT REFERENCES provider_validation_invocations(id),
    provider TEXT NOT NULL CHECK(provider IN ('codex','claude')),
    kind TEXT NOT NULL CHECK(kind IN ('installation','authentication','model_metadata','model_access')),
    evidence_key TEXT NOT NULL,
    identity_key TEXT NOT NULL,
    executable_path TEXT,
    executable_fingerprint_hash TEXT,
    executable_version TEXT,
    adapter_id TEXT NOT NULL,
    adapter_mode TEXT NOT NULL,
    account_identity TEXT,
    account_identity_state TEXT NOT NULL CHECK(account_identity_state IN ('validated','unavailable')),
    config_identity_hash TEXT NOT NULL,
    model TEXT,
    effort TEXT,
    outcome TEXT NOT NULL CHECK(outcome IN ('succeeded','failed','unknown','unqualified')),
    reason TEXT NOT NULL,
    detail_json TEXT NOT NULL CHECK(json_valid(detail_json)),
    observed_at TEXT NOT NULL,
    expires_at TEXT,
    invalidated_at TEXT,
    invalidation_reason TEXT,
    CHECK((account_identity_state = 'validated') = (account_identity IS NOT NULL)),
    CHECK(kind != 'model_access' OR (invocation_id IS NOT NULL AND model IS NOT NULL
        AND expires_at IS NOT NULL))
);

CREATE INDEX provider_observations_current
ON provider_observations(provider, kind, evidence_key, observed_at)
WHERE invalidated_at IS NULL;

CREATE TABLE sessions_rebuilt (
    id TEXT PRIMARY KEY,
    role_generation_id TEXT REFERENCES role_generations(id),
    provider TEXT NOT NULL,
    validation_cell TEXT,
    status TEXT NOT NULL,
    launch_config_json TEXT NOT NULL,
    executable_version TEXT NOT NULL,
    native_session_id TEXT,
    native_identity_source TEXT,
    process_identity_json TEXT,
    transcript_epoch TEXT NOT NULL,
    transcript_last_sequence INTEGER NOT NULL DEFAULT 0,
    capture_state TEXT NOT NULL DEFAULT 'capturing',
    capture_error TEXT,
    hook_trust_state TEXT NOT NULL DEFAULT 'pending_observation',
    exit_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    native_identity_verified_at TEXT,
    readiness_state TEXT NOT NULL DEFAULT 'unknown',
    invocation_input_json TEXT,
    workflow_version TEXT NOT NULL DEFAULT 'trip-v1',
    workflow_hash TEXT NOT NULL DEFAULT '',
    prompt_hash TEXT NOT NULL DEFAULT '',
    launch_state TEXT NOT NULL DEFAULT 'reserved',
    launch_error TEXT,
    resume_count INTEGER NOT NULL DEFAULT 0,
    capability_key TEXT,
    capability_identity_json TEXT,
    recovery_root_pid INTEGER,
    recovery_process_group_id INTEGER,
    desired_running INTEGER NOT NULL DEFAULT 0,
    launch_boot_identity TEXT,
    recovery_anchor_json TEXT,
    initial_hook_event_boundary_rowid INTEGER NOT NULL DEFAULT 0,
    lane_id TEXT NOT NULL DEFAULT 'default',
    setup_permit_id TEXT,
    interrupt_requested_at TEXT,
    provider_validation_invocation_id TEXT REFERENCES provider_validation_invocations(id),
    CHECK((role_generation_id IS NULL) <> (provider_validation_invocation_id IS NULL))
);

INSERT INTO sessions_rebuilt(
    id,role_generation_id,provider,validation_cell,status,launch_config_json,executable_version,
    native_session_id,native_identity_source,process_identity_json,transcript_epoch,
    transcript_last_sequence,capture_state,capture_error,hook_trust_state,exit_json,created_at,
    updated_at,native_identity_verified_at,readiness_state,invocation_input_json,workflow_version,
    workflow_hash,prompt_hash,launch_state,launch_error,resume_count,capability_key,
    capability_identity_json,recovery_root_pid,recovery_process_group_id,desired_running,
    launch_boot_identity,recovery_anchor_json,initial_hook_event_boundary_rowid,lane_id,
    setup_permit_id,interrupt_requested_at,provider_validation_invocation_id
)
SELECT
    id,role_generation_id,provider,validation_cell,status,launch_config_json,executable_version,
    native_session_id,native_identity_source,process_identity_json,transcript_epoch,
    transcript_last_sequence,capture_state,capture_error,hook_trust_state,exit_json,created_at,
    updated_at,native_identity_verified_at,readiness_state,invocation_input_json,workflow_version,
    workflow_hash,prompt_hash,launch_state,launch_error,resume_count,capability_key,
    capability_identity_json,recovery_root_pid,recovery_process_group_id,desired_running,
    launch_boot_identity,recovery_anchor_json,initial_hook_event_boundary_rowid,lane_id,
    setup_permit_id,interrupt_requested_at,NULL
FROM sessions ORDER BY rowid;

DROP TABLE sessions;
ALTER TABLE sessions_rebuilt RENAME TO sessions;

CREATE INDEX sessions_launch_state_idx
ON sessions(status, launch_state);

CREATE INDEX sessions_interrupt_requested_deadline
ON sessions(status, interrupt_requested_at)
WHERE status = 'interrupt_requested' AND interrupt_requested_at IS NOT NULL;

CREATE UNIQUE INDEX sessions_one_per_validation_invocation
ON sessions(provider_validation_invocation_id)
WHERE provider_validation_invocation_id IS NOT NULL;

CREATE TRIGGER state_revision_sessions_insert AFTER INSERT ON sessions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_sessions_update AFTER UPDATE ON sessions
WHEN OLD.id IS NOT NEW.id OR OLD.role_generation_id IS NOT NEW.role_generation_id
  OR OLD.provider IS NOT NEW.provider OR OLD.validation_cell IS NOT NEW.validation_cell
  OR OLD.status IS NOT NEW.status OR OLD.launch_config_json IS NOT NEW.launch_config_json
  OR OLD.executable_version IS NOT NEW.executable_version
  OR OLD.native_session_id IS NOT NEW.native_session_id
  OR OLD.native_identity_source IS NOT NEW.native_identity_source
  OR OLD.process_identity_json IS NOT NEW.process_identity_json
  OR OLD.transcript_epoch IS NOT NEW.transcript_epoch
  OR OLD.capture_state IS NOT NEW.capture_state OR OLD.capture_error IS NOT NEW.capture_error
  OR OLD.hook_trust_state IS NOT NEW.hook_trust_state OR OLD.exit_json IS NOT NEW.exit_json
  OR OLD.created_at IS NOT NEW.created_at
  OR OLD.native_identity_verified_at IS NOT NEW.native_identity_verified_at
  OR OLD.readiness_state IS NOT NEW.readiness_state
  OR OLD.invocation_input_json IS NOT NEW.invocation_input_json
  OR OLD.workflow_version IS NOT NEW.workflow_version
  OR OLD.workflow_hash IS NOT NEW.workflow_hash OR OLD.prompt_hash IS NOT NEW.prompt_hash
  OR OLD.launch_state IS NOT NEW.launch_state OR OLD.launch_error IS NOT NEW.launch_error
  OR OLD.resume_count IS NOT NEW.resume_count OR OLD.capability_key IS NOT NEW.capability_key
  OR OLD.capability_identity_json IS NOT NEW.capability_identity_json
  OR OLD.recovery_root_pid IS NOT NEW.recovery_root_pid
  OR OLD.recovery_process_group_id IS NOT NEW.recovery_process_group_id
  OR OLD.desired_running IS NOT NEW.desired_running
  OR OLD.launch_boot_identity IS NOT NEW.launch_boot_identity
  OR OLD.recovery_anchor_json IS NOT NEW.recovery_anchor_json
  OR OLD.initial_hook_event_boundary_rowid IS NOT NEW.initial_hook_event_boundary_rowid
  OR OLD.lane_id IS NOT NEW.lane_id OR OLD.setup_permit_id IS NOT NEW.setup_permit_id
  OR OLD.interrupt_requested_at IS NOT NEW.interrupt_requested_at
  OR OLD.provider_validation_invocation_id IS NOT NEW.provider_validation_invocation_id
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_sessions_delete AFTER DELETE ON sessions
BEGIN UPDATE state_revision SET revision = revision + 1; END;

-- Presentation only: no lease, desired input state or provider route lives here.
CREATE TABLE history_pane_views (
    id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL UNIQUE,
    request_hash TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role_generation_id TEXT REFERENCES role_generations(id),
    transcript_epoch TEXT NOT NULL,
    high_water_sequence INTEGER NOT NULL CHECK(high_water_sequence >= 0),
    export_hash TEXT,
    service_boot_id TEXT NOT NULL,
    workspace_id TEXT,
    surface_id TEXT,
    state TEXT NOT NULL CHECK(state IN ('reserved','opened','unknown','failed','closed')),
    failure_reason TEXT,
    retry_allowed INTEGER NOT NULL DEFAULT 0 CHECK(retry_allowed IN (0,1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK(retry_allowed = 0 OR state = 'failed')
);

CREATE INDEX history_pane_views_session ON history_pane_views(session_id, transcript_epoch, created_at);
CREATE INDEX task_workflow_selections_admission ON task_workflow_selections(admission_state, family);
CREATE INDEX workflow_result_bindings_attempt ON workflow_result_bindings(attempt_id, created_at);
CREATE INDEX provider_validation_invocations_state ON provider_validation_invocations(state, batch_id);

CREATE TRIGGER state_revision_workflow_definitions_insert AFTER INSERT ON workflow_definitions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_definitions_update AFTER UPDATE ON workflow_definitions
WHEN OLD.id IS NOT NEW.id OR OLD.family IS NOT NEW.family OR OLD.version IS NOT NEW.version
  OR OLD.content_hash IS NOT NEW.content_hash OR OLD.definition_text IS NOT NEW.definition_text
  OR OLD.evaluator_id IS NOT NEW.evaluator_id OR OLD.evaluator_revision IS NOT NEW.evaluator_revision
  OR OLD.result_schema_revision IS NOT NEW.result_schema_revision
  OR OLD.check_requirement IS NOT NEW.check_requirement OR OLD.provenance IS NOT NEW.provenance
  OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_definitions_delete AFTER DELETE ON workflow_definitions
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_execution_config_revisions_insert AFTER INSERT ON execution_config_revisions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_execution_config_revisions_update AFTER UPDATE ON execution_config_revisions
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id OR OLD.revision IS NOT NEW.revision
  OR OLD.config_json IS NOT NEW.config_json OR OLD.content_hash IS NOT NEW.content_hash
  OR OLD.approved_at IS NOT NEW.approved_at OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_execution_config_revisions_delete AFTER DELETE ON execution_config_revisions
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_execution_verification_checks_insert AFTER INSERT ON execution_verification_checks
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_execution_verification_checks_update AFTER UPDATE ON execution_verification_checks
WHEN OLD.id IS NOT NEW.id OR OLD.execution_config_revision_id IS NOT NEW.execution_config_revision_id
  OR OLD.check_key IS NOT NEW.check_key OR OLD.command_kind IS NOT NEW.command_kind
  OR OLD.executable IS NOT NEW.executable OR OLD.arguments_json IS NOT NEW.arguments_json
  OR OLD.shell_command IS NOT NEW.shell_command OR OLD.cwd IS NOT NEW.cwd
  OR OLD.timeout_seconds IS NOT NEW.timeout_seconds
  OR OLD.acceptance_rows_json IS NOT NEW.acceptance_rows_json
  OR OLD.relevant_inputs_json IS NOT NEW.relevant_inputs_json
  OR OLD.invalidation_json IS NOT NEW.invalidation_json OR OLD.original_text IS NOT NEW.original_text
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_execution_verification_checks_delete AFTER DELETE ON execution_verification_checks
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_agent_profiles_insert AFTER INSERT ON agent_profiles
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_agent_profiles_update AFTER UPDATE ON agent_profiles
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id
  OR OLD.display_name IS NOT NEW.display_name OR OLD.current_revision_id IS NOT NEW.current_revision_id
  OR OLD.version IS NOT NEW.version OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_agent_profiles_delete AFTER DELETE ON agent_profiles
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_agent_profile_revisions_insert AFTER INSERT ON agent_profile_revisions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_agent_profile_revisions_update AFTER UPDATE ON agent_profile_revisions
WHEN OLD.id IS NOT NEW.id OR OLD.profile_id IS NOT NEW.profile_id OR OLD.revision IS NOT NEW.revision
  OR OLD.provider IS NOT NEW.provider OR OLD.adapter_id IS NOT NEW.adapter_id
  OR OLD.adapter_hash IS NOT NEW.adapter_hash OR OLD.model IS NOT NEW.model
  OR OLD.effort IS NOT NEW.effort OR OLD.instructions_text IS NOT NEW.instructions_text
  OR OLD.instructions_hash IS NOT NEW.instructions_hash OR OLD.policy_id IS NOT NEW.policy_id
  OR OLD.policy_revision IS NOT NEW.policy_revision OR OLD.policy_hash IS NOT NEW.policy_hash
  OR OLD.authority IS NOT NEW.authority OR OLD.session_contract IS NOT NEW.session_contract
  OR OLD.profile_hash IS NOT NEW.profile_hash OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_agent_profile_revisions_delete AFTER DELETE ON agent_profile_revisions
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_profile_admissions_insert AFTER INSERT ON profile_admissions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_profile_admissions_update AFTER UPDATE ON profile_admissions
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id
  OR OLD.profile_revision_id IS NOT NEW.profile_revision_id
  OR OLD.execution_config_revision_id IS NOT NEW.execution_config_revision_id
  OR OLD.policy_hash IS NOT NEW.policy_hash OR OLD.adapter_hash IS NOT NEW.adapter_hash
  OR OLD.session_contract IS NOT NEW.session_contract OR OLD.capability_key IS NOT NEW.capability_key
  OR OLD.proof_hash IS NOT NEW.proof_hash OR OLD.prepared_scope_hash IS NOT NEW.prepared_scope_hash
  OR OLD.probe_task_id IS NOT NEW.probe_task_id
  OR OLD.state IS NOT NEW.state OR OLD.state_reason IS NOT NEW.state_reason
  OR OLD.activated_at IS NOT NEW.activated_at OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_profile_admissions_delete AFTER DELETE ON profile_admissions
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_project_workflow_defaults_insert AFTER INSERT ON project_workflow_defaults
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_project_workflow_defaults_update AFTER UPDATE ON project_workflow_defaults
WHEN OLD.project_id IS NOT NEW.project_id OR OLD.family IS NOT NEW.family
  OR OLD.definition_id IS NOT NEW.definition_id OR OLD.version IS NOT NEW.version
  OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_project_workflow_defaults_delete AFTER DELETE ON project_workflow_defaults
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_task_workflow_selections_insert AFTER INSERT ON task_workflow_selections
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_task_workflow_selections_update AFTER UPDATE ON task_workflow_selections
WHEN OLD.task_id IS NOT NEW.task_id OR OLD.family IS NOT NEW.family
  OR OLD.definition_id IS NOT NEW.definition_id
  OR OLD.execution_config_revision_id IS NOT NEW.execution_config_revision_id
  OR OLD.profile_revision_id IS NOT NEW.profile_revision_id
  OR OLD.check_ids_json IS NOT NEW.check_ids_json
  OR OLD.selection_revision IS NOT NEW.selection_revision
  OR OLD.selection_hash IS NOT NEW.selection_hash OR OLD.admission_state IS NOT NEW.admission_state
  OR OLD.admitted_task_version IS NOT NEW.admitted_task_version
  OR OLD.admission_reason IS NOT NEW.admission_reason OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_task_workflow_selections_delete AFTER DELETE ON task_workflow_selections
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_workflow_run_bindings_insert AFTER INSERT ON workflow_run_bindings
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_run_bindings_update AFTER UPDATE ON workflow_run_bindings
WHEN OLD.attempt_id IS NOT NEW.attempt_id OR OLD.family IS NOT NEW.family
  OR OLD.definition_id IS NOT NEW.definition_id OR OLD.definition_hash IS NOT NEW.definition_hash
  OR OLD.definition_state IS NOT NEW.definition_state
  OR OLD.evaluator_revision IS NOT NEW.evaluator_revision
  OR OLD.execution_config_revision_id IS NOT NEW.execution_config_revision_id
  OR OLD.execution_config_hash IS NOT NEW.execution_config_hash
  OR OLD.selection_hash IS NOT NEW.selection_hash OR OLD.scope_hash IS NOT NEW.scope_hash
  OR OLD.parent_attempt_id IS NOT NEW.parent_attempt_id
  OR OLD.source_snapshot_id IS NOT NEW.source_snapshot_id OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_run_bindings_delete AFTER DELETE ON workflow_run_bindings
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_workflow_run_profiles_insert AFTER INSERT ON workflow_run_profiles
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_run_profiles_update AFTER UPDATE ON workflow_run_profiles
WHEN OLD.attempt_id IS NOT NEW.attempt_id OR OLD.slot_key IS NOT NEW.slot_key
  OR OLD.profile_revision_id IS NOT NEW.profile_revision_id
  OR OLD.admission_id IS NOT NEW.admission_id OR OLD.policy_hash IS NOT NEW.policy_hash
  OR OLD.session_contract IS NOT NEW.session_contract OR OLD.profile_hash IS NOT NEW.profile_hash
  OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_run_profiles_delete AFTER DELETE ON workflow_run_profiles
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_execution_bindings_insert AFTER INSERT ON execution_bindings
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_execution_bindings_update AFTER UPDATE ON execution_bindings
WHEN OLD.role_generation_id IS NOT NEW.role_generation_id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.slot_key IS NOT NEW.slot_key OR OLD.actor IS NOT NEW.actor
  OR OLD.policy_hash IS NOT NEW.policy_hash OR OLD.session_contract IS NOT NEW.session_contract
  OR OLD.channel_operations_json IS NOT NEW.channel_operations_json
  OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_execution_bindings_delete AFTER DELETE ON execution_bindings
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_workflow_check_selections_insert AFTER INSERT ON workflow_check_selections
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_check_selections_update AFTER UPDATE ON workflow_check_selections
WHEN OLD.attempt_id IS NOT NEW.attempt_id OR OLD.revision IS NOT NEW.revision
  OR OLD.check_id IS NOT NEW.check_id OR OLD.selected_by IS NOT NEW.selected_by
  OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_check_selections_delete AFTER DELETE ON workflow_check_selections
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_workflow_check_authorizations_insert AFTER INSERT ON workflow_check_authorizations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_check_authorizations_update AFTER UPDATE ON workflow_check_authorizations
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id OR OLD.check_id IS NOT NEW.check_id
  OR OLD.selected_revision IS NOT NEW.selected_revision
  OR OLD.exact_command_hash IS NOT NEW.exact_command_hash OR OLD.scope_hash IS NOT NEW.scope_hash
  OR OLD.decision IS NOT NEW.decision OR OLD.lifetime IS NOT NEW.lifetime
  OR OLD.created_at IS NOT NEW.created_at OR OLD.consumed_at IS NOT NEW.consumed_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_check_authorizations_delete AFTER DELETE ON workflow_check_authorizations
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_workflow_result_bindings_insert AFTER INSERT ON workflow_result_bindings
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_result_bindings_update AFTER UPDATE ON workflow_result_bindings
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.role_result_id IS NOT NEW.role_result_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id OR OLD.session_id IS NOT NEW.session_id
  OR OLD.transcript_epoch IS NOT NEW.transcript_epoch OR OLD.criteria_hash IS NOT NEW.criteria_hash
  OR OLD.evidence_hash IS NOT NEW.evidence_hash
  OR OLD.profile_binding_hash IS NOT NEW.profile_binding_hash
  OR OLD.selected_check_revision IS NOT NEW.selected_check_revision
  OR OLD.snapshot_id IS NOT NEW.snapshot_id OR OLD.candidate_hash IS NOT NEW.candidate_hash
  OR OLD.state IS NOT NEW.state OR OLD.state_reason IS NOT NEW.state_reason
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_result_bindings_delete AFTER DELETE ON workflow_result_bindings
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_workflow_result_acceptances_insert AFTER INSERT ON workflow_result_acceptances
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_result_acceptances_update AFTER UPDATE ON workflow_result_acceptances
WHEN OLD.id IS NOT NEW.id OR OLD.result_binding_id IS NOT NEW.result_binding_id
  OR OLD.operation_id IS NOT NEW.operation_id OR OLD.request_hash IS NOT NEW.request_hash
  OR OLD.decision IS NOT NEW.decision OR OLD.expected_task_version IS NOT NEW.expected_task_version
  OR OLD.candidate_hash IS NOT NEW.candidate_hash OR OLD.definition_hash IS NOT NEW.definition_hash
  OR OLD.profile_binding_hash IS NOT NEW.profile_binding_hash
  OR OLD.criteria_hash IS NOT NEW.criteria_hash
  OR OLD.check_receipts_json IS NOT NEW.check_receipts_json OR OLD.feedback IS NOT NEW.feedback
  OR OLD.child_attempt_id IS NOT NEW.child_attempt_id OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workflow_result_acceptances_delete AFTER DELETE ON workflow_result_acceptances
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_provider_validation_batches_insert AFTER INSERT ON provider_validation_batches
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_provider_validation_batches_update AFTER UPDATE ON provider_validation_batches
WHEN OLD.id IS NOT NEW.id OR OLD.operation_id IS NOT NEW.operation_id
  OR OLD.request_hash IS NOT NEW.request_hash OR OLD.preview_hash IS NOT NEW.preview_hash
  OR OLD.selection_json IS NOT NEW.selection_json OR OLD.max_fresh_calls IS NOT NEW.max_fresh_calls
  OR OLD.native_calls_authorized IS NOT NEW.native_calls_authorized OR OLD.state IS NOT NEW.state
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
  OR OLD.finished_at IS NOT NEW.finished_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_provider_validation_batches_delete AFTER DELETE ON provider_validation_batches
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_provider_validation_invocations_insert AFTER INSERT ON provider_validation_invocations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_provider_validation_invocations_update AFTER UPDATE ON provider_validation_invocations
WHEN OLD.id IS NOT NEW.id OR OLD.batch_id IS NOT NEW.batch_id OR OLD.ordinal IS NOT NEW.ordinal
  OR OLD.provider IS NOT NEW.provider OR OLD.model IS NOT NEW.model OR OLD.effort IS NOT NEW.effort
  OR OLD.evidence_key IS NOT NEW.evidence_key OR OLD.scope_root IS NOT NEW.scope_root
  OR OLD.nonce_hash IS NOT NEW.nonce_hash OR OLD.state IS NOT NEW.state
  OR OLD.failure_reason IS NOT NEW.failure_reason OR OLD.result_hash IS NOT NEW.result_hash
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
  OR OLD.finished_at IS NOT NEW.finished_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_provider_validation_invocations_delete AFTER DELETE ON provider_validation_invocations
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_provider_observations_insert AFTER INSERT ON provider_observations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_provider_observations_update AFTER UPDATE ON provider_observations
WHEN OLD.id IS NOT NEW.id OR OLD.batch_id IS NOT NEW.batch_id
  OR OLD.invocation_id IS NOT NEW.invocation_id OR OLD.provider IS NOT NEW.provider
  OR OLD.kind IS NOT NEW.kind OR OLD.evidence_key IS NOT NEW.evidence_key
  OR OLD.identity_key IS NOT NEW.identity_key
  OR OLD.executable_path IS NOT NEW.executable_path
  OR OLD.executable_fingerprint_hash IS NOT NEW.executable_fingerprint_hash
  OR OLD.executable_version IS NOT NEW.executable_version OR OLD.adapter_id IS NOT NEW.adapter_id
  OR OLD.adapter_mode IS NOT NEW.adapter_mode OR OLD.account_identity IS NOT NEW.account_identity
  OR OLD.account_identity_state IS NOT NEW.account_identity_state
  OR OLD.config_identity_hash IS NOT NEW.config_identity_hash OR OLD.model IS NOT NEW.model
  OR OLD.effort IS NOT NEW.effort OR OLD.outcome IS NOT NEW.outcome OR OLD.reason IS NOT NEW.reason
  OR OLD.detail_json IS NOT NEW.detail_json OR OLD.observed_at IS NOT NEW.observed_at
  OR OLD.expires_at IS NOT NEW.expires_at OR OLD.invalidated_at IS NOT NEW.invalidated_at
  OR OLD.invalidation_reason IS NOT NEW.invalidation_reason
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_provider_observations_delete AFTER DELETE ON provider_observations
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_history_pane_views_insert AFTER INSERT ON history_pane_views
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_history_pane_views_update AFTER UPDATE ON history_pane_views
WHEN OLD.id IS NOT NEW.id OR OLD.operation_id IS NOT NEW.operation_id
  OR OLD.request_hash IS NOT NEW.request_hash OR OLD.session_id IS NOT NEW.session_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id
  OR OLD.transcript_epoch IS NOT NEW.transcript_epoch
  OR OLD.high_water_sequence IS NOT NEW.high_water_sequence
  OR OLD.export_hash IS NOT NEW.export_hash OR OLD.service_boot_id IS NOT NEW.service_boot_id
  OR OLD.workspace_id IS NOT NEW.workspace_id OR OLD.surface_id IS NOT NEW.surface_id
  OR OLD.state IS NOT NEW.state OR OLD.failure_reason IS NOT NEW.failure_reason
  OR OLD.retry_allowed IS NOT NEW.retry_allowed OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_history_pane_views_delete AFTER DELETE ON history_pane_views
BEGIN UPDATE state_revision SET revision = revision + 1; END;
