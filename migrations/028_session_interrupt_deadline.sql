-- A graceful-stop request has a durable clock.  The timestamp belongs to the
-- exact managed session and is cleared only after a verified exit or a lawful
-- later reservation for that same session.
ALTER TABLE sessions ADD COLUMN interrupt_requested_at TEXT;

CREATE INDEX sessions_interrupt_requested_deadline
ON sessions(status, interrupt_requested_at)
WHERE status = 'interrupt_requested' AND interrupt_requested_at IS NOT NULL;
