CREATE UNIQUE INDEX recipe_schedules_project_identity ON recipe_schedules(project_id,id);

CREATE TABLE recipe_schedule_authorizations (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    schedule_id TEXT NOT NULL,
    recipe_revision_id TEXT NOT NULL REFERENCES task_recipe_revisions(id),
    scope_json TEXT NOT NULL CHECK(json_valid(scope_json) AND json_type(scope_json)='object'),
    scope_hash TEXT NOT NULL CHECK(length(scope_hash)=64 AND scope_hash NOT GLOB '*[^0-9a-f]*'),
    max_occurrences INTEGER NOT NULL CHECK(typeof(max_occurrences)='integer' AND max_occurrences BETWEEN 1 AND 100),
    max_invocations_per_occurrence INTEGER NOT NULL CHECK(typeof(max_invocations_per_occurrence)='integer' AND max_invocations_per_occurrence BETWEEN 1 AND 8),
    max_runtime_seconds INTEGER NOT NULL CHECK(typeof(max_runtime_seconds)='integer' AND max_runtime_seconds BETWEEN 60 AND 3600),
    start_window_seconds INTEGER NOT NULL CHECK(typeof(start_window_seconds)='integer' AND start_window_seconds BETWEEN 1 AND 3600),
    created_by TEXT NOT NULL CHECK(length(trim(created_by))>0),
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    revoked_by TEXT,
    revocation_reason TEXT,
    restore_invalidated_at TEXT,
    UNIQUE(schedule_id,id),
    FOREIGN KEY(project_id,schedule_id) REFERENCES recipe_schedules(project_id,id),
    CHECK((revoked_at IS NULL AND revoked_by IS NULL AND revocation_reason IS NULL)
       OR (revoked_at IS NOT NULL AND revoked_by IS NOT NULL AND length(trim(revoked_by))>0
           AND revocation_reason IS NOT NULL AND length(trim(revocation_reason))>0))
);

ALTER TABLE recipe_schedules ADD COLUMN mode TEXT NOT NULL DEFAULT 'create_draft'
    CHECK(mode IN ('create_draft','run_authorized_research'));
ALTER TABLE recipe_schedules ADD COLUMN active_authorization_id TEXT REFERENCES recipe_schedule_authorizations(id);

DROP TRIGGER state_revision_recipe_schedule_fires_insert;
DROP TRIGGER state_revision_recipe_schedule_fires_update;
DROP TRIGGER state_revision_recipe_schedule_fires_delete;
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
    CHECK(outcome NOT IN ('waiting','reserved','running','stop_requested','review_ready','finished')
        OR (task_id IS NOT NULL AND allowance_reserved=1)),
    CHECK(outcome NOT IN ('reserved','running','stop_requested','review_ready')
        OR (attempt_id IS NOT NULL AND invocations_spent>0)),
    CHECK((first_reserved_at IS NULL) = (runtime_deadline_utc IS NULL)),
    CHECK((invocations_spent=0) = (first_reserved_at IS NULL)),
    CHECK(first_reserved_at IS NULL OR (allowance_reserved=1 AND attempt_id IS NOT NULL
        AND julianday(first_reserved_at) IS NOT NULL AND julianday(runtime_deadline_utc) IS NOT NULL
        AND julianday(runtime_deadline_utc)>julianday(first_reserved_at))),
    CHECK((stop_requested_at IS NULL) = (stop_reason IS NULL)),
    CHECK(stop_requested_at IS NULL OR invocations_spent>0),
    CHECK(outcome!='stop_requested' OR stop_requested_at IS NOT NULL),
    CHECK((stop_session_id IS NULL) = (stop_transcript_epoch IS NULL)),
    CHECK(stop_session_id IS NULL OR stop_requested_at IS NOT NULL),
    CHECK(stop_resume_invocation_id IS NULL OR stop_session_id IS NOT NULL),
    CHECK(stop_process_identity_json IS NULL OR stop_session_id IS NOT NULL)
);
INSERT INTO recipe_schedule_fires_rebuilt
    (schedule_id,scheduled_for_utc,recipe_revision_id,outcome,task_id,reason,
     missed_first_utc,missed_last_utc,missed_count,created_at)
SELECT schedule_id,scheduled_for_utc,recipe_revision_id,outcome,task_id,reason,
       missed_first_utc,missed_last_utc,missed_count,created_at FROM recipe_schedule_fires;
DROP TABLE recipe_schedule_fires;
ALTER TABLE recipe_schedule_fires_rebuilt RENAME TO recipe_schedule_fires;
CREATE UNIQUE INDEX recipe_schedule_fires_authorized_task ON recipe_schedule_fires(task_id)
    WHERE authorization_id IS NOT NULL AND task_id IS NOT NULL;
CREATE INDEX recipe_schedule_fires_authorization ON recipe_schedule_fires(authorization_id,allowance_reserved);
CREATE INDEX recipe_schedule_fires_pending ON recipe_schedule_fires(outcome,runtime_deadline_utc)
    WHERE authorization_id IS NOT NULL;
CREATE UNIQUE INDEX recipe_schedule_fires_one_execution_fence ON recipe_schedule_fires(schedule_id)
    WHERE authorization_id IS NOT NULL AND outcome IN ('reserved','running','stop_requested','recovery_required');

ALTER TABLE sessions ADD COLUMN schedule_occurrence_id TEXT REFERENCES recipe_schedule_fires(id);
ALTER TABLE sessions ADD COLUMN schedule_authorization_id TEXT REFERENCES recipe_schedule_authorizations(id);
ALTER TABLE resume_invocations ADD COLUMN schedule_occurrence_id TEXT REFERENCES recipe_schedule_fires(id);
ALTER TABLE resume_invocations ADD COLUMN schedule_authorization_id TEXT REFERENCES recipe_schedule_authorizations(id);
CREATE INDEX sessions_schedule_occurrence ON sessions(schedule_occurrence_id) WHERE schedule_occurrence_id IS NOT NULL;
CREATE INDEX resume_invocations_schedule_occurrence ON resume_invocations(schedule_occurrence_id) WHERE schedule_occurrence_id IS NOT NULL;

CREATE TRIGGER schedule_authorization_scope_insert BEFORE INSERT ON recipe_schedule_authorizations
WHEN NOT EXISTS(SELECT 1 FROM task_recipe_revisions rr JOIN task_recipes r ON r.id=rr.recipe_id
    WHERE rr.id=NEW.recipe_revision_id AND r.project_id=NEW.project_id
      AND rr.family='single_agent' AND rr.preset='repository_research')
BEGIN SELECT RAISE(ABORT,'schedule authorization requires a research recipe in its project'); END;
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
 OR (NEW.mode='run_authorized_research' AND NOT EXISTS(SELECT 1 FROM recipe_schedule_authorizations a
     WHERE a.id=NEW.active_authorization_id AND a.schedule_id=NEW.id AND a.project_id=NEW.project_id
       AND a.recipe_revision_id=NEW.recipe_revision_id AND a.revoked_at IS NULL AND a.restore_invalidated_at IS NULL))
BEGIN SELECT RAISE(ABORT,'schedule mode requires its own active authorization'); END;
CREATE TRIGGER schedule_active_authorization_update BEFORE UPDATE OF mode,active_authorization_id,project_id,recipe_revision_id ON recipe_schedules
WHEN (NEW.mode='create_draft' AND NEW.active_authorization_id IS NOT NULL)
 OR (NEW.mode='run_authorized_research' AND NOT EXISTS(SELECT 1 FROM recipe_schedule_authorizations a
     WHERE a.id=NEW.active_authorization_id AND a.schedule_id=NEW.id AND a.project_id=NEW.project_id
       AND a.recipe_revision_id=NEW.recipe_revision_id AND a.revoked_at IS NULL AND a.restore_invalidated_at IS NULL))
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
     SELECT 1 FROM recipe_schedule_fires f JOIN role_generations g ON g.attempt_id=f.attempt_id
     WHERE f.id=NEW.schedule_occurrence_id AND f.authorization_id=NEW.schedule_authorization_id
       AND g.id=NEW.role_generation_id AND f.allowance_reserved=1 AND f.invocations_spent>0))
BEGIN SELECT RAISE(ABORT,'session schedule binding does not match its occurrence attempt'); END;
CREATE TRIGGER sessions_schedule_binding_update BEFORE UPDATE OF schedule_occurrence_id,schedule_authorization_id,role_generation_id ON sessions
WHEN (NEW.schedule_occurrence_id IS NULL)!=(NEW.schedule_authorization_id IS NULL)
 OR (OLD.schedule_occurrence_id IS NOT NULL AND (OLD.schedule_occurrence_id,OLD.schedule_authorization_id)
     IS NOT (NEW.schedule_occurrence_id,NEW.schedule_authorization_id))
 OR (NEW.schedule_occurrence_id IS NOT NULL AND NOT EXISTS(
     SELECT 1 FROM recipe_schedule_fires f JOIN role_generations g ON g.attempt_id=f.attempt_id
     WHERE f.id=NEW.schedule_occurrence_id AND f.authorization_id=NEW.schedule_authorization_id
       AND g.id=NEW.role_generation_id AND f.allowance_reserved=1 AND f.invocations_spent>0))
BEGIN SELECT RAISE(ABORT,'session schedule binding does not match its occurrence attempt'); END;
CREATE TRIGGER resume_schedule_binding_insert BEFORE INSERT ON resume_invocations
WHEN NOT EXISTS(SELECT 1 FROM sessions s WHERE s.id=NEW.session_id
    AND s.schedule_occurrence_id IS NEW.schedule_occurrence_id
    AND s.schedule_authorization_id IS NEW.schedule_authorization_id)
BEGIN SELECT RAISE(ABORT,'resume schedule binding must match its session'); END;
CREATE TRIGGER resume_schedule_binding_update BEFORE UPDATE OF schedule_occurrence_id,schedule_authorization_id,session_id ON resume_invocations
WHEN (OLD.schedule_occurrence_id IS NOT NULL AND (OLD.schedule_occurrence_id,OLD.schedule_authorization_id)
     IS NOT (NEW.schedule_occurrence_id,NEW.schedule_authorization_id))
 OR NOT EXISTS(SELECT 1 FROM sessions s WHERE s.id=NEW.session_id
    AND s.schedule_occurrence_id IS NEW.schedule_occurrence_id
    AND s.schedule_authorization_id IS NEW.schedule_authorization_id)
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

DROP TRIGGER state_revision_sessions_update;
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

DROP TRIGGER state_revision_resume_invocations_update;
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

-- Extend existing cursor triggers rather than counting one update twice.
DROP TRIGGER state_revision_recipe_schedules_update;
CREATE TRIGGER state_revision_recipe_schedules_update AFTER UPDATE ON recipe_schedules
WHEN (OLD.id,OLD.project_id,OLD.name,OLD.recipe_revision_id,OLD.cadence,OLD.anchor_utc,OLD.next_fire_utc,
 OLD.paused,OLD.version,OLD.archived_at,OLD.created_at,OLD.updated_at,OLD.mode,OLD.active_authorization_id)
 IS NOT (NEW.id,NEW.project_id,NEW.name,NEW.recipe_revision_id,NEW.cadence,NEW.anchor_utc,NEW.next_fire_utc,
 NEW.paused,NEW.version,NEW.archived_at,NEW.created_at,NEW.updated_at,NEW.mode,NEW.active_authorization_id)
BEGIN UPDATE state_revision SET revision=revision+1; END;
