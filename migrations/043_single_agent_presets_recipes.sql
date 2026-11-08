-- The migration runner disables foreign keys outside this transaction, then
-- checks all references before commit and restores enforcement on every path.
ALTER TABLE agent_profile_revisions ADD COLUMN purpose TEXT NOT NULL DEFAULT 'workspace_writer'
    CHECK(purpose IN ('workspace_writer','repository_reader','repository_reviewer'));
ALTER TABLE project_workflow_defaults ADD COLUMN preset TEXT NOT NULL DEFAULT 'single_agent'
    CHECK(preset IN ('single_agent','repository_research','build_and_review'));
ALTER TABLE task_workflow_selections ADD COLUMN preset TEXT NOT NULL DEFAULT 'single_agent'
    CHECK(preset IN ('single_agent','repository_research','build_and_review'));
ALTER TABLE task_workflow_selections ADD COLUMN reviewer_profile_revision_id TEXT REFERENCES agent_profile_revisions(id);
ALTER TABLE workflow_run_bindings ADD COLUMN research_snapshot_id TEXT REFERENCES snapshots(id);

CREATE TABLE workflow_run_profiles_rebuilt (
    attempt_id TEXT NOT NULL REFERENCES workflow_run_bindings(attempt_id),
    slot_key TEXT NOT NULL CHECK(slot_key IN ('single_agent','single_agent_reviewer')),
    profile_revision_id TEXT NOT NULL REFERENCES agent_profile_revisions(id),
    admission_id TEXT NOT NULL REFERENCES profile_admissions(id),
    policy_hash TEXT NOT NULL,
    session_contract TEXT NOT NULL CHECK(session_contract IN ('retained','fresh')),
    profile_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(attempt_id,slot_key),
    CHECK(slot_key != 'single_agent_reviewer' OR session_contract='fresh')
);
INSERT INTO workflow_run_profiles_rebuilt SELECT * FROM workflow_run_profiles;
DROP TABLE workflow_run_profiles;
ALTER TABLE workflow_run_profiles_rebuilt RENAME TO workflow_run_profiles;

CREATE TABLE execution_bindings_rebuilt (
    role_generation_id TEXT PRIMARY KEY REFERENCES role_generations(id),
    attempt_id TEXT NOT NULL,
    slot_key TEXT NOT NULL,
    actor TEXT NOT NULL CHECK(actor IN ('single_agent','single_agent_reviewer')),
    policy_hash TEXT NOT NULL,
    session_contract TEXT NOT NULL CHECK(session_contract IN ('retained','fresh')),
    channel_operations_json TEXT NOT NULL CHECK(json_valid(channel_operations_json)),
    created_at TEXT NOT NULL,
    FOREIGN KEY(attempt_id,slot_key) REFERENCES workflow_run_profiles(attempt_id,slot_key),
    CHECK(actor=slot_key)
);
INSERT INTO execution_bindings_rebuilt SELECT * FROM execution_bindings;
DROP TABLE execution_bindings;
ALTER TABLE execution_bindings_rebuilt RENAME TO execution_bindings;

CREATE TABLE workflow_result_bindings_rebuilt (
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
        'awaiting_review','review_running','review_changes_requested','review_blocked',
        'accepted','changes_requested','superseded')),
    state_reason TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK(state IN ('pending_quiescence','superseded') OR (snapshot_id IS NOT NULL AND candidate_hash IS NOT NULL))
);
INSERT INTO workflow_result_bindings_rebuilt SELECT * FROM workflow_result_bindings;
DROP TABLE workflow_result_bindings;
ALTER TABLE workflow_result_bindings_rebuilt RENAME TO workflow_result_bindings;
CREATE UNIQUE INDEX workflow_result_bindings_one_current ON workflow_result_bindings(attempt_id)
WHERE state IN ('pending_quiescence','awaiting_checks','awaiting_review','review_running','review_changes_requested','review_blocked','awaiting_human');
CREATE INDEX workflow_result_bindings_attempt ON workflow_result_bindings(attempt_id,created_at);

CREATE TABLE task_recipe_revisions_rebuilt (
    id TEXT PRIMARY KEY,
    recipe_id TEXT NOT NULL REFERENCES task_recipes(id),
    revision INTEGER NOT NULL CHECK(revision >= 1),
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    acceptance_criteria_json TEXT NOT NULL CHECK(json_valid(acceptance_criteria_json) AND json_type(acceptance_criteria_json)='array'),
    priority INTEGER NOT NULL,
    profile_revision_id TEXT REFERENCES project_profile_set_revisions(id),
    required_check_ids_json TEXT NOT NULL CHECK(json_valid(required_check_ids_json) AND json_type(required_check_ids_json)='array'),
    config_revision_id TEXT REFERENCES trip_config_revisions(id),
    configuration_hash TEXT NOT NULL,
    workflow_version TEXT NOT NULL,
    workflow_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    family TEXT NOT NULL DEFAULT 'trip' CHECK(family IN ('trip','single_agent')),
    preset TEXT NOT NULL DEFAULT 'single_agent' CHECK(preset IN ('single_agent','repository_research','build_and_review')),
    definition_id TEXT REFERENCES workflow_definitions(id),
    execution_config_revision_id TEXT REFERENCES execution_config_revisions(id),
    primary_profile_revision_id TEXT REFERENCES agent_profile_revisions(id),
    primary_profile_hash TEXT,
    reviewer_profile_revision_id TEXT REFERENCES agent_profile_revisions(id),
    reviewer_profile_hash TEXT,
    UNIQUE(recipe_id,revision),
    CHECK((family='trip' AND profile_revision_id IS NOT NULL AND config_revision_id IS NOT NULL
        AND definition_id IS NULL AND execution_config_revision_id IS NULL AND primary_profile_revision_id IS NULL
        AND primary_profile_hash IS NULL AND reviewer_profile_revision_id IS NULL AND reviewer_profile_hash IS NULL AND preset='single_agent')
      OR (family='single_agent' AND profile_revision_id IS NULL AND config_revision_id IS NULL
        AND definition_id IS NOT NULL AND execution_config_revision_id IS NOT NULL AND primary_profile_revision_id IS NOT NULL
        AND primary_profile_hash IS NOT NULL
        AND ((preset='build_and_review' AND reviewer_profile_revision_id IS NOT NULL AND reviewer_profile_hash IS NOT NULL)
          OR (preset!='build_and_review' AND reviewer_profile_revision_id IS NULL AND reviewer_profile_hash IS NULL)))),
    CHECK(preset!='repository_research' OR required_check_ids_json='[]')
);
INSERT INTO task_recipe_revisions_rebuilt(id,recipe_id,revision,title,description,acceptance_criteria_json,priority,profile_revision_id,required_check_ids_json,config_revision_id,configuration_hash,workflow_version,workflow_hash,created_at)
SELECT * FROM task_recipe_revisions;
DROP TABLE task_recipe_revisions;
ALTER TABLE task_recipe_revisions_rebuilt RENAME TO task_recipe_revisions;

CREATE TABLE task_recipe_bindings_rebuilt (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id),
    project_id TEXT NOT NULL REFERENCES projects(id),
    recipe_revision_id TEXT NOT NULL REFERENCES task_recipe_revisions(id),
    profile_revision_id TEXT REFERENCES project_profile_set_revisions(id),
    required_check_ids_json TEXT NOT NULL CHECK(json_valid(required_check_ids_json) AND json_type(required_check_ids_json)='array'),
    config_revision_id TEXT REFERENCES trip_config_revisions(id),
    configuration_hash TEXT NOT NULL,
    workflow_version TEXT NOT NULL,
    workflow_hash TEXT NOT NULL,
    schedule_id TEXT REFERENCES recipe_schedules(id),
    scheduled_for_utc TEXT,
    created_at TEXT NOT NULL,
    family TEXT NOT NULL DEFAULT 'trip' CHECK(family IN ('trip','single_agent')),
    preset TEXT NOT NULL DEFAULT 'single_agent' CHECK(preset IN ('single_agent','repository_research','build_and_review')),
    definition_id TEXT REFERENCES workflow_definitions(id),
    execution_config_revision_id TEXT REFERENCES execution_config_revisions(id),
    primary_profile_revision_id TEXT REFERENCES agent_profile_revisions(id),
    primary_profile_hash TEXT,
    reviewer_profile_revision_id TEXT REFERENCES agent_profile_revisions(id),
    reviewer_profile_hash TEXT,
    CHECK((schedule_id IS NULL) = (scheduled_for_utc IS NULL)),
    CHECK((family='trip' AND profile_revision_id IS NOT NULL AND config_revision_id IS NOT NULL
        AND definition_id IS NULL AND execution_config_revision_id IS NULL AND primary_profile_revision_id IS NULL
        AND primary_profile_hash IS NULL AND reviewer_profile_revision_id IS NULL AND reviewer_profile_hash IS NULL AND preset='single_agent')
      OR (family='single_agent' AND profile_revision_id IS NULL AND config_revision_id IS NULL
        AND definition_id IS NOT NULL AND execution_config_revision_id IS NOT NULL AND primary_profile_revision_id IS NOT NULL
        AND primary_profile_hash IS NOT NULL
        AND ((preset='build_and_review' AND reviewer_profile_revision_id IS NOT NULL AND reviewer_profile_hash IS NOT NULL)
          OR (preset!='build_and_review' AND reviewer_profile_revision_id IS NULL AND reviewer_profile_hash IS NULL)))),
    CHECK(preset!='repository_research' OR required_check_ids_json='[]')
);
INSERT INTO task_recipe_bindings_rebuilt(task_id,project_id,recipe_revision_id,profile_revision_id,required_check_ids_json,config_revision_id,configuration_hash,workflow_version,workflow_hash,schedule_id,scheduled_for_utc,created_at)
SELECT * FROM task_recipe_bindings;
DROP TABLE task_recipe_bindings;
ALTER TABLE task_recipe_bindings_rebuilt RENAME TO task_recipe_bindings;

CREATE TABLE single_agent_review_budgets (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id),
    root_attempt_id TEXT NOT NULL UNIQUE REFERENCES workflow_run_bindings(attempt_id),
    repair_attempt_id TEXT UNIQUE REFERENCES attempts(id),
    created_at TEXT NOT NULL
);
CREATE TABLE single_agent_review_deliveries (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES single_agent_review_budgets(task_id),
    root_attempt_id TEXT NOT NULL REFERENCES single_agent_review_budgets(root_attempt_id),
    attempt_id TEXT NOT NULL REFERENCES workflow_run_bindings(attempt_id),
    ordinal INTEGER NOT NULL CHECK(ordinal IN (1,2)),
    result_binding_id TEXT NOT NULL UNIQUE REFERENCES workflow_result_bindings(id),
    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),
    candidate_hash TEXT NOT NULL,
    definition_hash TEXT NOT NULL,
    profile_binding_hash TEXT NOT NULL,
    reviewer_profile_revision_id TEXT NOT NULL REFERENCES agent_profile_revisions(id),
    reviewer_profile_hash TEXT NOT NULL,
    selected_check_revision INTEGER NOT NULL,
    check_receipts_json TEXT NOT NULL CHECK(json_valid(check_receipts_json)),
    materialization_path TEXT NOT NULL UNIQUE,
    materialization_identity TEXT,
    role_generation_id TEXT UNIQUE REFERENCES role_generations(id),
    session_id TEXT UNIQUE REFERENCES sessions(id),
    credential_id TEXT UNIQUE REFERENCES role_credentials(id),
    transcript_epoch TEXT,
    state TEXT NOT NULL CHECK(state IN ('reserved','materializing','ready','launched','finished','abandoned')),
    reason TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(root_attempt_id,ordinal),
    CHECK(state NOT IN ('ready','launched','finished') OR materialization_identity IS NOT NULL),
    CHECK(state NOT IN ('launched','finished') OR (role_generation_id IS NOT NULL AND session_id IS NOT NULL AND credential_id IS NOT NULL AND transcript_epoch IS NOT NULL))
);
CREATE TABLE single_agent_review_results (
    delivery_id TEXT PRIMARY KEY REFERENCES single_agent_review_deliveries(id),
    role_result_id TEXT NOT NULL UNIQUE REFERENCES role_results(id),
    operation_id TEXT NOT NULL UNIQUE,
    request_hash TEXT NOT NULL,
    verdict TEXT NOT NULL CHECK(verdict IN ('approved','changes_requested','needs_rework')),
    summary TEXT NOT NULL,
    findings_json TEXT NOT NULL CHECK(json_valid(findings_json) AND json_type(findings_json)='array'),
    created_at TEXT NOT NULL
);
CREATE TABLE single_agent_repair_receipts (
    root_attempt_id TEXT PRIMARY KEY REFERENCES single_agent_review_budgets(root_attempt_id),
    delivery_id TEXT NOT NULL UNIQUE REFERENCES single_agent_review_results(delivery_id),
    result_binding_id TEXT NOT NULL UNIQUE REFERENCES workflow_result_bindings(id),
    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),
    candidate_hash TEXT NOT NULL,
    child_attempt_id TEXT NOT NULL UNIQUE REFERENCES attempts(id),
    rework_intent_id TEXT NOT NULL UNIQUE REFERENCES rework_intents(id),
    created_at TEXT NOT NULL
);
CREATE INDEX single_agent_review_deliveries_state ON single_agent_review_deliveries(state,attempt_id);

CREATE TABLE single_agent_admission_challenges (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id),
    admission_id TEXT NOT NULL REFERENCES profile_admissions(id),
    purpose TEXT NOT NULL CHECK(purpose IN ('repository_reader','repository_reviewer')),
    nonce TEXT NOT NULL UNIQUE,
    answer_hash TEXT NOT NULL,
    first_session_id TEXT REFERENCES sessions(id),
    first_native_session_id TEXT,
    first_result_id TEXT REFERENCES role_results(id),
    final_result_id TEXT REFERENCES role_results(id),
    state TEXT NOT NULL CHECK(state IN ('prepared','first_reported','challenge_removed','complete')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TRIGGER state_revision_single_agent_admission_challenges_insert AFTER INSERT ON single_agent_admission_challenges BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_single_agent_admission_challenges_delete AFTER DELETE ON single_agent_admission_challenges BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_single_agent_admission_challenges_update AFTER UPDATE ON single_agent_admission_challenges
WHEN (OLD.task_id,OLD.admission_id,OLD.purpose,OLD.nonce,OLD.answer_hash,OLD.first_session_id,OLD.first_native_session_id,OLD.first_result_id,OLD.final_result_id,OLD.state,OLD.created_at,OLD.updated_at)
IS NOT (NEW.task_id,NEW.admission_id,NEW.purpose,NEW.nonce,NEW.answer_hash,NEW.first_session_id,NEW.first_native_session_id,NEW.first_result_id,NEW.final_result_id,NEW.state,NEW.created_at,NEW.updated_at)
BEGIN UPDATE state_revision SET revision=revision+1; END;

-- One cursor increment per semantically changed row, including new pins.
DROP TRIGGER state_revision_agent_profile_revisions_update;
CREATE TRIGGER state_revision_agent_profile_revisions_update AFTER UPDATE ON agent_profile_revisions
WHEN (OLD.id,OLD.profile_id,OLD.revision,OLD.provider,OLD.adapter_id,OLD.adapter_hash,OLD.model,OLD.effort,OLD.instructions_text,OLD.instructions_hash,OLD.policy_id,OLD.policy_revision,OLD.policy_hash,OLD.authority,OLD.session_contract,OLD.profile_hash,OLD.created_at,OLD.purpose)
IS NOT (NEW.id,NEW.profile_id,NEW.revision,NEW.provider,NEW.adapter_id,NEW.adapter_hash,NEW.model,NEW.effort,NEW.instructions_text,NEW.instructions_hash,NEW.policy_id,NEW.policy_revision,NEW.policy_hash,NEW.authority,NEW.session_contract,NEW.profile_hash,NEW.created_at,NEW.purpose)
BEGIN UPDATE state_revision SET revision=revision+1; END;
DROP TRIGGER state_revision_project_workflow_defaults_update;
CREATE TRIGGER state_revision_project_workflow_defaults_update AFTER UPDATE ON project_workflow_defaults
WHEN (OLD.project_id,OLD.family,OLD.definition_id,OLD.version,OLD.updated_at,OLD.preset)
IS NOT (NEW.project_id,NEW.family,NEW.definition_id,NEW.version,NEW.updated_at,NEW.preset)
BEGIN UPDATE state_revision SET revision=revision+1; END;
DROP TRIGGER state_revision_task_workflow_selections_update;
CREATE TRIGGER state_revision_task_workflow_selections_update AFTER UPDATE ON task_workflow_selections
WHEN (OLD.task_id,OLD.family,OLD.definition_id,OLD.execution_config_revision_id,OLD.profile_revision_id,OLD.check_ids_json,OLD.selection_revision,OLD.selection_hash,OLD.admission_state,OLD.admitted_task_version,OLD.admission_reason,OLD.created_at,OLD.updated_at,OLD.workspace_mode,OLD.preset,OLD.reviewer_profile_revision_id)
IS NOT (NEW.task_id,NEW.family,NEW.definition_id,NEW.execution_config_revision_id,NEW.profile_revision_id,NEW.check_ids_json,NEW.selection_revision,NEW.selection_hash,NEW.admission_state,NEW.admitted_task_version,NEW.admission_reason,NEW.created_at,NEW.updated_at,NEW.workspace_mode,NEW.preset,NEW.reviewer_profile_revision_id)
BEGIN UPDATE state_revision SET revision=revision+1; END;
DROP TRIGGER state_revision_workflow_run_bindings_update;
CREATE TRIGGER state_revision_workflow_run_bindings_update AFTER UPDATE ON workflow_run_bindings
WHEN (OLD.attempt_id,OLD.family,OLD.definition_id,OLD.definition_hash,OLD.definition_state,OLD.evaluator_revision,OLD.execution_config_revision_id,OLD.execution_config_hash,OLD.selection_hash,OLD.scope_hash,OLD.parent_attempt_id,OLD.source_snapshot_id,OLD.created_at,OLD.research_snapshot_id)
IS NOT (NEW.attempt_id,NEW.family,NEW.definition_id,NEW.definition_hash,NEW.definition_state,NEW.evaluator_revision,NEW.execution_config_revision_id,NEW.execution_config_hash,NEW.selection_hash,NEW.scope_hash,NEW.parent_attempt_id,NEW.source_snapshot_id,NEW.created_at,NEW.research_snapshot_id)
BEGIN UPDATE state_revision SET revision=revision+1; END;

CREATE TRIGGER state_revision_workflow_run_profiles_insert AFTER INSERT ON workflow_run_profiles BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_workflow_run_profiles_delete AFTER DELETE ON workflow_run_profiles BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_workflow_run_profiles_update AFTER UPDATE ON workflow_run_profiles
WHEN (OLD.attempt_id,OLD.slot_key,OLD.profile_revision_id,OLD.admission_id,OLD.policy_hash,OLD.session_contract,OLD.profile_hash,OLD.created_at)
IS NOT (NEW.attempt_id,NEW.slot_key,NEW.profile_revision_id,NEW.admission_id,NEW.policy_hash,NEW.session_contract,NEW.profile_hash,NEW.created_at)
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_execution_bindings_insert AFTER INSERT ON execution_bindings BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_execution_bindings_delete AFTER DELETE ON execution_bindings BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_execution_bindings_update AFTER UPDATE ON execution_bindings
WHEN (OLD.role_generation_id,OLD.attempt_id,OLD.slot_key,OLD.actor,OLD.policy_hash,OLD.session_contract,OLD.channel_operations_json,OLD.created_at)
IS NOT (NEW.role_generation_id,NEW.attempt_id,NEW.slot_key,NEW.actor,NEW.policy_hash,NEW.session_contract,NEW.channel_operations_json,NEW.created_at)
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_workflow_result_bindings_insert AFTER INSERT ON workflow_result_bindings BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_workflow_result_bindings_delete AFTER DELETE ON workflow_result_bindings BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_workflow_result_bindings_update AFTER UPDATE ON workflow_result_bindings
WHEN (OLD.id,OLD.attempt_id,OLD.role_result_id,OLD.role_generation_id,OLD.session_id,OLD.transcript_epoch,OLD.criteria_hash,OLD.evidence_hash,OLD.profile_binding_hash,OLD.selected_check_revision,OLD.snapshot_id,OLD.candidate_hash,OLD.state,OLD.state_reason,OLD.created_at,OLD.updated_at)
IS NOT (NEW.id,NEW.attempt_id,NEW.role_result_id,NEW.role_generation_id,NEW.session_id,NEW.transcript_epoch,NEW.criteria_hash,NEW.evidence_hash,NEW.profile_binding_hash,NEW.selected_check_revision,NEW.snapshot_id,NEW.candidate_hash,NEW.state,NEW.state_reason,NEW.created_at,NEW.updated_at)
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipe_revisions_insert AFTER INSERT ON task_recipe_revisions BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipe_revisions_delete AFTER DELETE ON task_recipe_revisions BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipe_revisions_update AFTER UPDATE ON task_recipe_revisions
WHEN (OLD.id,OLD.recipe_id,OLD.revision,OLD.title,OLD.description,OLD.acceptance_criteria_json,OLD.priority,OLD.profile_revision_id,OLD.required_check_ids_json,OLD.config_revision_id,OLD.configuration_hash,OLD.workflow_version,OLD.workflow_hash,OLD.created_at,OLD.family,OLD.preset,OLD.definition_id,OLD.execution_config_revision_id,OLD.primary_profile_revision_id,OLD.primary_profile_hash,OLD.reviewer_profile_revision_id,OLD.reviewer_profile_hash)
IS NOT (NEW.id,NEW.recipe_id,NEW.revision,NEW.title,NEW.description,NEW.acceptance_criteria_json,NEW.priority,NEW.profile_revision_id,NEW.required_check_ids_json,NEW.config_revision_id,NEW.configuration_hash,NEW.workflow_version,NEW.workflow_hash,NEW.created_at,NEW.family,NEW.preset,NEW.definition_id,NEW.execution_config_revision_id,NEW.primary_profile_revision_id,NEW.primary_profile_hash,NEW.reviewer_profile_revision_id,NEW.reviewer_profile_hash)
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER task_recipe_revisions_immutable BEFORE UPDATE ON task_recipe_revisions BEGIN SELECT RAISE(ABORT,'recipe revisions are immutable'); END;
CREATE TRIGGER state_revision_task_recipe_bindings_insert AFTER INSERT ON task_recipe_bindings BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipe_bindings_delete AFTER DELETE ON task_recipe_bindings BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipe_bindings_update AFTER UPDATE ON task_recipe_bindings
WHEN (OLD.task_id,OLD.project_id,OLD.recipe_revision_id,OLD.profile_revision_id,OLD.required_check_ids_json,OLD.config_revision_id,OLD.configuration_hash,OLD.workflow_version,OLD.workflow_hash,OLD.schedule_id,OLD.scheduled_for_utc,OLD.created_at,OLD.family,OLD.preset,OLD.definition_id,OLD.execution_config_revision_id,OLD.primary_profile_revision_id,OLD.primary_profile_hash,OLD.reviewer_profile_revision_id,OLD.reviewer_profile_hash)
IS NOT (NEW.task_id,NEW.project_id,NEW.recipe_revision_id,NEW.profile_revision_id,NEW.required_check_ids_json,NEW.config_revision_id,NEW.configuration_hash,NEW.workflow_version,NEW.workflow_hash,NEW.schedule_id,NEW.scheduled_for_utc,NEW.created_at,NEW.family,NEW.preset,NEW.definition_id,NEW.execution_config_revision_id,NEW.primary_profile_revision_id,NEW.primary_profile_hash,NEW.reviewer_profile_revision_id,NEW.reviewer_profile_hash)
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER task_recipe_bindings_immutable BEFORE UPDATE ON task_recipe_bindings BEGIN SELECT RAISE(ABORT,'recipe provenance is immutable'); END;
CREATE TRIGGER state_revision_single_agent_review_budgets_insert AFTER INSERT ON single_agent_review_budgets BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_single_agent_review_budgets_delete AFTER DELETE ON single_agent_review_budgets BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_single_agent_review_budgets_update AFTER UPDATE ON single_agent_review_budgets
WHEN (OLD.task_id,OLD.root_attempt_id,OLD.repair_attempt_id,OLD.created_at) IS NOT (NEW.task_id,NEW.root_attempt_id,NEW.repair_attempt_id,NEW.created_at)
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_single_agent_review_deliveries_insert AFTER INSERT ON single_agent_review_deliveries BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_single_agent_review_deliveries_delete AFTER DELETE ON single_agent_review_deliveries BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_single_agent_review_deliveries_update AFTER UPDATE ON single_agent_review_deliveries
WHEN (OLD.id,OLD.task_id,OLD.root_attempt_id,OLD.attempt_id,OLD.ordinal,OLD.result_binding_id,OLD.snapshot_id,OLD.candidate_hash,OLD.definition_hash,OLD.profile_binding_hash,OLD.reviewer_profile_revision_id,OLD.reviewer_profile_hash,OLD.selected_check_revision,OLD.check_receipts_json,OLD.materialization_path,OLD.materialization_identity,OLD.role_generation_id,OLD.session_id,OLD.credential_id,OLD.transcript_epoch,OLD.state,OLD.reason,OLD.created_at,OLD.updated_at)
IS NOT (NEW.id,NEW.task_id,NEW.root_attempt_id,NEW.attempt_id,NEW.ordinal,NEW.result_binding_id,NEW.snapshot_id,NEW.candidate_hash,NEW.definition_hash,NEW.profile_binding_hash,NEW.reviewer_profile_revision_id,NEW.reviewer_profile_hash,NEW.selected_check_revision,NEW.check_receipts_json,NEW.materialization_path,NEW.materialization_identity,NEW.role_generation_id,NEW.session_id,NEW.credential_id,NEW.transcript_epoch,NEW.state,NEW.reason,NEW.created_at,NEW.updated_at)
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_single_agent_review_results_insert AFTER INSERT ON single_agent_review_results BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_single_agent_review_results_update AFTER UPDATE ON single_agent_review_results
WHEN (OLD.delivery_id,OLD.role_result_id,OLD.operation_id,OLD.request_hash,OLD.verdict,OLD.summary,OLD.findings_json,OLD.created_at)
IS NOT (NEW.delivery_id,NEW.role_result_id,NEW.operation_id,NEW.request_hash,NEW.verdict,NEW.summary,NEW.findings_json,NEW.created_at)
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_single_agent_review_results_delete AFTER DELETE ON single_agent_review_results BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER single_agent_review_results_immutable BEFORE UPDATE ON single_agent_review_results BEGIN SELECT RAISE(ABORT,'review verdicts are immutable'); END;
CREATE TRIGGER state_revision_single_agent_repair_receipts_insert AFTER INSERT ON single_agent_repair_receipts BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_single_agent_repair_receipts_update AFTER UPDATE ON single_agent_repair_receipts
WHEN (OLD.root_attempt_id,OLD.delivery_id,OLD.result_binding_id,OLD.snapshot_id,OLD.candidate_hash,OLD.child_attempt_id,OLD.rework_intent_id,OLD.created_at)
IS NOT (NEW.root_attempt_id,NEW.delivery_id,NEW.result_binding_id,NEW.snapshot_id,NEW.candidate_hash,NEW.child_attempt_id,NEW.rework_intent_id,NEW.created_at)
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_single_agent_repair_receipts_delete AFTER DELETE ON single_agent_repair_receipts BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER single_agent_repair_receipts_immutable BEFORE UPDATE ON single_agent_repair_receipts BEGIN SELECT RAISE(ABORT,'review repair receipts are immutable'); END;
