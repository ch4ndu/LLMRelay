import type {
  AppState,
  CmuxKeyboardControlAction,
  CmuxKeyboardControlOutcome,
  CmuxViewOutcome,
  Command,
  CompatibilityExplanation,
  ModelCatalog,
  Operation,
  ProtocolDescriptor,
  Provider,
  RestartPreview,
  RolePreparation,
  StateCursor,
  StateWaitResult,
} from "./types";

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
          required_features: [PROTOCOL_FEATURE],
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
const attentionItemIsCurrent = (value: unknown) => {
  const target = field(value, "target");
  const held = field(value, "held_tasks");
  return hasStrings(value, ["id", "title", "reason"]) &&
    attentionCategories.includes(String(field(value, "category"))) &&
    (target === null || attentionTargetIsCurrent(target)) &&
    Array.isArray(held) && held.every(taskTargetIsCurrent);
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
    !state.attention.every(attentionItemIsCurrent)
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
