CREATE TABLE codex_usage_observations (
    provider TEXT NOT NULL CHECK(provider='codex'),
    native_thread_id TEXT NOT NULL,
    native_turn_id TEXT NOT NULL,
    response_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    transcript_epoch TEXT NOT NULL,
    invocation_start INTEGER NOT NULL,
    accepted_hook_event_id TEXT NOT NULL REFERENCES hook_events(id),
    input_tokens INTEGER NOT NULL CHECK(input_tokens BETWEEN 0 AND 9007199254740991),
    cached_input_tokens INTEGER NOT NULL CHECK(cached_input_tokens BETWEEN 0 AND input_tokens),
    cache_write_input_tokens INTEGER CHECK(cache_write_input_tokens BETWEEN 0 AND 9007199254740991),
    output_tokens INTEGER NOT NULL CHECK(output_tokens BETWEEN 0 AND 9007199254740991),
    reasoning_output_tokens INTEGER NOT NULL CHECK(reasoning_output_tokens BETWEEN 0 AND output_tokens),
    total_tokens INTEGER NOT NULL CHECK(total_tokens BETWEEN 0 AND 9007199254740991),
    source_version TEXT NOT NULL,
    contract_revision TEXT NOT NULL,
    source_identity_json TEXT NOT NULL,
    received_at TEXT NOT NULL,
    invalid INTEGER NOT NULL DEFAULT 0 CHECK(invalid IN (0,1)),
    conflict_owners_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(conflict_owners_json)),
    UNIQUE(provider,native_thread_id,native_turn_id,response_id)
);

CREATE INDEX codex_usage_task ON codex_usage_observations(task_id);
CREATE INDEX codex_usage_session_turn ON codex_usage_observations(session_id,accepted_hook_event_id);
CREATE INDEX codex_usage_invalid ON codex_usage_observations(invalid) WHERE invalid=1;

CREATE TRIGGER state_revision_codex_usage_insert AFTER INSERT ON codex_usage_observations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_codex_usage_update AFTER UPDATE ON codex_usage_observations
WHEN OLD.provider IS NOT NEW.provider OR OLD.native_thread_id IS NOT NEW.native_thread_id
  OR OLD.native_turn_id IS NOT NEW.native_turn_id OR OLD.response_id IS NOT NEW.response_id
  OR OLD.task_id IS NOT NEW.task_id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id OR OLD.session_id IS NOT NEW.session_id
  OR OLD.transcript_epoch IS NOT NEW.transcript_epoch OR OLD.invocation_start IS NOT NEW.invocation_start
  OR OLD.accepted_hook_event_id IS NOT NEW.accepted_hook_event_id
  OR OLD.input_tokens IS NOT NEW.input_tokens OR OLD.cached_input_tokens IS NOT NEW.cached_input_tokens
  OR OLD.cache_write_input_tokens IS NOT NEW.cache_write_input_tokens
  OR OLD.output_tokens IS NOT NEW.output_tokens OR OLD.reasoning_output_tokens IS NOT NEW.reasoning_output_tokens
  OR OLD.total_tokens IS NOT NEW.total_tokens OR OLD.source_version IS NOT NEW.source_version
  OR OLD.contract_revision IS NOT NEW.contract_revision OR OLD.source_identity_json IS NOT NEW.source_identity_json
  OR OLD.received_at IS NOT NEW.received_at OR OLD.invalid IS NOT NEW.invalid
  OR OLD.conflict_owners_json IS NOT NEW.conflict_owners_json
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_codex_usage_delete AFTER DELETE ON codex_usage_observations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
