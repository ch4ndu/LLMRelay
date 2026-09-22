UPDATE capabilities
SET status = 'unsupported',
    gaps_json = '["stock interactive Codex cannot attest that inherited native allow rules will emit PermissionRequest; central AgenticJira approval coverage cannot be guaranteed"]'
WHERE provider = 'codex'
  AND role = 'implementer'
  AND status = 'supported';
