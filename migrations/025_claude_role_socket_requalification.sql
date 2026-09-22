UPDATE capabilities
SET status = 'unverified',
    gaps_json = CASE
        WHEN EXISTS (
            SELECT 1
            FROM json_each(capabilities.gaps_json)
            WHERE value = 'historical Claude capability evidence predates the required canonical role.sock native sandbox policy; use Prepare exact runtime verification for this exact profile before launch or resume'
        ) THEN gaps_json
        ELSE json_insert(
            gaps_json,
            '$[#]',
            'historical Claude capability evidence predates the required canonical role.sock native sandbox policy; use Prepare exact runtime verification for this exact profile before launch or resume'
        )
    END
WHERE provider = 'claude'
  AND status = 'supported';

UPDATE trip_runtime_probes
SET state = 'stale',
    failure_reason = 'historical Claude runtime proof predates the required canonical role.sock native sandbox policy; prepare corrected runtime verification for this exact profile',
    updated_at = CURRENT_TIMESTAMP
WHERE json_extract(profile_json, '$.provider') = 'claude'
  AND state IN ('authorized', 'running', 'awaiting_resume', 'evidence_recorded', 'current', 'published');

UPDATE trip_runtime_admissions
SET state = 'stale',
    failure_reason = 'historical Claude runtime proof predates the required canonical role.sock native sandbox policy; prepare corrected runtime verification for the affected exact profile',
    updated_at = CURRENT_TIMESTAMP
WHERE EXISTS (
    SELECT 1
    FROM trip_runtime_probes probe
    WHERE probe.admission_id = trip_runtime_admissions.id
      AND probe.state = 'stale'
)
  AND state IN ('authorized', 'running', 'awaiting_publication', 'ready');
