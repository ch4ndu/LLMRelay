import type {
  AppState,
  Command,
  CompatibilityExplanation,
  HistoryExportText,
  HistoryPaneOutcome,
  HistoryPaneView,
  ProviderValidationBatch,
  ProviderValidationPreview,
  ProviderValidationStartOutcome,
  WorkflowCommandResult,
  TranscriptPage,
  ConversationAdmission,
  ConversationDetail,
  ConversationMessages,
  ConversationMutation,
  ConversationPage,
  ConversationProfile,
  ConversationRead,
  ConversationReceipt,
  ConversationSummary,
  ConversationWorkspaceIdentity,
} from "./types";
import { ROLES } from "./types";

export const field = (value: unknown, name: string): unknown =>
  typeof value === "object" && value !== null && !Array.isArray(value)
    ? Reflect.get(value, name)
    : undefined;
export const decisionIsCurrent = (value: unknown): boolean => {
  const prerequisites = field(value, "prerequisites");
  const policyControls = field(
    field(value, "control_policy"),
    "allowed_controls",
  );
  const action = field(value, "next_action");
  const owner = field(field(value, "ownership"), "owner");
  const blocker = field(value, "primary_blocker");
  return field(value, "decision_schema") === 1 &&
    typeof field(value, "reason_code") === "string" &&
    ["ready", "waiting", "held", "retry_deferred", "terminal"].includes(
      String(field(value, "disposition")),
    ) &&
    ["service", "human", "provider", "external"].includes(String(owner)) &&
    field(value, "subject") !== undefined &&
    Array.isArray(prerequisites) &&
    prerequisites.every((item: unknown) =>
      typeof field(item, "code") === "string" &&
      ["satisfied", "missing", "stale", "pending", "uncertain", "unknown"]
        .includes(String(field(item, "state")))
    ) &&
    (blocker === undefined || blocker === null ||
      ["satisfied", "missing", "stale", "pending", "uncertain", "unknown"]
        .includes(String(field(blocker, "state")))) &&
    Array.isArray(policyControls) &&
    policyControls.every((control: unknown) => typeof control === "string") &&
    (action === undefined || action === null ||
      (typeof field(action, "operation") === "string" &&
        typeof field(action, "enabled") === "boolean" &&
        field(action, "binding") !== undefined));
};
const MAX_REVISION = "9223372036854775807";
const MAX_INCARNATION_BYTES = 128;
// The service spells each revision one way (no sign or leading zero) within
// SQLite's signed 64-bit range, so clients may order revisions as strings.
export const cursorIsCanonical = (incarnation: string, revision: string) =>
  incarnation.length > 0 &&
  new TextEncoder().encode(incarnation).length <= MAX_INCARNATION_BYTES &&
  /^(0|[1-9][0-9]*)$/.test(revision) &&
  (revision.length < MAX_REVISION.length ||
    (revision.length === MAX_REVISION.length && revision <= MAX_REVISION));
export const hasStrings = (value: unknown, names: string[]) =>
  names.every((name) => typeof field(value, name) === "string");
export const hasNullableString = (value: unknown, name: string) => {
  const item = field(value, name);
  return item === null || typeof item === "string";
};
const taskTargetIsCurrent = (value: unknown) =>
  hasStrings(value, ["project_id", "task_id"]) &&
  Number.isInteger(field(value, "task_version"));
const attentionTargetIsCurrent = (value: unknown): boolean => {
  switch (field(value, "kind")) {
    case "conversation":
      return hasStrings(value, ["project_id", "conversation_id", "generation"]) &&
        ["request_id", "turn_id", "operation_id"].every((key) => hasNullableString(value, key));
    case "task":
      return taskTargetIsCurrent(value);
    case "attempt":
      return hasStrings(value, [
        "project_id",
        "task_id",
        "attempt_id",
        "phase",
      ]) &&
        hasNullableString(value, "plan_hash") &&
        hasNullableString(value, "candidate_hash");
    case "session":
      return hasStrings(value, [
        "project_id",
        "task_id",
        "attempt_id",
        "session_id",
        "role_generation_id",
      ]);
    case "permission_request":
      return hasStrings(value, [
        "project_id",
        "task_id",
        "attempt_id",
        "session_id",
        "request_id",
      ]) && Number.isInteger(field(value, "request_revision"));
    case "recovery_record":
      return hasStrings(value, [
        "project_id",
        "task_id",
        "attempt_id",
        "recovery_id",
      ]);
    case "project_setup":
      return hasStrings(value, ["project_id"]) &&
        hasNullableString(value, "setup_operation_id");
    case "role_settings":
      return hasStrings(value, ["project_id", "task_id", "role"]) &&
        Number.isInteger(field(value, "settings_revision"));
    case "diagnostics":
      return true;
    default:
      return false;
  }
};
const attentionCategories = [
  "permission",
  "decision",
  "recovery",
  "compatibility",
  "blocked",
  "awaiting_acceptance",
];
const compatibilityStatuses: CompatibilityExplanation["status"][] = [
  "matched",
  "unknown_version",
  "ambiguous_manifest",
  "contract_changed",
  "evidence_stale",
  "manifest_invalid",
];
const compatibilityActions: CompatibilityExplanation["action"][] = [
  "install_supported_provider_version",
  "update_llmrelay_release",
  "requalify_exact_profile",
  "inspect_local_provider_configuration",
  "contact_operator",
];
export const compatibilityIsCurrent = (value: unknown) => {
  if (value == null) return true;
  const status = field(value, "status");
  const action = field(value, "action");
  const missing = field(value, "missing_evidence");
  return compatibilityStatuses.some((value) => value === status) &&
    compatibilityActions.some((value) => value === action) &&
    typeof field(value, "message") === "string" &&
    Array.isArray(missing) && missing.every((item) => typeof item === "string");
};
const attentionActionIsCurrent = (value: unknown) =>
  value === undefined || value === null ||
  (hasStrings(value, ["kind", "label"]) &&
    [
      "review_plan",
      "review_request",
      "review_result",
      "answer_question",
      "open_task",
      "open_agent_output",
      "open_project_setup",
      "open_agent_settings",
      "open_diagnostics",
      "resolve_issue",
    ].includes(String(field(value, "kind"))));
export const attentionItemIsCurrent = (value: unknown) => {
  const target = field(value, "target");
  const held = field(value, "held_tasks");
  const details = field(value, "details");
  return hasStrings(value, ["id", "title", "reason"]) &&
    (details === undefined || details === null || typeof details === "string") &&
    attentionActionIsCurrent(field(value, "action")) &&
    attentionCategories.includes(String(field(value, "category"))) &&
    (target === null || attentionTargetIsCurrent(target)) &&
    Array.isArray(held) && held.every(taskTargetIsCurrent);
};
// A task action must name one of the snapshot's attention items for its task,
// so a card or header can never route anywhere the inbox would not.
export const taskActionsAreCurrent = (state: AppState) => {
  const actions: unknown = state.task_actions;
  if (actions === undefined) return true;
  return Array.isArray(actions) && actions.every((value) => {
    const ids = field(value, "item_ids");
    const primary = state.attention.find((item) =>
      item.id === field(value, "item_id")
    );
    return hasStrings(value, ["task_id", "item_id"]) &&
      attentionActionIsCurrent(field(value, "action")) &&
      field(value, "action") != null &&
      Array.isArray(ids) && ids.length > 0 &&
      ids.every((id) => typeof id === "string") &&
      ids[0] === field(value, "item_id") &&
      primary !== undefined &&
      primary.target !== null &&
      "task_id" in primary.target &&
      primary.target.task_id === field(value, "task_id") &&
      ids.every((id) =>
        state.attention.some((item) =>
          item.id === id && item.target !== null && "task_id" in item.target &&
          item.target.task_id === field(value, "task_id")
        )
      );
  });
};
const validationProviders = ["codex", "claude"];
const providerCheckStates = [
  "pending",
  "reserved",
  "running",
  "succeeded",
  "failed",
  "delivery_unknown",
  "cancel_requested",
  "cancelled",
  "skipped",
];
const isNonNegativeInteger = (value: unknown) =>
  Number.isSafeInteger(value) && Number(value) >= 0;
const providerInstallationIsCurrent = (value: unknown) =>
  validationProviders.includes(String(field(value, "provider"))) &&
  typeof field(value, "ready") === "boolean" &&
  [
    "executable_path",
    "executable_version",
    "account_identity",
    "authentication",
    "model_calls_disabled_reason",
    "guidance",
  ].every((name) => hasNullableString(value, name));
const providerCheckPreviewIsCurrent = (value: unknown) => {
  const listed = field(value, "model_listed");
  return hasStrings(value, ["model", "effort", "reason"]) &&
    validationProviders.includes(String(field(value, "provider"))) &&
    ["reuse_proof", "model_call", "unavailable"].includes(
      String(field(value, "plan")),
    ) &&
    isNonNegativeInteger(field(value, "ordinal")) &&
    hasNullableString(value, "proof_observed_at") &&
    hasNullableString(value, "proof_expires_at") &&
    (listed === null || typeof listed === "boolean");
};
export const providerValidationPreviewIsCurrent = (
  value: unknown,
): value is ProviderValidationPreview => {
  const installations = field(value, "installations");
  const checks = field(value, "checks");
  return typeof field(value, "preview_hash") === "string" &&
    isNonNegativeInteger(field(value, "model_calls_required")) &&
    Array.isArray(installations) &&
    installations.every(providerInstallationIsCurrent) &&
    Array.isArray(checks) && checks.every(providerCheckPreviewIsCurrent);
};
const providerCheckIsCurrent = (value: unknown) =>
  hasStrings(value, ["id", "model", "effort", "updated_at"]) &&
  validationProviders.includes(String(field(value, "provider"))) &&
  providerCheckStates.includes(String(field(value, "state"))) &&
  isNonNegativeInteger(field(value, "ordinal")) &&
  typeof field(value, "reused_proof") === "boolean" &&
  ["reason", "proof_observed_at", "proof_expires_at"].every((name) =>
    hasNullableString(value, name)
  );
export const providerValidationBatchIsCurrent = (
  value: unknown,
): value is ProviderValidationBatch => {
  const checks = field(value, "checks");
  return hasStrings(value, ["id", "operation_id", "created_at", "updated_at"]) &&
    ["running", "cancel_requested", "finished", "cancelled"].includes(
      String(field(value, "state")),
    ) &&
    typeof field(value, "model_calls_authorized") === "boolean" &&
    isNonNegativeInteger(field(value, "max_model_calls")) &&
    hasNullableString(value, "finished_at") &&
    Array.isArray(checks) && checks.every(providerCheckIsCurrent);
};
// An outcome counts only for the exact operation it was requested for.
export const providerValidationStartOutcomeIsFor = (operation: string) =>
(
  value: unknown,
): value is ProviderValidationStartOutcome => {
  const outcome = field(value, "outcome");
  if (outcome === "started") {
    const batch = field(value, "batch");
    return providerValidationBatchIsCurrent(batch) &&
      batch.operation_id === operation;
  }
  return outcome === "closed_without_start" &&
    field(value, "operation_id") === operation &&
    typeof field(value, "closed_at") === "string";
};
const singleAgentStages = [
  "waiting_to_start",
  "writing",
  "result_submitted_waiting_for_stop",
  "awaiting_selected_checks",
  "awaiting_review", "review_running", "review_blocked",
  "awaiting_human",
  "accepted",
  "probe_evidence_recorded",
  "changes_requested",
  "needs_action",
  "blocked",
];
const singleAgentCheckIsCurrent = (value: unknown) =>
  hasStrings(value, [
    "check_id",
    "check_key",
    "original_text",
    "cwd",
    "exact_command_hash",
    "scope_hash",
  ]) &&
  ["exact_argv", "exact_shell"].includes(String(field(value, "command_kind"))) &&
  ["authorized", "denied", "missing"].includes(
    String(field(value, "authorization")),
  ) &&
  Array.isArray(field(value, "arguments")) &&
  hasNullableString(value, "latest_run_id") &&
  hasNullableString(value, "latest_run_status");
const acceptanceRowIsCurrent = (value: unknown) => {
  const evidence = field(value, "evidence");
  return typeof field(value, "criterion") === "string" &&
    ["met", "unmet"].includes(String(field(value, "status"))) &&
    Array.isArray(evidence) && evidence.every((item) =>
      ["source", "check", "manual", "not_run"].includes(String(field(item, "kind"))) &&
      typeof field(item, "detail") === "string"
    );
};
const singleAgentRunIsCurrent = (value: unknown) => {
  if (value === null) return true;
  const actions = field(value, "available_actions");
  const checks = field(value, "selected_check_ids");
  const runChecks = field(value, "checks");
  const result = field(value, "result");
  const rows = field(result, "acceptance_rows");
  const review = field(value, "review");
  const findings = field(review, "findings");
  return hasStrings(value, ["attempt_id", "definition_state", "stage_reason"]) &&
    singleAgentStages.includes(String(field(value, "stage"))) &&
    (field(value, "preset") === undefined || ["single_agent", "repository_research", "build_and_review"].includes(String(field(value, "preset")))) &&
    (review == null || (hasStrings(review, ["delivery_id", "state", "candidate_hash", "result_binding_id"]) &&
      Number.isSafeInteger(field(review, "ordinal")) && Number.isSafeInteger(field(review, "remaining_deliveries")) &&
      typeof field(review, "repair_used") === "boolean" && hasNullableString(review, "reason") &&
      hasNullableString(review, "summary") && hasNullableString(review, "verdict") &&
      Array.isArray(findings) && findings.every((finding) => typeof finding === "string"))) &&
    typeof field(value, "integration_recorded") === "boolean" &&
    Array.isArray(actions) && actions.every((action) =>
      [
        "accept_result",
        "request_rework",
        "authorize_check",
        "resume_run",
        "cancel",
      ].includes(String(action))
    ) &&
    (field(value, "resume") === null ||
      (hasStrings(field(value, "resume"), ["session_id", "transcript_epoch"]) &&
        typeof field(field(value, "resume"), "available") === "boolean")) &&
    Array.isArray(checks) && checks.every((id) => typeof id === "string") &&
    Array.isArray(runChecks) && runChecks.every(singleAgentCheckIsCurrent) &&
    Array.isArray(field(value, "guidance_changes")) &&
    hasNullableString(value, "guidance_change_hash") &&
    (result === null ||
      (hasStrings(result, ["id", "state", "summary", "created_at"]) &&
        Array.isArray(rows) && rows.every(acceptanceRowIsCurrent) &&
        hasNullableString(result, "candidate_hash") &&
        hasNullableString(result, "snapshot_id")));
};
// Absent for tasks without a selection; a present selection must be current.
export const taskWorkflowIsCurrent = (value: unknown) => {
  if (value === undefined) return true;
  const checks = field(value, "check_ids");
  return ["trip", "single_agent"].includes(String(field(value, "family"))) &&
    ["draft", "admitted", "invalidated"].includes(
      String(field(value, "admission_state")),
    ) &&
    isNonNegativeInteger(field(value, "selection_revision")) &&
    Array.isArray(checks) && checks.every((id) => typeof id === "string") &&
    singleAgentRunIsCurrent(field(value, "run") ?? null);
};
// A probe is offered only with an explicit availability and, when it is
// unavailable, the reason the command would give.
const profileAdmissionIsCurrent = (value: unknown) => {
  const available = field(value, "probe_available");
  const reason = field(value, "probe_unavailable_reason");
  return typeof available === "boolean" &&
    (available ? reason === null : typeof reason === "string");
};
export const workflowExecutionIsCurrent = (value: unknown) =>
  value === undefined ||
  (Array.isArray(value) && value.every((project) => {
    const admissions = field(project, "admissions");
    return typeof field(project, "project_id") === "string" &&
      ["trip", "single_agent"].includes(String(field(project, "default_family"))) &&
      ["configurations", "profiles"].every((name) =>
        Array.isArray(field(project, name))
      ) &&
      Array.isArray(admissions) && admissions.every(profileAdmissionIsCurrent);
  }));
const roleGenerationStatuses = [
  "launch_reserved",
  "running",
  "stopping",
  "exited",
  "launch_failed",
  "recovery_required",
  "replaced",
  "revoked",
];
// Both fields are absent from older services; a present value must be known.
export const sessionGenerationIsCurrent = (value: unknown) => {
  const status = field(value, "generation_status");
  const newer = field(value, "has_newer_generation");
  return (status === undefined || roleGenerationStatuses.includes(String(status))) &&
    (newer === undefined || typeof newer === "boolean" || newer === 0 || newer === 1);
};
export const workflowCommandResultIsCurrent = (
  value: unknown,
): value is WorkflowCommandResult =>
  hasStrings(value, ["operation_id", "entity_kind", "entity_id", "state"]) &&
  (field(value, "version") === null ||
    Number.isSafeInteger(field(value, "version")));
// Absent from older services; when present every part must be current.
export const providerValidationStateIsCurrent = (value: unknown) => {
  if (value === undefined) return true;
  const batches = field(value, "batches");
  const proofs = field(value, "proofs");
  return Array.isArray(batches) &&
    batches.every(providerValidationBatchIsCurrent) &&
    Array.isArray(proofs) && proofs.every((proof) =>
      hasStrings(proof, ["model", "effort", "observed_at", "expires_at"]) &&
      validationProviders.includes(String(field(proof, "provider"))) &&
      hasNullableString(proof, "executable_version") &&
      hasNullableString(proof, "account_identity") &&
      typeof field(proof, "expired") === "boolean"
    );
};
export const taskContentRecordIsCurrent = (value: unknown) =>
  ["report", "rework_request"].includes(String(field(value, "kind"))) &&
  hasStrings(value, ["id", "created_at", "summary"]) &&
  ["plan", "role", "outcome", "review_kind"].every((name) => {
    const item = field(value, name);
    return item === undefined || item === null || typeof item === "string";
  });
const executionActors: string[] = [...ROLES, "single_agent", "single_agent_reviewer"];
const currentInputOutcome = (kind: unknown) =>
  kind === "question" ? "needs_input" : kind === "blocked" ? "blocked" : undefined;
const currentInputUnavailableReasons = [
  "not_recorded",
  "missing",
  "mismatched",
  "superseded",
  "generation_replaced",
];
// Absent from older services. A present source must be bound to the same
// attempt, and an available report must be exactly the one the hold names:
// same ID, actor and outcome, never retired by a later native turn.
export const currentInputIsCurrent = (value: unknown, attemptId: string | null) => {
  if (value === undefined || value === null) return true;
  const outcome = currentInputOutcome(field(value, "kind"));
  const role = field(value, "role");
  if (
    attemptId === null || field(value, "attempt_id") !== attemptId ||
    outcome === undefined ||
    !(role === null || executionActors.includes(String(role))) ||
    !hasNullableString(value, "result_id") ||
    !hasNullableString(value, "role_generation_id")
  ) return false;
  const report = field(value, "report");
  switch (field(value, "state")) {
    case "available":
      return field(value, "unavailable_reason") === undefined &&
        taskContentRecordIsCurrent(report) &&
        field(report, "kind") === "report" &&
        typeof field(value, "result_id") === "string" &&
        field(report, "id") === field(value, "result_id") &&
        typeof field(value, "role_generation_id") === "string" &&
        role !== null && field(report, "role") === role &&
        field(report, "outcome") === outcome &&
        field(report, "superseded_by_native_turn") == null;
    case "unavailable":
      return report === undefined &&
        currentInputUnavailableReasons.includes(
          String(field(value, "unavailable_reason")),
        );
    default:
      return false;
  }
};
// Older services send neither part. A present comparison must be complete:
// a pinned baseline hashes to the reviewed pin, and a diff exists exactly
// when a baseline was available.
export const guidanceComparisonIsCurrent = (file: unknown) => {
  const baseline = field(file, "baseline");
  const diff = field(file, "diff");
  if (baseline === undefined) return diff === undefined;
  const source = field(baseline, "source");
  const sha256 = field(baseline, "sha256");
  const reason = field(baseline, "reason");
  if (
    typeof field(baseline, "revision") !== "string" ||
    !["pinned", "base_commit", "unavailable"].includes(String(source))
  ) return false;
  if (source === "unavailable") {
    return sha256 === null && typeof reason === "string" && diff === null;
  }
  return typeof sha256 === "string" && /^[0-9a-f]{64}$/.test(sha256) &&
    (source === "pinned") === (sha256 === field(file, "previous_sha256")) &&
    reason === null && Array.isArray(diff) && diff.every((line) => {
      const kind = field(line, "kind");
      return kind === "unchanged"
        ? Number.isSafeInteger(field(line, "lines")) && Number(field(line, "lines")) > 0
        : ["context", "removed", "added"].includes(String(kind)) &&
          typeof field(line, "text") === "string";
    });
};
const nullableText = (value: unknown): boolean => value === null || typeof value === "string";
export const conversationProfileIsCurrent = (value: unknown): value is ConversationProfile =>
  hasStrings(value, ["id", "project_id", "execution_config_revision_id", "model", "effort", "instructions", "instructions_hash", "profile_hash"]);
const conversationAdmissionIsCurrent = (value: unknown): value is ConversationAdmission =>
  hasStrings(value, ["id", "profile_revision_id", "recorded_state"]) &&
  ["evidence_hash", "probe_invocation_id"].every((key) => nullableText(field(value, key))) &&
  typeof field(value, "live_eligibility_checked") === "boolean";
const conversationSummaryIsCurrent = (value: unknown): value is ConversationSummary =>
  hasStrings(value, ["id", "project_id", "admission_id", "state", "generation", "created_at", "updated_at"]) &&
  ["session_id", "reservation_state", "active_turn_id"].every((key) => nullableText(field(value, key))) &&
  ["binding_valid", "is_probe"].every((key) => typeof field(value, key) === "boolean");

export function conversationMutationIsCurrent(value: unknown): value is ConversationMutation {
  const action = field(value, "action");
  if (action === "create_profile") return hasStrings(field(value, "input"),
    ["operation_id", "project_id", "execution_config_revision_id", "model", "effort", "instructions"]);
  if (!hasStrings(value, ["operation_id"])) return false;
  switch (action) {
    case "with_knowledge": {
      const selection = field(value, "selection");
      return hasStrings(value, ["project_id", "preview_fingerprint"]) && Number.isSafeInteger(field(value, "expected_consumer_version")) && Array.isArray(selection) && selection.length <= 16 && selection.every(knowledgeSelectionIsCurrent) && ["start", "submit", "steer"].includes(String(field(field(value, "operation"), "action"))) && conversationMutationIsCurrent(field(value, "operation")) && field(value, "operation_id") === field(field(value, "operation"), "operation_id");
    }
    case "prepare": return hasStrings(value, ["profile_revision_id"]);
    case "probe": return hasStrings(value, ["admission_id"]);
    case "activate": return hasStrings(value, ["admission_id", "evidence_hash"]);
    case "start": return hasStrings(value, ["admission_id", "input"]);
    case "submit": return hasStrings(value, ["conversation_id", "input"]);
    case "steer": return hasStrings(value, ["conversation_id", "expected_turn_id", "input"]);
    case "interrupt_turn": return hasStrings(value, ["conversation_id", "expected_session_id", "expected_generation", "expected_turn_id"]);
    case "disconnect_idle": return hasStrings(value, ["conversation_id", "expected_session_id", "expected_generation"]);
    case "resume": case "reconcile": return hasStrings(value, ["conversation_id"]);
    default: return false;
  }
}
export interface ConversationReadResults {
  workspace_identity: ConversationWorkspaceIdentity;
  list: ConversationPage<ConversationSummary>;
  profiles: ConversationPage<ConversationProfile>;
  admissions: ConversationPage<ConversationAdmission>;
  detail: ConversationDetail;
  messages: ConversationMessages;
  task_sources: import("./types").TaskSources;
  receipt: ConversationReceipt;
}
export function conversationReadIsCurrent(action: ConversationRead["action"], value: unknown): boolean {
  if (action === "workspace_identity") return hasStrings(value, ["project_id", "storage_scope"]) &&
    /^[a-f0-9]{64}$/.test(String(field(value, "storage_scope")));
  if (action === "list" || action === "profiles" || action === "admissions") {
    const items = field(value, "items");
    const valid = action === "list" ? conversationSummaryIsCurrent : action === "profiles" ? conversationProfileIsCurrent : conversationAdmissionIsCurrent;
    return Array.isArray(items) && items.length <= 50 && items.every(valid) && nullableText(field(value, "next_cursor"));
  }
  if (action === "receipt") return hasStrings(value, ["operation_id", "request_hash"]) &&
    typeof field(value, "found") === "boolean" &&
    ["kind", "delivery_state", "conversation_id", "generation", "profile_revision_id", "admission_id", "state", "quiescence"].every((key) => nullableText(field(value, key))) &&
    (field(value, "retained_uncertainty_session_id") === undefined || nullableText(field(value, "retained_uncertainty_session_id")));
  if (action === "task_sources") {
    const frozen = field(value, "conversation_source");
    if (!hasStrings(value, ["project_id", "task_id"])) return false;
    if (frozen === null) return true;
    const bundle = field(frozen, "bundle"), messages = field(bundle, "messages");
    return hasStrings(frozen, ["bundle_hash"]) && hasStrings(bundle, ["project_id", "conversation_id", "history_coverage"]) &&
      isNonNegativeInteger(field(bundle, "format_revision")) && isNonNegativeInteger(field(bundle, "through_sequence")) &&
      hasStrings(field(bundle, "attribution"), ["identity_hash", "profile_revision_id", "profile_hash", "model", "effort", "executable_version", "adapter_revision", "adapter_hash", "schema_hash"]) &&
      Array.isArray(messages) && messages.length <= 16 && messages.every((message) =>
        hasStrings(message, ["id", "text", "content_hash", "coverage"]) &&
        ["first_sequence", "last_sequence"].every((key) => isNonNegativeInteger(field(message, key))) &&
        hasStrings(field(message, "source"), ["conversation_id"]) &&
        (field(field(message, "source"), "kind") === "user" ? hasStrings(field(message, "source"), ["operation_id"]) :
          field(field(message, "source"), "kind") === "assistant" && ["turn_id", "item_id"].every((key) => nullableText(field(field(message, "source"), key)))));
  }
  if (action === "messages") {
    const messages = field(value, "messages");
    return Array.isArray(messages) && messages.every((message) => {
      const source = field(message, "source");
      return hasStrings(message, ["id", "text", "content_hash", "coverage"]) &&
        ["first_sequence", "last_sequence"].every((key) => isNonNegativeInteger(field(message, key))) &&
        typeof field(message, "selectable") === "boolean" && hasStrings(source, ["conversation_id"]) &&
        (field(source, "kind") === "user" ? hasStrings(source, ["operation_id"]) :
          field(source, "kind") === "assistant" && ["turn_id", "item_id"].every((key) => nullableText(field(source, key))));
    }) && isNonNegativeInteger(field(value, "through_sequence")) && hasStrings(value, ["history_coverage"]);
  }
  const events = field(value, "events"), requests = field(value, "requests");
  return conversationSummaryIsCurrent(field(value, "conversation")) &&
    conversationProfileIsCurrent(field(value, "profile")) && conversationAdmissionIsCurrent(field(value, "admission")) &&
    typeof field(value, "unresolved_operation") === "boolean" && nullableText(field(value, "unresolved_operation_id")) && typeof field(value, "has_more") === "boolean" &&
    isNonNegativeInteger(field(value, "next_after")) && hasStrings(value, ["history_coverage"]) &&
    Array.isArray(requests) && requests.every((request) => hasStrings(request, ["generation", "thread_id", "turn_id", "request_id", "method", "state"])) &&
    Array.isArray(events) && events.length <= 50 && events.every((event) =>
      isNonNegativeInteger(field(event, "sequence")) && hasStrings(event, ["kind", "observed_at"]) &&
      ["turn_id", "operation_id"].every((key) => nullableText(field(event, key))) &&
      typeof field(event, "payload") === "object" && field(event, "payload") !== null && !Array.isArray(field(event, "payload")));
}
export const transcriptPageIsCurrent = (value: unknown): value is TranscriptPage => {
  const frames = field(value, "frames");
  return Array.isArray(frames) &&
    frames.every((frame) =>
      hasStrings(frame, ["epoch", "captured_at", "data"]) &&
      ["base64", "utf8"].includes(String(field(frame, "encoding"))) &&
      isNonNegativeInteger(field(frame, "sequence")) &&
      typeof field(frame, "gap") === "boolean"
    ) &&
    isNonNegativeInteger(field(value, "next_sequence")) &&
    typeof field(value, "has_more") === "boolean";
};
const historyPaneStates = ["reserved", "opened", "unknown", "failed", "closed"];
const historyPaneViewIsCurrent = (value: unknown): value is HistoryPaneView =>
  hasStrings(value, [
    "id",
    "operation_id",
    "session_id",
    "transcript_epoch",
    "service_boot_id",
    "created_at",
    "updated_at",
  ]) &&
  historyPaneStates.includes(String(field(value, "state"))) &&
  typeof field(value, "retry_allowed") === "boolean" &&
  isNonNegativeInteger(field(value, "high_water_sequence")) &&
  [
    "role_generation_id",
    "export_hash",
    "workspace_id",
    "surface_id",
    "failure_reason",
  ].every((name) => hasNullableString(value, name));
export const historyPaneOutcomeIsCurrent = (
  value: unknown,
): value is HistoryPaneOutcome =>
  typeof field(value, "message") === "string" &&
  historyPaneViewIsCurrent(field(value, "view"));
export const historyPaneViewsAreCurrent = (
  value: unknown,
): value is HistoryPaneView[] =>
  Array.isArray(value) && value.every(historyPaneViewIsCurrent);
export const historyExportTextIsCurrent = (
  value: unknown,
): value is HistoryExportText => {
  const retention = field(value, "retention");
  const first = field(retention, "first_retained_sequence");
  return hasStrings(value, [
    "session_id",
    "transcript_epoch",
    "export_hash",
    "text",
  ]) &&
    isNonNegativeInteger(field(value, "high_water_sequence")) &&
    (first === null || isNonNegativeInteger(first)) &&
    isNonNegativeInteger(field(retention, "high_water_sequence")) &&
    isNonNegativeInteger(field(retention, "gap_count")) &&
    typeof field(retention, "complete") === "boolean";
};
export function comparisonRecipeAttributionIsCurrent(value: unknown): boolean {
  if (value === undefined || value === null) return true;
  const nullable = ["schedule_id", "scheduled_for_utc", "occurrence_id", "authorization_id", "root_attempt_id", "occurrence_outcome", "occurrence_reason"];
  if (!hasStrings(value, ["recipe_revision_id", "recipe_title"]) || !String(field(value, "recipe_revision_id")).trim() ||
      !nullable.every((key) => hasNullableString(value, key))) return false;
  const schedule = field(value, "schedule_id");
  const occurrence = field(value, "occurrence_id");
  const authorization = field(value, "authorization_id");
  const root = field(value, "root_attempt_id");
  return (schedule === null) === (field(value, "scheduled_for_utc") === null) &&
    (occurrence === null) === (field(value, "occurrence_outcome") === null) &&
    (occurrence === null || schedule !== null) &&
    (authorization === null || occurrence !== null) &&
    (root === null || authorization !== null) &&
    (field(value, "occurrence_outcome") === null || scheduleOutcomes.includes(String(field(value, "occurrence_outcome"))));
}


export const utf8Bytes = (value: string): number => new TextEncoder().encode(value).length;
export const scheduleOutcomes = ["task_created", "skipped_ineligible", "missed", "waiting", "held", "reserved", "running", "stop_requested", "review_ready", "finished", "recovery_required"];
const positiveInteger = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value > 0;
export const nonnegativeInteger = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
export function scheduleLimitsAreCurrent(value: unknown): value is import("./types").RecipeScheduleExecutionLimits {
  return [["max_occurrences", 1, 100], ["max_invocations_per_occurrence", 1, 8], ["max_runtime_seconds", 60, 3600], ["start_window_seconds", 1, 3600]].every(([key, min, max]) => {
    const number = field(value, String(key));
    return typeof number === "number" && Number.isSafeInteger(number) && number >= Number(min) && number <= Number(max);
  });
}
export function schedulePreviewRequestIsCurrent(value: unknown): value is import("./types").RecipeSchedulePreviewRequest {
  const selection = field(value, "knowledge_selection");
  return hasStrings(value, ["project_id", "schedule_id"]) && positiveInteger(field(value, "expected_version")) &&
    scheduleLimitsAreCurrent(field(value, "limits")) && Array.isArray(selection) && selection.length <= 16 && selection.every(knowledgeSelectionIsCurrent);
}
export function scheduleStopBindingIsCurrent(value: unknown): value is import("./types").ScheduleStopBinding {
  const process = field(value, "process");
  return hasStrings(value, ["occurrence_id", "authorization_id", "attempt_id", "session_id", "role_generation_id", "transcript_epoch"]) &&
    ["occurrence_id", "authorization_id", "attempt_id", "session_id", "role_generation_id", "transcript_epoch"].every((key) => !!String(field(value, key)).trim()) &&
    hasNullableString(value, "resume_invocation_id") && (process === null ||
      (positiveInteger(field(process, "pid")) && positiveInteger(field(process, "process_group_id")) &&
        hasStrings(process, ["native_start_marker", "observed_started_at"]) && !!String(field(process, "native_start_marker")).trim()));
}
export function scheduleStopIntentIsCurrent(value: unknown): value is import("./types").ScheduleOccurrenceStopIntent {
  return !!value && typeof value === "object" &&
    Object.keys(value).sort().join() === "authorization_id,expected_version,occurrence_id,project_id,schedule_id" &&
    ["project_id", "schedule_id", "occurrence_id", "authorization_id"].every((key) => {
      const identity = field(value, key);
      return typeof identity === "string" && !!identity.trim() && !identity.includes("\0");
    }) && positiveInteger(field(value, "expected_version"));
}
export function scheduleExecutionCommandIsCurrent(value: unknown): boolean {
  const operation = field(value, "operation_id");
  if (typeof operation !== "string" || !operation.trim() || operation.includes("\0") || utf8Bytes(operation) > 128) return false;
  if (field(value, "kind") === "request_recipe_schedule_occurrence_stop") return scheduleStopIntentIsCurrent(field(value, "intent")) &&
    (field(value, "project_id") === undefined || field(value, "project_id") === field(field(value, "intent"), "project_id"));
  if (field(value, "kind") === "authorize_recipe_schedule_execution") return schedulePreviewRequestIsCurrent(field(value, "request")) &&
    typeof field(value, "preview_digest") === "string" && !!String(field(value, "preview_digest")).trim() &&
    (field(value, "project_id") === undefined || field(value, "project_id") === field(field(value, "request"), "project_id"));
  return field(value, "kind") === "stop_recipe_schedule_occurrence" && hasStrings(value, ["project_id", "schedule_id"]) &&
    positiveInteger(field(value, "expected_version")) && scheduleStopBindingIsCurrent(field(value, "binding")) && field(field(value, "binding"), "process") !== null;
}
export function scheduleCommandResultMatches(command: Command, result: unknown): boolean {
  if (field(result, "operation_id") !== command.operation_id) return false;
  const detail = field(result, "detail");
  if (command.kind === "authorize_recipe_schedule_execution") {
    const request = command.request;
    return field(result, "entity_kind") === "recipe_schedule" && field(result, "entity_id") === field(request, "schedule_id") &&
      field(result, "version") === Number(field(request, "expected_version")) + 1 && field(result, "state") === "authorized" &&
      hasStrings(detail, ["authorization_id"]) && !!String(field(detail, "authorization_id")).trim() && field(detail, "scope_hash") === command.preview_digest && typeof field(detail, "scheduled_execution_intake_enabled") === "boolean";
  }
  if (command.kind === "request_recipe_schedule_occurrence_stop") {
    const intent = field(detail, "intent"), expected = command.intent;
    return scheduleStopIntentIsCurrent(intent) &&
      field(result, "entity_kind") === "recipe_schedule_occurrence" && field(result, "entity_id") === field(expected, "occurrence_id") &&
      field(result, "version") === field(expected, "expected_version") && field(result, "state") === "stop_requested" &&
      ["project_id", "schedule_id", "expected_version", "occurrence_id", "authorization_id"].every((key) => field(intent, key) === field(expected, key));
  }
  const binding = field(detail, "binding"), expected = command.binding;
  return field(result, "entity_kind") === "recipe_schedule_occurrence" && field(result, "entity_id") === field(expected, "occurrence_id") &&
    field(result, "version") === command.expected_version && field(result, "state") === "stop_requested" && scheduleStopBindingIsCurrent(binding) &&
    ["occurrence_id", "authorization_id", "attempt_id", "session_id", "resume_invocation_id", "role_generation_id", "transcript_epoch"].every((key) => field(binding, key) === field(expected, key)) &&
    ["pid", "process_group_id", "native_start_marker", "observed_started_at"].every((key) => field(field(binding, "process"), key) === field(field(expected, "process"), key));
}
function scheduleKnowledgeIsCurrent(value: unknown): boolean {
  const items = field(value, "items");
  return hasStrings(value, ["block", "block_hash", "effective_digest", "fingerprint"]) &&
    ["block_serialized_bytes", "block_limit", "selected_count", "included_count", "omitted_count"].every((key) => nonnegativeInteger(field(value, key))) &&
    typeof field(value, "requires_consumer_validation") === "boolean" && Array.isArray(items) && items.length <= 16 && items.every((item) =>
      knowledgeSelectionIsCurrent(field(item, "selection")) && hasStrings(item, ["content_hash", "source_identity_hash", "checked_at", "text_hash"]) &&
      ["verified", "needs_revalidation", "source_unavailable", "user_assertion"].includes(String(field(item, "freshness"))) &&
      hasNullableString(item, "warning") && ["included", "omitted_byte_limit"].includes(String(field(item, "disposition"))));
}
function scheduleAdmissionIsCurrent(value: unknown, scope: unknown): boolean {
  const identities = ["project_id", "repository_identity", "repository_path", "base_revision", "execution_config_revision_id", "execution_config_hash", "profile_revision_id", "profile_hash", "policy_hash", "adapter_hash", "admission_id", "prepared_scope_hash", "capability_key", "capability_id", "capability_proof_hash", "native_identity_hash"];
  if (!identities.every((key) => typeof field(value, key) === "string" && !!String(field(value, key)).trim())) return false;
  const guidance = field(value, "guidance");
  return hasStrings(value, ["project_id", "repository_identity", "repository_path", "base_revision", "execution_config_revision_id", "execution_config_hash", "profile_revision_id", "profile_hash", "policy_hash", "adapter_hash", "admission_id", "prepared_scope_hash", "capability_key", "capability_id", "capability_proof_hash", "native_identity_hash"]) &&
    ["codex", "claude"].includes(String(field(value, "provider"))) && hasNullableString(value, "effective_policy_hash") &&
    field(value, "project_id") === field(scope, "project_id") &&
    Array.isArray(guidance) && guidance.every((item) => hasStrings(item, ["path", "sha256"]) && nonnegativeInteger(field(item, "bytes")));
}
function scheduleScopeIsCurrent(value: unknown): value is import("./types").RecipeScheduleScope {
  const recipe = field(value, "recipe"), admission = field(value, "admission"), criteria = field(recipe, "acceptance_criteria");
  if (!hasStrings(value, ["project_id", "schedule_id", "recipe_revision_id", "anchor_utc"]) ||
    !["daily", "weekly"].includes(String(field(value, "cadence"))) || field(value, "deadline_stop_policy") !== "interrupt_then_identity_verified_force_stop_within_runtime" ||
    !scheduleLimitsAreCurrent(field(value, "limits")) || !scheduleKnowledgeIsCurrent(field(value, "knowledge")) ||
    !hasStrings(recipe, ["id", "title", "description", "definition_id", "workflow_version", "workflow_hash"]) || !positiveInteger(field(recipe, "revision")) ||
    !Number.isSafeInteger(field(recipe, "priority")) || !Array.isArray(criteria) || !criteria.every((item) => typeof item === "string") ||
    field(recipe, "id") !== field(value, "recipe_revision_id") || !scheduleAdmissionIsCurrent(admission, value)) return false;
  if (field(value, "schema") === 1) return ["preset", "workspace_mode", "reviewer_admission", "check_ids", "check_execution_policy", "max_repair_attempts", "writer_resume_policy", "human_request_changes_policy"].every((key) => field(value, key) === undefined);
  const checks = field(value, "check_ids"), reviewer = field(value, "reviewer_admission");
  if (field(value, "schema") !== 2 || field(value, "workspace_mode") !== "isolated_worktree" ||
    field(admission, "session_contract") !== "retained" || !Array.isArray(checks) ||
    !checks.every((id) => typeof id === "string" && !!id.trim()) || new Set(checks).size !== checks.length ||
    field(value, "check_execution_policy") !== "per_candidate_human_authorization" ||
    field(value, "writer_resume_policy") !== "explicit_retained_only_within_allowance" ||
    field(value, "human_request_changes_policy") !== "separate_draft_and_new_grant_required") return false;
  if (field(value, "preset") === "single_agent") return reviewer === null && field(value, "max_repair_attempts") === 0;
  return field(value, "preset") === "build_and_review" && field(value, "max_repair_attempts") === 1 &&
    Number(field(field(value, "limits"), "max_invocations_per_occurrence")) >= 4 &&
    scheduleAdmissionIsCurrent(reviewer, value) && field(reviewer, "session_contract") === "fresh" &&
    ["repository_identity", "repository_path", "base_revision", "execution_config_revision_id", "execution_config_hash"].every((key) => field(reviewer, key) === field(admission, key)) &&
    JSON.stringify(field(reviewer, "guidance")) === JSON.stringify(field(admission, "guidance"));
}
export function scheduleOccurrenceIsCurrent(value: unknown, schedule: unknown): boolean {
  const intent = field(value, "stop_intent"), available = field(value, "stop_intent_available");
  if (intent !== undefined || available !== undefined) {
    if (typeof available !== "boolean" || !hasNullableString(value, "stop_intent_reason") ||
      !hasNullableString(value, "stop_requested_at") || !hasNullableString(value, "stop_reason") ||
      (available ? !scheduleStopIntentIsCurrent(intent) ||
        field(intent, "project_id") !== field(schedule, "project_id") || field(intent, "schedule_id") !== field(schedule, "id") ||
        field(intent, "expected_version") !== field(schedule, "version") || field(intent, "occurrence_id") !== field(value, "id") ||
        field(intent, "authorization_id") !== field(value, "authorization_id") || field(value, "stop_requested_at") !== null ||
        !["waiting", "reserved", "running", "recovery_required"].includes(String(field(value, "outcome"))) ||
        (field(value, "allowance_reserved") !== true && field(value, "allowance_reserved") !== 1) : intent !== null)) return false;
  }
  const binding = field(value, "stop_binding"), reserved = field(value, "allowance_reserved");
  return hasStrings(value, ["id", "scheduled_for_utc"]) && scheduleOutcomes.includes(String(field(value, "outcome"))) &&
    ["authorization_id", "task_id", "attempt_id", "reason", "runtime_deadline_utc"].every((key) => hasNullableString(value, key)) &&
    (typeof reserved === "boolean" || reserved === 0 || reserved === 1) && nonnegativeInteger(field(value, "invocations_spent")) &&
    (binding === undefined || binding === null || (scheduleStopBindingIsCurrent(binding) && field(binding, "occurrence_id") === field(value, "id") &&
      field(binding, "authorization_id") === field(value, "authorization_id") &&
      (["single_agent", "build_and_review"].includes(String(field(schedule, "recipe_preset"))) || field(binding, "attempt_id") === field(value, "attempt_id"))));
}
export function scheduleExecutionStateIsCurrent(value: unknown): boolean {
  const mode = field(value, "mode"), grant = field(value, "authorization"), occurrences = field(value, "occurrences");
  const supported = field(value, "execution_authorization_supported");
  if (supported !== undefined || field(value, "execution_qualification_state") !== undefined || field(value, "execution_authorization_reason") !== undefined) {
    if (typeof supported !== "boolean" || typeof field(value, "execution_authorization_reason") !== "string" ||
      field(value, "execution_qualification_state") !== (supported ? "preview_required" : "unsupported")) return false;
  }
  // Older services omit the complete execution extension. Partial extensions are refused.
  if (mode === undefined) return ["authorization", "active_authorization_id", "execution_intake_enabled", "occurrences"].every((key) => field(value, key) === undefined);
  return ["create_draft", "run_authorized_research", "run_authorized_recipe"].includes(String(mode)) && hasNullableString(value, "active_authorization_id") &&
    typeof field(value, "execution_intake_enabled") === "boolean" && Array.isArray(occurrences) && occurrences.length <= 100 && occurrences.every((occurrence) => scheduleOccurrenceIsCurrent(occurrence, value)) &&
    (grant === null ? field(value, "active_authorization_id") === null :
      hasStrings(grant, ["id", "scope_hash", "created_at"]) && field(grant, "id") === field(value, "active_authorization_id") &&
      ["revoked_at", "restore_invalidated_at"].every((key) => hasNullableString(grant, key)) && scheduleLimitsAreCurrent(grant) &&
      nonnegativeInteger(field(grant, "reserved_occurrences")) && nonnegativeInteger(field(grant, "remaining_occurrences")) &&
      Number(field(grant, "reserved_occurrences")) + Number(field(grant, "remaining_occurrences")) === field(grant, "max_occurrences") &&
      scheduleScopeIsCurrent(field(grant, "scope")) &&
      (mode === "create_draft" || field(field(grant, "scope"), "recipe_revision_id") === field(value, "recipe_revision_id") && (mode === "run_authorized_research" ? field(field(grant, "scope"), "schema") === 1 :
        field(field(grant, "scope"), "schema") === 2 && field(field(grant, "scope"), "preset") === field(value, "recipe_preset"))) &&
      field(field(grant, "scope"), "project_id") === field(value, "project_id") &&
      field(field(grant, "scope"), "schedule_id") === field(value, "id") &&
      ["max_occurrences", "max_invocations_per_occurrence", "max_runtime_seconds", "start_window_seconds"].every((key) => field(field(field(grant, "scope"), "limits"), key) === field(grant, key)));
}
export function knowledgeSelectionIsCurrent(value: unknown): value is import("./types").KnowledgeSelection {
  return hasStrings(value, ["entry_id", "revision_id", "observation_fingerprint"]) &&
    ["entry_id", "revision_id", "observation_fingerprint"].every((key) => { const text = field(value, key); return typeof text === "string" && !!text.trim() && !text.includes("\0") && utf8Bytes(text) <= 128; }) &&
    Number.isSafeInteger(field(value, "expected_entry_version")) && typeof field(value, "acknowledged_warning") === "boolean";
}
export function knowledgeCommandIsCurrent(value: unknown): value is import("./types").KnowledgeCommand {
  if (field(value, "kind") === "revalidate") return hasStrings(value, ["project_id", "operation_id", "entry_id", "revision_id", "expected_observation"]) && Number.isSafeInteger(field(value, "expected_version"));
  if (field(value, "kind") !== "mutate") return false;
  const request = field(value, "request"), mutation = field(request, "mutation");
  if (!hasStrings(request, ["project_id", "operation_id"])) return false;
  if (field(mutation, "kind") === "archive") return hasStrings(mutation, ["entry_id"]) && Number.isSafeInteger(field(mutation, "expected_version"));
  if (field(mutation, "kind") === "approve") return hasStrings(mutation, ["entry_id", "revision_id"]) && nullableText(field(mutation, "replaces_revision_id")) && Number.isSafeInteger(field(mutation, "expected_version"));
  if (field(mutation, "kind") !== "draft") return false;
  const body = field(mutation, "body"), sources = field(body, "sources"), tags = field(body, "tags");
  return nullableText(field(mutation, "entry_id")) && (field(mutation, "expected_version") === null || Number.isSafeInteger(field(mutation, "expected_version"))) &&
    hasStrings(body, ["title", "statement", "category"]) && Array.isArray(tags) && tags.length <= 16 && tags.every((tag) => typeof tag === "string") &&
    Array.isArray(sources) && sources.length > 0 && sources.length <= 16 && sources.every((source) => {
      switch (field(source, "kind")) {
        case "user_assertion": return true;
        case "repository": return hasStrings(source, ["commit", "path"]);
        case "historical": return hasStrings(field(source, "source"), ["task_id", "kind", "id"]) && nullableText(field(field(source, "source"), "attempt_id"));
        case "finding": return hasStrings(source, ["field", "content_hash"]) && hasStrings(field(source, "parent"), ["kind", "attempt_id"]) && ["explorer", "single_agent_review"].includes(String(field(field(source, "parent"), "kind")));
        case "conversation": { const selected = field(field(source, "source"), "messages"); return hasStrings(field(source, "source"), ["conversation_id"]) && Array.isArray(selected) && selected.length > 0 && selected.length <= 16 && selected.every((message) => hasStrings(message, ["message_id", "content_hash"])); }
        default: return false;
      }
    });
}
