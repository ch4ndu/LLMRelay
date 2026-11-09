CREATE TABLE workflow_execution_boundaries (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    transcript_epoch TEXT NOT NULL,
    process_identity_hash TEXT NOT NULL,
    process_identity_json TEXT NOT NULL CHECK(json_valid(process_identity_json)),
    kind TEXT NOT NULL CHECK(kind IN ('started','ended')),
    observed_at TEXT NOT NULL,
    UNIQUE(session_id,role_generation_id,transcript_epoch,process_identity_hash,kind)
);

CREATE TABLE workflow_execution_boundary_conflicts (
    boundary_id TEXT NOT NULL REFERENCES workflow_execution_boundaries(id),
    fact_hash TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    PRIMARY KEY(boundary_id,fact_hash)
);

CREATE INDEX workflow_execution_boundaries_generation
ON workflow_execution_boundaries(role_generation_id,session_id,transcript_epoch);

CREATE TRIGGER state_revision_execution_boundary_insert AFTER INSERT ON workflow_execution_boundaries
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_execution_boundary_update AFTER UPDATE ON workflow_execution_boundaries
WHEN OLD.id IS NOT NEW.id OR OLD.session_id IS NOT NEW.session_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id
  OR OLD.transcript_epoch IS NOT NEW.transcript_epoch
  OR OLD.process_identity_hash IS NOT NEW.process_identity_hash
  OR OLD.process_identity_json IS NOT NEW.process_identity_json
  OR OLD.kind IS NOT NEW.kind OR OLD.observed_at IS NOT NEW.observed_at
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_execution_boundary_delete AFTER DELETE ON workflow_execution_boundaries
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_execution_boundary_conflict_insert AFTER INSERT ON workflow_execution_boundary_conflicts
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_execution_boundary_conflict_update AFTER UPDATE ON workflow_execution_boundary_conflicts
WHEN OLD.boundary_id IS NOT NEW.boundary_id OR OLD.fact_hash IS NOT NEW.fact_hash
  OR OLD.observed_at IS NOT NEW.observed_at
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_execution_boundary_conflict_delete AFTER DELETE ON workflow_execution_boundary_conflicts
BEGIN UPDATE state_revision SET revision=revision+1; END;
