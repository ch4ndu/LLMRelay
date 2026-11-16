-- Retain immutable schedule lineage and first-revocation history. The existing
-- revocation trigger invalidates permissions before retirement is committed.
UPDATE recipe_schedule_authorizations
SET revoked_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),
    revoked_by='execution_retirement',
    revocation_reason='Automatic recipe execution has been retired; schedules create drafts only'
WHERE revoked_at IS NULL;

-- Do not touch the columns watched by schedule_active_authorization_update.
UPDATE recipe_schedules
SET paused=1,next_fire_utc=NULL,version=version+1,
    updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE mode IN ('run_authorized_research','run_authorized_recipe');

UPDATE recipe_schedule_fires
SET outcome=CASE WHEN invocations_spent=0 THEN 'held' ELSE 'recovery_required' END,
    reason='Automatic recipe execution has been retired; reconcile recorded processes before cleanup'
WHERE authorization_id IS NOT NULL
  AND outcome IN ('waiting','reserved','running','stop_requested','recovery_required');
