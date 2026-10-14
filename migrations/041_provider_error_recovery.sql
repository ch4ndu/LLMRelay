CREATE TABLE provider_failure_holds_new (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    transcript_epoch TEXT NOT NULL,
    accepted_hook_event_id TEXT REFERENCES hook_events(id),
    source TEXT NOT NULL DEFAULT 'failure_hook' CHECK(source IN ('failure_hook','codex_history')),
    failure_hook_event_id TEXT UNIQUE REFERENCES hook_events(id),
    history_observation_id TEXT UNIQUE REFERENCES audit_events(id),
    failure_kind TEXT NOT NULL,
    attribution TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT,
    state TEXT NOT NULL CHECK(state IN ('active','human_released','superseded')),
    resolved_at TEXT,
    resolution_kind TEXT CHECK(resolution_kind IN ('human_release','accepted_turn','role_replacement')),
    resolution_ref TEXT,
    CHECK((source='failure_hook' AND failure_hook_event_id IS NOT NULL
            AND history_observation_id IS NULL
            AND attribution IN ('prompt_id_matched','arrival_order','startup_invocation'))
       OR (source='codex_history' AND history_observation_id IS NOT NULL
            AND failure_hook_event_id IS NULL AND accepted_hook_event_id IS NOT NULL
            AND attribution='accepted_turn_history')),
    CHECK((state='active') = (resolved_at IS NULL)),
    CHECK((resolved_at IS NULL) = (resolution_kind IS NULL))
);

INSERT INTO provider_failure_holds_new (
    id,attempt_id,role_generation_id,session_id,transcript_epoch,accepted_hook_event_id,
    source,failure_hook_event_id,history_observation_id,failure_kind,attribution,
    created_at,expires_at,state,resolved_at,resolution_kind,resolution_ref
)
SELECT id,attempt_id,role_generation_id,session_id,transcript_epoch,accepted_hook_event_id,
       'failure_hook',failure_hook_event_id,NULL,failure_kind,attribution,
       created_at,expires_at,state,resolved_at,resolution_kind,resolution_ref
FROM provider_failure_holds;

DROP TABLE provider_failure_holds;
ALTER TABLE provider_failure_holds_new RENAME TO provider_failure_holds;

CREATE INDEX provider_failure_holds_attempt ON provider_failure_holds(attempt_id, state);
CREATE INDEX provider_failure_holds_session ON provider_failure_holds(session_id, state);

CREATE TRIGGER state_revision_provider_failure_holds_insert AFTER INSERT ON provider_failure_holds
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_provider_failure_holds_update AFTER UPDATE ON provider_failure_holds
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id
  OR OLD.session_id IS NOT NEW.session_id OR OLD.transcript_epoch IS NOT NEW.transcript_epoch
  OR OLD.accepted_hook_event_id IS NOT NEW.accepted_hook_event_id
  OR OLD.source IS NOT NEW.source
  OR OLD.failure_hook_event_id IS NOT NEW.failure_hook_event_id
  OR OLD.history_observation_id IS NOT NEW.history_observation_id
  OR OLD.failure_kind IS NOT NEW.failure_kind OR OLD.attribution IS NOT NEW.attribution
  OR OLD.created_at IS NOT NEW.created_at OR OLD.expires_at IS NOT NEW.expires_at
  OR OLD.state IS NOT NEW.state OR OLD.resolved_at IS NOT NEW.resolved_at
  OR OLD.resolution_kind IS NOT NEW.resolution_kind OR OLD.resolution_ref IS NOT NEW.resolution_ref
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_provider_failure_holds_delete AFTER DELETE ON provider_failure_holds
BEGIN UPDATE state_revision SET revision = revision + 1; END;
