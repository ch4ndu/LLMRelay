-- Dedicated one-shot final-repair recheck accounting lane.
--
-- The single retained-reviewer code recheck of a final-repair candidate is
-- never an extension or reset of the ordinary review_budgets allowance. A row
-- exists only after an explicit authenticated recovery derived its historical
-- bindings from the ledger, and it is never deleted. The attempt key allows one
-- receipt per attempt and the request key binds exactly one review request;
-- existing review requests, verdicts and ordinary budgets are not rewritten.
--
-- The dashboard projection does not read this table, so it has no
-- state_revision triggers; every receipt transition also changes a projected
-- attempt, task or review request row.
CREATE TABLE final_repair_rechecks (
    attempt_id TEXT PRIMARY KEY REFERENCES attempts(id),
    task_id TEXT NOT NULL REFERENCES tasks(id),
    operation_id TEXT NOT NULL UNIQUE,
    request_hash TEXT NOT NULL,
    authorized_task_version INTEGER NOT NULL,
    plan_hash TEXT NOT NULL,
    configuration_hash TEXT,
    approved_code_request_id TEXT NOT NULL REFERENCES review_requests(id),
    prior_candidate_hash TEXT NOT NULL,
    final_request_id TEXT NOT NULL REFERENCES review_requests(id),
    rejected_code_request_id TEXT NOT NULL UNIQUE REFERENCES review_requests(id),
    rejected_code_result_id TEXT NOT NULL UNIQUE REFERENCES role_results(id),
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
    CHECK((state='authorized') = (review_request_id IS NULL)),
    CHECK((review_request_id IS NULL) = (candidate_hash IS NULL)),
    CHECK((review_request_id IS NULL) = (candidate_snapshot_id IS NULL)),
    CHECK((spent_at IS NULL) = (state IN ('authorized','reserved'))),
    CHECK((state='approved') = (verdict IS 'approved')),
    CHECK((state='closed') = (closed_reason IS NOT NULL))
);
