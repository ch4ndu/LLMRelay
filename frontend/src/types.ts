export type Role =
  | "manager"
  | "explorer"
  | "plan_reviewer"
  | "implementer"
  | "code_reviewer"
  | "final_verifier";
export type Provider = "codex" | "claude";
export interface ProtocolDescriptor {
  generation: number;
  server_version: string;
  instance_id: string;
  supported_features: string[];
}
export interface RoleConfig {
  provider: Provider;
  model: string;
  effort: string;
}
export interface ModelCatalogEntry {
  slug: string;
  display_name?: string;
  visibility?: string;
  efforts: string[];
}
export interface ModelCatalog {
  provider: Provider;
  state: "available" | "unavailable";
  advisory_only: true;
  fetched_at?: string;
  stale?: boolean;
  models: ModelCatalogEntry[];
  reason?: string;
}
export interface TripAdapter {
  kind?: string;
  provider?: string;
  capabilities?: Record<string, boolean>;
  [key: string]: unknown;
}
export interface TripSetupProfile {
  adapter: string;
  provider: Provider;
  model: string;
  effort: string;
  service_tier?: string | null;
  authority: "read-only" | "workspace-write";
  session: "retained" | "fresh";
}
export interface TripSetupProposal {
  project_name: string;
  host_manager: RoleConfig;
  guidance: string[];
  documentation: { no_change_text: string; [key: string]: unknown };
  verification: { focused: string[]; broad: string[]; cleanup: string[] };
  verification_contracts: Record<string, Record<string, unknown>>;
  testing: { coverage: "minimal" | "moderate" | "extensive" };
  observability: { cmux: "auto" | "on" | "off" };
  roles: Record<Exclude<Role, "manager">, { profile: string }>;
  profiles: Record<string, TripSetupProfile>;
  adapters: { adapters: Record<string, TripAdapter>; [key: string]: unknown };
  agents_file: { relative_path: "AGENTS.md"; approved_content: string };
  local_exclude: { pattern: "/.local/trip-explorer/"; approved: boolean };
  canonical_migration?: {
    observed_kind: string;
    observation_hash: string;
    unresolved_conflicts: string[];
    resolutions: Record<string, string>;
  };
}
export interface TripSetupSuggestion {
  project_name?: string;
  guidance?: string[];
  documentation?: { no_change_text: string };
  verification?: {
    focused?: string[];
    broad?: string[];
    cleanup?: string[];
  };
  agents_content?: string;
}
export interface TripProjectConfiguration {
  id: string;
  revision: number;
  state: string;
  configuration_hash: string;
  config: Record<string, unknown> & {
    guidance?: string[];
    documentation?: Record<string, unknown>;
    verification?: TripSetupProposal["verification"];
    testing?: { coverage?: string };
    observability?: Record<string, unknown>;
    roles?: Record<string, { profile: string }>;
    profiles?: Record<string, TripSetupProfile>;
  };
  adapters: TripSetupProposal["adapters"];
  preflight: { status: string; receipt_count: number };
  verification: TripSetupProposal["verification"];
  created_at: string;
  activated_at?: string | null;
}
export type TripReadiness =
  | "not_initialized"
  | "setup_in_progress"
  | "ready"
  | "needs_upgrade_review"
  | "invalid"
  | "recovery_required";
export interface TripProjectState {
  readiness: TripReadiness;
  reason: string;
  detected_installation: string;
  setup_operation_id?: string | null;
  active_config_revision_id?: string | null;
  workflow_id?: string | null;
  package_version?: string | null;
  upstream_source_hash?: string | null;
  overlay_hash?: string | null;
  manifest_hash?: string | null;
  activated_at?: string | null;
  updated_at?: string | null;
  detected?: Record<string, unknown> & {
    kind?: string;
    conflicts?: string[];
    partial_paths?: string[];
    alternate_roots?: string[];
    observation_hash?: string;
    configuration?: Record<string, unknown>;
    adapters?: TripSetupProposal["adapters"];
    configuration_error?: string;
  };
  configuration?: TripProjectConfiguration | null;
}
export interface TripSetupSelection {
  role: Role;
  selection_state: "selected" | "unselected";
  profile?: RoleConfig | TripSetupProfile | null;
  profile_hash?: string | null;
  selected_at?: string | null;
  compatibility?: CompatibilityExplanation | null;
}
export interface TripSetupProbeReceipt {
  id: string;
  profile_id: string;
  profile_hash: string;
  provider: Provider;
  role: Role;
  authority: "read-only" | "workspace-write";
  session_mode: "retained" | "fresh";
  generation_id: string;
  result: string;
  model_evidence: string;
  evidence: Record<string, unknown>;
  created_at: string;
  capability_key?: string | null;
  adapter_hash?: string | null;
  reused_from_receipt_id?: string;
}
export interface TripSetupSession {
  id: string;
  attempt_id: string;
  role: Role;
  provider: Provider;
  generation: number;
  lane_id: string;
  status: string;
  launch_state: string;
  launch_error?: string | null;
  readiness: string;
  capture_state: string;
  has_native_session: boolean;
  resume_count: number;
  updated_at: string;
  cmux_surface?: CmuxSessionSurface | null;
  input_control?: ActiveInputControl | null;
}
export interface ActiveInputControl {
  owner_kind: "human";
  expires_at: string;
}
export interface TripSetupRecovery {
  record_id: string;
  session_id: string;
  attempt_id: string;
  task_id: string;
  task_version: number;
  role: Role;
  validation_cell:
    | "trip_setup_discovery"
    | "trip_setup_probe"
    | "trip_runtime_probe";
  state: "attention_required";
  session_status: string;
  attempt_status: string;
  task_attention: string;
  runtime_admission_id?: string | null;
  ownership_state: "recorded_process_verification_required";
  created_at: string;
  updated_at: string;
}
export interface RuntimeProbeState {
  role: Role;
  settings_revision?: number | null;
  profile: RoleConfig & Record<string, unknown>;
  profile_hash: string;
  project_config_revision_id: string;
  project_configuration_hash: string;
  adapter: string;
  adapter_hash: string;
  capability_key: string;
  nonce: string;
  state: string;
  session_id?: string | null;
  capability_id?: string | null;
  failure_reason?: string | null;
  failure_category?: string | null;
  published_at?: string | null;
  attempt_id?: string | null;
  workspace_path?: string | null;
  session_status?: string | null;
  readiness?: string | null;
  hook_trust?: string | null;
  has_native_session: boolean;
  cmux_surface?: CmuxSessionSurface | null;
}
export interface RuntimeAdmissionState {
  id: string;
  project_id?: string;
  task_id?: string | null;
  scope_hash: string;
  state: string;
  fresh_call_count: number;
  authorized_at?: string | null;
  failure_reason?: string | null;
  failure_category?: string | null;
  probes: RuntimeProbeState[];
  created_at?: string;
  updated_at?: string;
  probe_state?: string;
  session_id?: string | null;
  session_status?: string | null;
  readiness?: string | null;
  hook_trust?: string | null;
  has_native_session?: boolean;
}
export interface TripSetupState {
  setup_operation_id: string;
  project_id: string;
  state:
    | "discovery"
    | "draft"
    | "probing"
    | "preflight_complete"
    | "finalized"
    | "install_authorized"
    | "applying"
    | "activated"
    | "workspace_recovery_required"
    | "recovery_required"
    | "aborted"
    | "superseded";
  proposal_hash?: string | null;
  selected_profiles_hash?: string | null;
  approved_preimages_hash?: string | null;
  final_source_set_hash?: string | null;
  approved_source_set_hash?: string | null;
  finalized_at?: string | null;
  supersedes_setup_operation_id?: string | null;
  fixture_project_id?: string | null;
  validation_task_id?: string | null;
  discovery_attempt_id?: string | null;
  probe_attempt_id?: string | null;
  discovery_status?: TripSetupAttemptState | null;
  probe_status?: TripSetupAttemptState | null;
  manager_control?: TripSetupManagerControl | null;
  target_inventory: Record<string, unknown>;
  proposal?: TripSetupProposal | null;
  destination_preview?:
    | Array<{
      relative_path: string;
      preimage_sha256?: string | null;
      source_sha256: string;
    }>
    | null;
  destination_preview_error?: string | null;
  final_files: Array<{
    relative_path: string;
    source_sha256: string;
    preimage_sha256?: string | null;
    content: string;
    preimage_content?: string | null;
  }>;
  installation_source_binding_complete: boolean;
  installation_source_binding_reason: string;
  selected_profiles: TripSetupSelection[];
  probe_receipts: TripSetupProbeReceipt[];
  sessions: TripSetupSession[];
  recoveries?: TripSetupRecovery[];
  continuation_actions?: ContinuationAction[];
  runtime_admissions: RuntimeAdmissionState[];
  agents_file: {
    exists?: boolean;
    content?: string;
    sha256?: string;
    error?: string;
  };
  probe_authorized_at?: string | null;
  install_authorized_at?: string | null;
  error?: string | null;
  created_at: string;
  updated_at: string;
}
export interface TripSetupAttemptState {
  attempt_id: string;
  phase: string;
  status: string;
  workspace_state?: string | null;
  issued_permits: number;
  consumed_permits: number;
  updated_at: string;
}
export interface TripSetupManagerControl {
  hold?: {
    id: string;
    state: string;
    requested_operation_id?: string | null;
    created_at: string;
    updated_at: string;
  } | null;
  current?: RoleConfig | null;
  requested?: RoleConfig | null;
  effective?: RoleConfig | null;
  interrupt_requested: boolean;
  quiescent: boolean;
  next_action: {
    action: "stop" | "retry_stop" | "waiting" | "change" | "launch" | "none";
    reason: string;
  };
}
export interface TripIntegrationRequest {
  id: string;
  state: "requested" | "dispatched";
  capsule: {
    ordered_lanes: string[];
    merge_strategy: unknown;
    verification_boundary: unknown;
    [key: string]: unknown;
  };
  requested_by_generation_id: string;
  created_at: string;
  dispatched_at?: string | null;
}
export interface TripLaneState {
  id: string;
  attempt_id: string;
  lane_key: string;
  owned_paths: string[];
  shared_paths: string[];
  protected_paths: string[];
  dependencies: string[];
  source_hashes: Record<string, string>;
  frozen_seams_hash: string;
  required: boolean | number;
  state: "admitted" | "active" | "yielded";
  effective_generation_id?: string | null;
  pending_settings_revision?: number | null;
  yielded_at?: string | null;
  receipt: Record<string, unknown>;
  integration_request?: TripIntegrationRequest | null;
}
export interface TripExplorerState {
  id: string;
  attempt_id: string;
  stage: "planning" | "rescue" | "final";
  trigger: string;
  activated: boolean;
  census: Record<string, unknown>;
  limits: Record<string, unknown>;
  candidate_hash: string | null;
  role_generation_id: string | null;
  outcome: Record<string, unknown> | null;
  additional_authorization: {
    id: string;
    stage: "rescue";
    justification: string;
    authorized_at: string;
    consumed_at: string | null;
  } | null;
  created_at: string;
}
export interface TripVerificationCheck {
  id: string;
  project_id: string;
  config_revision_id: string;
  check_key: string;
  category: "focused" | "broad" | "cleanup";
  command_kind: "structured_argv" | "exact_shell";
  executable?: string | null;
  arguments?: string[] | null;
  shell_command?: string | null;
  cwd: string;
  timeout_seconds: number;
  acceptance_rows: unknown[];
  relevant_inputs: string[];
  invalidation: Record<string, unknown>;
  original_text: string;
  enabled: boolean | number;
}
export interface TripTaskVerification {
  attempt_id: string;
  selected_revision: number;
  check_id: string;
  required: boolean;
  candidate_hash?: string | null;
  exact_command_hash: string;
  scope_hash?: string | null;
  command: {
    kind: "structured_argv" | "exact_shell";
    executable?: string | null;
    arguments?: string[] | null;
    shell?: string | null;
    cwd: string;
  };
  authorization: {
    once_available: boolean;
    reusable: boolean;
    family: boolean;
    authorized: boolean;
    action_state: "actionable" | "current_receipt" | "inactive";
    inactive_reason?: string | null;
    state:
      | "pending"
      | "denied"
      | "approved_once"
      | "approved_exact"
      | "approved_family";
    source: "service_check";
    family_preview?: Record<string, unknown> | null;
    family_unavailable_reason?: string | null;
    matching_rule?: {
      id: string;
      revision: number;
      display_family: string;
      source: "service_check";
    } | null;
  };
  latest_run?: CheckRun | null;
}
export interface Project {
  id: string;
  display_name: string;
  repository_path: string;
  repository_identity: string;
  base_revision: string;
  queue_paused: boolean;
  version: number;
  settings: Record<string, unknown>;
  trip?: TripProjectState;
}
export interface ProfileSet {
  id: string;
  project_id: string;
  name: string;
  version: number;
  archived: boolean;
  revision: number;
  revision_id: string;
  roles: Record<Role, RoleConfig>;
  config_revision_id: string;
  configuration_hash: string;
}
export interface TaskRecipe {
  id: string;
  project_id: string;
  name: string;
  version: number;
  archived: boolean;
  revision: number;
  revision_id: string;
  title: string;
  description: string;
  acceptance_criteria: string[];
  priority: number;
  profile_revision_id: string;
  required_check_ids: string[];
  config_revision_id: string;
  configuration_hash: string;
  workflow_version: string;
  workflow_hash: string;
}
export interface RecipeSchedule {
  id: string;
  project_id: string;
  name: string;
  version: number;
  archived: boolean;
  paused: boolean;
  recipe_revision_id: string;
  recipe_name: string;
  recipe_config_revision_id: string;
  recipe_archived: boolean;
  cadence: "daily" | "weekly";
  anchor_utc: string;
  next_fire_utc: string | null;
  last_fire: null | {
    scheduled_for_utc: string;
    outcome: "task_created" | "skipped_ineligible" | "missed";
    task_id: string | null;
    reason: string | null;
    missed_first_utc: string | null;
    missed_last_utc: string | null;
    missed_count: number;
  };
}
export interface Review {
  id: string;
  kind: string;
  candidate_hash: string;
  delivery_state: string;
  verdict?: string;
  feedback?: string;
  session_id?: string;
  role_generation_id?: string;
  settings_revision?: number;
  ambiguity_state?: string;
}
export interface Attempt {
  id: string;
  phase: string;
  status: string;
  base_revision: string;
  /** Changes when the attempt's plans, reports or review state change. */
  content_revision?: string;
  plan_hash?: string;
  plan?: string;
  candidate_hash?: string;
  accepted_snapshot_id?: string;
  parent_attempt_id?: string;
  workflow_version?: string;
  workflow_hash?: string;
  structured_plan_id?: string | null;
  selected_checks_revision?: number;
  manager_conformance_revision?: number;
  final_repair_round?: number;
  legacy_migration_required?: boolean | number;
  human_acceptance_at?: string | null;
}
export interface Snapshot {
  id: string;
  attempt_id: string;
  kind: string;
  manifest_hash: string;
  manifest: Record<string, unknown>;
  complete: boolean;
  created_at: string;
  original_base?: string;
  candidate_head?: string;
  source_role_generation_id?: string;
  source_settings_revision?: number;
  workspace_id?: string;
  workspace_hash?: string;
}
export interface ReviewBudget {
  id: string;
  attempt_id: string;
  kind: string;
  initial_allowance: number;
  extension_allowance: number;
  spent: number;
  remaining: number;
  version: number;
}
export interface Task {
  id: string;
  project_id: string;
  title: string;
  description: string;
  acceptance_criteria: string[];
  priority: number;
  manual_order: number;
  lifecycle: string;
  attention: string;
  version: number;
  archived: boolean;
  can_archive: boolean;
  recipe_provenance?: null | {
    recipe_id: string;
    recipe_name: string;
    recipe_revision_id: string;
    recipe_revision: number;
    profile_revision_id: string;
    required_check_ids: string[];
    config_revision_id: string;
    configuration_hash: string;
    workflow_version: string;
    workflow_hash: string;
    schedule_id: string | null;
    scheduled_for_utc: string | null;
  };
  permission_waiting: boolean;
  role_overrides: Partial<Record<Role, RoleConfig>>;
  dependencies: Array<Record<string, unknown>>;
  active_attempt?: Attempt;
  /** Absent from older services and for finished tasks. */
  progress?: TaskProgress | null;
  role_settings: Array<
    {
      id: string;
      role: Role;
      revision: number;
      config: RoleConfig;
      effective_generation_id?: string;
      activation?: {
        id: string;
        source: "task_override";
        profile_hash: string;
        project_config_revision_id: string;
        project_configuration_hash: string;
        adapter: string;
        adapter_hash: string;
        capability_id: string;
        capability_key: string;
        capability_proof_hash: string;
        activated_at: string;
      } | null;
    }
  >;
  reviews: Review[];
  snapshots: Snapshot[];
  review_budgets: ReviewBudget[];
  legacy: Record<string, unknown>;
}
export type NativeTurnFailureKind =
  | "authentication_failed" | "oauth_org_not_allowed" | "account_on_hold"
  | "verification_required" | "billing_error" | "rate_limit" | "overloaded"
  | "invalid_request" | "model_not_found" | "server_error" | "max_output_tokens"
  | "cloud_credential_error" | "unknown";
export interface NativeTurnFailure {
  hook_event_id: string;
  kind: NativeTurnFailureKind;
  provider_error: string;
  details: string | null;
  observed_at: string;
}
export interface NativeTurn {
  accepted_hook_event_id: string | null;
  accepted_at: string | null;
  failure: NativeTurnFailure | null;
}
export interface NativePrompt {
  hook_event_id: string;
  kind: "permission_prompt" | "elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input";
  observed_at: string;
}
export interface Session {
  id: string;
  role_generation_id: string;
  provider: Provider;
  status: string;
  launch_state?: string;
  launch_error?: string | null;
  exit_code?: number | null;
  exit_success?: boolean | null;
  exit_status?: string | null;
  exit_reason?: string | null;
  process_group_quiescent?: boolean | number | null;
  native_session_id?: string;
  validation_cell?:
    | "trip_setup_discovery"
    | "trip_setup_probe"
    | "trip_runtime_probe"
    | null;
  runtime_admission_id?: string | null;
  readiness: string;
  capture_state: string;
  updated_at: string;
  task_id: string;
  attempt_id: string;
  role: Role;
  generation: number;
  transcript_epoch?: string;
  process_identity?: Record<string, unknown> | null;
  interrupt_requested_at?: string | null;
  config_revision: number;
  lane_id?: string;
  setup_operation_id?: string | null;
  workflow_version?: string;
  workflow_hash?: string;
  role_prompt_hash?: string;
  launch: {
    model: string;
    effort: string;
    permission_policy: string;
    security_policy: Record<string, unknown>;
  };
  /** Whether the session reported its result in its latest run. */
  reported_in_latest_invocation?: boolean | number;
  latest_invocation_report?: {
    id: string;
    outcome: string;
    created_at: string;
    consumed_at: string | null;
  } | null;
  native_turn?: NativeTurn | null;
  native_prompt?: NativePrompt | null;
  cmux_surface?: CmuxSessionSurface | null;
  input_control?: ActiveInputControl | null;
}
export interface CmuxSessionSurface {
  id: string;
  task_workspace_id: string;
  workspace_id?: string | null;
  surface_id?: string | null;
  binding_revision: number;
  surface_state: "opening" | "open" | "failed" | "unknown" | "lost" | "retired";
  attachment_state: "pending" | "live" | "ended" | "failed";
  desired_input_state: "view_only" | "control";
  actual_input_state: "view_only" | "control" | "blocked" | "lost";
  control_revision: number;
  applied_revision: number;
  last_error?: string | null;
  updated_at: string;
}
export type CmuxViewState =
  | "pending"
  | "control"
  | "view_only"
  | "blocked"
  | "unknown"
  | "unknown_live"
  | "lost"
  | "failed"
  | "recorded_output";
export type CmuxKeyboardControlAction = "acquire" | "release";
export interface TranscriptFrame {
  epoch: string;
  sequence: number;
  captured_at: string;
  encoding: "base64" | "utf8";
  data: string;
  gap: boolean;
}
export interface TranscriptPage {
  frames: TranscriptFrame[];
  next_epoch?: string | null;
  next_sequence: number;
  has_more: boolean;
}
export interface CmuxViewOutcome {
  state: CmuxViewState;
  message: string;
  retry_available: boolean;
  surface?: CmuxSessionSurface;
  recorded_output?: TranscriptPage;
}
export interface CmuxKeyboardControlOutcome {
  state: "pending" | "control" | "view_only" | "blocked" | "retired";
  message: string;
  surface: CmuxSessionSurface;
}
export interface CheckRun {
  [key: string]: unknown;
  id: string;
  attempt_id: string;
  candidate_hash?: string | null;
  check_id?: string | null;
  selected_check_revision?: number | null;
  suite_name?: string | null;
  suite_version?: number | null;
  executable?: string | null;
  arguments?: string[] | null;
  status: string;
  launch_state?: string | null;
  launch_error?: string | null;
  exit_code?: number | null;
  inputs_hash?: string | null;
  acceptance_coverage: unknown[];
  elapsed_millis?: number | null;
  freshness_state: string;
  evidence: Record<string, unknown>;
  created_at: string;
  finished_at?: string | null;
}
export interface SwitchIntent {
  id: string;
  attempt_id: string;
  role: Role;
  lane_id: string;
  old_generation_id: string;
  new_generation_id?: string | null;
  requested_settings_revision: number;
  checkpoint_snapshot_id: string;
  handoff: Record<string, unknown>;
  state: string;
  updated_at: string;
}
export interface WorkflowControl {
  id: string;
  attempt_id: string;
  role_generation_id?: string | null;
  kind: string;
  state: string;
  payload: Record<string, unknown>;
  updated_at: string;
}
export type ContinuationActionKind =
  | "exact_resume"
  | "fresh_accounted_retry"
  | "replace_stale_authority"
  | "wait_for_exit"
  | "wait_for_capacity"
  | "wait_for_service"
  | "recover_ownership"
  | "retry_graceful_stop"
  | "force_stop_exact_process"
  | "prepare_corrected_runtime"
  | "authorize_implementation"
  | "migrate_attempt"
  | "authorize_additional_explorer"
  | "recover_setup_apply"
  | "recover_workspace_reservation"
  | "continue_fresh_dispatch"
  | "start_managed_legacy_attempt"
  | "refresh_and_reconcile"
  | "authorization_required"
  | "recover_failed_step"
  | "terminal_incomplete";
export interface ContinuationAction {
  kind: ContinuationActionKind;
  enabled: boolean;
  reason: string;
  owner: "service" | "human" | "provider" | "external" | string;
  waiting_for?: string | null;
  since?: string | null;
  deadline_at?: string | null;
  operation: string;
  binding: Record<string, unknown>;
  accounting_note?: string | null;
}
/** `recovery[].detail` of a failed automatic step; its record has no session. */
export interface FailedStepDetail {
  kind: "coordinator_failure";
  task_id: string;
  operation: string;
  causal_identity: unknown;
  effect_certainty: "none" | "possible";
  cause: string;
  failure_key: string;
}
export interface ProductionRoleRestriction {
  provider: Provider;
  role: Role;
  status: "unverified" | "limited" | "unsupported";
  reason: string;
}
export interface CapabilityEvidence {
  provider: Provider;
  version?: string;
  role: Role;
  mode: string;
  config_hash?: string;
  hook_hash?: string;
  status: "unverified" | "supported" | "limited" | "unsupported";
  proof?: Record<string, unknown>;
  gaps: string[];
  checked_at?: string;
  compatibility?: CompatibilityExplanation | null;
}
export type CompatibilityStatus =
  | "matched"
  | "unknown_version"
  | "ambiguous_manifest"
  | "contract_changed"
  | "evidence_stale"
  | "manifest_invalid";
export type CompatibilitySafeAction =
  | "install_supported_provider_version"
  | "update_llmrelay_release"
  | "requalify_exact_profile"
  | "inspect_local_provider_configuration"
  | "contact_operator";
export interface CompatibilityExplanation {
  status: CompatibilityStatus;
  observed_version: string | null;
  pack_id: string | null;
  pack_revision: string | null;
  contract_id: string | null;
  contract_revision: string | null;
  short_hash: string | null;
  predicate_id: string | null;
  missing_evidence: string[];
  action: CompatibilitySafeAction;
  message: string;
}
export interface RolePreparation {
  task_id: string;
  role: Role;
  requested_revision: number;
  config: RoleConfig;
  capability_key?: string;
  generic_capability_supported?: boolean;
  exact_runtime_authority?: boolean;
  exact_runtime_reason?: string;
  task_profile_activated?: boolean;
  task_profile_reason?: string;
  task_profile_source?: "project_default" | "task_override";
  adapter?: string;
  runtime_admission?: RuntimeAdmissionState | null;
  status: "unverified" | "supported" | "unsupported";
  reason: string;
  compatibility?: CompatibilityExplanation | null;
}
export type AttentionCategory =
  | "permission"
  | "decision"
  | "recovery"
  | "compatibility"
  | "blocked"
  | "awaiting_acceptance";
export interface TaskAttentionTarget {
  project_id: string;
  task_id: string;
  task_version: number;
}
export type AttentionTarget =
  | ({ kind: "task" } & TaskAttentionTarget)
  | {
    kind: "attempt";
    project_id: string;
    task_id: string;
    attempt_id: string;
    phase: string;
    plan_hash: string | null;
    candidate_hash: string | null;
  }
  | {
    kind: "session";
    project_id: string;
    task_id: string;
    attempt_id: string;
    session_id: string;
    role_generation_id: string;
  }
  | {
    kind: "permission_request";
    project_id: string;
    task_id: string;
    attempt_id: string;
    session_id: string;
    request_id: string;
    request_revision: number;
  }
  | {
    kind: "recovery_record";
    project_id: string;
    task_id: string;
    attempt_id: string;
    recovery_id: string;
  }
  | {
    kind: "project_setup";
    project_id: string;
    setup_operation_id: string | null;
  }
  | {
    kind: "role_settings";
    project_id: string;
    task_id: string;
    role: Role;
    settings_revision: number;
  }
  | { kind: "diagnostics" };
export type AttentionActionKind =
  | "review_plan"
  | "review_request"
  | "review_result"
  | "answer_question"
  | "open_agent_output"
  | "open_project_setup"
  | "open_agent_settings"
  | "open_diagnostics"
  | "resolve_issue";
export interface AttentionItem {
  id: string;
  category: AttentionCategory;
  title: string;
  reason: string;
  task_title?: string | null;
  role?: Role | null;
  /** Label for the item's button; routing uses only `target`. */
  action?: { kind: AttentionActionKind; label: string } | null;
  target: AttentionTarget | null;
  held_tasks: TaskAttentionTarget[];
  /** Raw diagnostic text, shown only under Technical details. */
  details?: string | null;
}
/** Where an unfinished task stands, from authoritative workflow evidence. */
export interface TaskProgress {
  reason_code: string;
  waiting_reason: string;
  responsible: "you" | "llmrelay" | "agent" | "external";
  responsible_role: Role | null;
  next_operation: string | null;
  next_target: AttentionTarget | null;
  waiting_since: string | null;
  last_meaningful_at: string | null;
  last_meaningful_event: string | null;
  last_agent_activity_at: string | null;
  activity: "no_live_agent" | "agent_live_idle" | "agent_active_without_progress";
}
/**
 * The next step a task offers on its board card and in its header: the first
 * of the task's attention items, which is the one `item_id` opens.
 */
export interface TaskAction {
  task_id: string;
  item_id: string;
  action: { kind: AttentionActionKind; label: string };
  /** Every attention item naming this task, in precedence order. */
  item_ids: string[];
}
export interface TaskContentRecord {
  kind: "report" | "rework_request";
  id: string;
  created_at: string;
  role?: Role;
  outcome?: string;
  summary: string;
  plan?: string | null;
  review_kind?: string | null;
  superseded_by_native_turn?: { hook_event_id: string; superseded_at: string } | null;
}
export interface TaskContent {
  task_id: string;
  attempt_id: string | null;
  content_revision: string;
  records: TaskContentRecord[];
  truncated: boolean;
}
/** Revisions are opaque canonical decimals, comparable only within one incarnation. */
export interface StateCursor {
  incarnation: string;
  revision: string;
}
export type StateWaitResult =
  | (StateCursor & { outcome: "state_changed" | "reset"; state: AppState })
  | (StateCursor & { outcome: "unchanged" });
export interface AppState extends StateCursor {
  schema: number;
  generated_at: string;
  projects: Project[];
  tasks: Task[];
  profile_sets: ProfileSet[];
  task_recipes: TaskRecipe[];
  recipe_schedules: RecipeSchedule[];
  production_role_restrictions: ProductionRoleRestriction[];
  capabilities: CapabilityEvidence[];
  active_sessions: Session[];
  controls: WorkflowControl[];
  guidance: Array<Record<string, unknown>>;
  check_suites: Array<Record<string, unknown>>;
  checks: CheckRun[];
  switches: SwitchIntent[];
  recovery: Array<Record<string, unknown>>;
  history: Array<Record<string, unknown>>;
  instance_settings: {
    version: number;
    auto_resume_eligible: boolean;
    updated_at: string;
  };
  restart_candidates: Array<{
    session_id: string;
    attempt_id: string;
    task_id: string;
    source: string;
    state: string;
    reason: string;
    requested_by?: string;
    result: Record<string, unknown>;
    updated_at: string;
  }>;
  permission_requests: PermissionRequest[];
  permission_rules: PermissionRule[];
  trip_setups?: TripSetupState[];
  trip_explorer?: TripExplorerState[];
  trip_lanes?: TripLaneState[];
  trip_checks?: TripVerificationCheck[];
  trip_task_verification?: TripTaskVerification[];
  continuation_actions: ContinuationAction[];
  decisions: DecisionExplanation[];
  attention: AttentionItem[];
  /** Absent from older services; derived from `attention` by the host. */
  task_actions?: TaskAction[];
  resources: {
    active_sessions: number;
    active_controls: number;
    queued_guidance: number;
    running_checks: number;
    observed_at: string;
    capacity?: {
      global_processes: number;
      active_invocations_per_provider: number;
      managers_per_provider: number;
      idle_persistent_managers_count_as_invocations: boolean;
      occupied_global: number;
      occupied_by_provider: { codex: number; claude: number };
      occupied_managers: number;
      occupied_managers_by_provider: { codex: number; claude: number };
      issued_reservations: number;
      policy: string;
    };
    processes: Array<Record<string, unknown>>;
  };
}

export type DecisionEvidenceState =
  | "satisfied"
  | "missing"
  | "stale"
  | "pending"
  | "uncertain"
  | "unknown";
export type DecisionOwner = "service" | "human" | "provider" | "external";
export interface DecisionPrerequisite {
  code: string;
  state: DecisionEvidenceState;
  owner: DecisionOwner;
  evidence: unknown;
  message?: string;
}
export interface DecisionBinding {
  project_id?: string;
  task_id?: string;
  attempt_id?: string;
  session_id?: string;
  role_generation_id?: string;
  recovery_id?: string;
  expected_task_version?: number;
  expected_project_version?: number;
  expected_instance_version?: number;
  candidate_state?: string;
}
export interface DecisionExplanation {
  decision_schema: 1;
  reason_code: string;
  disposition: "ready" | "waiting" | "held" | "retry_deferred" | "terminal";
  subject: DecisionBinding;
  observed_revision: Record<string, unknown>;
  primary_blocker?: DecisionPrerequisite | null;
  prerequisites: DecisionPrerequisite[];
  ownership: { owner: DecisionOwner; state: string; binding: DecisionBinding };
  next_action?: {
    operation: string;
    enabled: boolean;
    owner: DecisionOwner;
    binding: DecisionBinding;
    accounting_note?: string;
  } | null;
  control_policy: { allowed_controls: string[]; disabled_reason_code?: string };
}
export interface RestartPreview {
  decision_schema: 1;
  snapshot: {
    captured_at: string;
    process_inventory: DecisionEvidenceState;
    boot_identity: DecisionEvidenceState;
    dispatch_enabled: boolean;
    draining: boolean;
    revalidation_required: boolean;
    notice: string;
  };
  sessions: Array<{
    classification:
      | "resumable"
      | "fresh_only"
      | "blocked"
      | "uncertain"
      | "awaiting_approval"
      | "complete";
    can_resume_now: boolean;
    could_resume_after_confirmed_shutdown: boolean;
    decision: DecisionExplanation;
  }>;
}
export interface RestartResumeResult {
  operation_id?: string | null;
  mode: "selected" | "eligible";
  state: "admitting" | "resumed" | "queued" | "completed";
  selected_ids: string[];
  queued_ids: string[];
  omitted_ids: string[];
  omitted_count: number;
  outcomes: Array<
    { session_id: string; state: string; reason?: string; next_due_at?: string }
  >;
}
export interface PermissionNativeResolution {
  kind: "tool_finished" | "tool_failed" | "native_denied";
  hook_event_id: string;
  tool_use_id: string;
  observed_at: string;
}
export interface PermissionRequest {
  id: string;
  project_id: string;
  task_id: string;
  attempt_id: string;
  session_id: string;
  role_generation_id: string;
  role: Role;
  provider: Provider;
  native_session_id: string;
  tool_name: string;
  input: unknown;
  requested_access: unknown;
  reason?: string;
  command_display?: string;
  family_preview?: {
    session?: PermissionScopePreview;
    project?: PermissionScopePreview;
  };
  family_unavailable_reason?: string;
  created_at: string;
  deadline_at: string;
  state: string;
  actionable: boolean;
  native_correlation_available: boolean;
  native_resolution: PermissionNativeResolution | null;
  revision: number;
  decision_kind?: string;
  decision_actor?: string;
  decision_reason?: string;
  decided_at?: string;
  matching_rule_id?: string;
  delivery_state: string;
  delivery_reserved_at?: string;
  reserved_behavior?: string;
  delivered_at?: string;
  delivery_unknown_at?: string;
  delivery_reason?: string;
  consumed_at?: string;
}
export interface PermissionScopePreview {
  lifetime: "session" | "project";
  command_family: string;
  arguments: string;
  provider: Provider;
  role: Role;
  native_session?: string;
  worktree?: string;
  registered_root?: string;
  coverage: string;
  warning: string;
  configuration_binding: string;
}
export interface PermissionRule {
  id: string;
  provider: Provider;
  project_id: string;
  role: Role;
  lifetime: "session" | "project";
  session_id?: string;
  display_family: string;
  scope: Record<string, unknown>;
  created_at: string;
  revoked_at?: string;
  last_used_at?: string;
  use_count: number;
  revision: number;
}
export type TripHumanAction =
  | { action: "inspect_project"; project_id: string }
  | {
    action: "begin_setup";
    project_id: string;
    expected_project_version: number;
    host_manager: RoleConfig;
  }
  | {
    action: "save_setup_draft";
    setup_operation_id: string;
    expected_project_version: number;
    proposal: Record<string, unknown>;
  }
  | {
    action: "revise_setup_draft";
    setup_operation_id: string;
    expected_project_version: number;
    proposal: Record<string, unknown>;
  }
  | {
    action: "stop_setup_manager";
    setup_operation_id: string;
    expected_project_version: number;
  }
  | {
    action: "change_setup_manager";
    setup_operation_id: string;
    expected_project_version: number;
    host_manager: RoleConfig;
  }
  | {
    action: "authorize_setup_probes";
    setup_operation_id: string;
    proposal_hash: string;
  }
  | {
    action: "prepare_runtime_admission";
    project_id: string;
    task_id?: string;
    role?: Role;
    settings_revision?: number;
    cmux_socket_path?: string;
    expected_version: number;
  }
  | {
    action: "authorize_runtime_admission";
    admission_id: string;
    scope_hash: string;
  }
  | {
    action: "publish_runtime_proof";
    admission_id: string;
    role: Role;
  }
  | {
    action: "finalize_installation";
    setup_operation_id: string;
    proposal_hash: string;
  }
  | {
    action: "authorize_installation";
    setup_operation_id: string;
    proposal_hash: string;
    approved_preimages_hash: string;
    final_source_set_hash: string;
  }
  | {
    action: "apply_installation";
    setup_operation_id: string;
    proposal_hash: string;
  }
  | { action: "recover_installation"; setup_operation_id: string }
  | {
    action: "adopt_installation";
    project_id: string;
    expected_project_version: number;
    configuration: Record<string, unknown>;
  }
  | {
    action: "migrate_attempt";
    task_id: string;
    attempt_id: string;
    expected_task_version: number;
    reviewed_plan_hash: string;
    config_revision_id: string;
  }
  | {
    action: "authorize_check";
    attempt_id: string;
    check_id: string;
    selected_revision: number;
    exact_command_hash: string;
    scope_hash: string;
    decision?: "approved" | "denied";
    lifetime: string;
  }
  | {
    action: "revoke_check_permission_rule";
    rule_id: string;
    expected_revision: number;
  }
  | {
    action: "extend_review_budget";
    task_id: string;
    attempt_id: string;
    expected_task_version: number;
    review_kind: "plan" | "code";
    additional: number;
  }
  | {
    action: "authorize_implementation";
    task_id: string;
    attempt_id: string;
    expected_task_version: number;
    plan_hash: string;
  }
  | {
    action: "authorize_additional_explorer";
    task_id: string;
    attempt_id: string;
    expected_task_version: number;
    stage: "rescue";
    justification: string;
  };
export type TripCommand =
  & { kind: "trip"; operation_id: string }
  & TripHumanAction;
export type TripOperation =
  | {
    kind: "trip_setup_dispatch";
    operation_id: string;
    attempt_id: string;
    role: Role;
    fresh_resume_rejection?: Record<string, unknown>;
  }
  | {
    kind: "runtime_probe_launch";
    operation_id: string;
    admission_id: string;
    role: Role;
  }
  | {
    kind: "runtime_probe_resume";
    operation_id: string;
    admission_id: string;
    role: Role;
  }
  | {
    kind: "role_dispatch";
    attempt_id: string;
    role: Role;
    lane?: string;
    prompt: string;
  }
  | {
    kind: "check_run";
    operation_id: string;
    attempt_id: string;
    check_id: string;
    suite_name?: never;
  };
export type Command = Record<string, unknown> & {
  kind: string;
  operation_id: string;
};
export type Operation = Record<string, unknown> & { kind: string };
export const ROLES: Role[] = [
  "manager",
  "explorer",
  "plan_reviewer",
  "implementer",
  "code_reviewer",
  "final_verifier",
];
export const roleLabel = (role: Role) =>
  role.split("_").map((part) => part[0].toUpperCase() + part.slice(1)).join(
    " ",
  );
