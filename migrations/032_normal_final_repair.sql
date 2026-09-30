-- Final-repair recheck receipts for the normal product path.
--
-- Migration 031 only admitted the historical recovery of an invalid sixth
-- ordinary code review, so it required that rejected request and its result.
-- A normal receipt is created when the first final review requests changes and
-- instead binds that authenticated final result. `provenance_kind` tells the
-- two apart and the CHECK constraints keep their bindings mutually exclusive.
--
-- The table is rebuilt with an explicit column list. Every existing row is a
-- historical recovery and keeps all identities, states and timestamps. Its
-- final result is recorded only when exactly one authenticated matching final
-- result exists; otherwise it stays NULL, which only historical rows allow.
-- The guard insert aborts the whole migration unless the copy is complete and
-- identical.
CREATE TABLE final_repair_rechecks_v32 (
    attempt_id TEXT PRIMARY KEY REFERENCES attempts(id),
    task_id TEXT NOT NULL REFERENCES tasks(id),
    provenance_kind TEXT NOT NULL
        CHECK(provenance_kind IN ('normal_final_request_changes','historical_sixth_review_recovery')),
    operation_id TEXT NOT NULL UNIQUE,
    request_hash TEXT NOT NULL,
    authorized_task_version INTEGER NOT NULL,
    plan_hash TEXT NOT NULL,
    configuration_hash TEXT,
    approved_code_request_id TEXT NOT NULL REFERENCES review_requests(id),
    prior_candidate_hash TEXT NOT NULL,
    final_request_id TEXT NOT NULL REFERENCES review_requests(id),
    final_result_id TEXT UNIQUE REFERENCES role_results(id),
    rejected_code_request_id TEXT UNIQUE REFERENCES review_requests(id),
    rejected_code_result_id TEXT UNIQUE REFERENCES role_results(id),
    reviewer_generation_id TEXT NOT NULL REFERENCES role_generations(id),
    reviewer_session_id TEXT NOT NULL REFERENCES sessions(id),
    reviewer_settings_revision INTEGER NOT NULL,
    reviewer_profile_hash TEXT NOT NULL,
    detail_json TEXT NOT NULL CHECK(json_valid(detail_json)),
    state TEXT NOT NULL CHECK(state IN ('authorized','reserved','spent','approved','closed')),
    candidate_hash TEXT,
    candidate_snapshot_id TEXT REFERENCES snapshots(id),
    review_request_id TEXT UNIQUE REFERENCES review_requests(id),
    spent_at TEXT,
    verdict TEXT CHECK(verdict IS NULL OR verdict IN ('approved','request_changes','needs_rework')),
    closed_reason TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK(provenance_kind!='normal_final_request_changes'
      OR (final_result_id IS NOT NULL
        AND rejected_code_request_id IS NULL AND rejected_code_result_id IS NULL)),
    CHECK(provenance_kind!='historical_sixth_review_recovery'
      OR (rejected_code_request_id IS NOT NULL AND rejected_code_result_id IS NOT NULL)),
    CHECK((state='authorized') = (review_request_id IS NULL)),
    CHECK((review_request_id IS NULL) = (candidate_hash IS NULL)),
    CHECK((review_request_id IS NULL) = (candidate_snapshot_id IS NULL)),
    CHECK((spent_at IS NULL) = (state IN ('authorized','reserved'))),
    CHECK((state='approved') = (verdict IS 'approved')),
    CHECK((state='closed') = (closed_reason IS NOT NULL))
);

INSERT INTO final_repair_rechecks_v32(
    attempt_id,task_id,provenance_kind,operation_id,request_hash,authorized_task_version,
    plan_hash,configuration_hash,approved_code_request_id,prior_candidate_hash,
    final_request_id,final_result_id,rejected_code_request_id,rejected_code_result_id,
    reviewer_generation_id,reviewer_session_id,reviewer_settings_revision,
    reviewer_profile_hash,detail_json,state,candidate_hash,candidate_snapshot_id,
    review_request_id,spent_at,verdict,closed_reason,created_at,updated_at)
SELECT old.attempt_id,old.task_id,'historical_sixth_review_recovery',old.operation_id,
    old.request_hash,old.authorized_task_version,old.plan_hash,old.configuration_hash,
    old.approved_code_request_id,old.prior_candidate_hash,old.final_request_id,
    (SELECT CASE WHEN COUNT(*)=1 THEN MIN(result.id) END
       FROM review_requests final
       JOIN role_results result ON result.role_generation_id=final.role_generation_id
         AND result.session_id=final.session_id
       JOIN role_generations verifier ON verifier.id=result.role_generation_id
         AND verifier.attempt_id=old.attempt_id AND verifier.role='final_verifier'
       WHERE final.id=old.final_request_id AND final.review_kind='final'
         AND result.outcome='request_changes' AND result.consumed_at IS NOT NULL
         AND json_extract(result.metadata_json,'$.review_request_id')=final.id
         AND json_extract(result.metadata_json,'$.review_kind')='final'
         AND json_extract(result.metadata_json,'$.candidate_hash')=final.candidate_hash),
    old.rejected_code_request_id,old.rejected_code_result_id,old.reviewer_generation_id,
    old.reviewer_session_id,old.reviewer_settings_revision,old.reviewer_profile_hash,
    old.detail_json,old.state,old.candidate_hash,old.candidate_snapshot_id,
    old.review_request_id,old.spent_at,old.verdict,old.closed_reason,old.created_at,
    old.updated_at
FROM final_repair_rechecks old;

CREATE TEMP TABLE final_repair_rechecks_v32_guard(copied INTEGER NOT NULL CHECK(copied=1));
INSERT INTO final_repair_rechecks_v32_guard(copied)
SELECT (SELECT COUNT(*) FROM final_repair_rechecks)
         =(SELECT COUNT(*) FROM final_repair_rechecks_v32)
   AND NOT EXISTS(SELECT 1 FROM final_repair_rechecks old
     LEFT JOIN final_repair_rechecks_v32 new ON new.attempt_id=old.attempt_id
     WHERE new.attempt_id IS NULL
        OR new.provenance_kind IS NOT 'historical_sixth_review_recovery'
        OR new.task_id IS NOT old.task_id OR new.operation_id IS NOT old.operation_id
        OR new.request_hash IS NOT old.request_hash
        OR new.authorized_task_version IS NOT old.authorized_task_version
        OR new.plan_hash IS NOT old.plan_hash
        OR new.configuration_hash IS NOT old.configuration_hash
        OR new.approved_code_request_id IS NOT old.approved_code_request_id
        OR new.prior_candidate_hash IS NOT old.prior_candidate_hash
        OR new.final_request_id IS NOT old.final_request_id
        OR new.rejected_code_request_id IS NOT old.rejected_code_request_id
        OR new.rejected_code_result_id IS NOT old.rejected_code_result_id
        OR new.reviewer_generation_id IS NOT old.reviewer_generation_id
        OR new.reviewer_session_id IS NOT old.reviewer_session_id
        OR new.reviewer_settings_revision IS NOT old.reviewer_settings_revision
        OR new.reviewer_profile_hash IS NOT old.reviewer_profile_hash
        OR new.detail_json IS NOT old.detail_json OR new.state IS NOT old.state
        OR new.candidate_hash IS NOT old.candidate_hash
        OR new.candidate_snapshot_id IS NOT old.candidate_snapshot_id
        OR new.review_request_id IS NOT old.review_request_id
        OR new.spent_at IS NOT old.spent_at OR new.verdict IS NOT old.verdict
        OR new.closed_reason IS NOT old.closed_reason
        OR new.created_at IS NOT old.created_at OR new.updated_at IS NOT old.updated_at);
DROP TABLE final_repair_rechecks_v32_guard;

DROP TABLE final_repair_rechecks;
ALTER TABLE final_repair_rechecks_v32 RENAME TO final_repair_rechecks;
