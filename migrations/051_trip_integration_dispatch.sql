-- Existing requests lack exact dispatch attribution and get no dispatch row.
CREATE TABLE trip_integration_dispatch (
    request_id TEXT NOT NULL PRIMARY KEY REFERENCES trip_integration_requests(id),
    session_id TEXT UNIQUE REFERENCES sessions(id),
    role_generation_id TEXT REFERENCES role_generations(id),
    transcript_epoch TEXT,
    CHECK ((session_id IS NULL AND role_generation_id IS NULL AND transcript_epoch IS NULL)
        OR (session_id IS NOT NULL AND role_generation_id IS NOT NULL AND transcript_epoch IS NOT NULL))
);
