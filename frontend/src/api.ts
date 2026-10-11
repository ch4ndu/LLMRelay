import type {
  AppState,
  CmuxKeyboardControlAction,
  CmuxKeyboardControlOutcome,
  CmuxViewOutcome,
  Command,
  CompatibilityExplanation,
  HistoryExportText,
  HistoryPaneOutcome,
  HistoryPaneView,
  HistorySelection,
  ModelCatalog,
  Operation,
  ProtocolDescriptor,
  Provider,
  ProviderValidationBatch,
  ProviderValidationPreview,
  ProviderValidationSelection,
  ProviderValidationStartOutcome,
  RestartPreview,
  WorkflowCommand,
  WorkflowCommandResult,
  RolePreparation,
  StateCursor,
  StateWaitResult,
  TaskContent,
  TranscriptPage,
  GuidanceReauthorizationPreview,
} from "./types";
import { ROLES } from "./types";

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly operationId?: string,
    readonly ambiguous = false,
  ) {
    super(message);
  }
}
export class ProtocolError extends Error {
  constructor(message: string, readonly reason: string) {
    super(message);
  }
}
const PROTOCOL_GENERATION = 1;
const PROTOCOL_FEATURE = "http_operational_v1";
const HISTORY_VIEW_FEATURE = "history_view_v1";
const PROVIDER_VALIDATION_FEATURE = "provider_validation_v1";
const MULTI_WORKFLOW_FEATURE = "multi_workflow_v1";
const PROTOCOL_GUIDANCE =
  "Reload the dashboard or use a CLI built for this service version. Do not restart the service automatically.";
let negotiated: ProtocolDescriptor | undefined;
let negotiating: Promise<ProtocolDescriptor> | undefined;
let negotiationEpoch = 0;
let observedIncarnation: string | undefined;

function protocolDescriptor(value: unknown): ProtocolDescriptor {
  const candidate = value as Partial<ProtocolDescriptor> | null;
  if (
    !candidate || typeof candidate !== "object" ||
    candidate.generation !== PROTOCOL_GENERATION ||
    typeof candidate.server_version !== "string" || !candidate.server_version ||
    typeof candidate.instance_id !== "string" || !candidate.instance_id ||
    !Array.isArray(candidate.supported_features) ||
    !candidate.supported_features.every((feature) =>
      typeof feature === "string"
    ) ||
    !candidate.supported_features.includes(PROTOCOL_FEATURE)
  ) {
    throw new ProtocolError(
      `Incompatible service protocol. ${PROTOCOL_GUIDANCE}`,
      "incompatible_generation",
    );
  }
  return candidate as ProtocolDescriptor;
}

async function negotiate(): Promise<ProtocolDescriptor> {
  for (;;) {
    if (negotiated) return negotiated;
    const epoch = negotiationEpoch;
    if (!negotiating) {
      const flight = (async () => {
        const response = await fetch("/api/protocol", {
          credentials: "same-origin",
          signal: AbortSignal.timeout(READ_TIMEOUT_MS),
        });
        if (!response.ok) {
          if (response.status === 401 || response.status === 403) {
            throw new ApiError("browser session required", response.status);
          }
          throw new ProtocolError(
            `Protocol discovery failed. ${PROTOCOL_GUIDANCE}`,
            "malformed",
          );
        }
        let wire: unknown;
        try {
          wire = await response.json();
        } catch {
          throw new ProtocolError(
            `Invalid protocol descriptor. ${PROTOCOL_GUIDANCE}`,
            "malformed",
          );
        }
        const descriptor = protocolDescriptor(wire);
        if (epoch === negotiationEpoch) {
          if (observedIncarnation && descriptor.instance_id !== observedIncarnation) {
            // A state read can predate a replacement service. Retry discovery once
            // without treating that older observation as current authority.
            observedIncarnation = undefined;
            negotiationEpoch += 1;
            negotiating = undefined;
          } else {
            negotiated = descriptor;
          }
        }
        return descriptor;
      })();
      negotiating = flight;
      void flight.finally(() => {
        if (negotiating === flight) negotiating = undefined;
      }).catch(() => {});
    }
    const descriptor = await negotiating;
    if (epoch === negotiationEpoch && negotiated) return negotiated;
  }
}

function observeIncarnation(instanceId: string): void {
  if (observedIncarnation !== instanceId ||
    (negotiated && negotiated.instance_id !== instanceId)) {
    negotiationEpoch += 1;
    negotiated = undefined;
    negotiating = undefined;
  }
  observedIncarnation = instanceId;
}

const wireGeneration = (value: unknown): value is number =>
  Number.isSafeInteger(value) && typeof value === "number" &&
  value >= 0 && value <= 0xffff_ffff;

function protocolRefusal(value: unknown): ProtocolError | undefined {
  const record = value as { protocol_error?: unknown } | null;
  const detail = record && typeof record === "object"
    ? record.protocol_error as Record<string, unknown> | null
    : null;
  if (!detail || typeof detail !== "object" || Array.isArray(detail)) return;
  const reasons = [
    "missing",
    "malformed",
    "incompatible_generation",
    "missing_required_feature",
    "wrong_client_kind",
  ];
  if (
    typeof detail.reason !== "string" || !reasons.includes(detail.reason) ||
    !wireGeneration(detail.expected_generation) ||
    typeof detail.guidance !== "string" || !detail.guidance ||
    !(detail.observed_generation === null ||
      wireGeneration(detail.observed_generation))
  ) return;
  return new ProtocolError(detail.guidance, detail.reason);
}
export const READ_TIMEOUT_MS = 8_000;
export const MUTATION_TIMEOUT_MS = 15_000;
export const transportTimeouts = {
  read: READ_TIMEOUT_MS,
  mutation: MUTATION_TIMEOUT_MS,
};
async function json<T>(
  path: string,
  init?: RequestInit,
  timeoutMs = transportTimeouts.read,
  features: readonly string[] = [],
): Promise<T> {
  const dispatchedMutation = init?.method !== undefined &&
    !["GET", "HEAD"].includes(init.method.toUpperCase());
  let businessDispatched = false;
  const controller = new AbortController();
  let timedOut = false;
  const deadline = window.setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, timeoutMs);
  const abortFromCaller = () => controller.abort();
  init?.signal?.addEventListener("abort", abortFromCaller, { once: true });
  try {
    const headers = new Headers(init?.headers);
    headers.set("content-type", "application/json");
    if (path !== "/api/bootstrap" && path !== "/api/protocol") {
      if (!negotiated) await negotiate();
      headers.set(
        "x-llmrelay-protocol",
        JSON.stringify({
          generation: PROTOCOL_GENERATION,
          client_kind: "browser",
          required_features: [PROTOCOL_FEATURE, ...features],
        }),
      );
    }
    businessDispatched = true;
    const response = await fetch(path, {
      credentials: "same-origin",
      ...init,
      headers,
      signal: controller.signal,
    });
    const text = await response.text();
    let value: unknown;
    try {
      value = text ? JSON.parse(text) : {};
    } catch {
      value = { error: text || response.statusText };
    }
    if (!response.ok) {
      if (response.status === 409) {
        const refusal = protocolRefusal(value);
        if (refusal) throw refusal;
      }
      const body = value as { error?: string; operation_id?: string };
      throw new ApiError(
        body.error || response.statusText,
        response.status,
        body.operation_id,
      );
    }
    return value as T;
  } catch (cause) {
    if (cause instanceof ApiError || cause instanceof ProtocolError) {
      throw cause;
    }
    if (timedOut && businessDispatched) {
      throw new ApiError(
        "The local service did not respond before the request deadline. Refresh and reconcile before retrying.",
        0,
        undefined,
        true,
      );
    }
    if (dispatchedMutation && businessDispatched) {
      throw new ApiError(
        "The local service connection ended after the mutation may have been dispatched. Refresh and reconcile before retrying.",
        0,
        undefined,
        true,
      );
    }
    throw cause;
  } finally {
    window.clearTimeout(deadline);
    init?.signal?.removeEventListener("abort", abortFromCaller);
  }
}
export async function bootstrap(): Promise<void> {
  const hash = new URLSearchParams(location.hash.slice(1));
  const token = hash.get("bootstrap");
  if (!token) return;
  await json("/api/bootstrap", {
    method: "POST",
    body: JSON.stringify({ token }),
  }, transportTimeouts.mutation);
  history.replaceState(null, "", location.pathname + location.search);
}
const field = (value: unknown, name: string): unknown =>
  typeof value === "object" && value !== null && !Array.isArray(value)
    ? Reflect.get(value, name)
    : undefined;
const decisionIsCurrent = (value: unknown): boolean => {
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
const cursorIsCanonical = (incarnation: string, revision: string) =>
  incarnation.length > 0 &&
  new TextEncoder().encode(incarnation).length <= MAX_INCARNATION_BYTES &&
  /^(0|[1-9][0-9]*)$/.test(revision) &&
  (revision.length < MAX_REVISION.length ||
    (revision.length === MAX_REVISION.length && revision <= MAX_REVISION));
const hasStrings = (value: unknown, names: string[]) =>
  names.every((name) => typeof field(value, name) === "string");
const hasNullableString = (value: unknown, name: string) => {
  const item = field(value, name);
  return item === null || typeof item === "string";
};
const taskTargetIsCurrent = (value: unknown) =>
  hasStrings(value, ["project_id", "task_id"]) &&
  Number.isInteger(field(value, "task_version"));
const attentionTargetIsCurrent = (value: unknown): boolean => {
  switch (field(value, "kind")) {
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
const compatibilityIsCurrent = (value: unknown) => {
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
const attentionItemIsCurrent = (value: unknown) => {
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
const taskActionsAreCurrent = (state: AppState) => {
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
const providerValidationPreviewIsCurrent = (
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
const providerValidationBatchIsCurrent = (
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
const providerValidationStartOutcomeIsFor = (operation: string) =>
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
  return hasStrings(value, ["attempt_id", "definition_state", "stage_reason"]) &&
    singleAgentStages.includes(String(field(value, "stage"))) &&
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
const taskWorkflowIsCurrent = (value: unknown) => {
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
const workflowExecutionIsCurrent = (value: unknown) =>
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
const sessionGenerationIsCurrent = (value: unknown) => {
  const status = field(value, "generation_status");
  const newer = field(value, "has_newer_generation");
  return (status === undefined || roleGenerationStatuses.includes(String(status))) &&
    (newer === undefined || typeof newer === "boolean" || newer === 0 || newer === 1);
};
const workflowCommandResultIsCurrent = (
  value: unknown,
): value is WorkflowCommandResult =>
  hasStrings(value, ["operation_id", "entity_kind", "entity_id", "state"]) &&
  (field(value, "version") === null ||
    Number.isSafeInteger(field(value, "version")));
// Absent from older services; when present every part must be current.
const providerValidationStateIsCurrent = (value: unknown) => {
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
const currentSnapshot = (state: AppState): AppState => {
  const recipeArray = (value: unknown, keys: string[]) =>
    Array.isArray(value) && value.every((item) =>
      keys.every((key) => typeof field(item, key) === "string") &&
      Number.isSafeInteger(field(item, "version")) &&
      typeof field(item, "archived") === "boolean"
    );
  if (
    state.schema !== 8 ||
    !recipeArray(state.profile_sets, ["id", "project_id", "name", "revision_id", "config_revision_id", "configuration_hash"]) ||
    !state.profile_sets.every((item) =>
      Number.isSafeInteger(item.revision) &&
      item.roles !== null && typeof item.roles === "object" &&
      Object.keys(item.roles).length === 6 &&
      ["manager", "explorer", "plan_reviewer", "implementer", "code_reviewer", "final_verifier"].every((role) => {
        const config = field(item.roles, role);
        return ["codex", "claude"].includes(String(field(config, "provider"))) &&
          typeof field(config, "model") === "string" &&
          typeof field(config, "effort") === "string";
      })
    ) ||
    !recipeArray(state.task_recipes, ["id", "project_id", "name", "revision_id", "profile_revision_id", "config_revision_id", "configuration_hash", "workflow_version", "workflow_hash", "title", "description"]) ||
    !state.task_recipes.every((item) =>
      Number.isSafeInteger(item.revision) && Number.isSafeInteger(item.priority) &&
      Array.isArray(item.acceptance_criteria) && item.acceptance_criteria.every((text) => typeof text === "string") &&
      Array.isArray(item.required_check_ids) && item.required_check_ids.every((id) => typeof id === "string")
    ) ||
    !recipeArray(state.recipe_schedules, ["id", "project_id", "name", "recipe_revision_id", "recipe_name", "recipe_config_revision_id", "cadence", "anchor_utc"]) ||
    !state.recipe_schedules.every((item) =>
      typeof item.paused === "boolean" && typeof item.recipe_archived === "boolean" &&
      ["daily", "weekly"].includes(item.cadence) &&
      (item.next_fire_utc === null || typeof item.next_fire_utc === "string") &&
      (item.last_fire === null ||
        (["task_created", "skipped_ineligible", "missed"].includes(item.last_fire.outcome) &&
          typeof item.last_fire.scheduled_for_utc === "string" &&
          Number.isSafeInteger(item.last_fire.missed_count)))
    )
    || !Array.isArray(state.tasks) || !state.tasks.every((task) => {
      if (typeof task.can_archive !== "boolean") return false;
      const deletion = task.draft_deletion;
      if (deletion != null &&
        (typeof deletion.allowed !== "boolean" ||
          (deletion.reason !== null && typeof deletion.reason !== "string"))) {
        return false;
      }
      const provenance = task.recipe_provenance;
      const requiredCheckIds = field(provenance, "required_check_ids");
      return provenance == null ||
        (["recipe_id", "recipe_name", "recipe_revision_id", "profile_revision_id", "config_revision_id", "configuration_hash", "workflow_version", "workflow_hash"].every((key) => typeof field(provenance, key) === "string") &&
          Number.isSafeInteger(field(provenance, "recipe_revision")) &&
          Array.isArray(requiredCheckIds) && requiredCheckIds.every((id) => typeof id === "string") &&
          (field(provenance, "schedule_id") === null || typeof field(provenance, "schedule_id") === "string") &&
          (field(provenance, "scheduled_for_utc") === null || typeof field(provenance, "scheduled_for_utc") === "string"));
    })
  ) {
    throw new Error("The service returned unsupported recipe state. Refresh after updating the service; recipe controls were not applied.");
  }
  if (
    typeof state.incarnation !== "string" ||
    typeof state.revision !== "string" ||
    !cursorIsCanonical(state.incarnation, state.revision)
  ) {
    throw new Error(
      "The service returned a snapshot without a canonical state cursor. Refresh after updating the service; the last state was kept.",
    );
  }
  if (
    !Array.isArray(state.decisions) || !state.decisions.every(decisionIsCurrent)
  ) {
    throw new Error(
      "The service returned an unsupported decision schema. Refresh after updating the service; no decision controls were applied.",
    );
  }
  if (
    !Array.isArray(state.attention) ||
    !state.attention.every(attentionItemIsCurrent) ||
    !taskActionsAreCurrent(state)
  ) {
    throw new Error(
      "The service returned an unsupported attention schema. Refresh after updating the service; no attention navigation was applied.",
    );
  }
  if (
    !Array.isArray(state.capabilities) ||
    !state.capabilities.every((item) =>
      compatibilityIsCurrent(item.compatibility)
    ) ||
    !(state.trip_setups || []).every((setup) =>
      setup.selected_profiles.every((selection) =>
        compatibilityIsCurrent(selection.compatibility)
      )
    )
  ) {
    throw new Error(
      "The service returned unsupported compatibility data. Refresh after updating the service.",
    );
  }
  if (!providerValidationStateIsCurrent(state.provider_validation)) {
    throw new Error(
      "The service returned unsupported provider check data. Refresh after updating the service.",
    );
  }
  if (
    !state.tasks.every((task) => taskWorkflowIsCurrent(task.workflow)) ||
    !workflowExecutionIsCurrent(state.workflow_execution) ||
    !(state.active_sessions || []).every(sessionGenerationIsCurrent)
  ) {
    throw new Error(
      "The service returned unsupported workflow data. Refresh after updating the service; no workflow decision was applied.",
    );
  }
  return state;
};
export const getState = async (signal?: AbortSignal): Promise<AppState> => {
  const state = currentSnapshot(
    await json<AppState>(
      "/api/state",
      signal && { signal },
      transportTimeouts.read,
    ),
  );
  observeIncarnation(state.incarnation);
  return state;
};
interface StateWaitWire {
  outcome?: unknown;
  incarnation?: unknown;
  revision?: unknown;
  state?: AppState;
}
/** Parks until the committed revision differs from `cursor` or the wait times out. */
export const waitForState = async (
  cursor: StateCursor,
  timing: { waitMs: number; requestMs: number },
  signal: AbortSignal,
): Promise<StateWaitResult> => {
  const query = new URLSearchParams({
    incarnation: cursor.incarnation,
    revision: cursor.revision,
    timeout_ms: String(timing.waitMs),
  });
  const { outcome, incarnation, revision, state } = await json<StateWaitWire>(
    `/api/state/wait?${query}`,
    { signal },
    timing.requestMs,
  );
  if (
    typeof incarnation !== "string" || typeof revision !== "string" ||
    !cursorIsCanonical(incarnation, revision)
  ) {
    throw new Error(
      "The service returned a state wait without a canonical cursor; the last state was kept.",
    );
  }
  observeIncarnation(incarnation);
  if (outcome === "unchanged") {
    if (
      state !== undefined || incarnation !== cursor.incarnation ||
      revision !== cursor.revision
    ) {
      throw new Error(
        "The service returned an unchanged state wait for a different cursor; the last state was kept.",
      );
    }
    return { outcome, incarnation, revision };
  }
  if ((outcome !== "state_changed" && outcome !== "reset") || !state) {
    throw new Error(
      "The service returned an unsupported state wait outcome; the last state was kept.",
    );
  }
  const snapshot = currentSnapshot(state);
  if (snapshot.incarnation !== incarnation || snapshot.revision !== revision) {
    throw new Error(
      "The service returned a state wait whose snapshot does not match its cursor; the last state was kept.",
    );
  }
  return { outcome, incarnation, revision, state: snapshot };
};
export const getRestartPreview = async (): Promise<RestartPreview> => {
  const preview = await json<RestartPreview>(
    "/api/restart-preview",
    undefined,
    transportTimeouts.mutation,
  );
  if (
    preview.decision_schema !== 1 || !Array.isArray(preview.sessions) ||
    typeof preview.snapshot?.notice !== "string" ||
    !preview.sessions.every((session) =>
      decisionIsCurrent(session.decision) &&
      [
        "resumable",
        "fresh_only",
        "blocked",
        "uncertain",
        "awaiting_approval",
        "complete",
      ].includes(session.classification) &&
      typeof session.can_resume_now === "boolean" &&
      typeof session.could_resume_after_confirmed_shutdown === "boolean"
    )
  ) {
    throw new Error(
      "Restart preview has an unsupported decision schema; no resume action is available from it.",
    );
  }
  return preview;
};
export const getRolePreparations = async (taskId: string) => {
  const preparations = await json<RolePreparation[]>(
    `/api/tasks/${encodeURIComponent(taskId)}/role-preparations`,
    undefined,
    transportTimeouts.read,
  );
  if (
    !Array.isArray(preparations) ||
    !preparations.every((item) => compatibilityIsCurrent(item.compatibility))
  ) {
    throw new Error(
      "The service returned unsupported role compatibility data.",
    );
  }
  return preparations;
};
const taskContentRecordIsCurrent = (value: unknown) =>
  ["report", "rework_request"].includes(String(field(value, "kind"))) &&
  hasStrings(value, ["id", "created_at", "summary"]) &&
  ["plan", "role", "outcome", "review_kind"].every((name) => {
    const item = field(value, name);
    return item === undefined || item === null || typeof item === "string";
  });
const executionActors: string[] = [...ROLES, "single_agent"];
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
const currentInputIsCurrent = (value: unknown, attemptId: string | null) => {
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
/** Plans, reports and feedback for the task's current attempt. */
export const getTaskContent = async (taskId: string, signal?: AbortSignal) => {
  const content = await json<TaskContent>(
    `/api/tasks/${encodeURIComponent(taskId)}/content`,
    signal && { signal },
    transportTimeouts.read,
  );
  if (
    content.task_id !== taskId ||
    !(content.attempt_id === null || typeof content.attempt_id === "string") ||
    typeof content.content_revision !== "string" ||
    typeof content.truncated !== "boolean" ||
    !Array.isArray(content.records) ||
    !content.records.every(taskContentRecordIsCurrent) ||
    !currentInputIsCurrent(content.current_input, content.attempt_id)
  ) {
    throw new Error(
      "The service returned unsupported task content. Refresh after updating the service.",
    );
  }
  return content;
};
// Older services send neither part. A present comparison must be complete:
// a pinned baseline hashes to the reviewed pin, and a diff exists exactly
// when a baseline was available.
const guidanceComparisonIsCurrent = (file: unknown) => {
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
export const getGuidanceReauthorization = async (taskId: string) => {
  const preview = await json<GuidanceReauthorizationPreview>(
    `/api/tasks/${encodeURIComponent(taskId)}/guidance-reauthorization`,
    undefined, transportTimeouts.read,
  );
  if (preview.task_id !== taskId ||
    !hasStrings(preview, ["attempt_id", "plan_hash", "config_revision_id", "policy_hash"]) ||
    !Number.isSafeInteger(preview.expected_version) || !Array.isArray(preview.files) ||
    (preview.workspace !== undefined &&
      !hasStrings(preview.workspace, ["path", "base_revision"])) ||
    preview.files.length > 32 || !preview.files.every((file) =>
      hasStrings(file, ["path", "previous_sha256", "sha256", "content"]) &&
      /^[0-9a-f]{64}$/.test(file.previous_sha256) && /^[0-9a-f]{64}$/.test(file.sha256) &&
      guidanceComparisonIsCurrent(file)
    )) {
    throw new Error("The service returned unsupported instruction-file recovery information. Refresh after updating the service.");
  }
  return preview;
};

export const getModelCatalog = (provider: Provider) =>
  json<ModelCatalog>(
    `/api/model-catalog?provider=${encodeURIComponent(provider)}`,
    undefined,
    transportTimeouts.read,
  );
export const diagnostics = () =>
  json<{ events: Array<Record<string, unknown>> }>(
    "/api/diagnostics",
    undefined,
    transportTimeouts.read,
  );
const preserveMutationIdentity = <T>(
  request: Promise<T>,
  operationId: string | undefined,
) =>
  request.catch((cause) => {
    if (cause instanceof ApiError && cause.ambiguous && !cause.operationId) {
      throw new ApiError(cause.message, cause.status, operationId, true);
    }
    throw cause;
  });
export const command = (body: Command) =>
  preserveMutationIdentity(
    json<{ result: Record<string, unknown> }>("/api/command", {
      method: "POST",
      body: JSON.stringify(body),
    }, transportTimeouts.mutation),
    body.operation_id,
  );
export const operation = async <T = Record<string, unknown>>(
  body: Operation,
) => {
  const response = await preserveMutationIdentity(
    json<{ result: T }>("/api/operation", {
      method: "POST",
      body: JSON.stringify(body),
    }, transportTimeouts.mutation),
    typeof body.operation_id === "string" ? body.operation_id : undefined,
  );
  return response.result;
};
export const viewCmuxSurface = (
  sessionId: string,
  id: string,
) =>
  operation<CmuxViewOutcome>({
    kind: "cmux_view",
    operation_id: id,
    session_id: sessionId,
  });
export const setCmuxKeyboardControl = (
  sessionId: string,
  surfaceRouteId: string,
  expectedBindingRevision: number,
  expectedControlRevision: number,
  action: CmuxKeyboardControlAction,
  id: string,
) =>
  operation<CmuxKeyboardControlOutcome>({
    kind: "cmux_set_keyboard_control",
    operation_id: id,
    session_id: sessionId,
    surface_route_id: surfaceRouteId,
    expected_binding_revision: expectedBindingRevision,
    expected_control_revision: expectedControlRevision,
    action,
  });
export const discardUnknownCmuxSurface = (
  sessionId: string,
  surfaceRouteId: string,
  id: string,
) =>
  operation<CmuxViewOutcome>({
    kind: "cmux_discard_unknown",
    operation_id: id,
    session_id: sessionId,
    surface_route_id: surfaceRouteId,
  });
/**
 * Operations added after protocol generation 1 declare their feature on the
 * request; a service that does not advertise it is never sent the request.
 */
const featureOperation = async <T>(
  feature: string,
  body: Operation,
  current: (value: unknown) => value is T,
): Promise<T> => {
  const descriptor = negotiated ?? await negotiate();
  if (!descriptor.supported_features.includes(feature)) {
    throw new ProtocolError(
      `This service version does not support this action. ${PROTOCOL_GUIDANCE}`,
      "missing_required_feature",
    );
  }
  const response = await preserveMutationIdentity(
    json<{ result: unknown }>("/api/operation", {
      method: "POST",
      body: JSON.stringify(body),
    }, transportTimeouts.mutation, [feature]),
    typeof body.operation_id === "string" ? body.operation_id : undefined,
  );
  if (!current(response.result)) {
    throw new Error(
      "The service returned an unsupported response. Refresh after updating the service.",
    );
  }
  return response.result;
};
const transcriptPageIsCurrent = (value: unknown): value is TranscriptPage => {
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
const historyPaneOutcomeIsCurrent = (
  value: unknown,
): value is HistoryPaneOutcome =>
  typeof field(value, "message") === "string" &&
  historyPaneViewIsCurrent(field(value, "view"));
const historyPaneViewsAreCurrent = (
  value: unknown,
): value is HistoryPaneView[] =>
  Array.isArray(value) && value.every(historyPaneViewIsCurrent);
const historyExportTextIsCurrent = (
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

/** One page of one exact epoch; follow `next_sequence` while `has_more`. */
export const readHistoryPage = (
  selection: HistorySelection,
  afterSequence: number,
  limitBytes = 512 * 1024,
) =>
  featureOperation(HISTORY_VIEW_FEATURE, {
    kind: "history_page",
    session_id: selection.session_id,
    transcript_epoch: selection.transcript_epoch,
    after_sequence: afterSequence,
    limit_bytes: limitBytes,
  }, transcriptPageIsCurrent);
/** The complete retained selection, for search and download. */
export const getHistoryExportText = (selection: HistorySelection) =>
  featureOperation(HISTORY_VIEW_FEATURE, {
    kind: "history_export_text",
    session_id: selection.session_id,
    transcript_epoch: selection.transcript_epoch,
  }, historyExportTextIsCurrent);
/**
 * Opens a read-only cmux pane. Reusing `id` replays the recorded outcome; a
 * new request after an unknown outcome must name that unknown view.
 */
export const openHistoryPane = (
  selection: HistorySelection,
  id: string,
  acknowledgedUnknownViewId?: string,
) =>
  featureOperation(HISTORY_VIEW_FEATURE, {
    kind: "history_pane_open",
    operation_id: id,
    session_id: selection.session_id,
    transcript_epoch: selection.transcript_epoch,
    acknowledged_unknown_view_id: acknowledgedUnknownViewId ?? null,
  }, historyPaneOutcomeIsCurrent);
export const listHistoryPaneViews = (selection: HistorySelection) =>
  featureOperation(HISTORY_VIEW_FEATURE, {
    kind: "history_pane_views",
    session_id: selection.session_id,
    transcript_epoch: selection.transcript_epoch,
  }, historyPaneViewsAreCurrent);
/** Inspects providers locally and plans each check; no model is contacted. */
export const previewProviderValidation = (
  selections: ProviderValidationSelection[],
) =>
  featureOperation(PROVIDER_VALIDATION_FEATURE, {
    kind: "provider_validation_preview",
    selections,
  }, providerValidationPreviewIsCurrent);
/**
 * Starts exactly the previewed plan. Reusing `id` replays the same batch; a
 * changed local input since the preview is refused, so preview again.
 */
export const startProviderValidation = (
  preview: ProviderValidationPreview,
  selections: ProviderValidationSelection[],
  approval: { authorizeModelCalls: boolean; maxModelCalls: number },
  id: string,
) =>
  featureOperation(PROVIDER_VALIDATION_FEATURE, {
    kind: "provider_validation_start",
    operation_id: id,
    preview_hash: preview.preview_hash,
    selections,
    authorize_model_calls: approval.authorizeModelCalls,
    max_model_calls: approval.maxModelCalls,
  }, providerValidationBatchIsCurrent);
/**
 * Settles the exact earlier start request without running anything: returns
 * its batch, or closes a request that never started so it never can. The
 * request must repeat the start exactly.
 */
export const reconcileProviderValidationStart = (
  preview: ProviderValidationPreview,
  selections: ProviderValidationSelection[],
  approval: { authorizeModelCalls: boolean; maxModelCalls: number },
  id: string,
) =>
  featureOperation(PROVIDER_VALIDATION_FEATURE, {
    kind: "provider_validation_reconcile",
    operation_id: id,
    preview_hash: preview.preview_hash,
    selections,
    authorize_model_calls: approval.authorizeModelCalls,
    max_model_calls: approval.maxModelCalls,
  }, providerValidationStartOutcomeIsFor(id));
export const inspectProviderValidation = (batchId: string) =>
  featureOperation(PROVIDER_VALIDATION_FEATURE, {
    kind: "provider_validation_inspect",
    batch_id: batchId,
  }, providerValidationBatchIsCurrent);
/**
 * Submits one multi-workflow human operation. Reusing `operation_id` replays
 * its first outcome; changed input under the same ID is refused.
 */
export const submitWorkflowCommand = (command: WorkflowCommand) =>
  featureOperation(MULTI_WORKFLOW_FEATURE, {
    kind: "workflow",
    command,
  }, workflowCommandResultIsCurrent);
export const cancelProviderValidation = (batchId: string) =>
  featureOperation(PROVIDER_VALIDATION_FEATURE, {
    kind: "provider_validation_cancel",
    batch_id: batchId,
  }, providerValidationBatchIsCurrent);
export const operationId = () => crypto.randomUUID();

export type StableOperationIdentity = { body: string; id: string };

export const operationIntent = (request: Record<string, unknown>) =>
  JSON.stringify(
    Object.fromEntries(
      Object.entries(request).filter(([key]) => !key.startsWith("expected_")),
    ),
  );

export const reuseOperationIdentity = (
  previous: StableOperationIdentity | undefined,
  request: Record<string, unknown>,
) => {
  const intent = operationIntent(request);
  if (previous) {
    try {
      const prior = JSON.parse(previous.body) as Record<string, unknown>;
      if (operationIntent(prior) === intent) {
        return { id: previous.id, request: prior };
      }
    } catch {}
  }
  return { id: operationId(), request };
};
