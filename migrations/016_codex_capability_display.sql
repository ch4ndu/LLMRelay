UPDATE capabilities
SET status = 'unsupported'
WHERE provider = 'codex'
  AND role = 'implementer'
  AND status IN ('unverified', 'limited');

UPDATE capabilities
SET gaps_json = json_insert(
    gaps_json,
    '$[#]',
    'stock interactive Codex cannot attest that inherited native allow rules will emit PermissionRequest; central AgenticJira approval coverage cannot be guaranteed'
)
WHERE provider = 'codex'
  AND role = 'implementer'
  AND NOT EXISTS (
      SELECT 1
      FROM json_each(capabilities.gaps_json)
      WHERE value = 'stock interactive Codex cannot attest that inherited native allow rules will emit PermissionRequest; central AgenticJira approval coverage cannot be guaranteed'
  );
