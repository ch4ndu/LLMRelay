-- Persistent cmux presentation state.  Migration 024 remains immutable
-- audit/history data; these rows are the only routes the current dashboard
-- may create, focus, or control.
CREATE TABLE cmux_task_workspaces (
    id TEXT PRIMARY KEY,
    service_boot_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    generation INTEGER NOT NULL CHECK(generation > 0),
    workspace_id TEXT,
    opening_surface_id TEXT,
    state TEXT NOT NULL,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK(state IN ('opening','open','unknown','lost','retired','failed')),
    UNIQUE(service_boot_id, task_id, generation)
);

-- At most one unresolved durable reservation exists for one task in one
-- service boot.  Unknown is intentionally included: it must be discarded by
-- an authenticated human rather than retried automatically.
CREATE UNIQUE INDEX cmux_task_workspaces_current
ON cmux_task_workspaces(service_boot_id, task_id)
WHERE state IN ('opening','open','unknown');

CREATE INDEX cmux_task_workspaces_task_history
ON cmux_task_workspaces(task_id, created_at DESC);

CREATE TABLE cmux_session_surfaces (
    id TEXT PRIMARY KEY,
    task_workspace_id TEXT NOT NULL REFERENCES cmux_task_workspaces(id),
    service_boot_id TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    transcript_epoch TEXT NOT NULL,
    process_identity_json TEXT NOT NULL,
    binding_revision INTEGER NOT NULL CHECK(binding_revision > 0),
    workspace_id TEXT,
    surface_id TEXT,
    surface_state TEXT NOT NULL,
    attachment_state TEXT NOT NULL,
    desired_input_state TEXT NOT NULL,
    actual_input_state TEXT NOT NULL,
    control_revision INTEGER NOT NULL DEFAULT 0 CHECK(control_revision >= 0),
    applied_revision INTEGER NOT NULL DEFAULT 0 CHECK(applied_revision >= 0),
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK(surface_state IN ('opening','open','unknown','lost','retired','failed')),
    CHECK(attachment_state IN ('pending','live','ended','failed')),
    CHECK(desired_input_state IN ('view_only','control')),
    CHECK(actual_input_state IN ('view_only','control','blocked','lost')),
    CHECK(applied_revision <= control_revision)
);

-- This deliberately omits watch/control mode.  A current exact provider
-- binding owns one presentation surface, even if View and Take race.
CREATE UNIQUE INDEX cmux_session_surfaces_current_binding
ON cmux_session_surfaces(
    service_boot_id, session_id, role_generation_id, transcript_epoch,
    process_identity_json
)
WHERE (surface_state IN ('opening','open')
       AND attachment_state IN ('pending','live'))
   OR surface_state='unknown';

CREATE INDEX cmux_session_surfaces_workspace
ON cmux_session_surfaces(task_workspace_id, created_at DESC);

CREATE INDEX cmux_session_surfaces_session
ON cmux_session_surfaces(session_id, binding_revision DESC, created_at DESC);
