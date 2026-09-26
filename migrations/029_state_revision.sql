-- Durable committed-state cursor for dashboard invalidation.
--
-- Every INSERT and DELETE, and every UPDATE that changes a stored value, on a
-- table the dashboard state projection reads directly or through its decision,
-- recovery, setup and permission helpers advances the revision inside the
-- writer's statement, so rolling the write back also restores the revision.
-- Each changed row adds one. Clients treat the value as opaque and compare it
-- only within one service incarnation. Integer overflow yields a REAL, which
-- the CHECK rejects, so exhaustion fails the write instead of wrapping.
--
-- UPDATE predicates compare every column except liveness progress that the
-- dashboard watchdog refreshes: session_processes.last_seen_at, input lease
-- renewal (input_leases.expires_at and updated_at), and per-frame transcript
-- progress (sessions.transcript_last_sequence and updated_at).
--
-- Tables the projection never reads have no triggers: check_processes,
-- cmux_attachment_routes, cmux_task_workspaces, config_revisions,
-- import_records, operation_receipts, reviews, trip_apply_journal,
-- trip_legacy_migrations and trip_setup_reads.
CREATE TABLE state_revision (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    revision INTEGER NOT NULL CHECK(typeof(revision) = 'integer' AND revision >= 0)
);

INSERT INTO state_revision(singleton,revision) VALUES(1,0);

CREATE TRIGGER state_revision_attempts_insert AFTER INSERT ON attempts
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_attempts_update AFTER UPDATE ON attempts
WHEN OLD.id IS NOT NEW.id OR OLD.task_id IS NOT NEW.task_id
  OR OLD.context_id IS NOT NEW.context_id OR OLD.phase IS NOT NEW.phase
  OR OLD.base_revision IS NOT NEW.base_revision
  OR OLD.configuration_revision IS NOT NEW.configuration_revision
  OR OLD.status IS NOT NEW.status OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at OR OLD.parent_attempt_id IS NOT NEW.parent_attempt_id
  OR OLD.plan_hash IS NOT NEW.plan_hash OR OLD.plan_approved_at IS NOT NEW.plan_approved_at
  OR OLD.candidate_hash IS NOT NEW.candidate_hash
  OR OLD.accepted_snapshot_id IS NOT NEW.accepted_snapshot_id
  OR OLD.scope_hash IS NOT NEW.scope_hash
  OR OLD.configuration_hash IS NOT NEW.configuration_hash
  OR OLD.step_budget IS NOT NEW.step_budget
  OR OLD.last_coordinator_at IS NOT NEW.last_coordinator_at
  OR OLD.workflow_version IS NOT NEW.workflow_version
  OR OLD.workflow_hash IS NOT NEW.workflow_hash
  OR OLD.setup_operation_id IS NOT NEW.setup_operation_id
  OR OLD.upstream_source_hash IS NOT NEW.upstream_source_hash
  OR OLD.overlay_hash IS NOT NEW.overlay_hash
  OR OLD.structured_plan_id IS NOT NEW.structured_plan_id
  OR OLD.final_repair_round IS NOT NEW.final_repair_round
  OR OLD.legacy_migration_required IS NOT NEW.legacy_migration_required
  OR OLD.manager_conformance_revision IS NOT NEW.manager_conformance_revision
  OR OLD.selected_checks_revision IS NOT NEW.selected_checks_revision
  OR OLD.human_acceptance_at IS NOT NEW.human_acceptance_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_attempts_delete AFTER DELETE ON attempts
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_audit_events_insert AFTER INSERT ON audit_events
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_audit_events_update AFTER UPDATE ON audit_events
WHEN OLD.id IS NOT NEW.id OR OLD.operation_id IS NOT NEW.operation_id
  OR OLD.actor_kind IS NOT NEW.actor_kind OR OLD.actor_id IS NOT NEW.actor_id
  OR OLD.event_code IS NOT NEW.event_code OR OLD.entity_kind IS NOT NEW.entity_kind
  OR OLD.entity_id IS NOT NEW.entity_id OR OLD.old_version IS NOT NEW.old_version
  OR OLD.new_version IS NOT NEW.new_version OR OLD.detail_json IS NOT NEW.detail_json
  OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_audit_events_delete AFTER DELETE ON audit_events
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_capabilities_insert AFTER INSERT ON capabilities
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_capabilities_update AFTER UPDATE ON capabilities
WHEN OLD.id IS NOT NEW.id OR OLD.provider IS NOT NEW.provider
  OR OLD.executable_version IS NOT NEW.executable_version OR OLD.role IS NOT NEW.role
  OR OLD.mode IS NOT NEW.mode OR OLD.config_hash IS NOT NEW.config_hash
  OR OLD.status IS NOT NEW.status OR OLD.evidence_reference IS NOT NEW.evidence_reference
  OR OLD.gaps_json IS NOT NEW.gaps_json OR OLD.checked_at IS NOT NEW.checked_at
  OR OLD.hook_hash IS NOT NEW.hook_hash OR OLD.proof_json IS NOT NEW.proof_json
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_capabilities_delete AFTER DELETE ON capabilities
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_check_runs_insert AFTER INSERT ON check_runs
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_check_runs_update AFTER UPDATE ON check_runs
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.candidate_hash IS NOT NEW.candidate_hash OR OLD.executable IS NOT NEW.executable
  OR OLD.arguments_json IS NOT NEW.arguments_json OR OLD.cwd IS NOT NEW.cwd
  OR OLD.status IS NOT NEW.status OR OLD.exit_code IS NOT NEW.exit_code
  OR OLD.evidence_json IS NOT NEW.evidence_json OR OLD.created_at IS NOT NEW.created_at
  OR OLD.finished_at IS NOT NEW.finished_at OR OLD.suite_name IS NOT NEW.suite_name
  OR OLD.check_suite_version IS NOT NEW.check_suite_version
  OR OLD.launch_state IS NOT NEW.launch_state OR OLD.launch_error IS NOT NEW.launch_error
  OR OLD.recovery_root_pid IS NOT NEW.recovery_root_pid
  OR OLD.recovery_process_group_id IS NOT NEW.recovery_process_group_id
  OR OLD.launch_boot_identity IS NOT NEW.launch_boot_identity
  OR OLD.recovery_anchor_json IS NOT NEW.recovery_anchor_json
  OR OLD.check_id IS NOT NEW.check_id
  OR OLD.selected_check_revision IS NOT NEW.selected_check_revision
  OR OLD.inputs_hash IS NOT NEW.inputs_hash
  OR OLD.acceptance_coverage_json IS NOT NEW.acceptance_coverage_json
  OR OLD.elapsed_millis IS NOT NEW.elapsed_millis
  OR OLD.freshness_state IS NOT NEW.freshness_state
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_check_runs_delete AFTER DELETE ON check_runs
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_check_suites_insert AFTER INSERT ON check_suites
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_check_suites_update AFTER UPDATE ON check_suites
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id OR OLD.name IS NOT NEW.name
  OR OLD.position IS NOT NEW.position OR OLD.executable IS NOT NEW.executable
  OR OLD.arguments_json IS NOT NEW.arguments_json
  OR OLD.timeout_seconds IS NOT NEW.timeout_seconds OR OLD.enabled IS NOT NEW.enabled
  OR OLD.version IS NOT NEW.version OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_check_suites_delete AFTER DELETE ON check_suites
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_claims_insert AFTER INSERT ON claims
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_claims_update AFTER UPDATE ON claims
WHEN OLD.id IS NOT NEW.id OR OLD.task_id IS NOT NEW.task_id
  OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.repository_identity IS NOT NEW.repository_identity OR OLD.state IS NOT NEW.state
  OR OLD.process_identity_json IS NOT NEW.process_identity_json
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_claims_delete AFTER DELETE ON claims
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_cmux_session_surfaces_insert AFTER INSERT ON cmux_session_surfaces
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_cmux_session_surfaces_update AFTER UPDATE ON cmux_session_surfaces
WHEN OLD.id IS NOT NEW.id OR OLD.task_workspace_id IS NOT NEW.task_workspace_id
  OR OLD.service_boot_id IS NOT NEW.service_boot_id OR OLD.session_id IS NOT NEW.session_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id
  OR OLD.transcript_epoch IS NOT NEW.transcript_epoch
  OR OLD.process_identity_json IS NOT NEW.process_identity_json
  OR OLD.binding_revision IS NOT NEW.binding_revision
  OR OLD.workspace_id IS NOT NEW.workspace_id OR OLD.surface_id IS NOT NEW.surface_id
  OR OLD.surface_state IS NOT NEW.surface_state
  OR OLD.attachment_state IS NOT NEW.attachment_state
  OR OLD.desired_input_state IS NOT NEW.desired_input_state
  OR OLD.actual_input_state IS NOT NEW.actual_input_state
  OR OLD.control_revision IS NOT NEW.control_revision
  OR OLD.applied_revision IS NOT NEW.applied_revision OR OLD.last_error IS NOT NEW.last_error
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_cmux_session_surfaces_delete AFTER DELETE ON cmux_session_surfaces
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_controls_insert AFTER INSERT ON controls
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_controls_update AFTER UPDATE ON controls
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id OR OLD.kind IS NOT NEW.kind
  OR OLD.state IS NOT NEW.state OR OLD.expected_version IS NOT NEW.expected_version
  OR OLD.payload_json IS NOT NEW.payload_json OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at
  OR OLD.requested_operation_id IS NOT NEW.requested_operation_id
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_controls_delete AFTER DELETE ON controls
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_freeze_intents_insert AFTER INSERT ON freeze_intents
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_freeze_intents_update AFTER UPDATE ON freeze_intents
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id OR OLD.kind IS NOT NEW.kind
  OR OLD.source_role_generation_id IS NOT NEW.source_role_generation_id
  OR OLD.state IS NOT NEW.state OR OLD.result_snapshot_id IS NOT NEW.result_snapshot_id
  OR OLD.error IS NOT NEW.error OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_freeze_intents_delete AFTER DELETE ON freeze_intents
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_guidance_messages_insert AFTER INSERT ON guidance_messages
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_guidance_messages_update AFTER UPDATE ON guidance_messages
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id OR OLD.body IS NOT NEW.body
  OR OLD.state IS NOT NEW.state OR OLD.reason IS NOT NEW.reason
  OR OLD.created_at IS NOT NEW.created_at OR OLD.acknowledged_at IS NOT NEW.acknowledged_at
  OR OLD.written_at IS NOT NEW.written_at OR OLD.submitted_at IS NOT NEW.submitted_at
  OR OLD.delivery_session_id IS NOT NEW.delivery_session_id
  OR OLD.delivery_transcript_epoch IS NOT NEW.delivery_transcript_epoch
  OR OLD.delivery_resume_invocation_id IS NOT NEW.delivery_resume_invocation_id
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_guidance_messages_delete AFTER DELETE ON guidance_messages
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_hook_events_insert AFTER INSERT ON hook_events
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_hook_events_update AFTER UPDATE ON hook_events
WHEN OLD.id IS NOT NEW.id OR OLD.session_id IS NOT NEW.session_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id OR OLD.provider IS NOT NEW.provider
  OR OLD.event_name IS NOT NEW.event_name OR OLD.native_session_id IS NOT NEW.native_session_id
  OR OLD.payload_json IS NOT NEW.payload_json OR OLD.peer_pid IS NOT NEW.peer_pid
  OR OLD.peer_process_group_id IS NOT NEW.peer_process_group_id
  OR OLD.peer_start_marker IS NOT NEW.peer_start_marker
  OR OLD.provenance_state IS NOT NEW.provenance_state OR OLD.received_at IS NOT NEW.received_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_hook_events_delete AFTER DELETE ON hook_events
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_implementation_lanes_insert AFTER INSERT ON implementation_lanes
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_implementation_lanes_update AFTER UPDATE ON implementation_lanes
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.lane_key IS NOT NEW.lane_key OR OLD.owned_paths_json IS NOT NEW.owned_paths_json
  OR OLD.shared_paths_json IS NOT NEW.shared_paths_json
  OR OLD.protected_paths_json IS NOT NEW.protected_paths_json
  OR OLD.dependencies_json IS NOT NEW.dependencies_json
  OR OLD.source_hashes_json IS NOT NEW.source_hashes_json
  OR OLD.frozen_seams_hash IS NOT NEW.frozen_seams_hash OR OLD.required IS NOT NEW.required
  OR OLD.state IS NOT NEW.state
  OR OLD.admitted_by_generation_id IS NOT NEW.admitted_by_generation_id
  OR OLD.yielded_at IS NOT NEW.yielded_at OR OLD.receipt_json IS NOT NEW.receipt_json
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_implementation_lanes_delete AFTER DELETE ON implementation_lanes
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_input_leases_insert AFTER INSERT ON input_leases
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_input_leases_update AFTER UPDATE ON input_leases
WHEN OLD.session_id IS NOT NEW.session_id OR OLD.lease_id_hash IS NOT NEW.lease_id_hash
  OR OLD.owner_kind IS NOT NEW.owner_kind OR OLD.owner_id IS NOT NEW.owner_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id
  OR OLD.process_identity_json IS NOT NEW.process_identity_json
  OR OLD.revoked_at IS NOT NEW.revoked_at OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_input_leases_delete AFTER DELETE ON input_leases
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_instance_settings_insert AFTER INSERT ON instance_settings
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_instance_settings_update AFTER UPDATE ON instance_settings
WHEN OLD.singleton IS NOT NEW.singleton OR OLD.version IS NOT NEW.version
  OR OLD.auto_resume_eligible IS NOT NEW.auto_resume_eligible
  OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_instance_settings_delete AFTER DELETE ON instance_settings
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_lane_generations_insert AFTER INSERT ON lane_generations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_lane_generations_update AFTER UPDATE ON lane_generations
WHEN OLD.lane_id IS NOT NEW.lane_id
  OR OLD.effective_generation_id IS NOT NEW.effective_generation_id
  OR OLD.pending_settings_revision IS NOT NEW.pending_settings_revision
  OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_lane_generations_delete AFTER DELETE ON lane_generations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_launch_permits_insert AFTER INSERT ON launch_permits
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_launch_permits_update AFTER UPDATE ON launch_permits
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id OR OLD.role IS NOT NEW.role
  OR OLD.settings_revision IS NOT NEW.settings_revision
  OR OLD.switch_intent_id IS NOT NEW.switch_intent_id OR OLD.state IS NOT NEW.state
  OR OLD.created_at IS NOT NEW.created_at OR OLD.consumed_at IS NOT NEW.consumed_at
  OR OLD.validation_dispatch IS NOT NEW.validation_dispatch OR OLD.lane_id IS NOT NEW.lane_id
  OR OLD.setup_permit_id IS NOT NEW.setup_permit_id
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_launch_permits_delete AFTER DELETE ON launch_permits
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_permission_requests_insert AFTER INSERT ON permission_requests
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_permission_requests_update AFTER UPDATE ON permission_requests
WHEN OLD.id IS NOT NEW.id OR OLD.hook_invocation_nonce IS NOT NEW.hook_invocation_nonce
  OR OLD.connection_nonce IS NOT NEW.connection_nonce OR OLD.provider IS NOT NEW.provider
  OR OLD.project_id IS NOT NEW.project_id OR OLD.task_id IS NOT NEW.task_id
  OR OLD.attempt_id IS NOT NEW.attempt_id OR OLD.session_id IS NOT NEW.session_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id OR OLD.role IS NOT NEW.role
  OR OLD.service_boot_id IS NOT NEW.service_boot_id
  OR OLD.native_session_id IS NOT NEW.native_session_id OR OLD.cwd IS NOT NEW.cwd
  OR OLD.policy_fingerprint IS NOT NEW.policy_fingerprint OR OLD.tool_name IS NOT NEW.tool_name
  OR OLD.input_digest IS NOT NEW.input_digest OR OLD.input_json IS NOT NEW.input_json
  OR OLD.requested_access_json IS NOT NEW.requested_access_json OR OLD.reason IS NOT NEW.reason
  OR OLD.command_display IS NOT NEW.command_display OR OLD.family_json IS NOT NEW.family_json
  OR OLD.family_unavailable_reason IS NOT NEW.family_unavailable_reason
  OR OLD.created_at IS NOT NEW.created_at OR OLD.deadline_at IS NOT NEW.deadline_at
  OR OLD.state IS NOT NEW.state OR OLD.revision IS NOT NEW.revision
  OR OLD.decision_kind IS NOT NEW.decision_kind OR OLD.decision_actor IS NOT NEW.decision_actor
  OR OLD.decision_reason IS NOT NEW.decision_reason
  OR OLD.matching_rule_id IS NOT NEW.matching_rule_id OR OLD.decided_at IS NOT NEW.decided_at
  OR OLD.delivery_state IS NOT NEW.delivery_state
  OR OLD.delivery_reserved_at IS NOT NEW.delivery_reserved_at
  OR OLD.reserved_behavior IS NOT NEW.reserved_behavior
  OR OLD.delivered_at IS NOT NEW.delivered_at
  OR OLD.delivery_unknown_at IS NOT NEW.delivery_unknown_at
  OR OLD.delivery_reason IS NOT NEW.delivery_reason OR OLD.consumed_at IS NOT NEW.consumed_at
  OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_permission_requests_delete AFTER DELETE ON permission_requests
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_permission_rules_insert AFTER INSERT ON permission_rules
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_permission_rules_update AFTER UPDATE ON permission_rules
WHEN OLD.id IS NOT NEW.id OR OLD.provider IS NOT NEW.provider
  OR OLD.project_id IS NOT NEW.project_id OR OLD.role IS NOT NEW.role
  OR OLD.lifetime IS NOT NEW.lifetime OR OLD.session_id IS NOT NEW.session_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id
  OR OLD.native_session_id IS NOT NEW.native_session_id
  OR OLD.registered_root IS NOT NEW.registered_root
  OR OLD.repository_identity IS NOT NEW.repository_identity
  OR OLD.worktree_path IS NOT NEW.worktree_path
  OR OLD.executable_kind IS NOT NEW.executable_kind
  OR OLD.executable_value IS NOT NEW.executable_value
  OR OLD.display_family IS NOT NEW.display_family
  OR OLD.policy_fingerprint IS NOT NEW.policy_fingerprint
  OR OLD.created_by IS NOT NEW.created_by OR OLD.created_at IS NOT NEW.created_at
  OR OLD.revoked_at IS NOT NEW.revoked_at OR OLD.revoked_by IS NOT NEW.revoked_by
  OR OLD.revoke_reason IS NOT NEW.revoke_reason OR OLD.last_used_at IS NOT NEW.last_used_at
  OR OLD.use_count IS NOT NEW.use_count OR OLD.revision IS NOT NEW.revision
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_permission_rules_delete AFTER DELETE ON permission_rules
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_projects_insert AFTER INSERT ON projects
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_projects_update AFTER UPDATE ON projects
WHEN OLD.id IS NOT NEW.id OR OLD.display_name IS NOT NEW.display_name
  OR OLD.repository_path IS NOT NEW.repository_path
  OR OLD.repository_identity IS NOT NEW.repository_identity
  OR OLD.base_revision IS NOT NEW.base_revision OR OLD.queue_paused IS NOT NEW.queue_paused
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
  OR OLD.version IS NOT NEW.version OR OLD.settings_json IS NOT NEW.settings_json
  OR OLD.internal_purpose IS NOT NEW.internal_purpose
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_projects_delete AFTER DELETE ON projects
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_recovery_records_insert AFTER INSERT ON recovery_records
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_recovery_records_update AFTER UPDATE ON recovery_records
WHEN OLD.id IS NOT NEW.id OR OLD.session_id IS NOT NEW.session_id
  OR OLD.attempt_id IS NOT NEW.attempt_id OR OLD.state IS NOT NEW.state
  OR OLD.process_identity_json IS NOT NEW.process_identity_json
  OR OLD.detail_json IS NOT NEW.detail_json OR OLD.resolved_at IS NOT NEW.resolved_at
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_recovery_records_delete AFTER DELETE ON recovery_records
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_restart_candidates_insert AFTER INSERT ON restart_candidates
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_restart_candidates_update AFTER UPDATE ON restart_candidates
WHEN OLD.session_id IS NOT NEW.session_id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.task_id IS NOT NEW.task_id OR OLD.source IS NOT NEW.source
  OR OLD.state IS NOT NEW.state OR OLD.reason IS NOT NEW.reason
  OR OLD.requested_by IS NOT NEW.requested_by OR OLD.result_json IS NOT NEW.result_json
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_restart_candidates_delete AFTER DELETE ON restart_candidates
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_resume_invocations_insert AFTER INSERT ON resume_invocations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_resume_invocations_update AFTER UPDATE ON resume_invocations
WHEN OLD.id IS NOT NEW.id OR OLD.session_id IS NOT NEW.session_id
  OR OLD.resume_ordinal IS NOT NEW.resume_ordinal
  OR OLD.transcript_epoch IS NOT NEW.transcript_epoch
  OR OLD.launch_config_json IS NOT NEW.launch_config_json
  OR OLD.capability_key IS NOT NEW.capability_key
  OR OLD.capability_identity_json IS NOT NEW.capability_identity_json
  OR OLD.state IS NOT NEW.state OR OLD.process_identity_json IS NOT NEW.process_identity_json
  OR OLD.error IS NOT NEW.error OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at OR OLD.prior_exit_json IS NOT NEW.prior_exit_json
  OR OLD.prior_transcript_epoch IS NOT NEW.prior_transcript_epoch
  OR OLD.prior_launch_boot_identity IS NOT NEW.prior_launch_boot_identity
  OR OLD.prior_recovery_anchor_json IS NOT NEW.prior_recovery_anchor_json
  OR OLD.prior_recovery_root_pid IS NOT NEW.prior_recovery_root_pid
  OR OLD.prior_recovery_process_group_id IS NOT NEW.prior_recovery_process_group_id
  OR OLD.hook_event_boundary_rowid IS NOT NEW.hook_event_boundary_rowid
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_resume_invocations_delete AFTER DELETE ON resume_invocations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_review_budgets_insert AFTER INSERT ON review_budgets
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_review_budgets_update AFTER UPDATE ON review_budgets
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.review_kind IS NOT NEW.review_kind
  OR OLD.initial_allowance IS NOT NEW.initial_allowance
  OR OLD.extension_allowance IS NOT NEW.extension_allowance OR OLD.spent IS NOT NEW.spent
  OR OLD.version IS NOT NEW.version
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_review_budgets_delete AFTER DELETE ON review_budgets
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_review_requests_insert AFTER INSERT ON review_requests
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_review_requests_update AFTER UPDATE ON review_requests
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.review_kind IS NOT NEW.review_kind OR OLD.candidate_hash IS NOT NEW.candidate_hash
  OR OLD.role_generation_id IS NOT NEW.role_generation_id
  OR OLD.prompt_hash IS NOT NEW.prompt_hash OR OLD.handoff_hash IS NOT NEW.handoff_hash
  OR OLD.delivery_state IS NOT NEW.delivery_state OR OLD.verdict IS NOT NEW.verdict
  OR OLD.feedback IS NOT NEW.feedback OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at OR OLD.session_id IS NOT NEW.session_id
  OR OLD.settings_revision IS NOT NEW.settings_revision
  OR OLD.budget_spent_at IS NOT NEW.budget_spent_at
  OR OLD.ambiguity_state IS NOT NEW.ambiguity_state OR OLD.resume_count IS NOT NEW.resume_count
  OR OLD.operation_key IS NOT NEW.operation_key OR OLD.prompt_text IS NOT NEW.prompt_text
  OR OLD.handoff_json IS NOT NEW.handoff_json
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_review_requests_delete AFTER DELETE ON review_requests
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_rework_intents_insert AFTER INSERT ON rework_intents
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_rework_intents_update AFTER UPDATE ON rework_intents
WHEN OLD.id IS NOT NEW.id OR OLD.operation_id IS NOT NEW.operation_id
  OR OLD.parent_attempt_id IS NOT NEW.parent_attempt_id
  OR OLD.new_attempt_id IS NOT NEW.new_attempt_id OR OLD.snapshot_id IS NOT NEW.snapshot_id
  OR OLD.feedback IS NOT NEW.feedback OR OLD.carry_plan_approval IS NOT NEW.carry_plan_approval
  OR OLD.scope_hash IS NOT NEW.scope_hash
  OR OLD.configuration_hash IS NOT NEW.configuration_hash OR OLD.state IS NOT NEW.state
  OR OLD.result_json IS NOT NEW.result_json OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_rework_intents_delete AFTER DELETE ON rework_intents
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_credentials_insert AFTER INSERT ON role_credentials
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_credentials_update AFTER UPDATE ON role_credentials
WHEN OLD.id IS NOT NEW.id OR OLD.role_generation_id IS NOT NEW.role_generation_id
  OR OLD.token_hash IS NOT NEW.token_hash OR OLD.permissions_json IS NOT NEW.permissions_json
  OR OLD.revoked_at IS NOT NEW.revoked_at OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_credentials_delete AFTER DELETE ON role_credentials
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_generations_insert AFTER INSERT ON role_generations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_generations_update AFTER UPDATE ON role_generations
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id OR OLD.role IS NOT NEW.role
  OR OLD.provider IS NOT NEW.provider OR OLD.generation IS NOT NEW.generation
  OR OLD.config_revision IS NOT NEW.config_revision OR OLD.status IS NOT NEW.status
  OR OLD.authority_generation IS NOT NEW.authority_generation
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
  OR OLD.lane_id IS NOT NEW.lane_id
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_generations_delete AFTER DELETE ON role_generations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_results_insert AFTER INSERT ON role_results
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_results_update AFTER UPDATE ON role_results
WHEN OLD.id IS NOT NEW.id OR OLD.operation_id IS NOT NEW.operation_id
  OR OLD.session_id IS NOT NEW.session_id
  OR OLD.role_generation_id IS NOT NEW.role_generation_id OR OLD.outcome IS NOT NEW.outcome
  OR OLD.summary IS NOT NEW.summary OR OLD.evidence_json IS NOT NEW.evidence_json
  OR OLD.metadata_json IS NOT NEW.metadata_json OR OLD.created_at IS NOT NEW.created_at
  OR OLD.consumed_at IS NOT NEW.consumed_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_results_delete AFTER DELETE ON role_results
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_settings_insert AFTER INSERT ON role_settings
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_settings_update AFTER UPDATE ON role_settings
WHEN OLD.id IS NOT NEW.id OR OLD.task_id IS NOT NEW.task_id OR OLD.role IS NOT NEW.role
  OR OLD.revision IS NOT NEW.revision OR OLD.config_json IS NOT NEW.config_json
  OR OLD.effective_generation_id IS NOT NEW.effective_generation_id
  OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_role_settings_delete AFTER DELETE ON role_settings
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_scheduler_projects_insert AFTER INSERT ON scheduler_projects
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_scheduler_projects_update AFTER UPDATE ON scheduler_projects
WHEN OLD.project_id IS NOT NEW.project_id OR OLD.last_claimed_at IS NOT NEW.last_claimed_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_scheduler_projects_delete AFTER DELETE ON scheduler_projects
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_session_processes_insert AFTER INSERT ON session_processes
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_session_processes_update AFTER UPDATE ON session_processes
WHEN OLD.session_id IS NOT NEW.session_id OR OLD.pid IS NOT NEW.pid
  OR OLD.native_start_marker IS NOT NEW.native_start_marker
  OR OLD.process_group_id IS NOT NEW.process_group_id OR OLD.parent_pid IS NOT NEW.parent_pid
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_session_processes_delete AFTER DELETE ON session_processes
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_sessions_insert AFTER INSERT ON sessions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_sessions_update AFTER UPDATE ON sessions
WHEN OLD.id IS NOT NEW.id OR OLD.role_generation_id IS NOT NEW.role_generation_id
  OR OLD.provider IS NOT NEW.provider OR OLD.validation_cell IS NOT NEW.validation_cell
  OR OLD.status IS NOT NEW.status OR OLD.launch_config_json IS NOT NEW.launch_config_json
  OR OLD.executable_version IS NOT NEW.executable_version
  OR OLD.native_session_id IS NOT NEW.native_session_id
  OR OLD.native_identity_source IS NOT NEW.native_identity_source
  OR OLD.process_identity_json IS NOT NEW.process_identity_json
  OR OLD.transcript_epoch IS NOT NEW.transcript_epoch
  OR OLD.capture_state IS NOT NEW.capture_state OR OLD.capture_error IS NOT NEW.capture_error
  OR OLD.hook_trust_state IS NOT NEW.hook_trust_state OR OLD.exit_json IS NOT NEW.exit_json
  OR OLD.created_at IS NOT NEW.created_at
  OR OLD.native_identity_verified_at IS NOT NEW.native_identity_verified_at
  OR OLD.readiness_state IS NOT NEW.readiness_state
  OR OLD.invocation_input_json IS NOT NEW.invocation_input_json
  OR OLD.workflow_version IS NOT NEW.workflow_version
  OR OLD.workflow_hash IS NOT NEW.workflow_hash OR OLD.prompt_hash IS NOT NEW.prompt_hash
  OR OLD.launch_state IS NOT NEW.launch_state OR OLD.launch_error IS NOT NEW.launch_error
  OR OLD.resume_count IS NOT NEW.resume_count OR OLD.capability_key IS NOT NEW.capability_key
  OR OLD.capability_identity_json IS NOT NEW.capability_identity_json
  OR OLD.recovery_root_pid IS NOT NEW.recovery_root_pid
  OR OLD.recovery_process_group_id IS NOT NEW.recovery_process_group_id
  OR OLD.desired_running IS NOT NEW.desired_running
  OR OLD.launch_boot_identity IS NOT NEW.launch_boot_identity
  OR OLD.recovery_anchor_json IS NOT NEW.recovery_anchor_json
  OR OLD.initial_hook_event_boundary_rowid IS NOT NEW.initial_hook_event_boundary_rowid
  OR OLD.lane_id IS NOT NEW.lane_id OR OLD.setup_permit_id IS NOT NEW.setup_permit_id
  OR OLD.interrupt_requested_at IS NOT NEW.interrupt_requested_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_sessions_delete AFTER DELETE ON sessions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_snapshots_insert AFTER INSERT ON snapshots
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_snapshots_update AFTER UPDATE ON snapshots
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id OR OLD.kind IS NOT NEW.kind
  OR OLD.snapshot_base IS NOT NEW.snapshot_base OR OLD.manifest_hash IS NOT NEW.manifest_hash
  OR OLD.manifest_json IS NOT NEW.manifest_json OR OLD.complete IS NOT NEW.complete
  OR OLD.incomplete_reason IS NOT NEW.incomplete_reason OR OLD.created_at IS NOT NEW.created_at
  OR OLD.original_base IS NOT NEW.original_base OR OLD.candidate_head IS NOT NEW.candidate_head
  OR OLD.source_role_generation_id IS NOT NEW.source_role_generation_id
  OR OLD.source_settings_revision IS NOT NEW.source_settings_revision
  OR OLD.workspace_id IS NOT NEW.workspace_id OR OLD.workspace_hash IS NOT NEW.workspace_hash
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_snapshots_delete AFTER DELETE ON snapshots
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_switch_intents_insert AFTER INSERT ON switch_intents
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_switch_intents_update AFTER UPDATE ON switch_intents
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id OR OLD.role IS NOT NEW.role
  OR OLD.old_generation_id IS NOT NEW.old_generation_id
  OR OLD.requested_settings_revision IS NOT NEW.requested_settings_revision
  OR OLD.checkpoint_snapshot_id IS NOT NEW.checkpoint_snapshot_id
  OR OLD.handoff_json IS NOT NEW.handoff_json OR OLD.state IS NOT NEW.state
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
  OR OLD.operation_id IS NOT NEW.operation_id
  OR OLD.expected_task_version IS NOT NEW.expected_task_version
  OR OLD.config_json IS NOT NEW.config_json OR OLD.authority_fence IS NOT NEW.authority_fence
  OR OLD.new_generation_id IS NOT NEW.new_generation_id
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_switch_intents_delete AFTER DELETE ON switch_intents
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_task_dependencies_insert AFTER INSERT ON task_dependencies
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_task_dependencies_update AFTER UPDATE ON task_dependencies
WHEN OLD.task_id IS NOT NEW.task_id OR OLD.depends_on_task_id IS NOT NEW.depends_on_task_id
  OR OLD.integration_ref IS NOT NEW.integration_ref
  OR OLD.integration_commit IS NOT NEW.integration_commit
  OR OLD.verified_at IS NOT NEW.verified_at OR OLD.created_at IS NOT NEW.created_at
  OR OLD.integration_manifest_hash IS NOT NEW.integration_manifest_hash
  OR OLD.verification_json IS NOT NEW.verification_json
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_task_dependencies_delete AFTER DELETE ON task_dependencies
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_tasks_insert AFTER INSERT ON tasks
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_tasks_update AFTER UPDATE ON tasks
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id OR OLD.title IS NOT NEW.title
  OR OLD.description IS NOT NEW.description
  OR OLD.acceptance_criteria_json IS NOT NEW.acceptance_criteria_json
  OR OLD.priority IS NOT NEW.priority OR OLD.manual_order IS NOT NEW.manual_order
  OR OLD.lifecycle IS NOT NEW.lifecycle OR OLD.attention IS NOT NEW.attention
  OR OLD.version IS NOT NEW.version OR OLD.archived_at IS NOT NEW.archived_at
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
  OR OLD.role_overrides_json IS NOT NEW.role_overrides_json OR OLD.ready_at IS NOT NEW.ready_at
  OR OLD.legacy_json IS NOT NEW.legacy_json
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_tasks_delete AFTER DELETE ON tasks
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_attempt_profiles_insert AFTER INSERT ON trip_attempt_profiles
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_attempt_profiles_update AFTER UPDATE ON trip_attempt_profiles
WHEN OLD.attempt_id IS NOT NEW.attempt_id OR OLD.role IS NOT NEW.role
  OR OLD.settings_revision IS NOT NEW.settings_revision
  OR OLD.activation_id IS NOT NEW.activation_id OR OLD.source IS NOT NEW.source
  OR OLD.profile_json IS NOT NEW.profile_json OR OLD.profile_hash IS NOT NEW.profile_hash
  OR OLD.project_config_revision_id IS NOT NEW.project_config_revision_id
  OR OLD.project_configuration_hash IS NOT NEW.project_configuration_hash
  OR OLD.adapter_name IS NOT NEW.adapter_name OR OLD.adapter_hash IS NOT NEW.adapter_hash
  OR OLD.capability_id IS NOT NEW.capability_id OR OLD.capability_key IS NOT NEW.capability_key
  OR OLD.capability_proof_hash IS NOT NEW.capability_proof_hash
  OR OLD.bound_at IS NOT NEW.bound_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_attempt_profiles_delete AFTER DELETE ON trip_attempt_profiles
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_check_authorizations_insert AFTER INSERT ON trip_check_authorizations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_check_authorizations_update AFTER UPDATE ON trip_check_authorizations
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.check_id IS NOT NEW.check_id OR OLD.selected_revision IS NOT NEW.selected_revision
  OR OLD.exact_command_hash IS NOT NEW.exact_command_hash
  OR OLD.scope_hash IS NOT NEW.scope_hash OR OLD.decision IS NOT NEW.decision
  OR OLD.lifetime IS NOT NEW.lifetime OR OLD.created_at IS NOT NEW.created_at
  OR OLD.consumed_at IS NOT NEW.consumed_at OR OLD.matching_rule_id IS NOT NEW.matching_rule_id
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_check_authorizations_delete AFTER DELETE ON trip_check_authorizations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_check_permission_rules_insert AFTER INSERT ON trip_check_permission_rules
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_check_permission_rules_update AFTER UPDATE ON trip_check_permission_rules
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id
  OR OLD.source IS NOT NEW.source OR OLD.registered_root IS NOT NEW.registered_root
  OR OLD.repository_identity IS NOT NEW.repository_identity
  OR OLD.executable_kind IS NOT NEW.executable_kind
  OR OLD.executable_value IS NOT NEW.executable_value
  OR OLD.display_family IS NOT NEW.display_family OR OLD.created_by IS NOT NEW.created_by
  OR OLD.created_at IS NOT NEW.created_at OR OLD.revoked_at IS NOT NEW.revoked_at
  OR OLD.revoked_by IS NOT NEW.revoked_by OR OLD.revoke_reason IS NOT NEW.revoke_reason
  OR OLD.last_used_at IS NOT NEW.last_used_at OR OLD.use_count IS NOT NEW.use_count
  OR OLD.revision IS NOT NEW.revision
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_check_permission_rules_delete AFTER DELETE ON trip_check_permission_rules
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_config_revisions_insert AFTER INSERT ON trip_config_revisions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_config_revisions_update AFTER UPDATE ON trip_config_revisions
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id
  OR OLD.revision IS NOT NEW.revision OR OLD.state IS NOT NEW.state
  OR OLD.config_json IS NOT NEW.config_json OR OLD.adapters_json IS NOT NEW.adapters_json
  OR OLD.preflight_json IS NOT NEW.preflight_json
  OR OLD.verification_json IS NOT NEW.verification_json
  OR OLD.source_hash IS NOT NEW.source_hash OR OLD.overlay_hash IS NOT NEW.overlay_hash
  OR OLD.configuration_hash IS NOT NEW.configuration_hash
  OR OLD.created_at IS NOT NEW.created_at OR OLD.activated_at IS NOT NEW.activated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_config_revisions_delete AFTER DELETE ON trip_config_revisions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_conformance_receipts_insert AFTER INSERT ON trip_conformance_receipts
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_conformance_receipts_update AFTER UPDATE ON trip_conformance_receipts
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.revision IS NOT NEW.revision OR OLD.candidate_hash IS NOT NEW.candidate_hash
  OR OLD.config_hash IS NOT NEW.config_hash OR OLD.acceptance_json IS NOT NEW.acceptance_json
  OR OLD.ownership_json IS NOT NEW.ownership_json
  OR OLD.documentation_json IS NOT NEW.documentation_json
  OR OLD.test_policy_json IS NOT NEW.test_policy_json
  OR OLD.readability_json IS NOT NEW.readability_json
  OR OLD.submitted_by_generation_id IS NOT NEW.submitted_by_generation_id
  OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_conformance_receipts_delete AFTER DELETE ON trip_conformance_receipts
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_explorer_decisions_insert AFTER INSERT ON trip_explorer_decisions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_explorer_decisions_update AFTER UPDATE ON trip_explorer_decisions
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id OR OLD.stage IS NOT NEW.stage
  OR OLD.census_json IS NOT NEW.census_json OR OLD.trigger IS NOT NEW.trigger
  OR OLD.activated IS NOT NEW.activated OR OLD.limits_json IS NOT NEW.limits_json
  OR OLD.role_generation_id IS NOT NEW.role_generation_id
  OR OLD.candidate_hash IS NOT NEW.candidate_hash OR OLD.outcome_json IS NOT NEW.outcome_json
  OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_explorer_decisions_delete AFTER DELETE ON trip_explorer_decisions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_explorer_extensions_insert AFTER INSERT ON trip_explorer_extensions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_explorer_extensions_update AFTER UPDATE ON trip_explorer_extensions
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id OR OLD.stage IS NOT NEW.stage
  OR OLD.justification IS NOT NEW.justification OR OLD.authorized_at IS NOT NEW.authorized_at
  OR OLD.consumed_at IS NOT NEW.consumed_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_explorer_extensions_delete AFTER DELETE ON trip_explorer_extensions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_frozen_install_files_insert AFTER INSERT ON trip_frozen_install_files
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_frozen_install_files_update AFTER UPDATE ON trip_frozen_install_files
WHEN OLD.setup_operation_id IS NOT NEW.setup_operation_id
  OR OLD.relative_path IS NOT NEW.relative_path OR OLD.source_hash IS NOT NEW.source_hash
  OR OLD.preimage_hash IS NOT NEW.preimage_hash OR OLD.source_bytes IS NOT NEW.source_bytes
  OR OLD.preimage_bytes IS NOT NEW.preimage_bytes
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_frozen_install_files_delete AFTER DELETE ON trip_frozen_install_files
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_integration_requests_insert AFTER INSERT ON trip_integration_requests
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_integration_requests_update AFTER UPDATE ON trip_integration_requests
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.capsule_json IS NOT NEW.capsule_json
  OR OLD.requested_by_generation_id IS NOT NEW.requested_by_generation_id
  OR OLD.state IS NOT NEW.state OR OLD.created_at IS NOT NEW.created_at
  OR OLD.dispatched_at IS NOT NEW.dispatched_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_integration_requests_delete AFTER DELETE ON trip_integration_requests
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_preflight_receipts_insert AFTER INSERT ON trip_preflight_receipts
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_preflight_receipts_update AFTER UPDATE ON trip_preflight_receipts
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id
  OR OLD.setup_operation_id IS NOT NEW.setup_operation_id
  OR OLD.profile_id IS NOT NEW.profile_id OR OLD.profile_hash IS NOT NEW.profile_hash
  OR OLD.provider IS NOT NEW.provider OR OLD.role IS NOT NEW.role
  OR OLD.authority IS NOT NEW.authority OR OLD.session_mode IS NOT NEW.session_mode
  OR OLD.generation_id IS NOT NEW.generation_id OR OLD.result IS NOT NEW.result
  OR OLD.model_evidence IS NOT NEW.model_evidence OR OLD.evidence_json IS NOT NEW.evidence_json
  OR OLD.created_at IS NOT NEW.created_at OR OLD.capability_key IS NOT NEW.capability_key
  OR OLD.capability_identity_json IS NOT NEW.capability_identity_json
  OR OLD.adapter_hash IS NOT NEW.adapter_hash
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_preflight_receipts_delete AFTER DELETE ON trip_preflight_receipts
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_project_state_insert AFTER INSERT ON trip_project_state
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_project_state_update AFTER UPDATE ON trip_project_state
WHEN OLD.project_id IS NOT NEW.project_id OR OLD.readiness IS NOT NEW.readiness
  OR OLD.reason IS NOT NEW.reason OR OLD.detected_installation IS NOT NEW.detected_installation
  OR OLD.detected_json IS NOT NEW.detected_json
  OR OLD.setup_operation_id IS NOT NEW.setup_operation_id
  OR OLD.active_config_revision_id IS NOT NEW.active_config_revision_id
  OR OLD.workflow_id IS NOT NEW.workflow_id OR OLD.package_version IS NOT NEW.package_version
  OR OLD.upstream_source_hash IS NOT NEW.upstream_source_hash
  OR OLD.overlay_hash IS NOT NEW.overlay_hash OR OLD.manifest_hash IS NOT NEW.manifest_hash
  OR OLD.activated_at IS NOT NEW.activated_at OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_project_state_delete AFTER DELETE ON trip_project_state
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_runtime_admissions_insert AFTER INSERT ON trip_runtime_admissions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_runtime_admissions_update AFTER UPDATE ON trip_runtime_admissions
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id
  OR OLD.task_id IS NOT NEW.task_id OR OLD.scope_hash IS NOT NEW.scope_hash
  OR OLD.state IS NOT NEW.state OR OLD.fresh_call_count IS NOT NEW.fresh_call_count
  OR OLD.authorized_at IS NOT NEW.authorized_at OR OLD.failure_reason IS NOT NEW.failure_reason
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_runtime_admissions_delete AFTER DELETE ON trip_runtime_admissions
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_runtime_probes_insert AFTER INSERT ON trip_runtime_probes
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_runtime_probes_update AFTER UPDATE ON trip_runtime_probes
WHEN OLD.admission_id IS NOT NEW.admission_id OR OLD.role IS NOT NEW.role
  OR OLD.settings_revision IS NOT NEW.settings_revision
  OR OLD.launch_config_json IS NOT NEW.launch_config_json
  OR OLD.profile_json IS NOT NEW.profile_json OR OLD.profile_hash IS NOT NEW.profile_hash
  OR OLD.project_config_revision_id IS NOT NEW.project_config_revision_id
  OR OLD.project_configuration_hash IS NOT NEW.project_configuration_hash
  OR OLD.adapter_name IS NOT NEW.adapter_name OR OLD.adapter_hash IS NOT NEW.adapter_hash
  OR OLD.capability_key IS NOT NEW.capability_key
  OR OLD.capability_identity_json IS NOT NEW.capability_identity_json
  OR OLD.fixture_project_id IS NOT NEW.fixture_project_id
  OR OLD.fixture_repository_identity IS NOT NEW.fixture_repository_identity
  OR OLD.fixture_root IS NOT NEW.fixture_root OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.workspace_path IS NOT NEW.workspace_path
  OR OLD.service_sentinel_path IS NOT NEW.service_sentinel_path
  OR OLD.control_socket_path IS NOT NEW.control_socket_path OR OLD.nonce IS NOT NEW.nonce
  OR OLD.state IS NOT NEW.state OR OLD.session_id IS NOT NEW.session_id
  OR OLD.capability_id IS NOT NEW.capability_id OR OLD.failure_reason IS NOT NEW.failure_reason
  OR OLD.published_at IS NOT NEW.published_at OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at OR OLD.cmux_socket_path IS NOT NEW.cmux_socket_path
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_runtime_probes_delete AFTER DELETE ON trip_runtime_probes
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_selected_checks_insert AFTER INSERT ON trip_selected_checks
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_selected_checks_update AFTER UPDATE ON trip_selected_checks
WHEN OLD.attempt_id IS NOT NEW.attempt_id OR OLD.revision IS NOT NEW.revision
  OR OLD.check_id IS NOT NEW.check_id OR OLD.required IS NOT NEW.required
  OR OLD.selected_by_generation_id IS NOT NEW.selected_by_generation_id
  OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_selected_checks_delete AFTER DELETE ON trip_selected_checks
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_setup_operations_insert AFTER INSERT ON trip_setup_operations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_setup_operations_update AFTER UPDATE ON trip_setup_operations
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id
  OR OLD.fixture_project_id IS NOT NEW.fixture_project_id
  OR OLD.validation_task_id IS NOT NEW.validation_task_id
  OR OLD.discovery_attempt_id IS NOT NEW.discovery_attempt_id
  OR OLD.probe_attempt_id IS NOT NEW.probe_attempt_id OR OLD.state IS NOT NEW.state
  OR OLD.target_inventory_json IS NOT NEW.target_inventory_json
  OR OLD.proposal_json IS NOT NEW.proposal_json OR OLD.proposal_hash IS NOT NEW.proposal_hash
  OR OLD.selected_profiles_hash IS NOT NEW.selected_profiles_hash
  OR OLD.probe_authorized_at IS NOT NEW.probe_authorized_at
  OR OLD.install_authorized_at IS NOT NEW.install_authorized_at
  OR OLD.approved_preimages_hash IS NOT NEW.approved_preimages_hash
  OR OLD.error IS NOT NEW.error OR OLD.created_at IS NOT NEW.created_at
  OR OLD.updated_at IS NOT NEW.updated_at
  OR OLD.final_source_set_hash IS NOT NEW.final_source_set_hash
  OR OLD.approved_source_set_hash IS NOT NEW.approved_source_set_hash
  OR OLD.finalized_at IS NOT NEW.finalized_at
  OR OLD.supersedes_setup_operation_id IS NOT NEW.supersedes_setup_operation_id
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_setup_operations_delete AFTER DELETE ON trip_setup_operations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_setup_permits_insert AFTER INSERT ON trip_setup_permits
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_setup_permits_update AFTER UPDATE ON trip_setup_permits
WHEN OLD.id IS NOT NEW.id OR OLD.setup_operation_id IS NOT NEW.setup_operation_id
  OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.fixture_project_id IS NOT NEW.fixture_project_id
  OR OLD.fixture_repository_identity IS NOT NEW.fixture_repository_identity
  OR OLD.role IS NOT NEW.role OR OLD.profile_hash IS NOT NEW.profile_hash
  OR OLD.settings_revision IS NOT NEW.settings_revision OR OLD.purpose IS NOT NEW.purpose
  OR OLD.approved_action IS NOT NEW.approved_action OR OLD.nonce IS NOT NEW.nonce
  OR OLD.state IS NOT NEW.state OR OLD.created_at IS NOT NEW.created_at
  OR OLD.consumed_at IS NOT NEW.consumed_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_setup_permits_delete AFTER DELETE ON trip_setup_permits
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_setup_profile_selections_insert AFTER INSERT ON trip_setup_profile_selections
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_setup_profile_selections_update AFTER UPDATE ON trip_setup_profile_selections
WHEN OLD.setup_operation_id IS NOT NEW.setup_operation_id OR OLD.role IS NOT NEW.role
  OR OLD.selection_state IS NOT NEW.selection_state OR OLD.profile_json IS NOT NEW.profile_json
  OR OLD.profile_hash IS NOT NEW.profile_hash OR OLD.selected_at IS NOT NEW.selected_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_setup_profile_selections_delete AFTER DELETE ON trip_setup_profile_selections
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_setup_proof_reuse_insert AFTER INSERT ON trip_setup_proof_reuse
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_setup_proof_reuse_update AFTER UPDATE ON trip_setup_proof_reuse
WHEN OLD.setup_operation_id IS NOT NEW.setup_operation_id OR OLD.role IS NOT NEW.role
  OR OLD.profile_hash IS NOT NEW.profile_hash
  OR OLD.source_receipt_id IS NOT NEW.source_receipt_id
  OR OLD.approved_at IS NOT NEW.approved_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_setup_proof_reuse_delete AFTER DELETE ON trip_setup_proof_reuse
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_structured_plans_insert AFTER INSERT ON trip_structured_plans
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_structured_plans_update AFTER UPDATE ON trip_structured_plans
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.plan_hash IS NOT NEW.plan_hash OR OLD.plan_json IS NOT NEW.plan_json
  OR OLD.workflow_id IS NOT NEW.workflow_id
  OR OLD.profile_revision_id IS NOT NEW.profile_revision_id
  OR OLD.criteria_hash IS NOT NEW.criteria_hash
  OR OLD.verification_hash IS NOT NEW.verification_hash
  OR OLD.ownership_hash IS NOT NEW.ownership_hash
  OR OLD.conformance_hash IS NOT NEW.conformance_hash
  OR OLD.explorer_decision_id IS NOT NEW.explorer_decision_id
  OR OLD.review_request_id IS NOT NEW.review_request_id
  OR OLD.reviewed_at IS NOT NEW.reviewed_at OR OLD.approved_at IS NOT NEW.approved_at
  OR OLD.implementation_authorized_at IS NOT NEW.implementation_authorized_at
  OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_structured_plans_delete AFTER DELETE ON trip_structured_plans
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_task_profile_activations_insert AFTER INSERT ON trip_task_profile_activations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_task_profile_activations_update AFTER UPDATE ON trip_task_profile_activations
WHEN OLD.id IS NOT NEW.id OR OLD.task_id IS NOT NEW.task_id OR OLD.role IS NOT NEW.role
  OR OLD.settings_id IS NOT NEW.settings_id
  OR OLD.settings_revision IS NOT NEW.settings_revision
  OR OLD.profile_json IS NOT NEW.profile_json OR OLD.profile_hash IS NOT NEW.profile_hash
  OR OLD.project_config_revision_id IS NOT NEW.project_config_revision_id
  OR OLD.project_configuration_hash IS NOT NEW.project_configuration_hash
  OR OLD.adapter_name IS NOT NEW.adapter_name OR OLD.adapter_hash IS NOT NEW.adapter_hash
  OR OLD.capability_id IS NOT NEW.capability_id OR OLD.capability_key IS NOT NEW.capability_key
  OR OLD.capability_proof_hash IS NOT NEW.capability_proof_hash
  OR OLD.activated_at IS NOT NEW.activated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_task_profile_activations_delete AFTER DELETE ON trip_task_profile_activations
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_verification_checks_insert AFTER INSERT ON trip_verification_checks
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_verification_checks_update AFTER UPDATE ON trip_verification_checks
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id
  OR OLD.config_revision_id IS NOT NEW.config_revision_id OR OLD.check_key IS NOT NEW.check_key
  OR OLD.category IS NOT NEW.category OR OLD.command_kind IS NOT NEW.command_kind
  OR OLD.executable IS NOT NEW.executable OR OLD.arguments_json IS NOT NEW.arguments_json
  OR OLD.shell_command IS NOT NEW.shell_command OR OLD.cwd IS NOT NEW.cwd
  OR OLD.timeout_seconds IS NOT NEW.timeout_seconds
  OR OLD.acceptance_rows_json IS NOT NEW.acceptance_rows_json
  OR OLD.relevant_inputs_json IS NOT NEW.relevant_inputs_json
  OR OLD.invalidation_json IS NOT NEW.invalidation_json
  OR OLD.original_text IS NOT NEW.original_text OR OLD.enabled IS NOT NEW.enabled
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_trip_verification_checks_delete AFTER DELETE ON trip_verification_checks
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workspaces_insert AFTER INSERT ON workspaces
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workspaces_update AFTER UPDATE ON workspaces
WHEN OLD.id IS NOT NEW.id OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.repository_identity IS NOT NEW.repository_identity OR OLD.path IS NOT NEW.path
  OR OLD.base_revision IS NOT NEW.base_revision OR OLD.worktree_head IS NOT NEW.worktree_head
  OR OLD.policy_json IS NOT NEW.policy_json OR OLD.state IS NOT NEW.state
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision = revision + 1; END;
CREATE TRIGGER state_revision_workspaces_delete AFTER DELETE ON workspaces
BEGIN UPDATE state_revision SET revision = revision + 1; END;
