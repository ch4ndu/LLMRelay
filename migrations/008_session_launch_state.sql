ALTER TABLE sessions ADD COLUMN launch_state TEXT NOT NULL DEFAULT 'reserved';
ALTER TABLE sessions ADD COLUMN launch_error TEXT;

UPDATE sessions
SET launch_state = CASE status
  WHEN 'running' THEN 'started'
  WHEN 'interrupt_requested' THEN 'started'
  WHEN 'exited' THEN 'finished'
  WHEN 'launch_failed' THEN 'failed'
  WHEN 'recovery_required' THEN 'delivery_unknown'
  WHEN 'launch_reserved' THEN 'delivery_unknown'
  ELSE 'reserved'
END;

CREATE INDEX IF NOT EXISTS sessions_launch_state_idx
ON sessions(status, launch_state);
