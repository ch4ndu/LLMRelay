CREATE TABLE knowledge_entries (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    version INTEGER NOT NULL CHECK(version >= 1),
    latest_draft_id TEXT,
    approved_revision_id TEXT,
    archived_at TEXT,
    created_at TEXT NOT NULL,
    UNIQUE(project_id,id),
    FOREIGN KEY(id,latest_draft_id) REFERENCES knowledge_revisions(entry_id,id),
    FOREIGN KEY(id,approved_revision_id) REFERENCES knowledge_revisions(entry_id,id)
);
CREATE TABLE knowledge_revisions (
    id TEXT PRIMARY KEY,
    entry_id TEXT NOT NULL REFERENCES knowledge_entries(id),
    parent_revision_id TEXT REFERENCES knowledge_revisions(id),
    body_json TEXT NOT NULL CHECK(json_valid(body_json)),
    content_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(entry_id,id),
    FOREIGN KEY(entry_id,parent_revision_id) REFERENCES knowledge_revisions(entry_id,id)
);
CREATE TABLE knowledge_events (
    id TEXT PRIMARY KEY,
    entry_id TEXT NOT NULL REFERENCES knowledge_entries(id),
    revision_id TEXT,
    replaced_revision_id TEXT,
    kind TEXT NOT NULL CHECK(kind IN ('draft','approved','archived')),
    actor TEXT NOT NULL,
    entry_version INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY(entry_id,revision_id) REFERENCES knowledge_revisions(entry_id,id),
    FOREIGN KEY(entry_id,replaced_revision_id) REFERENCES knowledge_revisions(entry_id,id),
    UNIQUE(entry_id,entry_version)
);
CREATE TABLE knowledge_observations (
    id TEXT PRIMARY KEY,
    revision_id TEXT NOT NULL REFERENCES knowledge_revisions(id),
    observation_json TEXT NOT NULL CHECK(json_valid(observation_json)),
    fingerprint TEXT NOT NULL,
    checked_at TEXT NOT NULL
);
CREATE INDEX knowledge_observations_revision ON knowledge_observations(revision_id,checked_at);
CREATE TABLE knowledge_operation_receipts (
    operation_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    request_hash TEXT NOT NULL,
    result_json TEXT NOT NULL CHECK(json_valid(result_json)),
    created_at TEXT NOT NULL
);
CREATE TABLE knowledge_manifests (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    attempt_id TEXT UNIQUE REFERENCES attempts(id),
    conversation_operation_id TEXT UNIQUE REFERENCES conversation_operations(operation_id),
    manifest_json TEXT NOT NULL CHECK(json_valid(manifest_json)),
    manifest_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    CHECK((attempt_id IS NOT NULL) != (conversation_operation_id IS NOT NULL))
);
CREATE TABLE knowledge_manifest_items (
    manifest_id TEXT NOT NULL REFERENCES knowledge_manifests(id),
    position INTEGER NOT NULL CHECK(position >= 0 AND position < 16),
    entry_id TEXT NOT NULL,
    revision_id TEXT NOT NULL,
    PRIMARY KEY(manifest_id,position),
    FOREIGN KEY(entry_id,revision_id) REFERENCES knowledge_revisions(entry_id,id)
);
CREATE INDEX knowledge_entries_project ON knowledge_entries(project_id,id);

ALTER TABLE tasks ADD COLUMN knowledge_selection_json TEXT CHECK(knowledge_selection_json IS NULL OR json_valid(knowledge_selection_json));
ALTER TABLE tasks ADD COLUMN knowledge_preparation_json TEXT CHECK(knowledge_preparation_json IS NULL OR json_valid(knowledge_preparation_json));
DROP TRIGGER state_revision_tasks_update;
CREATE TRIGGER state_revision_tasks_update AFTER UPDATE ON tasks
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id OR OLD.title IS NOT NEW.title
  OR OLD.description IS NOT NEW.description
  OR OLD.acceptance_criteria_json IS NOT NEW.acceptance_criteria_json
  OR OLD.priority IS NOT NEW.priority OR OLD.manual_order IS NOT NEW.manual_order
  OR OLD.lifecycle IS NOT NEW.lifecycle OR OLD.attention IS NOT NEW.attention
  OR OLD.version IS NOT NEW.version OR OLD.archived_at IS NOT NEW.archived_at
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
  OR OLD.role_overrides_json IS NOT NEW.role_overrides_json OR OLD.ready_at IS NOT NEW.ready_at
  OR OLD.legacy_json IS NOT NEW.legacy_json
  OR OLD.knowledge_selection_json IS NOT NEW.knowledge_selection_json
  OR OLD.knowledge_preparation_json IS NOT NEW.knowledge_preparation_json
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER knowledge_entry_identity_update BEFORE UPDATE ON knowledge_entries
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id OR OLD.created_at IS NOT NEW.created_at
BEGIN SELECT RAISE(ABORT,'knowledge entry identity is immutable'); END;
CREATE TRIGGER knowledge_revision_immutable_update BEFORE UPDATE ON knowledge_revisions
WHEN OLD.id IS NOT NEW.id OR OLD.entry_id IS NOT NEW.entry_id
 OR OLD.parent_revision_id IS NOT NEW.parent_revision_id OR OLD.body_json IS NOT NEW.body_json
 OR OLD.content_hash IS NOT NEW.content_hash OR OLD.created_at IS NOT NEW.created_at
BEGIN SELECT RAISE(ABORT,'knowledge revisions are immutable'); END;
CREATE TRIGGER knowledge_revision_immutable_delete BEFORE DELETE ON knowledge_revisions
BEGIN SELECT RAISE(ABORT,'knowledge revisions are immutable'); END;
CREATE TRIGGER knowledge_event_immutable_update BEFORE UPDATE ON knowledge_events
WHEN OLD.id IS NOT NEW.id OR OLD.entry_id IS NOT NEW.entry_id
 OR OLD.revision_id IS NOT NEW.revision_id OR OLD.replaced_revision_id IS NOT NEW.replaced_revision_id
 OR OLD.kind IS NOT NEW.kind OR OLD.actor IS NOT NEW.actor
 OR OLD.entry_version IS NOT NEW.entry_version OR OLD.created_at IS NOT NEW.created_at
BEGIN SELECT RAISE(ABORT,'knowledge lifecycle events are immutable'); END;
CREATE TRIGGER knowledge_event_immutable_delete BEFORE DELETE ON knowledge_events
BEGIN SELECT RAISE(ABORT,'knowledge lifecycle events are immutable'); END;
CREATE TRIGGER knowledge_observation_immutable_update BEFORE UPDATE ON knowledge_observations
WHEN OLD.id IS NOT NEW.id OR OLD.revision_id IS NOT NEW.revision_id
 OR OLD.observation_json IS NOT NEW.observation_json OR OLD.fingerprint IS NOT NEW.fingerprint
 OR OLD.checked_at IS NOT NEW.checked_at
BEGIN SELECT RAISE(ABORT,'knowledge observations are immutable'); END;
CREATE TRIGGER knowledge_observation_immutable_delete BEFORE DELETE ON knowledge_observations
BEGIN SELECT RAISE(ABORT,'knowledge observations are immutable'); END;
CREATE TRIGGER knowledge_receipt_immutable_update BEFORE UPDATE ON knowledge_operation_receipts
WHEN OLD.operation_id IS NOT NEW.operation_id OR OLD.project_id IS NOT NEW.project_id
 OR OLD.request_hash IS NOT NEW.request_hash OR OLD.result_json IS NOT NEW.result_json
 OR OLD.created_at IS NOT NEW.created_at
BEGIN SELECT RAISE(ABORT,'knowledge receipts are immutable'); END;
CREATE TRIGGER knowledge_receipt_immutable_delete BEFORE DELETE ON knowledge_operation_receipts
BEGIN SELECT RAISE(ABORT,'knowledge receipts are immutable'); END;
CREATE TRIGGER knowledge_manifest_immutable_update BEFORE UPDATE ON knowledge_manifests
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id
 OR OLD.attempt_id IS NOT NEW.attempt_id OR OLD.conversation_operation_id IS NOT NEW.conversation_operation_id
 OR OLD.manifest_json IS NOT NEW.manifest_json OR OLD.manifest_hash IS NOT NEW.manifest_hash
 OR OLD.created_at IS NOT NEW.created_at
BEGIN SELECT RAISE(ABORT,'knowledge manifests are immutable'); END;
CREATE TRIGGER knowledge_manifest_immutable_delete BEFORE DELETE ON knowledge_manifests
BEGIN SELECT RAISE(ABORT,'knowledge manifests are immutable'); END;
CREATE TRIGGER knowledge_manifest_item_immutable_update BEFORE UPDATE ON knowledge_manifest_items
WHEN OLD.manifest_id IS NOT NEW.manifest_id OR OLD.position IS NOT NEW.position
 OR OLD.entry_id IS NOT NEW.entry_id OR OLD.revision_id IS NOT NEW.revision_id
BEGIN SELECT RAISE(ABORT,'knowledge manifest items are immutable'); END;
CREATE TRIGGER knowledge_manifest_item_immutable_delete BEFORE DELETE ON knowledge_manifest_items
BEGIN SELECT RAISE(ABORT,'knowledge manifest items are immutable'); END;

CREATE TRIGGER state_revision_knowledge_entry_insert AFTER INSERT ON knowledge_entries
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_knowledge_entry_update AFTER UPDATE ON knowledge_entries
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id OR OLD.version IS NOT NEW.version
 OR OLD.latest_draft_id IS NOT NEW.latest_draft_id OR OLD.approved_revision_id IS NOT NEW.approved_revision_id
 OR OLD.archived_at IS NOT NEW.archived_at OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_knowledge_entry_delete AFTER DELETE ON knowledge_entries
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_knowledge_revision_insert AFTER INSERT ON knowledge_revisions
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_knowledge_event_insert AFTER INSERT ON knowledge_events
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_knowledge_observation_insert AFTER INSERT ON knowledge_observations
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_knowledge_receipt_insert AFTER INSERT ON knowledge_operation_receipts
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_knowledge_manifest_insert AFTER INSERT ON knowledge_manifests
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_knowledge_manifest_item_insert AFTER INSERT ON knowledge_manifest_items
BEGIN UPDATE state_revision SET revision=revision+1; END;
