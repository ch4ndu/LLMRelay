ALTER TABLE task_workflow_selections
ADD COLUMN workspace_mode TEXT NOT NULL DEFAULT 'isolated_worktree'
CHECK(workspace_mode IN ('isolated_worktree','main_checkout'));

DROP TRIGGER state_revision_task_workflow_selections_update;
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
  OR OLD.workspace_mode IS NOT NEW.workspace_mode
BEGIN UPDATE state_revision SET revision = revision + 1; END;
