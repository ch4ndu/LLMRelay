ALTER TABLE trip_setup_operations ADD COLUMN final_source_set_hash TEXT;
ALTER TABLE trip_setup_operations ADD COLUMN approved_source_set_hash TEXT;
ALTER TABLE trip_setup_operations ADD COLUMN finalized_at TEXT;
ALTER TABLE trip_setup_operations ADD COLUMN supersedes_setup_operation_id TEXT REFERENCES trip_setup_operations(id);

CREATE TABLE trip_frozen_install_files (
    setup_operation_id TEXT NOT NULL REFERENCES trip_setup_operations(id),
    relative_path TEXT NOT NULL,
    source_hash TEXT NOT NULL,
    preimage_hash TEXT,
    source_bytes BLOB NOT NULL,
    preimage_bytes BLOB,
    PRIMARY KEY(setup_operation_id, relative_path)
);

CREATE TABLE trip_setup_proof_reuse (
    setup_operation_id TEXT NOT NULL REFERENCES trip_setup_operations(id),
    role TEXT NOT NULL,
    profile_hash TEXT NOT NULL,
    source_receipt_id TEXT NOT NULL REFERENCES trip_preflight_receipts(id),
    approved_at TEXT NOT NULL,
    PRIMARY KEY(setup_operation_id, role)
);

CREATE INDEX trip_setup_supersession
ON trip_setup_operations(supersedes_setup_operation_id);
