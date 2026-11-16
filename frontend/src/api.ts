import type {
  DesktopActivation,
  DesktopTarget,
  AppState,
  CapabilityRecordFunctionalProofOperation,
  CmuxKeyboardControlAction,
  CmuxKeyboardControlOutcome,
  CmuxViewOutcome,
  Command,
  HistorySelection,
  HistoricalSource,
  HistoricalSourceKey,
  HistorySearchFilters,
  HistorySearchPage,
  ModelCatalog,
  Operation,
  ProtocolDescriptor,
  Provider,
  ProviderValidationPreview,
  ProviderValidationSelection,
  RestartPreview,
  WorkflowCommand,
  RolePreparation,
  StateCursor,
  StateWaitResult,
  TaskContent,
  UsageComparisonFilters,
  UsageComparisonPage,
  WorkflowComparisonQuery,
  WorkflowComparisonPage,
  ComparisonEvidenceQuery,
  ComparisonEvidencePage,
  GuidanceReauthorizationPreview,
} from "./types";
import type {
  ConversationMutation,
  ConversationMutationResult,
  ConversationRead,
} from "./types";

import {
  field,
  decisionIsCurrent,
  cursorIsCanonical,
  hasStrings,
  hasNullableString,
  compatibilityIsCurrent,
  attentionItemIsCurrent,
  taskActionsAreCurrent,
  providerValidationPreviewIsCurrent,
  providerValidationBatchIsCurrent,
  providerValidationStartOutcomeIsFor,
  taskWorkflowIsCurrent,
  workflowExecutionIsCurrent,
  sessionGenerationIsCurrent,
  workflowCommandResultIsCurrent,
  providerValidationStateIsCurrent,
  taskContentRecordIsCurrent,
  currentInputIsCurrent,
  guidanceComparisonIsCurrent,
  conversationProfileIsCurrent,
  conversationMutationIsCurrent,
  conversationReadIsCurrent,
  transcriptPageIsCurrent,
  historyPaneOutcomeIsCurrent,
  historyPaneViewsAreCurrent,
  historyExportTextIsCurrent,
  comparisonRecipeAttributionIsCurrent,
  utf8Bytes,
  scheduleOutcomes,
  nonnegativeInteger,
  scheduleExecutionCommandIsCurrent,
  scheduleCommandResultMatches,
  scheduleOccurrenceIsCurrent,
  scheduleExecutionStateIsCurrent,
  knowledgeSelectionIsCurrent,
  knowledgeCommandIsCurrent,
} from "./apiValidation";
import type { ConversationReadResults } from "./apiValidation";
export {
  conversationMutationIsCurrent,
  utf8Bytes,
  scheduleLimitsAreCurrent,
  schedulePreviewRequestIsCurrent,
  scheduleStopBindingIsCurrent,
  scheduleStopIntentIsCurrent,
  scheduleExecutionCommandIsCurrent,
  knowledgeSelectionIsCurrent,
  knowledgeCommandIsCurrent,
} from "./apiValidation";

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
      const body = value as { error?: string; operation_id?: string; outcome_unknown?: boolean };
      throw new ApiError(
        body.error || response.statusText,
        response.status,
        body.operation_id,
        dispatchedMutation && body.outcome_unknown === true,
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
let desktopActivation: DesktopActivation | "stale" | undefined;

export function pendingDesktopActivation(): DesktopActivation | "stale" | undefined {
  return desktopActivation;
}

export function clearDesktopActivation(
  activation: DesktopActivation | "stale",
): void {
  if (desktopActivation === activation) desktopActivation = undefined;
}

export function desktopTarget(
  activation: DesktopActivation,
  signal?: AbortSignal,
): Promise<DesktopTarget> {
  const query = new URLSearchParams({ ...activation });
  return json<DesktopTarget>(
    `/api/desktop-target?${query}`,
    { signal },
    transportTimeouts.read,
    ["desktop_target_v1"],
  );
}

export async function bootstrap(): Promise<void> {
  const hash = new URLSearchParams(location.hash.slice(1));
  const token = hash.get("bootstrap");
  // Scrub before decoding metadata or sending the exchange, including errors.
  if (location.hash) history.replaceState(null, "", location.pathname + location.search);
  const encoded = hash.get("activation");
  if (hash.get("desktop_stale") === "1") desktopActivation = "stale";
  if (encoded) {
    desktopActivation = "stale";
    try {
      if (encoded.length > 2048) throw new Error("Desktop activation is too large");
      const bytes = Uint8Array.from(atob(encoded.replace(/-/g, "+").replace(/_/g, "/")), (character) => character.charCodeAt(0));
      const candidate: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
      const instance = field(candidate, "instance_key");
      const attention = field(candidate, "attention_id");
      const episode = field(candidate, "episode_key");
      if (typeof instance === "string" && /^[a-f0-9]{64}$/.test(instance) &&
        typeof attention === "string" && attention.length > 0 && attention.length <= 512 &&
        typeof episode === "string" && /^v1:[a-f0-9]{64}$/.test(episode)) {
        desktopActivation = { instance_key: instance, attention_id: attention, episode_key: episode };
      }
    } catch { /* Invalid navigation metadata grants no navigation authority. */ }
  }
  if (!token) return;
  await json("/api/bootstrap", {
    method: "POST",
    body: JSON.stringify({ token }),
  }, transportTimeouts.mutation);
}
const currentSnapshot = (state: AppState): AppState => {
  const recipePins = (value: unknown) => {
    const family = field(value, "family");
    if (family == null || family === "trip") {
      return ["profile_revision_id", "config_revision_id"].every((key) => typeof field(value, key) === "string");
    }
    const preset = field(value, "preset");
    return family === "single_agent" &&
      ["single_agent", "repository_research", "build_and_review"].includes(String(preset)) &&
      field(value, "profile_revision_id") === null && field(value, "config_revision_id") === null &&
      ["definition_id", "execution_config_revision_id", "primary_profile_revision_id"].every((key) => typeof field(value, key) === "string") &&
      (preset === "build_and_review" ? typeof field(value, "reviewer_profile_revision_id") === "string" : field(value, "reviewer_profile_revision_id") === null);
  };
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
    !recipeArray(state.task_recipes, ["id", "project_id", "name", "revision_id", "configuration_hash", "workflow_version", "workflow_hash", "title", "description"]) ||
    !state.task_recipes.every((item) =>
      recipePins(item) && Number.isSafeInteger(item.revision) && Number.isSafeInteger(item.priority) &&
      Array.isArray(item.acceptance_criteria) && item.acceptance_criteria.every((text) => typeof text === "string") &&
      Array.isArray(item.required_check_ids) && item.required_check_ids.every((id) => typeof id === "string")
    ) ||
    !recipeArray(state.recipe_schedules, ["id", "project_id", "name", "recipe_revision_id", "recipe_name", "recipe_config_revision_id", "cadence", "anchor_utc"]) ||
    !state.recipe_schedules.every((item) =>
      scheduleExecutionStateIsCurrent(item) && typeof item.paused === "boolean" && typeof item.recipe_archived === "boolean" &&
      ["daily", "weekly"].includes(item.cadence) &&
      (item.next_fire_utc === null || typeof item.next_fire_utc === "string") &&
      (item.last_fire === null ||
        (scheduleOutcomes.includes(item.last_fire.outcome) &&
          typeof item.last_fire.scheduled_for_utc === "string" &&
          nonnegativeInteger(item.last_fire.missed_count) &&
          ["task_id", "reason", "missed_first_utc", "missed_last_utc"].every((key) => hasNullableString(item.last_fire, key)) &&
          (item.mode === undefined || scheduleOccurrenceIsCurrent(item.last_fire, item))))
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
        (recipePins(provenance) && ["recipe_id", "recipe_name", "recipe_revision_id", "configuration_hash", "workflow_version", "workflow_hash"].every((key) => typeof field(provenance, key) === "string") &&
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
export const command = async (body: Command) => {
  if (body.kind === "authorize_recipe_schedule_execution") throw new Error("Automatic recipe execution has been retired. Create a draft schedule instead.");
  const scheduleCommand = ["authorize_recipe_schedule_execution", "stop_recipe_schedule_occurrence", "request_recipe_schedule_occurrence_stop"].includes(body.kind);
  const scheduleFeatures = ["schedule_execution_authorization_v1", "project_knowledge_v1", ...(body.kind === "request_recipe_schedule_occurrence_stop" ? ["schedule_occurrence_stop_intent_v1"] : [])];
  if (scheduleCommand && !scheduleExecutionCommandIsCurrent(body)) return Promise.reject(new Error("Refresh the schedule and review its exact authorization or stop target before sending."));
  if (scheduleCommand) {
    const descriptor = negotiated ?? await negotiate();
    if (!scheduleFeatures.every((feature) => descriptor.supported_features.includes(feature))) throw new ProtocolError("This service cannot authorize or stop this scheduled run. Reload with a supporting service.", "missing_required_feature");
  }
  // The journal keeps project context; typed intent commands have no top-level project on the wire.
  const response = await preserveMutationIdentity(
    json<{ result: Record<string, unknown> }>(
      "/api/command",
      {
        method: "POST",
        body: JSON.stringify(body.kind === "request_recipe_schedule_occurrence_stop" ? { kind: body.kind, operation_id: body.operation_id, intent: body.intent } : body.kind === "prepare_next_run_knowledge" ? { kind: body.kind, request: body.request } : body),
      },
      transportTimeouts.mutation,
      ["add_task_comment", "edit_guidance", "cancel_guidance"].includes(
          String(body.kind),
        )
        ? ["task_interactions_v1"]
        : scheduleCommand ? scheduleFeatures : ["set_task_knowledge", "prepare_next_run_knowledge"].includes(body.kind) ? ["project_knowledge_v1"] : body.kind === "upsert_single_agent_recipe" ? ["single_agent_presets_v1"] : [],
    ),
    body.operation_id,
  );
  if (scheduleCommand && !scheduleCommandResultMatches(body, field(response, "result"))) throw new ApiError("The schedule response could not be matched to the saved operation. Its outcome is uncertain. Refresh and check the schedule before recovering the exact request.", 0, body.operation_id, true);
  return response;
};
export const operation = async <T = Record<string, unknown>>(
  body: Operation | CapabilityRecordFunctionalProofOperation,
): Promise<T> => {
  const identity = body.kind === "capability_record_functional_proof"
    ? field(field(body, "proof"), "operation_id")
    : field(body, "operation_id");
  const response = await preserveMutationIdentity(
    json<{ result: T }>("/api/operation", {
      method: "POST",
      body: JSON.stringify(body),
    }, transportTimeouts.mutation),
    typeof identity === "string" ? identity : undefined,
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

export function conversationRead<T extends ConversationRead>(operation: T): Promise<ConversationReadResults[T["action"]]> {
  return featureOperation("conversation_v1", { kind: "conversation", operation },
    (value): value is ConversationReadResults[T["action"]] => conversationReadIsCurrent(operation.action, value));
}
export async function prepareConversationMutation(operation: ConversationMutation): Promise<void> {
  if (!conversationMutationIsCurrent(operation)) throw new Error("The conversation request is invalid. Review its fields before sending.");
  if (operation.action === "with_knowledge") {
    await prepareConversationMutation(operation.operation);
    await requireKnowledgeFeature();
    if (utf8Bytes(JSON.stringify({ kind: "conversation", operation })) > 16384) throw new Error("Select fewer entries or shorten the message; the conversation request exceeds 16 KiB.");
    return;
  }
  const encoder = new TextEncoder();
  if (encoder.encode(JSON.stringify(operation)).length > 16384) throw new Error("The serialized conversation request exceeds 16 KiB. Shorten the input before sending.");
  const fields = operation.action === "create_profile" ? operation.input : operation;
  for (const [key, value] of Object.entries(fields)) {
    if (typeof value === "string" && key !== "action" && key !== "instructions" && key !== "input" &&
      (!value.trim() || value.includes("\0") || encoder.encode(value).length > (key === "effort" ? 32 : 128))) {
      throw new Error("The conversation request contains an invalid identifier. Review its fields before sending.");
    }
  }
  const descriptor = negotiated ?? await negotiate();
  if (!descriptor.supported_features.includes("conversation_v1")) throw new ProtocolError(
    `This service version does not support conversations. ${PROTOCOL_GUIDANCE}`, "missing_required_feature");
  const input = operation.action === "create_profile" ? operation.input.instructions : "input" in operation ? operation.input : undefined;
  if (input !== undefined && ((operation.action !== "create_profile" && !input.trim()) || input.includes("\0") || encoder.encode(input).length > 12288)) {
    throw new Error("Enter a message or instructions within the 12 KiB limit before sending.");
  }
}
export function conversationMutate(operation: ConversationMutation): Promise<ConversationMutationResult> {
  if (operation.action === "with_knowledge") return knowledgeConversationMutate(operation);
  return preserveMutationIdentity(featureOperation("conversation_v1", { kind: "conversation", operation },
    (value): value is ConversationMutationResult => typeof value === "object" && value !== null && !Array.isArray(value) &&
      ["state", "conversation_id", "admission_id"].every((key) => field(value, key) === undefined || typeof field(value, key) === "string") &&
      (field(value, "profile") === undefined || conversationProfileIsCurrent(field(value, "profile")))),
    operation.action === "create_profile" ? operation.input.operation_id : operation.operation_id);
}

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
  featureOperation((command.kind === "save_agent_profile" && command.profile.purpose && command.profile.purpose !== "workspace_writer") ||
    (command.kind === "set_project_workflow_default" && command.preset && command.preset !== "single_agent") ||
    ((command.kind === "set_task_workflow" || command.kind === "create_task") && command.selection && ((command.selection.preset && command.selection.preset !== "single_agent") || command.selection.reviewer_profile_revision_id)) ? "single_agent_presets_v1" : MULTI_WORKFLOW_FEATURE, {
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

export const permissionCoverage = (
  request_id: string,
  expected_revision: number,
  command: string,
  lifetime: "session" | "project",
) =>
  json<import("./types").PermissionCoveragePreview>(
    "/api/permission-coverage",
    {
      method: "POST",
      body: JSON.stringify({
        request_id,
        expected_revision,
        command,
        lifetime,
      }),
    },
    transportTimeouts.read,
    ["task_interactions_v1"],
  );

export const taskActivity = (taskId: string, cursor?: string) =>
  json<import("./types").TaskActivityPage>(
    `/api/tasks/${encodeURIComponent(taskId)}/activity${
      cursor ? `?cursor=${encodeURIComponent(cursor)}` : ""
    }`,
    undefined,
    transportTimeouts.read,
    ["task_interactions_v1"],
  );

export function historySearch(filters: HistorySearchFilters, cursor?: string, signal?: AbortSignal): Promise<HistorySearchPage> {
  const params = new URLSearchParams({ query: filters.query, exclude_archived: String(filters.exclude_archived) });
  if (filters.project_id) params.set("project_id", filters.project_id);
  if (filters.kind) params.set("kind", filters.kind);
  if (cursor) params.set("cursor", cursor);
  return json<HistorySearchPage>(`/api/history/search?${params}`, { signal }, transportTimeouts.read, ["history_insights_v1"]);
}

export function historicalSource(key: HistoricalSourceKey, signal?: AbortSignal): Promise<HistoricalSource> {
  const params = new URLSearchParams({ task_id: key.task_id, kind: key.kind, id: key.id });
  if (key.attempt_id !== null) params.set("attempt_id", key.attempt_id);
  return json<HistoricalSource>(`/api/history/source?${params}`, { signal }, transportTimeouts.read, ["history_insights_v1"]);
}

export function usageComparisons(filters: UsageComparisonFilters, cursor?: string, signal?: AbortSignal): Promise<UsageComparisonPage> {
  const params = new URLSearchParams({ group_by: filters.group_by });
  for (const [key, value] of Object.entries(filters)) if (value) params.set(key, value);
  if (cursor) params.set("cursor", cursor);
  return json<UsageComparisonPage>(`/api/history/usage?${params}`, { signal }, transportTimeouts.read, ["history_insights_v1"]);
}

async function requireWorkflowComparisons(signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) throw new DOMException("Request cancelled", "AbortError");
  const descriptor = await negotiate();
  if (signal?.aborted) throw new DOMException("Request cancelled", "AbortError");
  if (!descriptor.supported_features.includes("workflow_comparisons_v1")) {
    throw new ProtocolError(`This service version does not support workflow comparisons. ${PROTOCOL_GUIDANCE}`, "missing_required_feature");
  }
}

export async function workflowComparisons(selection: WorkflowComparisonQuery, signal?: AbortSignal): Promise<WorkflowComparisonPage> {
  await requireWorkflowComparisons(signal);
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(selection)) {
    if (value !== undefined && value !== null && value !== "") params.set(key, value);
  }
  return json<WorkflowComparisonPage>(`/api/history/comparisons?${params}`, { signal }, transportTimeouts.read, ["workflow_comparisons_v1"]);
}

export async function comparisonEvidence(query: ComparisonEvidenceQuery, signal?: AbortSignal): Promise<ComparisonEvidencePage> {
  await requireWorkflowComparisons(signal);
  const params = new URLSearchParams({ selection: query.selection, snapshot: query.snapshot, group: query.group, metric: query.metric });
  if (query.cursor) params.set("cursor", query.cursor);
  const page = await json<ComparisonEvidencePage>(`/api/history/comparisons/evidence?${params}`, { signal }, transportTimeouts.read, ["workflow_comparisons_v1"]);
  if (!Array.isArray(page.rows) || !page.rows.every((row) => comparisonRecipeAttributionIsCurrent(field(row, "recipe_attribution")))) {
    throw new ProtocolError("Saved schedule attribution could not be read. Refresh and apply the comparison again.", "invalid_comparison_attribution");
  }
  return page;
}

export async function recipeScheduleExecutionPreview(_request: import("./types").RecipeSchedulePreviewRequest): Promise<import("./types").RecipeScheduleExecutionPreview> {
  throw new Error("Automatic recipe execution has been retired. Create a draft schedule instead.");
}
async function requireKnowledgeFeature(): Promise<void> {
  const descriptor = negotiated ?? await negotiate();
  if (!descriptor.supported_features.includes("project_knowledge_v1")) throw new ProtocolError(`Reload the dashboard with a service supporting project knowledge. ${PROTOCOL_GUIDANCE}`, "missing_required_feature");
}
async function knowledgeRequest<T>(path: string, body: unknown, mutation = false, id?: string, features: readonly string[] = []): Promise<T> {
  await requireKnowledgeFeature();
  const serialized = JSON.stringify(body);
  if (utf8Bytes(serialized) > 32768) throw new Error("Shorten the input or select fewer entries; knowledge requests allow at most 32 KiB.");
  return preserveMutationIdentity(json<T>(path, { method: "POST", body: serialized }, mutation ? transportTimeouts.mutation : transportTimeouts.read, ["project_knowledge_v1", ...features]), id);
}
export function knowledgeRead<T extends import("./types").KnowledgeRead>(projectId: string, read: T): Promise<import("./types").KnowledgeReadResults[T["kind"]]> {
  return knowledgeRequest("/api/knowledge/read", { project_id: projectId, read });
}
export async function knowledgePreview(request: import("./types").KnowledgePreviewRequest): Promise<import("./types").KnowledgePreview> {
  if (request.selection.length > 16 || !request.selection.every(knowledgeSelectionIsCurrent)) return Promise.reject(new Error("Select at most 16 exact revisions with bounded identifiers and current observation fingerprints."));
  if (request.consumer.kind === "schedule") {
    const descriptor = negotiated ?? await negotiate();
    if (!descriptor.supported_features.includes("schedule_execution_authorization_v1")) throw new ProtocolError("This service cannot preview schedule Knowledge. Reload with a supporting service.", "missing_required_feature");
  }
  return knowledgeRequest("/api/knowledge/preview", request, false, undefined, request.consumer.kind === "schedule" ? ["schedule_execution_authorization_v1"] : []);
}
export function knowledgeCommand(request: import("./types").KnowledgeCommand): Promise<import("./types").KnowledgeEntry | import("./types").KnowledgeObservation> {
  if (!knowledgeCommandIsCurrent(request)) return Promise.reject(new Error("Review the exact knowledge request before sending."));
  return knowledgeRequest("/api/knowledge/command", request, true, request.kind === "mutate" ? request.request.operation_id : request.operation_id);
}
async function knowledgeConversationMutate(operation: Extract<ConversationMutation, { action: "with_knowledge" }>): Promise<ConversationMutationResult> {
  await requireKnowledgeFeature();
  const response = await preserveMutationIdentity(json<{ result: ConversationMutationResult }>("/api/operation", { method: "POST", body: JSON.stringify({ kind: "conversation", operation }) }, transportTimeouts.mutation, ["conversation_v1", "project_knowledge_v1"]), operation.operation_id);
  return response.result;
}
