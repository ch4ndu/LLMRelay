ALTER TABLE sessions ADD COLUMN initial_hook_event_boundary_rowid INTEGER NOT NULL DEFAULT 0;

ALTER TABLE resume_invocations ADD COLUMN hook_event_boundary_rowid INTEGER;

ALTER TABLE guidance_messages ADD COLUMN delivery_session_id TEXT REFERENCES sessions(id);
ALTER TABLE guidance_messages ADD COLUMN delivery_transcript_epoch TEXT;
ALTER TABLE guidance_messages ADD COLUMN delivery_resume_invocation_id TEXT REFERENCES resume_invocations(id);

CREATE INDEX guidance_delivery_identity
ON guidance_messages(
    role_generation_id,
    delivery_session_id,
    delivery_transcript_epoch,
    delivery_resume_invocation_id,
    state
);

-- Existing resumed epochs have no causal insertion boundary. Do not retain an
-- idle decision that the upgraded matcher cannot independently authorize.
UPDATE sessions
SET readiness_state = 'unknown'
WHERE EXISTS (
    SELECT 1
    FROM resume_invocations ri
    WHERE ri.session_id = sessions.id
      AND ri.transcript_epoch = sessions.transcript_epoch
      AND ri.hook_event_boundary_rowid IS NULL
);
