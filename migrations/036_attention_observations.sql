-- Classification only; the revision tracks open rows, never private candidate bookkeeping.
CREATE TABLE attention_observations (
    id TEXT PRIMARY KEY,
    entity_kind TEXT NOT NULL CHECK(entity_kind IN (
        'session','attempt','guidance_message','permission_request','resume_rejection','role_lane')),
    entity_key TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN (
        'process_without_accepted_turn','quiet_turn','attempt_without_live_session',
        'guidance_unaccepted','permission_on_exited_session','resume_failure_after_acceptance',
        'busy_after_exit','recurring_block')),
    task_id TEXT,
    attempt_id TEXT,
    role TEXT,
    lane_id TEXT,
    role_generation_id TEXT,
    session_id TEXT,
    transcript_epoch TEXT,
    source_id TEXT,
    state TEXT NOT NULL CHECK(state IN ('candidate','open','resolved')),
    evidence_fingerprint TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    uncertain INTEGER NOT NULL DEFAULT 0 CHECK(uncertain IN (0,1)),
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    observed_since TEXT NOT NULL,
    last_eligible_at TEXT,
    confirmations INTEGER NOT NULL DEFAULT 0 CHECK(confirmations BETWEEN 0 AND 2),
    recurrence_count INTEGER NOT NULL DEFAULT 0 CHECK(recurrence_count BETWEEN 0 AND 2),
    last_counted_result_id TEXT,
    last_counted_result_rowid INTEGER,
    reset_watermark_rowid INTEGER,
    reset_evidence_json TEXT,
    opened_at TEXT,
    resolved_at TEXT,
    resolution_reason TEXT,
    UNIQUE(entity_kind, entity_key, kind),
    CHECK(state!='open' OR opened_at IS NOT NULL),
    CHECK((state='resolved') = (resolved_at IS NOT NULL)),
    CHECK((resolved_at IS NULL) = (resolution_reason IS NULL))
);

CREATE INDEX attention_observations_state ON attention_observations(state, kind);
CREATE INDEX attention_observations_attempt ON attention_observations(attempt_id, state);
CREATE INDEX attention_observations_session ON attention_observations(session_id, state);
CREATE INDEX role_results_session_generation ON role_results(session_id, role_generation_id, created_at);

CREATE TRIGGER state_revision_attention_observations_insert AFTER INSERT ON attention_observations
WHEN NEW.state='open'
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_attention_observations_update AFTER UPDATE ON attention_observations
WHEN (OLD.state='open') IS NOT (NEW.state='open')
  OR (NEW.state='open' AND (
    OLD.id IS NOT NEW.id OR OLD.entity_kind IS NOT NEW.entity_kind
    OR OLD.entity_key IS NOT NEW.entity_key OR OLD.kind IS NOT NEW.kind
    OR OLD.task_id IS NOT NEW.task_id OR OLD.attempt_id IS NOT NEW.attempt_id
    OR OLD.role IS NOT NEW.role OR OLD.lane_id IS NOT NEW.lane_id
    OR OLD.role_generation_id IS NOT NEW.role_generation_id
    OR OLD.session_id IS NOT NEW.session_id OR OLD.transcript_epoch IS NOT NEW.transcript_epoch
    OR OLD.source_id IS NOT NEW.source_id OR OLD.evidence_json IS NOT NEW.evidence_json
    OR OLD.uncertain IS NOT NEW.uncertain OR OLD.recurrence_count IS NOT NEW.recurrence_count
    OR OLD.last_counted_result_id IS NOT NEW.last_counted_result_id
    OR OLD.opened_at IS NOT NEW.opened_at))
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_attention_observations_delete AFTER DELETE ON attention_observations
WHEN OLD.state='open'
BEGIN UPDATE state_revision SET revision = revision + 1; END;
