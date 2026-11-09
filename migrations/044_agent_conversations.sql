-- The runner disables foreign keys outside this transaction, verifies every
-- reference before commit, and restores enforcement on every exit path.
CREATE TABLE conversation_runtime (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    restore_epoch TEXT NOT NULL
);
INSERT INTO conversation_runtime VALUES(1,lower(hex(randomblob(16))));
CREATE TABLE conversation_profiles (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    execution_config_revision_id TEXT NOT NULL REFERENCES execution_config_revisions(id),
    profile_json TEXT NOT NULL CHECK(json_valid(profile_json)),
    profile_hash TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE conversation_admissions (
    id TEXT PRIMARY KEY,
    profile_revision_id TEXT NOT NULL REFERENCES conversation_profiles(id),
    identity_json TEXT NOT NULL CHECK(json_valid(identity_json)),
    identity_hash TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('prepared','probing','proven','active','invalidated','unknown')),
    probe_invocation_id TEXT REFERENCES provider_validation_invocations(id),
    evidence_hash TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE conversations (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    admission_id TEXT NOT NULL REFERENCES conversation_admissions(id),
    identity_json TEXT NOT NULL CHECK(json_valid(identity_json)),
    identity_hash TEXT NOT NULL,
    generation TEXT NOT NULL,
    native_thread_id TEXT,
    binding_valid INTEGER NOT NULL DEFAULT 1 CHECK(binding_valid IN (0,1)),
    state TEXT NOT NULL CHECK(state IN ('reserved','starting','idle','running','stopping','recovery_required','history_only')),
    active_turn_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE sessions_rebuilt (
    id TEXT PRIMARY KEY, role_generation_id TEXT REFERENCES role_generations(id),
    provider TEXT NOT NULL, validation_cell TEXT, status TEXT NOT NULL,
    launch_config_json TEXT NOT NULL, executable_version TEXT NOT NULL,
    native_session_id TEXT, native_identity_source TEXT, process_identity_json TEXT,
    transcript_epoch TEXT NOT NULL, transcript_last_sequence INTEGER NOT NULL DEFAULT 0,
    capture_state TEXT NOT NULL DEFAULT 'capturing', capture_error TEXT,
    hook_trust_state TEXT NOT NULL DEFAULT 'pending_observation', exit_json TEXT,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, native_identity_verified_at TEXT,
    readiness_state TEXT NOT NULL DEFAULT 'unknown', invocation_input_json TEXT,
    workflow_version TEXT NOT NULL DEFAULT 'trip-v1', workflow_hash TEXT NOT NULL DEFAULT '',
    prompt_hash TEXT NOT NULL DEFAULT '', launch_state TEXT NOT NULL DEFAULT 'reserved',
    launch_error TEXT, resume_count INTEGER NOT NULL DEFAULT 0, capability_key TEXT,
    capability_identity_json TEXT, recovery_root_pid INTEGER, recovery_process_group_id INTEGER,
    desired_running INTEGER NOT NULL DEFAULT 0, launch_boot_identity TEXT, recovery_anchor_json TEXT,
    initial_hook_event_boundary_rowid INTEGER NOT NULL DEFAULT 0, lane_id TEXT NOT NULL DEFAULT 'default',
    setup_permit_id TEXT, interrupt_requested_at TEXT,
    provider_validation_invocation_id TEXT REFERENCES provider_validation_invocations(id),
    conversation_id TEXT REFERENCES conversations(id),
    CHECK((role_generation_id IS NOT NULL)+(provider_validation_invocation_id IS NOT NULL)+(conversation_id IS NOT NULL)=1)
);
INSERT INTO sessions_rebuilt SELECT *,NULL FROM sessions;
DROP TABLE sessions;
ALTER TABLE sessions_rebuilt RENAME TO sessions;
CREATE INDEX sessions_launch_state_idx ON sessions(status,launch_state);
CREATE INDEX sessions_interrupt_requested_deadline ON sessions(status,interrupt_requested_at)
    WHERE status='interrupt_requested' AND interrupt_requested_at IS NOT NULL;
CREATE UNIQUE INDEX sessions_one_per_validation_invocation ON sessions(provider_validation_invocation_id)
    WHERE provider_validation_invocation_id IS NOT NULL;
CREATE UNIQUE INDEX sessions_one_live_conversation ON sessions(conversation_id)
    WHERE conversation_id IS NOT NULL AND status IN ('launch_reserved','running','interrupt_requested','recovery_required');
CREATE TRIGGER state_revision_sessions_insert AFTER INSERT ON sessions
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
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_sessions_delete AFTER DELETE ON sessions
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TABLE conversation_reservations (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations(id),
    session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id),
    repository_identity TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('reserved','running','stopping','unknown','released')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX conversation_repository_exclusive ON conversation_reservations(repository_identity)
    WHERE state!='released';
-- Both admissions are serialized by SQLite's write transaction, including
-- callers outside the scheduler. Uncertainty continues to occupy the slot.
CREATE TRIGGER claims_conversation_admission BEFORE INSERT ON claims
WHEN NEW.state IN ('reserved','launching','running','unknown','stopping')
BEGIN
    SELECT CASE WHEN EXISTS(SELECT 1 FROM conversation_reservations
        WHERE repository_identity=NEW.repository_identity AND state!='released')
        THEN RAISE(ABORT,'repository is reserved by a conversation') END;
    SELECT CASE WHEN (SELECT COUNT(*) FROM claims WHERE state IN ('reserved','launching','running','unknown','stopping'))
        +(SELECT COUNT(*) FROM conversation_reservations WHERE state!='released')>=2
        THEN RAISE(ABORT,'global active-work limit reached') END;
END;
CREATE TRIGGER claims_conversation_readmission BEFORE UPDATE OF state ON claims
WHEN NEW.state IN ('reserved','launching','running','unknown','stopping')
 AND OLD.state NOT IN ('reserved','launching','running','unknown','stopping')
BEGIN
    SELECT CASE WHEN EXISTS(SELECT 1 FROM conversation_reservations
        WHERE repository_identity=NEW.repository_identity AND state!='released')
        THEN RAISE(ABORT,'repository is reserved by a conversation') END;
    SELECT CASE WHEN (SELECT COUNT(*) FROM claims WHERE state IN ('reserved','launching','running','unknown','stopping'))
        +(SELECT COUNT(*) FROM conversation_reservations WHERE state!='released')>=2
        THEN RAISE(ABORT,'global active-work limit reached') END;
END;
CREATE TRIGGER conversation_task_admission BEFORE INSERT ON conversation_reservations
WHEN NEW.state!='released'
BEGIN
    SELECT CASE WHEN EXISTS(SELECT 1 FROM claims WHERE repository_identity=NEW.repository_identity
        AND state IN ('reserved','launching','running','unknown','stopping'))
        THEN RAISE(ABORT,'repository is reserved by a task') END;
    SELECT CASE WHEN (SELECT COUNT(*) FROM claims WHERE state IN ('reserved','launching','running','unknown','stopping'))
        +(SELECT COUNT(*) FROM conversation_reservations WHERE state!='released')>=2
        THEN RAISE(ABORT,'global active-work limit reached') END;
END;
CREATE TABLE conversation_operations (
    operation_id TEXT PRIMARY KEY,
    actor_key TEXT NOT NULL CHECK(actor_key='human_control'),
    request_hash TEXT NOT NULL,
    kind TEXT NOT NULL,
    conversation_id TEXT REFERENCES conversations(id),
    generation TEXT,
    native_thread_id TEXT,
    native_turn_id TEXT,
    state TEXT NOT NULL CHECK(state IN ('reserved','sending','bound','completed','failed','unknown')),
    input_json TEXT NOT NULL CHECK(json_valid(input_json)),
    result_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(result_json)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE conversation_events (
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    sequence INTEGER NOT NULL CHECK(sequence>0),
    generation TEXT NOT NULL,
    native_thread_id TEXT,
    native_turn_id TEXT,
    operation_id TEXT REFERENCES conversation_operations(operation_id),
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
    observed_at TEXT NOT NULL,
    PRIMARY KEY(conversation_id,sequence)
);
CREATE TABLE conversation_requests (
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    generation TEXT NOT NULL, native_thread_id TEXT NOT NULL, native_turn_id TEXT NOT NULL,
    request_id TEXT NOT NULL, method TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('denied','expired','acknowledged')),
    created_at TEXT NOT NULL,
    PRIMARY KEY(conversation_id,generation,request_id)
);
CREATE TRIGGER conversation_profile_immutable BEFORE UPDATE ON conversation_profiles
BEGIN SELECT RAISE(ABORT,'conversation profile revisions are immutable'); END;
CREATE TRIGGER state_revision_conversation_events_insert AFTER INSERT ON conversation_events
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_conversations_insert AFTER INSERT ON conversations
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_conversations_update AFTER UPDATE ON conversations
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_conversation_admissions_update AFTER UPDATE ON conversation_admissions
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_conversation_operations_insert AFTER INSERT ON conversation_operations
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_conversation_operations_update AFTER UPDATE ON conversation_operations
BEGIN UPDATE state_revision SET revision=revision+1; END;
