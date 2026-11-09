CREATE TABLE task_conversation_sources (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id),
    bundle_json TEXT NOT NULL CHECK(json_valid(bundle_json)),
    bundle_hash TEXT NOT NULL
);
CREATE TRIGGER task_conversation_source_immutable_update BEFORE UPDATE ON task_conversation_sources
BEGIN SELECT RAISE(ABORT,'task conversation sources are immutable'); END;
CREATE TRIGGER task_conversation_source_immutable_delete BEFORE DELETE ON task_conversation_sources
BEGIN SELECT RAISE(ABORT,'task conversation sources are immutable'); END;
-- Preserve source history without blocking deletion of unrelated drafts.
CREATE TRIGGER task_conversation_source_task_delete BEFORE DELETE ON tasks
WHEN EXISTS(SELECT 1 FROM task_conversation_sources WHERE task_id=OLD.id)
BEGIN SELECT RAISE(ABORT,'This task has frozen conversation sources and cannot be deleted. Keep its source history; archive it when the task workflow permits.'); END;
CREATE TRIGGER state_revision_task_conversation_sources_insert AFTER INSERT ON task_conversation_sources
BEGIN UPDATE state_revision SET revision=revision+1; END;
