CREATE TABLE cmux_attachment_routes (
    id TEXT PRIMARY KEY,
    service_boot_id TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    transcript_epoch TEXT NOT NULL,
    process_identity_json TEXT NOT NULL,
    attachment_mode TEXT NOT NULL,
    workspace_id TEXT,
    surface_id TEXT,
    surface_state TEXT NOT NULL,
    attachment_state TEXT NOT NULL,
    resume_state TEXT NOT NULL,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK(attachment_mode IN ('watch','control')),
    CHECK(surface_state IN ('opening','open','failed','unknown')),
    CHECK(attachment_state IN ('pending','live','ended','failed')),
    CHECK(resume_state IN ('pending','attachment_only','unavailable'))
);

CREATE INDEX cmux_attachment_routes_binding
ON cmux_attachment_routes(
    service_boot_id,
    session_id,
    role_generation_id,
    transcript_epoch,
    attachment_mode,
    created_at
);
