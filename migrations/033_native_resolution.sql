-- Native permission resolution and native-turn report supersession.
--
-- Both are observed provenance stored beside the rows they describe. A
-- permission request keeps its application decision and response delivery,
-- and a role report keeps its outcome and workflow consumption; neither is
-- rewritten to express what the provider did natively. Rows are insert-only.
--
-- A request is linked to its own PermissionRequest hook only when exactly one
-- unlinked identical hook of the current invocation exists. An unlinked
-- request can never be natively resolved, so it stays actionable.
CREATE TABLE permission_request_hooks (
    permission_request_id TEXT PRIMARY KEY REFERENCES permission_requests(id),
    hook_event_id TEXT NOT NULL UNIQUE REFERENCES hook_events(id),
    created_at TEXT NOT NULL
);

CREATE TABLE permission_native_resolutions (
    permission_request_id TEXT PRIMARY KEY
        REFERENCES permission_request_hooks(permission_request_id),
    resolution_kind TEXT NOT NULL
        CHECK(resolution_kind IN ('tool_finished','tool_failed','native_denied')),
    request_hook_event_id TEXT NOT NULL REFERENCES hook_events(id),
    pre_tool_hook_event_id TEXT NOT NULL REFERENCES hook_events(id),
    resolving_hook_event_id TEXT NOT NULL UNIQUE REFERENCES hook_events(id),
    tool_use_id TEXT NOT NULL CHECK(length(trim(tool_use_id)) > 0),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    native_session_id TEXT NOT NULL,
    transcript_epoch TEXT NOT NULL,
    resume_invocation_id TEXT REFERENCES resume_invocations(id),
    settings_revision INTEGER NOT NULL,
    observed_at TEXT NOT NULL
);

-- A blocked or needs_input report its own session provably moved past: the
-- trusted UserPromptSubmit of an exact running resume of that session. When
-- the resume also cleared a matching resume rejection, it is recorded here.
CREATE TABLE role_result_supersessions (
    role_result_id TEXT PRIMARY KEY REFERENCES role_results(id),
    superseding_hook_event_id TEXT NOT NULL REFERENCES hook_events(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    native_session_id TEXT NOT NULL,
    transcript_epoch TEXT NOT NULL,
    resume_invocation_id TEXT NOT NULL REFERENCES resume_invocations(id),
    settings_revision INTEGER NOT NULL,
    superseded_rejection_event_id TEXT REFERENCES audit_events(id),
    audit_event_id TEXT NOT NULL REFERENCES audit_events(id),
    created_at TEXT NOT NULL
);

CREATE INDEX role_result_supersessions_hook
ON role_result_supersessions(superseding_hook_event_id);

CREATE TRIGGER state_revision_permission_request_hooks_insert AFTER INSERT ON permission_request_hooks
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_permission_request_hooks_update AFTER UPDATE ON permission_request_hooks
WHEN OLD.permission_request_id IS NOT NEW.permission_request_id
  OR OLD.hook_event_id IS NOT NEW.hook_event_id OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_permission_request_hooks_delete AFTER DELETE ON permission_request_hooks
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_permission_native_resolutions_insert AFTER INSERT ON permission_native_resolutions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_permission_native_resolutions_update AFTER UPDATE ON permission_native_resolutions
WHEN OLD.permission_request_id IS NOT NEW.permission_request_id
  OR OLD.resolution_kind IS NOT NEW.resolution_kind
  OR OLD.request_hook_event_id IS NOT NEW.request_hook_event_id
  OR OLD.pre_tool_hook_event_id IS NOT NEW.pre_tool_hook_event_id
  OR OLD.resolving_hook_event_id IS NOT NEW.resolving_hook_event_id
  OR OLD.tool_use_id IS NOT NEW.tool_use_id OR OLD.session_id IS NOT NEW.session_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id
  OR OLD.native_session_id IS NOT NEW.native_session_id
  OR OLD.transcript_epoch IS NOT NEW.transcript_epoch
  OR OLD.resume_invocation_id IS NOT NEW.resume_invocation_id
  OR OLD.settings_revision IS NOT NEW.settings_revision
  OR OLD.observed_at IS NOT NEW.observed_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_permission_native_resolutions_delete AFTER DELETE ON permission_native_resolutions
BEGIN UPDATE state_revision SET revision = revision + 1; END;

CREATE TRIGGER state_revision_role_result_supersessions_insert AFTER INSERT ON role_result_supersessions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_result_supersessions_update AFTER UPDATE ON role_result_supersessions
WHEN OLD.role_result_id IS NOT NEW.role_result_id
  OR OLD.superseding_hook_event_id IS NOT NEW.superseding_hook_event_id
  OR OLD.session_id IS NOT NEW.session_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id
  OR OLD.native_session_id IS NOT NEW.native_session_id
  OR OLD.transcript_epoch IS NOT NEW.transcript_epoch
  OR OLD.resume_invocation_id IS NOT NEW.resume_invocation_id
  OR OLD.settings_revision IS NOT NEW.settings_revision
  OR OLD.superseded_rejection_event_id IS NOT NEW.superseded_rejection_event_id
  OR OLD.audit_event_id IS NOT NEW.audit_event_id OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_result_supersessions_delete AFTER DELETE ON role_result_supersessions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
