UPDATE capabilities
SET status = 'unverified',
    gaps_json = CASE
        WHEN EXISTS (
            SELECT 1
            FROM json_each(capabilities.gaps_json)
            WHERE value = 'historical Codex configuration predates the canonical control-socket denied-read and restricted-proxy floor; fresh native validation is required'
        ) THEN gaps_json
        ELSE json_insert(
            gaps_json,
            '$[#]',
            'historical Codex configuration predates the canonical control-socket denied-read and restricted-proxy floor; fresh native validation is required'
        )
    END
WHERE provider = 'codex'
  AND role != 'implementer'
  AND status = 'supported'
  AND COALESCE(
      json_extract(proof_json, '$.denied_read_floor.version'),
      ''
  ) != 'codex-denied-read-floor-v2';
