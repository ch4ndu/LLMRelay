CREATE TABLE trip_legacy_migrations_next (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    from_workflow_id TEXT NOT NULL,
    to_workflow_id TEXT NOT NULL,
    preserved_json TEXT NOT NULL,
    reviewed_plan_hash TEXT NOT NULL,
    config_revision_id TEXT NOT NULL REFERENCES trip_config_revisions(id),
    authorized_at TEXT NOT NULL,
    target_workflow_hash TEXT,
    target_source_hash TEXT,
    target_overlay_hash TEXT,
    target_manifest_hash TEXT,
    UNIQUE(attempt_id, to_workflow_id)
);

INSERT INTO trip_legacy_migrations_next (
    id, attempt_id, from_workflow_id, to_workflow_id, preserved_json,
    reviewed_plan_hash, config_revision_id, authorized_at
)
SELECT id, attempt_id, from_workflow_id, to_workflow_id, preserved_json,
       reviewed_plan_hash, config_revision_id, authorized_at
FROM trip_legacy_migrations;

DROP TABLE trip_legacy_migrations;
ALTER TABLE trip_legacy_migrations_next RENAME TO trip_legacy_migrations;
