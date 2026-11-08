ALTER TABLE guidance_messages ADD COLUMN message_revision INTEGER NOT NULL DEFAULT 1;
CREATE TABLE task_comments (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    body TEXT NOT NULL,
    actor_kind TEXT NOT NULL CHECK(actor_kind='human'),
    created_at TEXT NOT NULL
);
CREATE INDEX task_comments_task ON task_comments(task_id,created_at,id);
CREATE TRIGGER state_revision_task_comments_insert AFTER INSERT ON task_comments BEGIN
    UPDATE state_revision SET revision=revision+1 WHERE singleton=1;
END;
CREATE TRIGGER state_revision_task_comments_update AFTER UPDATE ON task_comments
WHEN OLD.id IS NOT NEW.id OR OLD.task_id IS NOT NEW.task_id
  OR OLD.body IS NOT NEW.body OR OLD.actor_kind IS NOT NEW.actor_kind
  OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision=revision+1 WHERE singleton=1; END;
CREATE TRIGGER state_revision_task_comments_delete AFTER DELETE ON task_comments BEGIN
    UPDATE state_revision SET revision=revision+1 WHERE singleton=1;
END;

DROP TRIGGER IF EXISTS state_revision_guidance_messages_update;
CREATE TRIGGER state_revision_guidance_messages_update AFTER UPDATE ON guidance_messages
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id OR OLD.body IS NOT NEW.body
  OR OLD.state IS NOT NEW.state OR OLD.reason IS NOT NEW.reason
  OR OLD.created_at IS NOT NEW.created_at OR OLD.acknowledged_at IS NOT NEW.acknowledged_at
  OR OLD.written_at IS NOT NEW.written_at OR OLD.submitted_at IS NOT NEW.submitted_at
  OR OLD.delivery_session_id IS NOT NEW.delivery_session_id
  OR OLD.delivery_transcript_epoch IS NOT NEW.delivery_transcript_epoch
  OR OLD.delivery_resume_invocation_id IS NOT NEW.delivery_resume_invocation_id
  OR OLD.message_revision IS NOT NEW.message_revision
  OR (OLD.submitted_text IS NULL AND OLD.submitted_digest IS NULL)
     IS NOT (NEW.submitted_text IS NULL AND NEW.submitted_digest IS NULL)
BEGIN UPDATE state_revision SET revision=revision+1 WHERE singleton=1; END;
