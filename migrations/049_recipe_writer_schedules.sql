-- The runner disables foreign keys outside this transaction and checks them before commit.
-- Drop incoming binding triggers during the documented parent-table rebuild.
DROP TRIGGER schedule_authorization_scope_insert;
DROP TRIGGER schedule_authorization_immutable_update;
DROP TRIGGER schedule_authorization_immutable_delete;
DROP TRIGGER schedule_active_authorization_insert;
DROP TRIGGER schedule_active_authorization_update;
DROP TRIGGER schedule_occurrence_identity_update;
DROP TRIGGER schedule_occurrence_history_delete;
DROP TRIGGER schedule_occurrence_scope_insert;
DROP TRIGGER schedule_occurrence_scope_update;
DROP TRIGGER sessions_schedule_binding_insert;
DROP TRIGGER sessions_schedule_binding_update;
DROP TRIGGER resume_schedule_binding_insert;
DROP TRIGGER resume_schedule_binding_update;
DROP TRIGGER schedule_stop_binding_insert;
DROP TRIGGER schedule_stop_binding_update;
DROP TRIGGER state_revision_recipe_schedule_authorizations_insert;
DROP TRIGGER state_revision_sessions_update;
DROP TRIGGER state_revision_resume_invocations_update;
DROP TRIGGER state_revision_recipe_schedule_authorizations_update;
DROP TRIGGER state_revision_recipe_schedule_authorizations_delete;
DROP TRIGGER state_revision_recipe_schedule_fires_insert;
DROP TRIGGER state_revision_recipe_schedule_fires_update;
DROP TRIGGER state_revision_recipe_schedule_fires_delete;
DROP TRIGGER state_revision_recipe_schedules_update;
DROP TRIGGER state_revision_recipe_schedules_insert;
DROP TRIGGER state_revision_recipe_schedules_delete;

CREATE TABLE recipe_schedules_rebuilt (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    name TEXT NOT NULL,
    recipe_revision_id TEXT NOT NULL REFERENCES task_recipe_revisions(id),
    cadence TEXT NOT NULL CHECK(cadence IN ('daily','weekly')),
    anchor_utc TEXT NOT NULL,
    next_fire_utc TEXT,
    paused INTEGER NOT NULL CHECK(paused IN (0,1)),
    version INTEGER NOT NULL CHECK(version >= 1),
    archived_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    mode TEXT NOT NULL DEFAULT 'create_draft'
        CHECK(mode IN ('create_draft','run_authorized_research','run_authorized_recipe')),
    active_authorization_id TEXT REFERENCES recipe_schedule_authorizations(id),
    UNIQUE(project_id, name),
    CHECK(paused=0 OR next_fire_utc IS NULL)
);
INSERT INTO recipe_schedules_rebuilt (id,project_id,name,recipe_revision_id,cadence,anchor_utc,next_fire_utc,paused,version,archived_at,created_at,updated_at,mode,active_authorization_id)
SELECT id,project_id,name,recipe_revision_id,cadence,anchor_utc,next_fire_utc,paused,version,archived_at,created_at,updated_at,mode,active_authorization_id FROM recipe_schedules;
DROP TABLE recipe_schedules;
ALTER TABLE recipe_schedules_rebuilt RENAME TO recipe_schedules;

CREATE TABLE recipe_schedule_fires_rebuilt (
    schedule_id TEXT NOT NULL REFERENCES recipe_schedules(id),
    scheduled_for_utc TEXT NOT NULL,
    recipe_revision_id TEXT NOT NULL REFERENCES task_recipe_revisions(id),
    outcome TEXT NOT NULL CHECK(outcome IN ('task_created','skipped_ineligible','missed',
        'waiting','held','reserved','running','stop_requested','review_ready','finished','recovery_required')),
    task_id TEXT REFERENCES tasks(id),
    reason TEXT,
    missed_first_utc TEXT,
    missed_last_utc TEXT,
    missed_count INTEGER NOT NULL DEFAULT 0 CHECK(missed_count >= 0),
    created_at TEXT NOT NULL,
    id TEXT NOT NULL UNIQUE DEFAULT (lower(hex(randomblob(16)))),
    authorization_id TEXT,
    attempt_id TEXT UNIQUE REFERENCES attempts(id),
    allowance_reserved INTEGER NOT NULL DEFAULT 0 CHECK(allowance_reserved IN (0,1)),
    invocations_spent INTEGER NOT NULL DEFAULT 0 CHECK(typeof(invocations_spent)='integer' AND invocations_spent BETWEEN 0 AND 8),
    first_reserved_at TEXT,
    runtime_deadline_utc TEXT,
    last_observed_wall_utc TEXT,
    stop_requested_at TEXT,
    stop_reason TEXT,
    stop_session_id TEXT REFERENCES sessions(id),
    stop_resume_invocation_id TEXT REFERENCES resume_invocations(id),
    stop_transcript_epoch TEXT,
    stop_process_identity_json TEXT CHECK(stop_process_identity_json IS NULL OR
        (json_valid(stop_process_identity_json) AND json_type(stop_process_identity_json)='object')),
    PRIMARY KEY(schedule_id,scheduled_for_utc),
    FOREIGN KEY(schedule_id,authorization_id) REFERENCES recipe_schedule_authorizations(schedule_id,id),
    CHECK(authorization_id IS NOT NULL OR outcome NOT IN ('task_created','skipped_ineligible','missed')
        OR ((outcome='task_created') = (task_id IS NOT NULL))),
    CHECK(authorization_id IS NOT NULL OR
        (outcome IN ('task_created','skipped_ineligible','missed') AND attempt_id IS NULL
         AND allowance_reserved=0 AND invocations_spent=0 AND first_reserved_at IS NULL
         AND runtime_deadline_utc IS NULL AND last_observed_wall_utc IS NULL AND stop_requested_at IS NULL)),
    CHECK(outcome NOT IN ('task_created','skipped_ineligible') OR authorization_id IS NULL),
    CHECK(task_id IS NULL OR authorization_id IS NULL OR allowance_reserved=1),
    CHECK(attempt_id IS NULL OR (task_id IS NOT NULL AND authorization_id IS NOT NULL)),
    CHECK(outcome NOT IN ('waiting','reserved','running','review_ready','finished')
        OR (task_id IS NOT NULL AND allowance_reserved=1)),
    CHECK(outcome NOT IN ('reserved','running','review_ready')
        OR (attempt_id IS NOT NULL AND invocations_spent>0)),
    CHECK((first_reserved_at IS NULL) = (runtime_deadline_utc IS NULL)),
    CHECK((invocations_spent=0) = (first_reserved_at IS NULL)),
    CHECK(first_reserved_at IS NULL OR (allowance_reserved=1 AND attempt_id IS NOT NULL
        AND julianday(first_reserved_at) IS NOT NULL AND julianday(runtime_deadline_utc) IS NOT NULL
        AND julianday(runtime_deadline_utc)>julianday(first_reserved_at))),
    CHECK((stop_requested_at IS NULL) = (stop_reason IS NULL)),
    CHECK(stop_requested_at IS NULL OR (authorization_id IS NOT NULL AND allowance_reserved=1)),
    CHECK(outcome!='stop_requested' OR stop_requested_at IS NOT NULL),
    CHECK((stop_session_id IS NULL) = (stop_transcript_epoch IS NULL)),
    CHECK(stop_session_id IS NULL OR stop_requested_at IS NOT NULL),
    CHECK(stop_resume_invocation_id IS NULL OR stop_session_id IS NOT NULL),
    CHECK(stop_process_identity_json IS NULL OR stop_session_id IS NOT NULL)
);
INSERT INTO recipe_schedule_fires_rebuilt (schedule_id,scheduled_for_utc,recipe_revision_id,outcome,task_id,reason,missed_first_utc,missed_last_utc,missed_count,created_at,id,authorization_id,attempt_id,allowance_reserved,invocations_spent,first_reserved_at,runtime_deadline_utc,last_observed_wall_utc,stop_requested_at,stop_reason,stop_session_id,stop_resume_invocation_id,stop_transcript_epoch,stop_process_identity_json)
SELECT schedule_id,scheduled_for_utc,recipe_revision_id,outcome,task_id,reason,missed_first_utc,missed_last_utc,missed_count,created_at,id,authorization_id,attempt_id,allowance_reserved,invocations_spent,first_reserved_at,runtime_deadline_utc,last_observed_wall_utc,stop_requested_at,stop_reason,stop_session_id,stop_resume_invocation_id,stop_transcript_epoch,stop_process_identity_json FROM recipe_schedule_fires;
DROP TABLE recipe_schedule_fires;
ALTER TABLE recipe_schedule_fires_rebuilt RENAME TO recipe_schedule_fires;

CREATE UNIQUE INDEX recipe_schedules_project_identity ON recipe_schedules(project_id,id);
CREATE UNIQUE INDEX recipe_schedule_fires_authorized_task ON recipe_schedule_fires(task_id)
    WHERE authorization_id IS NOT NULL AND task_id IS NOT NULL;
CREATE INDEX recipe_schedule_fires_authorization ON recipe_schedule_fires(authorization_id,allowance_reserved);
CREATE INDEX recipe_schedule_fires_pending ON recipe_schedule_fires(outcome,runtime_deadline_utc)
    WHERE authorization_id IS NOT NULL;
CREATE UNIQUE INDEX recipe_schedule_fires_one_execution_fence ON recipe_schedule_fires(schedule_id)
    WHERE authorization_id IS NOT NULL AND outcome IN ('reserved','running','stop_requested','recovery_required');


-- Scope schema and execution mode must match the immutable recipe family and preset.
CREATE TRIGGER schedule_authorization_scope_insert BEFORE INSERT ON recipe_schedule_authorizations
WHEN NOT EXISTS(SELECT 1 FROM task_recipe_revisions rr JOIN task_recipes r ON r.id=rr.recipe_id
    WHERE rr.id=NEW.recipe_revision_id AND r.project_id=NEW.project_id
      AND rr.family='single_agent'
      AND ((rr.preset='repository_research' AND json_extract(NEW.scope_json,'$.schema')=1)
        OR (rr.preset IN ('single_agent','build_and_review')
          AND json_extract(NEW.scope_json,'$.schema')=2
          AND json_extract(NEW.scope_json,'$.preset')=rr.preset)))
BEGIN SELECT RAISE(ABORT,'schedule authorization scope must match its recipe family and preset'); END;
CREATE TRIGGER schedule_authorization_immutable_update BEFORE UPDATE ON recipe_schedule_authorizations
WHEN (OLD.id,OLD.project_id,OLD.schedule_id,OLD.recipe_revision_id,OLD.scope_json,OLD.scope_hash,
      OLD.max_occurrences,OLD.max_invocations_per_occurrence,OLD.max_runtime_seconds,
      OLD.start_window_seconds,OLD.created_by,OLD.created_at)
 IS NOT (NEW.id,NEW.project_id,NEW.schedule_id,NEW.recipe_revision_id,NEW.scope_json,NEW.scope_hash,
      NEW.max_occurrences,NEW.max_invocations_per_occurrence,NEW.max_runtime_seconds,
      NEW.start_window_seconds,NEW.created_by,NEW.created_at)
 OR (OLD.revoked_at IS NOT NULL AND (OLD.revoked_at,OLD.revoked_by,OLD.revocation_reason)
     IS NOT (NEW.revoked_at,NEW.revoked_by,NEW.revocation_reason))
 OR (OLD.restore_invalidated_at IS NOT NULL AND OLD.restore_invalidated_at IS NOT NEW.restore_invalidated_at)
BEGIN SELECT RAISE(ABORT,'schedule authorization is immutable except first revocation or restore invalidation'); END;
CREATE TRIGGER schedule_authorization_immutable_delete BEFORE DELETE ON recipe_schedule_authorizations
BEGIN SELECT RAISE(ABORT,'schedule authorization history is immutable'); END;
CREATE TRIGGER schedule_active_authorization_insert BEFORE INSERT ON recipe_schedules
WHEN (NEW.mode='create_draft' AND NEW.active_authorization_id IS NOT NULL)
 OR (NEW.mode IN ('run_authorized_research','run_authorized_recipe') AND NOT EXISTS(SELECT 1 FROM recipe_schedule_authorizations a
     WHERE a.id=NEW.active_authorization_id AND a.schedule_id=NEW.id AND a.project_id=NEW.project_id
       AND a.recipe_revision_id=NEW.recipe_revision_id
       AND EXISTS(SELECT 1 FROM task_recipe_revisions rr WHERE rr.id=a.recipe_revision_id
         AND rr.family='single_agent'
         AND ((NEW.mode='run_authorized_research' AND rr.preset='repository_research'
             AND json_extract(a.scope_json,'$.schema')=1)
           OR (NEW.mode='run_authorized_recipe' AND rr.preset IN ('single_agent','build_and_review')
             AND json_extract(a.scope_json,'$.schema')=2
             AND json_extract(a.scope_json,'$.preset')=rr.preset)))
       AND a.revoked_at IS NULL AND a.restore_invalidated_at IS NULL))
BEGIN SELECT RAISE(ABORT,'schedule mode requires its own active authorization'); END;
CREATE TRIGGER schedule_active_authorization_update BEFORE UPDATE OF mode,active_authorization_id,project_id,recipe_revision_id ON recipe_schedules
WHEN (NEW.mode='create_draft' AND NEW.active_authorization_id IS NOT NULL)
 OR (NEW.mode IN ('run_authorized_research','run_authorized_recipe') AND NOT EXISTS(SELECT 1 FROM recipe_schedule_authorizations a
     WHERE a.id=NEW.active_authorization_id AND a.schedule_id=NEW.id AND a.project_id=NEW.project_id
       AND a.recipe_revision_id=NEW.recipe_revision_id
       AND EXISTS(SELECT 1 FROM task_recipe_revisions rr WHERE rr.id=a.recipe_revision_id
         AND rr.family='single_agent'
         AND ((NEW.mode='run_authorized_research' AND rr.preset='repository_research'
             AND json_extract(a.scope_json,'$.schema')=1)
           OR (NEW.mode='run_authorized_recipe' AND rr.preset IN ('single_agent','build_and_review')
             AND json_extract(a.scope_json,'$.schema')=2
             AND json_extract(a.scope_json,'$.preset')=rr.preset)))
       AND a.revoked_at IS NULL AND a.restore_invalidated_at IS NULL))
BEGIN SELECT RAISE(ABORT,'schedule mode requires its own active authorization'); END;
CREATE TRIGGER schedule_occurrence_identity_update BEFORE UPDATE ON recipe_schedule_fires
WHEN (OLD.id,OLD.schedule_id,OLD.scheduled_for_utc,OLD.recipe_revision_id,OLD.authorization_id,OLD.created_at)
 IS NOT (NEW.id,NEW.schedule_id,NEW.scheduled_for_utc,NEW.recipe_revision_id,NEW.authorization_id,NEW.created_at)
 OR (OLD.task_id IS NOT NULL AND OLD.task_id IS NOT NEW.task_id)
 OR (OLD.attempt_id IS NOT NULL AND OLD.attempt_id IS NOT NEW.attempt_id)
 OR NEW.allowance_reserved<OLD.allowance_reserved OR NEW.invocations_spent<OLD.invocations_spent
 OR (OLD.authorization_id IS NOT NULL AND OLD.outcome IN ('held','missed','finished')
     AND (NEW.outcome NOT IN ('held','missed','finished')
          OR NEW.task_id IS NOT OLD.task_id OR NEW.attempt_id IS NOT OLD.attempt_id
          OR NEW.allowance_reserved!=OLD.allowance_reserved OR NEW.invocations_spent!=OLD.invocations_spent))
 OR (OLD.first_reserved_at IS NOT NULL AND (OLD.first_reserved_at,OLD.runtime_deadline_utc)
     IS NOT (NEW.first_reserved_at,NEW.runtime_deadline_utc))
 OR (OLD.stop_requested_at IS NOT NULL AND (OLD.stop_requested_at,OLD.stop_reason)
     IS NOT (NEW.stop_requested_at,NEW.stop_reason))
 OR (OLD.stop_session_id IS NOT NULL AND (OLD.stop_session_id,OLD.stop_resume_invocation_id,
     OLD.stop_transcript_epoch) IS NOT (NEW.stop_session_id,
     NEW.stop_resume_invocation_id,NEW.stop_transcript_epoch))
 OR (OLD.stop_process_identity_json IS NOT NULL AND OLD.stop_process_identity_json IS NOT NEW.stop_process_identity_json)
BEGIN SELECT RAISE(ABORT,'schedule occurrence identity and spent allowance cannot be rewritten'); END;
CREATE TRIGGER schedule_occurrence_history_delete BEFORE DELETE ON recipe_schedule_fires
WHEN OLD.authorization_id IS NOT NULL
BEGIN SELECT RAISE(ABORT,'authorized schedule occurrence history is immutable'); END;
CREATE TRIGGER schedule_occurrence_scope_insert BEFORE INSERT ON recipe_schedule_fires
WHEN NEW.authorization_id IS NOT NULL AND (
 NOT EXISTS(SELECT 1 FROM recipe_schedule_authorizations a WHERE a.id=NEW.authorization_id
     AND a.schedule_id=NEW.schedule_id AND a.recipe_revision_id=NEW.recipe_revision_id
     AND (NEW.task_id IS NULL OR EXISTS(SELECT 1 FROM tasks t JOIN task_recipe_bindings b ON b.task_id=t.id
         WHERE t.id=NEW.task_id AND t.project_id=a.project_id AND b.project_id=a.project_id
           AND b.schedule_id=NEW.schedule_id AND b.scheduled_for_utc=NEW.scheduled_for_utc
           AND b.recipe_revision_id=NEW.recipe_revision_id)))
 OR (NEW.attempt_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM attempts WHERE id=NEW.attempt_id AND task_id=NEW.task_id))
 OR NEW.invocations_spent>(SELECT max_invocations_per_occurrence FROM recipe_schedule_authorizations WHERE id=NEW.authorization_id)
 OR (NEW.first_reserved_at IS NOT NULL AND
     (julianday(NEW.runtime_deadline_utc)-julianday(NEW.first_reserved_at))*86400>
     (SELECT max_runtime_seconds FROM recipe_schedule_authorizations WHERE id=NEW.authorization_id)+0.001)
 OR (NEW.allowance_reserved=1 AND (SELECT count(*) FROM recipe_schedule_fires
     WHERE authorization_id=NEW.authorization_id AND allowance_reserved=1)>=
     (SELECT max_occurrences FROM recipe_schedule_authorizations WHERE id=NEW.authorization_id)))
BEGIN SELECT RAISE(ABORT,'schedule occurrence scope or allowance is invalid'); END;
CREATE TRIGGER schedule_occurrence_scope_update BEFORE UPDATE ON recipe_schedule_fires
WHEN NEW.authorization_id IS NOT NULL AND (
 (NEW.task_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM tasks t JOIN recipe_schedule_authorizations a
     ON a.project_id=t.project_id JOIN task_recipe_bindings b ON b.task_id=t.id
     WHERE t.id=NEW.task_id AND a.id=NEW.authorization_id AND b.project_id=a.project_id
       AND b.schedule_id=NEW.schedule_id AND b.scheduled_for_utc=NEW.scheduled_for_utc
       AND b.recipe_revision_id=NEW.recipe_revision_id))
 OR (NEW.attempt_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM attempts WHERE id=NEW.attempt_id AND task_id=NEW.task_id))
 OR NEW.invocations_spent>(SELECT max_invocations_per_occurrence FROM recipe_schedule_authorizations WHERE id=NEW.authorization_id)
 OR (NEW.first_reserved_at IS NOT NULL AND
     (julianday(NEW.runtime_deadline_utc)-julianday(NEW.first_reserved_at))*86400>
     (SELECT max_runtime_seconds FROM recipe_schedule_authorizations WHERE id=NEW.authorization_id)+0.001)
 OR (NEW.allowance_reserved>OLD.allowance_reserved AND (SELECT count(*) FROM recipe_schedule_fires
     WHERE authorization_id=NEW.authorization_id AND allowance_reserved=1)>=
     (SELECT max_occurrences FROM recipe_schedule_authorizations WHERE id=NEW.authorization_id)))
BEGIN SELECT RAISE(ABORT,'schedule occurrence scope or allowance is invalid'); END;
CREATE TRIGGER sessions_schedule_binding_insert BEFORE INSERT ON sessions
WHEN (NEW.schedule_occurrence_id IS NULL)!=(NEW.schedule_authorization_id IS NULL)
 OR (NEW.schedule_occurrence_id IS NOT NULL AND NOT EXISTS(
     SELECT 1 FROM recipe_schedule_fires f
     JOIN recipe_schedule_authorizations a ON a.id=f.authorization_id
     JOIN role_generations g ON g.id=NEW.role_generation_id
     JOIN attempts member ON member.id=g.attempt_id AND member.task_id=f.task_id
     JOIN tasks t ON t.id=member.task_id AND t.project_id=a.project_id
     WHERE f.id=NEW.schedule_occurrence_id AND f.authorization_id=NEW.schedule_authorization_id
       AND f.allowance_reserved=1 AND f.invocations_spent>0
       AND (member.id=f.attempt_id OR (json_extract(a.scope_json,'$.schema')=2
         AND json_extract(a.scope_json,'$.preset')='build_and_review'
         AND member.parent_attempt_id=f.attempt_id AND EXISTS(
           SELECT 1 FROM single_agent_repair_receipts receipt
           JOIN single_agent_review_budgets budget ON budget.root_attempt_id=receipt.root_attempt_id
             AND budget.task_id=f.task_id AND budget.repair_attempt_id=receipt.child_attempt_id
           JOIN rework_intents intent ON intent.id=receipt.rework_intent_id
             AND intent.parent_attempt_id=f.attempt_id AND intent.new_attempt_id=member.id
             AND intent.snapshot_id=receipt.snapshot_id
           JOIN workflow_run_bindings binding ON binding.attempt_id=member.id
             AND binding.parent_attempt_id=f.attempt_id AND binding.source_snapshot_id=receipt.snapshot_id
           JOIN single_agent_review_deliveries delivery ON delivery.id=receipt.delivery_id
             AND delivery.root_attempt_id=f.attempt_id AND delivery.attempt_id=f.attempt_id
             AND delivery.task_id=f.task_id AND delivery.state='finished'
             AND delivery.result_binding_id=receipt.result_binding_id
             AND delivery.snapshot_id=receipt.snapshot_id AND delivery.candidate_hash=receipt.candidate_hash
           WHERE receipt.root_attempt_id=f.attempt_id AND receipt.child_attempt_id=member.id)))))
BEGIN SELECT RAISE(ABORT,'session schedule binding does not match its occurrence attempt'); END;
CREATE TRIGGER sessions_schedule_binding_update BEFORE UPDATE OF schedule_occurrence_id,schedule_authorization_id,role_generation_id ON sessions
WHEN (NEW.schedule_occurrence_id IS NULL)!=(NEW.schedule_authorization_id IS NULL)
 OR (OLD.schedule_occurrence_id IS NOT NULL AND (OLD.schedule_occurrence_id,OLD.schedule_authorization_id)
     IS NOT (NEW.schedule_occurrence_id,NEW.schedule_authorization_id))
 OR (NEW.schedule_occurrence_id IS NOT NULL AND NOT EXISTS(
     SELECT 1 FROM recipe_schedule_fires f
     JOIN recipe_schedule_authorizations a ON a.id=f.authorization_id
     JOIN role_generations g ON g.id=NEW.role_generation_id
     JOIN attempts member ON member.id=g.attempt_id AND member.task_id=f.task_id
     JOIN tasks t ON t.id=member.task_id AND t.project_id=a.project_id
     WHERE f.id=NEW.schedule_occurrence_id AND f.authorization_id=NEW.schedule_authorization_id
       AND f.allowance_reserved=1 AND f.invocations_spent>0
       AND (member.id=f.attempt_id OR (json_extract(a.scope_json,'$.schema')=2
         AND json_extract(a.scope_json,'$.preset')='build_and_review'
         AND member.parent_attempt_id=f.attempt_id AND EXISTS(
           SELECT 1 FROM single_agent_repair_receipts receipt
           JOIN single_agent_review_budgets budget ON budget.root_attempt_id=receipt.root_attempt_id
             AND budget.task_id=f.task_id AND budget.repair_attempt_id=receipt.child_attempt_id
           JOIN rework_intents intent ON intent.id=receipt.rework_intent_id
             AND intent.parent_attempt_id=f.attempt_id AND intent.new_attempt_id=member.id
             AND intent.snapshot_id=receipt.snapshot_id
           JOIN workflow_run_bindings binding ON binding.attempt_id=member.id
             AND binding.parent_attempt_id=f.attempt_id AND binding.source_snapshot_id=receipt.snapshot_id
           JOIN single_agent_review_deliveries delivery ON delivery.id=receipt.delivery_id
             AND delivery.root_attempt_id=f.attempt_id AND delivery.attempt_id=f.attempt_id
             AND delivery.task_id=f.task_id AND delivery.state='finished'
             AND delivery.result_binding_id=receipt.result_binding_id
             AND delivery.snapshot_id=receipt.snapshot_id AND delivery.candidate_hash=receipt.candidate_hash
           WHERE receipt.root_attempt_id=f.attempt_id AND receipt.child_attempt_id=member.id)))))
BEGIN SELECT RAISE(ABORT,'session schedule binding does not match its occurrence attempt'); END;
CREATE TRIGGER resume_schedule_binding_insert BEFORE INSERT ON resume_invocations
WHEN NOT EXISTS(SELECT 1 FROM sessions s WHERE s.id=NEW.session_id
    AND s.schedule_occurrence_id IS NEW.schedule_occurrence_id
    AND s.schedule_authorization_id IS NEW.schedule_authorization_id
    AND (NEW.schedule_occurrence_id IS NULL
      OR NOT EXISTS(SELECT 1 FROM recipe_schedule_authorizations a
        WHERE a.id=NEW.schedule_authorization_id AND json_extract(a.scope_json,'$.schema')=2)
      OR EXISTS(SELECT 1 FROM role_generations g
        JOIN execution_bindings e ON e.role_generation_id=g.id
        WHERE g.id=s.role_generation_id AND g.role='single_agent' AND e.session_contract='retained')))
BEGIN SELECT RAISE(ABORT,'resume schedule binding must match its session'); END;
CREATE TRIGGER resume_schedule_binding_update BEFORE UPDATE OF schedule_occurrence_id,schedule_authorization_id,session_id ON resume_invocations
WHEN (OLD.schedule_occurrence_id IS NOT NULL AND (OLD.schedule_occurrence_id,OLD.schedule_authorization_id)
     IS NOT (NEW.schedule_occurrence_id,NEW.schedule_authorization_id))
 OR NOT EXISTS(SELECT 1 FROM sessions s WHERE s.id=NEW.session_id
    AND s.schedule_occurrence_id IS NEW.schedule_occurrence_id
    AND s.schedule_authorization_id IS NEW.schedule_authorization_id
    AND (NEW.schedule_occurrence_id IS NULL
      OR NOT EXISTS(SELECT 1 FROM recipe_schedule_authorizations a
        WHERE a.id=NEW.schedule_authorization_id AND json_extract(a.scope_json,'$.schema')=2)
      OR EXISTS(SELECT 1 FROM role_generations g
        JOIN execution_bindings e ON e.role_generation_id=g.id
        WHERE g.id=s.role_generation_id AND g.role='single_agent' AND e.session_contract='retained')))
BEGIN SELECT RAISE(ABORT,'resume schedule binding must match its session'); END;
CREATE TRIGGER schedule_stop_binding_insert BEFORE INSERT ON recipe_schedule_fires
WHEN NEW.stop_session_id IS NOT NULL AND (
 NOT EXISTS(SELECT 1 FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
     WHERE s.id=NEW.stop_session_id AND g.attempt_id=NEW.attempt_id
       AND s.schedule_occurrence_id=NEW.id AND s.schedule_authorization_id=NEW.authorization_id
       AND (NEW.stop_resume_invocation_id IS NOT NULL OR s.transcript_epoch=NEW.stop_transcript_epoch))
 OR (NEW.stop_resume_invocation_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM resume_invocations r
     WHERE r.id=NEW.stop_resume_invocation_id AND r.session_id=NEW.stop_session_id
       AND r.transcript_epoch=NEW.stop_transcript_epoch AND r.schedule_occurrence_id=NEW.id)))
BEGIN SELECT RAISE(ABORT,'schedule stop target does not match its occurrence'); END;
CREATE TRIGGER schedule_stop_binding_update BEFORE UPDATE OF stop_session_id,stop_resume_invocation_id,stop_transcript_epoch ON recipe_schedule_fires
WHEN NEW.stop_session_id IS NOT NULL AND (
 NOT EXISTS(SELECT 1 FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
     WHERE s.id=NEW.stop_session_id AND g.attempt_id=NEW.attempt_id
       AND s.schedule_occurrence_id=NEW.id AND s.schedule_authorization_id=NEW.authorization_id
       AND (NEW.stop_resume_invocation_id IS NOT NULL OR s.transcript_epoch=NEW.stop_transcript_epoch))
 OR (NEW.stop_resume_invocation_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM resume_invocations r
     WHERE r.id=NEW.stop_resume_invocation_id AND r.session_id=NEW.stop_session_id
       AND r.transcript_epoch=NEW.stop_transcript_epoch AND r.schedule_occurrence_id=NEW.id)))
BEGIN SELECT RAISE(ABORT,'schedule stop target does not match its occurrence'); END;
CREATE TRIGGER state_revision_recipe_schedule_authorizations_insert AFTER INSERT ON recipe_schedule_authorizations
BEGIN UPDATE state_revision SET revision=revision+1; END;
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
  OR OLD.workflow_version IS NOT NEW.workflow_version OR OLD.workflow_hash IS NOT NEW.workflow_hash
  OR OLD.prompt_hash IS NOT NEW.prompt_hash OR OLD.launch_state IS NOT NEW.launch_state
  OR OLD.launch_error IS NOT NEW.launch_error OR OLD.resume_count IS NOT NEW.resume_count
  OR OLD.capability_key IS NOT NEW.capability_key OR OLD.capability_identity_json IS NOT NEW.capability_identity_json
  OR OLD.recovery_root_pid IS NOT NEW.recovery_root_pid OR OLD.recovery_process_group_id IS NOT NEW.recovery_process_group_id
  OR OLD.desired_running IS NOT NEW.desired_running OR OLD.launch_boot_identity IS NOT NEW.launch_boot_identity
  OR OLD.recovery_anchor_json IS NOT NEW.recovery_anchor_json
  OR OLD.initial_hook_event_boundary_rowid IS NOT NEW.initial_hook_event_boundary_rowid
  OR OLD.lane_id IS NOT NEW.lane_id OR OLD.setup_permit_id IS NOT NEW.setup_permit_id
  OR OLD.interrupt_requested_at IS NOT NEW.interrupt_requested_at
  OR OLD.provider_validation_invocation_id IS NOT NEW.provider_validation_invocation_id
  OR OLD.conversation_id IS NOT NEW.conversation_id
  OR OLD.schedule_occurrence_id IS NOT NEW.schedule_occurrence_id
  OR OLD.schedule_authorization_id IS NOT NEW.schedule_authorization_id
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_resume_invocations_update AFTER UPDATE ON resume_invocations
WHEN OLD.id IS NOT NEW.id OR OLD.session_id IS NOT NEW.session_id
  OR OLD.resume_ordinal IS NOT NEW.resume_ordinal
  OR OLD.transcript_epoch IS NOT NEW.transcript_epoch
  OR OLD.launch_config_json IS NOT NEW.launch_config_json
  OR OLD.capability_key IS NOT NEW.capability_key
  OR OLD.capability_identity_json IS NOT NEW.capability_identity_json
  OR OLD.state IS NOT NEW.state OR OLD.process_identity_json IS NOT NEW.process_identity_json
  OR OLD.error IS NOT NEW.error OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at OR OLD.prior_exit_json IS NOT NEW.prior_exit_json
  OR OLD.prior_transcript_epoch IS NOT NEW.prior_transcript_epoch
  OR OLD.prior_launch_boot_identity IS NOT NEW.prior_launch_boot_identity
  OR OLD.prior_recovery_anchor_json IS NOT NEW.prior_recovery_anchor_json
  OR OLD.prior_recovery_root_pid IS NOT NEW.prior_recovery_root_pid
  OR OLD.prior_recovery_process_group_id IS NOT NEW.prior_recovery_process_group_id
  OR OLD.hook_event_boundary_rowid IS NOT NEW.hook_event_boundary_rowid
  OR OLD.schedule_occurrence_id IS NOT NEW.schedule_occurrence_id
  OR OLD.schedule_authorization_id IS NOT NEW.schedule_authorization_id
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedule_authorizations_update AFTER UPDATE ON recipe_schedule_authorizations
WHEN (OLD.revoked_at,OLD.revoked_by,OLD.revocation_reason,OLD.restore_invalidated_at)
 IS NOT (NEW.revoked_at,NEW.revoked_by,NEW.revocation_reason,NEW.restore_invalidated_at)
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedule_authorizations_delete AFTER DELETE ON recipe_schedule_authorizations
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedule_fires_insert AFTER INSERT ON recipe_schedule_fires
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedule_fires_update AFTER UPDATE ON recipe_schedule_fires
WHEN (OLD.schedule_id,OLD.scheduled_for_utc,OLD.recipe_revision_id,OLD.outcome,OLD.task_id,OLD.reason,
 OLD.missed_first_utc,OLD.missed_last_utc,OLD.missed_count,OLD.created_at,OLD.id,OLD.authorization_id,
 OLD.attempt_id,OLD.allowance_reserved,OLD.invocations_spent,OLD.first_reserved_at,OLD.runtime_deadline_utc,
 OLD.last_observed_wall_utc,OLD.stop_requested_at,OLD.stop_reason,OLD.stop_session_id,
 OLD.stop_resume_invocation_id,OLD.stop_transcript_epoch,OLD.stop_process_identity_json)
 IS NOT (NEW.schedule_id,NEW.scheduled_for_utc,NEW.recipe_revision_id,NEW.outcome,NEW.task_id,NEW.reason,
 NEW.missed_first_utc,NEW.missed_last_utc,NEW.missed_count,NEW.created_at,NEW.id,NEW.authorization_id,
 NEW.attempt_id,NEW.allowance_reserved,NEW.invocations_spent,NEW.first_reserved_at,NEW.runtime_deadline_utc,
 NEW.last_observed_wall_utc,NEW.stop_requested_at,NEW.stop_reason,NEW.stop_session_id,
 NEW.stop_resume_invocation_id,NEW.stop_transcript_epoch,NEW.stop_process_identity_json)
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedule_fires_delete AFTER DELETE ON recipe_schedule_fires
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedules_update AFTER UPDATE ON recipe_schedules
WHEN (OLD.id,OLD.project_id,OLD.name,OLD.recipe_revision_id,OLD.cadence,OLD.anchor_utc,OLD.next_fire_utc,
 OLD.paused,OLD.version,OLD.archived_at,OLD.created_at,OLD.updated_at,OLD.mode,OLD.active_authorization_id)
 IS NOT (NEW.id,NEW.project_id,NEW.name,NEW.recipe_revision_id,NEW.cadence,NEW.anchor_utc,NEW.next_fire_utc,
 NEW.paused,NEW.version,NEW.archived_at,NEW.created_at,NEW.updated_at,NEW.mode,NEW.active_authorization_id)
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedules_insert AFTER INSERT ON recipe_schedules
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedules_delete AFTER DELETE ON recipe_schedules
BEGIN UPDATE state_revision SET revision=revision+1; END;

-- These are stop observations, never membership or launch authority. Repair-child
-- membership and launch authority require authenticated application validation.
CREATE TABLE recipe_schedule_stop_targets (
    id TEXT PRIMARY KEY NOT NULL,
    occurrence_id TEXT NOT NULL REFERENCES recipe_schedule_fires(id),
    authorization_id TEXT NOT NULL REFERENCES recipe_schedule_authorizations(id),
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    target_kind TEXT NOT NULL CHECK(target_kind IN ('session','resume','check')),
    session_id TEXT REFERENCES sessions(id),
    role_generation_id TEXT REFERENCES role_generations(id),
    resume_invocation_id TEXT REFERENCES resume_invocations(id),
    transcript_epoch TEXT,
    check_run_id TEXT REFERENCES check_runs(id),
    process_identity_json TEXT CHECK(process_identity_json IS NULL OR
        (json_valid(process_identity_json) AND json_type(process_identity_json)='object')),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK(status IN ('pending','stop_requested','quiescent','recovery_required')),
    created_at TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    restore_invalidated_at TEXT,
    CHECK((target_kind='session' AND session_id IS NOT NULL AND role_generation_id IS NOT NULL
        AND transcript_epoch IS NOT NULL AND resume_invocation_id IS NULL AND check_run_id IS NULL)
       OR (target_kind='resume' AND session_id IS NOT NULL AND role_generation_id IS NOT NULL
        AND transcript_epoch IS NOT NULL AND resume_invocation_id IS NOT NULL AND check_run_id IS NULL)
       OR (target_kind='check' AND check_run_id IS NOT NULL AND session_id IS NULL
        AND role_generation_id IS NULL AND transcript_epoch IS NULL AND resume_invocation_id IS NULL)),
    CHECK(transcript_epoch IS NULL OR length(trim(transcript_epoch))>0)
);
CREATE UNIQUE INDEX recipe_schedule_stop_targets_session ON recipe_schedule_stop_targets(occurrence_id,session_id,transcript_epoch)
    WHERE target_kind='session';
CREATE UNIQUE INDEX recipe_schedule_stop_targets_resume ON recipe_schedule_stop_targets(occurrence_id,resume_invocation_id)
    WHERE target_kind='resume';
CREATE UNIQUE INDEX recipe_schedule_stop_targets_check ON recipe_schedule_stop_targets(occurrence_id,check_run_id)
    WHERE target_kind='check';

CREATE TRIGGER schedule_stop_target_scope_insert BEFORE INSERT ON recipe_schedule_stop_targets
WHEN NOT EXISTS(SELECT 1 FROM recipe_schedule_fires f JOIN recipe_schedule_authorizations a ON a.id=f.authorization_id
    WHERE f.id=NEW.occurrence_id AND f.authorization_id=NEW.authorization_id
      AND EXISTS(SELECT 1 FROM attempts member JOIN tasks task ON task.id=member.task_id
        WHERE member.id=NEW.attempt_id AND member.task_id=f.task_id AND task.project_id=a.project_id)
      AND (f.attempt_id=NEW.attempt_id OR (json_extract(a.scope_json,'$.schema')=2
        AND json_extract(a.scope_json,'$.preset')='build_and_review' AND EXISTS(SELECT 1 FROM single_agent_repair_receipts receipt
         JOIN single_agent_review_budgets budget ON budget.root_attempt_id=receipt.root_attempt_id
           AND budget.task_id=f.task_id AND budget.repair_attempt_id=receipt.child_attempt_id
         JOIN attempts child ON child.id=receipt.child_attempt_id AND child.parent_attempt_id=receipt.root_attempt_id
         JOIN workflow_run_bindings cb ON cb.attempt_id=child.id AND cb.parent_attempt_id=receipt.root_attempt_id
           AND cb.source_snapshot_id=receipt.snapshot_id AND cb.scope_hash=child.scope_hash
         JOIN rework_intents intent ON intent.id=receipt.rework_intent_id
           AND intent.parent_attempt_id=receipt.root_attempt_id AND intent.new_attempt_id=child.id
           AND intent.snapshot_id=receipt.snapshot_id AND intent.scope_hash=cb.scope_hash
           AND intent.configuration_hash IS child.configuration_hash
         JOIN single_agent_review_deliveries d ON d.id=receipt.delivery_id
           AND d.root_attempt_id=receipt.root_attempt_id AND d.attempt_id=receipt.root_attempt_id
           AND d.task_id=budget.task_id AND d.ordinal=1 AND d.state='finished'
           AND d.result_binding_id=receipt.result_binding_id AND d.snapshot_id=receipt.snapshot_id
           AND d.candidate_hash=receipt.candidate_hash
           AND d.definition_hash=json_extract(a.scope_json,'$.recipe.workflow_hash')
           AND d.reviewer_profile_revision_id=json_extract(a.scope_json,'$.reviewer_admission.profile_revision_id')
           AND d.reviewer_profile_hash=json_extract(a.scope_json,'$.reviewer_admission.profile_hash')
         JOIN workflow_result_bindings result ON result.id=receipt.result_binding_id
           AND result.attempt_id=receipt.root_attempt_id AND result.snapshot_id=receipt.snapshot_id
           AND result.candidate_hash=receipt.candidate_hash AND result.profile_binding_hash=d.profile_binding_hash
           AND result.selected_check_revision=d.selected_check_revision AND result.state='changes_requested'
         JOIN snapshots snapshot ON snapshot.id=receipt.snapshot_id AND snapshot.attempt_id=receipt.root_attempt_id
           AND snapshot.complete=1
         JOIN single_agent_review_results rejection ON rejection.delivery_id=d.id AND rejection.verdict='changes_requested'
         JOIN role_results report ON report.id=rejection.role_result_id AND report.outcome='review_complete'
           AND report.operation_id=rejection.operation_id AND report.session_id=d.session_id
           AND report.role_generation_id=d.role_generation_id AND report.consumed_at IS NOT NULL
           AND report.summary=rejection.summary
           AND json_extract(report.metadata_json,'$.delivery_id')=d.id
           AND json_extract(report.metadata_json,'$.result_binding_id')=receipt.result_binding_id
           AND json_extract(report.metadata_json,'$.candidate_hash')=receipt.candidate_hash
           AND json_extract(report.metadata_json,'$.verdict')='changes_requested'
           AND json_extract(report.metadata_json,'$.selected_check_revision')=d.selected_check_revision
           AND json_extract(report.metadata_json,'$.findings')=rejection.findings_json
         JOIN sessions session ON session.id=d.session_id AND session.role_generation_id=d.role_generation_id
           AND session.transcript_epoch=d.transcript_epoch AND session.native_session_id IS NOT NULL
           AND session.status='exited' AND session.resume_count=0
           AND json_extract(session.exit_json,'$.process_group_quiescent')=1
         JOIN role_generations generation ON generation.id=d.role_generation_id
           AND generation.attempt_id=receipt.root_attempt_id AND generation.role='single_agent_reviewer'
           AND generation.status='exited'
         JOIN role_credentials credential ON credential.id=d.credential_id AND credential.role_generation_id=generation.id
           AND credential.revoked_at IS NOT NULL
         JOIN execution_bindings execution ON execution.role_generation_id=generation.id
           AND execution.actor='single_agent_reviewer' AND execution.slot_key=generation.role
           AND execution.session_contract='fresh'
           AND execution.policy_hash=json_extract(a.scope_json,'$.reviewer_admission.policy_hash')
         JOIN operation_receipts operation ON operation.operation_id=report.operation_id
           AND operation.actor_key=generation.id AND operation.operation_kind='role_report'
           AND operation.request_hash=rejection.request_hash
         WHERE receipt.root_attempt_id=f.attempt_id AND receipt.child_attempt_id=NEW.attempt_id
           AND NOT EXISTS(SELECT 1 FROM sessions other WHERE other.id!=session.id
             AND other.provider=session.provider AND other.native_session_id=session.native_session_id))))
      AND f.stop_requested_at IS NOT NULL
      AND a.restore_invalidated_at IS NULL)
 OR (NEW.target_kind IN ('session','resume') AND NOT EXISTS(
    SELECT 1 FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
    WHERE s.id=NEW.session_id AND g.id=NEW.role_generation_id AND g.attempt_id=NEW.attempt_id
      AND s.schedule_occurrence_id=NEW.occurrence_id AND s.schedule_authorization_id=NEW.authorization_id
      AND ((NEW.target_kind='session' AND s.transcript_epoch=NEW.transcript_epoch
            AND s.resume_count=0 AND (NEW.process_identity_json IS NULL OR s.process_identity_json=NEW.process_identity_json))
        OR (NEW.target_kind='resume' AND EXISTS(SELECT 1 FROM resume_invocations r
            WHERE r.id=NEW.resume_invocation_id AND r.session_id=s.id
              AND r.transcript_epoch=NEW.transcript_epoch AND r.schedule_occurrence_id=NEW.occurrence_id
              AND r.schedule_authorization_id=NEW.authorization_id
              AND (NEW.process_identity_json IS NULL OR r.process_identity_json=NEW.process_identity_json))))))
 OR (NEW.target_kind='check' AND NOT EXISTS(SELECT 1 FROM check_runs c
    WHERE c.id=NEW.check_run_id AND c.attempt_id=NEW.attempt_id
      AND (NEW.process_identity_json IS NULL OR c.recovery_anchor_json=NEW.process_identity_json)))
BEGIN SELECT RAISE(ABORT,'schedule stop target does not match its exact occurrence invocation'); END;

-- Null means the exact invocation is awaiting durable identity publication.
-- Its first binding must satisfy the same receipt/member fences as insertion,
-- plus the current native epoch; concrete signal identities never change.
CREATE TRIGGER schedule_stop_target_identity_update BEFORE UPDATE ON recipe_schedule_stop_targets
WHEN (OLD.id,OLD.occurrence_id,OLD.authorization_id,OLD.attempt_id,OLD.target_kind,OLD.session_id,
      OLD.role_generation_id,OLD.resume_invocation_id,OLD.transcript_epoch,OLD.check_run_id,
      OLD.created_at)
 IS NOT (NEW.id,NEW.occurrence_id,NEW.authorization_id,NEW.attempt_id,NEW.target_kind,NEW.session_id,
      NEW.role_generation_id,NEW.resume_invocation_id,NEW.transcript_epoch,NEW.check_run_id,
      NEW.created_at)
 OR (OLD.process_identity_json IS NOT NEW.process_identity_json AND (
      OLD.process_identity_json IS NOT NULL OR NEW.process_identity_json IS NULL
      OR OLD.restore_invalidated_at IS NOT NULL OR NEW.restore_invalidated_at IS NOT NULL
      OR OLD.status='quiescent'
      -- Publication must also advance the existing observation/revision seam.
      OR (OLD.status,OLD.observed_at) IS (NEW.status,NEW.observed_at)
      OR NOT EXISTS(SELECT 1 FROM recipe_schedule_fires f
          WHERE f.id=NEW.occurrence_id AND f.outcome IN ('stop_requested','recovery_required'))
      OR (NEW.target_kind IN ('session','resume') AND NOT EXISTS(
          SELECT 1 FROM sessions s WHERE s.id=NEW.session_id
            AND s.role_generation_id=NEW.role_generation_id AND s.transcript_epoch=NEW.transcript_epoch
            AND ((NEW.target_kind='session' AND s.resume_count=0 AND s.launch_state!='reserved')
              OR (NEW.target_kind='resume' AND EXISTS(SELECT 1 FROM resume_invocations r
                WHERE r.id=NEW.resume_invocation_id AND r.session_id=s.id
                  AND r.transcript_epoch=s.transcript_epoch AND r.state!='reserved')))))
      OR (NOT EXISTS(SELECT 1 FROM recipe_schedule_fires f JOIN recipe_schedule_authorizations a ON a.id=f.authorization_id
    WHERE f.id=NEW.occurrence_id AND f.authorization_id=NEW.authorization_id
      AND EXISTS(SELECT 1 FROM attempts member JOIN tasks task ON task.id=member.task_id
        WHERE member.id=NEW.attempt_id AND member.task_id=f.task_id AND task.project_id=a.project_id)
      AND (f.attempt_id=NEW.attempt_id OR (json_extract(a.scope_json,'$.schema')=2
        AND json_extract(a.scope_json,'$.preset')='build_and_review' AND EXISTS(SELECT 1 FROM single_agent_repair_receipts receipt
         JOIN single_agent_review_budgets budget ON budget.root_attempt_id=receipt.root_attempt_id
           AND budget.task_id=f.task_id AND budget.repair_attempt_id=receipt.child_attempt_id
         JOIN attempts child ON child.id=receipt.child_attempt_id AND child.parent_attempt_id=receipt.root_attempt_id
         JOIN workflow_run_bindings cb ON cb.attempt_id=child.id AND cb.parent_attempt_id=receipt.root_attempt_id
           AND cb.source_snapshot_id=receipt.snapshot_id AND cb.scope_hash=child.scope_hash
         JOIN rework_intents intent ON intent.id=receipt.rework_intent_id
           AND intent.parent_attempt_id=receipt.root_attempt_id AND intent.new_attempt_id=child.id
           AND intent.snapshot_id=receipt.snapshot_id AND intent.scope_hash=cb.scope_hash
           AND intent.configuration_hash IS child.configuration_hash
         JOIN single_agent_review_deliveries d ON d.id=receipt.delivery_id
           AND d.root_attempt_id=receipt.root_attempt_id AND d.attempt_id=receipt.root_attempt_id
           AND d.task_id=budget.task_id AND d.ordinal=1 AND d.state='finished'
           AND d.result_binding_id=receipt.result_binding_id AND d.snapshot_id=receipt.snapshot_id
           AND d.candidate_hash=receipt.candidate_hash
           AND d.definition_hash=json_extract(a.scope_json,'$.recipe.workflow_hash')
           AND d.reviewer_profile_revision_id=json_extract(a.scope_json,'$.reviewer_admission.profile_revision_id')
           AND d.reviewer_profile_hash=json_extract(a.scope_json,'$.reviewer_admission.profile_hash')
         JOIN workflow_result_bindings result ON result.id=receipt.result_binding_id
           AND result.attempt_id=receipt.root_attempt_id AND result.snapshot_id=receipt.snapshot_id
           AND result.candidate_hash=receipt.candidate_hash AND result.profile_binding_hash=d.profile_binding_hash
           AND result.selected_check_revision=d.selected_check_revision AND result.state='changes_requested'
         JOIN snapshots snapshot ON snapshot.id=receipt.snapshot_id AND snapshot.attempt_id=receipt.root_attempt_id
           AND snapshot.complete=1
         JOIN single_agent_review_results rejection ON rejection.delivery_id=d.id AND rejection.verdict='changes_requested'
         JOIN role_results report ON report.id=rejection.role_result_id AND report.outcome='review_complete'
           AND report.operation_id=rejection.operation_id AND report.session_id=d.session_id
           AND report.role_generation_id=d.role_generation_id AND report.consumed_at IS NOT NULL
           AND report.summary=rejection.summary
           AND json_extract(report.metadata_json,'$.delivery_id')=d.id
           AND json_extract(report.metadata_json,'$.result_binding_id')=receipt.result_binding_id
           AND json_extract(report.metadata_json,'$.candidate_hash')=receipt.candidate_hash
           AND json_extract(report.metadata_json,'$.verdict')='changes_requested'
           AND json_extract(report.metadata_json,'$.selected_check_revision')=d.selected_check_revision
           AND json_extract(report.metadata_json,'$.findings')=rejection.findings_json
         JOIN sessions session ON session.id=d.session_id AND session.role_generation_id=d.role_generation_id
           AND session.transcript_epoch=d.transcript_epoch AND session.native_session_id IS NOT NULL
           AND session.status='exited' AND session.resume_count=0
           AND json_extract(session.exit_json,'$.process_group_quiescent')=1
         JOIN role_generations generation ON generation.id=d.role_generation_id
           AND generation.attempt_id=receipt.root_attempt_id AND generation.role='single_agent_reviewer'
           AND generation.status='exited'
         JOIN role_credentials credential ON credential.id=d.credential_id AND credential.role_generation_id=generation.id
           AND credential.revoked_at IS NOT NULL
         JOIN execution_bindings execution ON execution.role_generation_id=generation.id
           AND execution.actor='single_agent_reviewer' AND execution.slot_key=generation.role
           AND execution.session_contract='fresh'
           AND execution.policy_hash=json_extract(a.scope_json,'$.reviewer_admission.policy_hash')
         JOIN operation_receipts operation ON operation.operation_id=report.operation_id
           AND operation.actor_key=generation.id AND operation.operation_kind='role_report'
           AND operation.request_hash=rejection.request_hash
         WHERE receipt.root_attempt_id=f.attempt_id AND receipt.child_attempt_id=NEW.attempt_id
           AND NOT EXISTS(SELECT 1 FROM sessions other WHERE other.id!=session.id
             AND other.provider=session.provider AND other.native_session_id=session.native_session_id))))
      AND f.stop_requested_at IS NOT NULL
      AND a.restore_invalidated_at IS NULL)
 OR (NEW.target_kind IN ('session','resume') AND NOT EXISTS(
    SELECT 1 FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
    WHERE s.id=NEW.session_id AND g.id=NEW.role_generation_id AND g.attempt_id=NEW.attempt_id
      AND s.schedule_occurrence_id=NEW.occurrence_id AND s.schedule_authorization_id=NEW.authorization_id
      AND ((NEW.target_kind='session' AND s.transcript_epoch=NEW.transcript_epoch
            AND s.resume_count=0 AND (NEW.process_identity_json IS NULL OR s.process_identity_json=NEW.process_identity_json))
        OR (NEW.target_kind='resume' AND EXISTS(SELECT 1 FROM resume_invocations r
            WHERE r.id=NEW.resume_invocation_id AND r.session_id=s.id
              AND r.transcript_epoch=NEW.transcript_epoch AND r.schedule_occurrence_id=NEW.occurrence_id
              AND r.schedule_authorization_id=NEW.authorization_id
              AND (NEW.process_identity_json IS NULL OR r.process_identity_json=NEW.process_identity_json))))))
 OR (NEW.target_kind='check' AND NOT EXISTS(SELECT 1 FROM check_runs c
    WHERE c.id=NEW.check_run_id AND c.attempt_id=NEW.attempt_id
      AND (NEW.process_identity_json IS NULL OR c.recovery_anchor_json=NEW.process_identity_json))))))
 OR (OLD.restore_invalidated_at IS NOT NULL AND OLD.restore_invalidated_at IS NOT NEW.restore_invalidated_at)
BEGIN SELECT RAISE(ABORT,'schedule stop target identity is immutable'); END;
CREATE TRIGGER schedule_stop_target_history_delete BEFORE DELETE ON recipe_schedule_stop_targets
BEGIN SELECT RAISE(ABORT,'schedule stop target history is immutable'); END;
CREATE TRIGGER state_revision_recipe_schedule_stop_targets_insert AFTER INSERT ON recipe_schedule_stop_targets
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedule_stop_targets_update AFTER UPDATE ON recipe_schedule_stop_targets
WHEN (OLD.status,OLD.observed_at,OLD.restore_invalidated_at)
 IS NOT (NEW.status,NEW.observed_at,NEW.restore_invalidated_at)
BEGIN UPDATE state_revision SET revision=revision+1; END;

-- A consumed repair relationship cannot be erased and replaced with another child.
-- Update/rebinding protection is already supplied by migration 043.
CREATE TRIGGER single_agent_repair_receipts_immutable_delete
BEFORE DELETE ON single_agent_repair_receipts
BEGIN SELECT RAISE(ABORT,'review repair receipt history is immutable'); END;

-- Mirror permissions::expire_request at the first revocation boundary. The
-- session/occurrence binding limits invalidation; it never proves membership
-- or grants authority to a descendant. Native resolutions remain independent.
CREATE TRIGGER schedule_authorization_revoke_permissions
AFTER UPDATE OF revoked_at ON recipe_schedule_authorizations
WHEN OLD.revoked_at IS NULL AND NEW.revoked_at IS NOT NULL
BEGIN
    INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
    SELECT lower(hex(randomblob(16))),lower(hex(randomblob(16))),'service',
        CASE pr.delivery_state WHEN 'reserved' THEN 'permission.response.delivery_unknown'
            ELSE 'permission.response.deny_required' END,
        'permission_request',pr.id,
        json_object('delivery_state',CASE pr.delivery_state WHEN 'reserved' THEN 'unknown'
            ELSE 'deny_required' END,
            'reason','Scheduled authorization revoked: ' || NEW.revocation_reason,
            'local_transport_only',json('true')),
        strftime('%Y-%m-%dT%H:%M:%fZ','now')
    FROM permission_requests pr
    WHERE pr.delivery_state IN ('not_reserved','reserved')
      AND EXISTS(SELECT 1 FROM sessions s JOIN recipe_schedule_fires f
          ON f.id=s.schedule_occurrence_id AND f.authorization_id=s.schedule_authorization_id
          WHERE s.id=pr.session_id AND s.schedule_authorization_id=NEW.id
            AND s.role_generation_id=pr.role_generation_id AND f.task_id=pr.task_id)
      AND NOT EXISTS(SELECT 1 FROM permission_native_resolutions native
          WHERE native.permission_request_id=pr.id);

    UPDATE permission_requests SET
        state=CASE WHEN state='pending' THEN 'expired' ELSE state END,
        decision_kind=CASE WHEN state='pending' THEN COALESCE(decision_kind,'expired') ELSE decision_kind END,
        decision_actor=CASE WHEN state='pending' THEN COALESCE(decision_actor,'service_policy') ELSE decision_actor END,
        decision_reason=CASE WHEN state='pending' THEN COALESCE(decision_reason,
            'Scheduled authorization revoked: ' || NEW.revocation_reason) ELSE decision_reason END,
        decided_at=CASE WHEN state='pending' THEN COALESCE(decided_at,
            strftime('%Y-%m-%dT%H:%M:%fZ','now')) ELSE decided_at END,
        delivery_state=CASE delivery_state WHEN 'reserved' THEN 'unknown' ELSE 'deny_required' END,
        delivery_unknown_at=CASE WHEN delivery_state='reserved'
            THEN strftime('%Y-%m-%dT%H:%M:%fZ','now') ELSE delivery_unknown_at END,
        delivery_reason='Scheduled authorization revoked: ' || NEW.revocation_reason,
        revision=revision+1,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
    WHERE delivery_state IN ('not_reserved','reserved')
      AND EXISTS(SELECT 1 FROM sessions s JOIN recipe_schedule_fires f
          ON f.id=s.schedule_occurrence_id AND f.authorization_id=s.schedule_authorization_id
          WHERE s.id=permission_requests.session_id AND s.schedule_authorization_id=NEW.id
            AND s.role_generation_id=permission_requests.role_generation_id AND f.task_id=permission_requests.task_id)
      AND NOT EXISTS(SELECT 1 FROM permission_native_resolutions native
          WHERE native.permission_request_id=permission_requests.id);
END;
