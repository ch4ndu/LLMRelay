import { ErrorNotice } from "./ErrorNotice";
import { useRef, useState } from "react";
import {
  ApiError,
  command,
  operation,
  operationId as newOperationId,
  operationIntent,
  reuseOperationIdentity,
  submitWorkflowCommand,
} from "../api";
import { TechnicalDetails } from "./ErrorNotice";
import {
  type ContinuationAction,
  type ContinuationActionKind,
  type DecisionExplanation,
  type DecisionOwner,
  type Project,
  roleLabel,
  type Session,
  type SwitchIntent,
  type Task,
  type WorkflowCommand,
  type WorkflowCommandResult,
  type WorkflowControl,
} from "../types";

type StoredIdentity = { body: string; id: string };

const storedIdentity = (value: unknown): StoredIdentity | undefined => {
  if (typeof value !== "object" || value === null) return undefined;
  const body: unknown = Reflect.get(value, "body");
  const id: unknown = Reflect.get(value, "id");
  return typeof body === "string" && typeof id === "string"
    ? { body, id }
    : undefined;
};

const readIdentities = (storageKey: string): Map<string, StoredIdentity> => {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(storageKey) || "[]");
    if (!Array.isArray(value)) return new Map();
    return new Map(value.flatMap((entry: unknown) => {
      if (!Array.isArray(entry)) return [];
      const name: unknown = entry[0];
      const identity = storedIdentity(entry[1]);
      if (typeof name !== "string" || !identity) return [];
      const pair: [string, StoredIdentity] = [name, identity];
      return [pair];
    }));
  } catch {
    return new Map();
  }
};

export interface WorkflowCommandRunner {
  /**
   * Submits `build(id)` once at a time. An identical earlier request whose
   * outcome is unknown keeps its operation ID, so the service applies it at
   * most once. A changed request (for example after the task version moved)
   * is held once for review instead; only the next explicit click sends it as
   * a new request, which the service then checks against current state.
   * With `keepUnresolved`, a changed request is never sent while an earlier
   * one is unresolved: commands without a version fence, such as creating a
   * profile, would otherwise apply both. Any failed resend of that earlier
   * request leaves it unresolved.
   */
  submit: (
    key: string,
    build: (operationId: string) => WorkflowCommand,
    options?: { keepUnresolved?: boolean },
  ) => Promise<WorkflowCommandResult | undefined>;
  /** The stored request whose outcome is still unknown, if any. */
  unresolved: (key: string) => { body: string } | undefined;
  busy: boolean;
  error: string;
  /** Keys whose last request may or may not have been applied. */
  uncertain: ReadonlySet<string>;
  /** Uncertain keys whose next click sends a changed, new request. */
  reviewed: ReadonlySet<string>;
}

/**
 * Typed multi-workflow commands with persisted, replay-safe identities.
 * Storage is re-read on every change because several panels of one task can
 * share its key.
 */
export function useWorkflowCommands(
  storageKey: string,
  onChanged: () => void,
): WorkflowCommandRunner {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [uncertain, setUncertain] = useState<ReadonlySet<string>>(
    () => new Set(readIdentities(storageKey).keys()),
  );
  const [reviewed, setReviewed] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const inFlight = useRef(false);
  const update = (
    change: (identities: Map<string, StoredIdentity>) => void,
  ) => {
    const identities = readIdentities(storageKey);
    change(identities);
    localStorage.setItem(storageKey, JSON.stringify([...identities]));
  };
  const mark = (key: string, unknown: boolean) =>
    setUncertain((current) => {
      const next = new Set(current);
      if (unknown) next.add(key);
      else next.delete(key);
      return next;
    });
  const submit = async (
    key: string,
    build: (operationId: string) => WorkflowCommand,
    options: { keepUnresolved?: boolean } = {},
  ) => {
    if (inFlight.current) return undefined;
    const body = JSON.stringify(build(""));
    const prior = readIdentities(storageKey).get(key);
    if (prior && prior.body !== body && options.keepUnresolved) {
      mark(key, true);
      return undefined;
    }
    if (prior && prior.body !== body && !reviewed.has(key)) {
      setReviewed((current) => new Set(current).add(key));
      mark(key, true);
      return undefined;
    }
    setReviewed((current) => {
      const next = new Set(current);
      next.delete(key);
      return next;
    });
    const id = prior?.body === body ? prior.id : newOperationId();
    // A refused replay (for example an expired sign-in rejected before the
    // service looks the operation up) says nothing about the stored original,
    // so only its own success settles it.
    const replayingUnresolved = options.keepUnresolved === true &&
      prior?.body === body;
    update((identities) => identities.set(key, { body, id }));
    const forget = () =>
      update((identities) => {
        if (identities.get(key)?.id === id) identities.delete(key);
      });
    inFlight.current = true;
    setBusy(true);
    setError("");
    try {
      const result = await submitWorkflowCommand(build(id));
      forget();
      mark(key, false);
      onChanged();
      return result;
    } catch (cause) {
      const unknown = replayingUnresolved ||
        (cause instanceof ApiError && cause.ambiguous);
      if (!unknown) forget();
      mark(key, unknown);
      setError(cause instanceof Error ? cause.message : String(cause));
      onChanged();
      return undefined;
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  const unresolved = (key: string) => readIdentities(storageKey).get(key);
  return { submit, unresolved, busy, error, uncertain, reviewed };
}

export function UncertainRequestNotice(
  { changed = false }: { changed?: boolean },
) {
  return (
    <p className="warning" role="status">
      {changed
        ? "An earlier request for this action may already have been applied, and the page has changed since. Review the current state shown here. Choosing the action again sends a new request for what is shown now; LLMRelay refuses it if it no longer applies."
        : "An earlier request for this action may already have been applied. Refresh and check the current state first. Trying again without other changes sends the same request, which LLMRelay applies at most once."}
    </p>
  );
}

/**
 * Whether a TRIP role's effective session and generation agree on a settled
 * state. An exited session whose generation is still stopping is not settled:
 * replacing or probing the role then would race the old generation's stop.
 */
export function generationSettled(session: Session | undefined): boolean {
  // An older snapshot without these fields is never treated as settled.
  if (
    !session?.generation_status || session.has_newer_generation === undefined
  ) {
    return false;
  }
  if (
    session.has_newer_generation === true || session.has_newer_generation === 1
  ) {
    return false;
  }
  const quiescent = session.process_group_quiescent === true ||
    session.process_group_quiescent === 1;
  return (session.status === "running" &&
    session.generation_status === "running") ||
    (session.status === "exited" && session.generation_status === "exited" &&
      quiescent);
}

/** The backend's family resolver treats a task without a selection as TRIP. */
export const isSingleAgentTask = (task: Task) =>
  task.workflow?.family === "single_agent";

/** Who is expected to act, in plain words. */
export const ownerLabel = (owner: DecisionOwner | string) =>
  owner === "human"
    ? "You"
    : owner === "service"
    ? "LLMRelay"
    : owner === "provider"
    ? "The agent"
    : "Outside LLMRelay";

/** Blocker codes meaning the project's workflow setup must be finished or
 * reviewed first. Their backend message is a diagnostic, so it belongs under
 * Technical details rather than as the explanation. */
const PROJECT_SETUP_BLOCKERS = new Set([
  "task.project_ready",
  "scheduler.project_readiness_stale",
  "workflow.project_readiness_stale",
]);

/** A plain explanation when `code` is a project-setup blocker, otherwise
 * undefined so the caller keeps its own message. */
export const projectSetupProblem = (
  project: Project | undefined,
  code: string | undefined,
) => {
  if (!code || !PROJECT_SETUP_BLOCKERS.has(code)) return undefined;
  switch (project?.trip?.readiness ?? "not_initialized") {
    case "not_initialized":
      return "This project has not been set up yet, so this task cannot start. Open project setup to set it up; the task is kept.";
    case "setup_in_progress":
      return "Project setup has not been finished yet, so this task cannot start. Open project setup to finish it; the task is kept.";
    case "needs_upgrade_review":
      return "The project's workflow setup changed and needs your review before this task can start. Open project setup to review it; the task is kept.";
    default:
      return "The project's workflow setup needs attention before this task can start. Open project setup to see what to fix; the task is kept.";
  }
};

const actionTitles: Record<ContinuationActionKind, string> = {
  exact_resume: "Session stopped before finishing",
  fresh_accounted_retry: "Needs a fresh session",
  replace_stale_authority: "Needs updated agent settings",
  wait_for_exit: "Waiting for the agent to stop",
  wait_for_capacity: "Waiting for a free agent slot",
  wait_for_service: "Waiting for LLMRelay to be ready",
  recover_ownership: "Confirm the agent has stopped",
  retry_graceful_stop: "Agent did not stop in time",
  force_stop_exact_process: "Force stop is available",
  prepare_corrected_runtime: "Profile needs a new verification",
  authorize_implementation: "Approved plan is waiting to start",
  migrate_attempt: "Move to the current workflow",
  authorize_additional_explorer: "Extra Explorer call needs your approval",
  recover_setup_apply: "Setup installation needs recovery",
  recover_workspace_reservation: "Workspace could not be prepared",
  continue_fresh_dispatch: "Can continue with a new session",
  start_managed_legacy_attempt: "Imported task needs a fresh start",
  refresh_and_reconcile: "A request was rejected",
  authorization_required: "Needs a new approval",
  recover_failed_step: "An automatic step failed",
  terminal_incomplete: "Cannot continue automatically",
};

export function WorkflowControls(
  {
    task,
    project,
    controls = [],
    sessions = [],
    switches = [],
    actions = [],
    decision,
    onChanged,
    onOpenSetup = () => {},
    onOpenRecoveryRecord,
  }: {
    task: Task;
    project?: Project;
    controls?: WorkflowControl[];
    sessions?: Session[];
    switches?: SwitchIntent[];
    actions?: ContinuationAction[];
    decision?: DecisionExplanation;
    onChanged: () => void;
    onOpenSetup?: (projectId: string) => void;
    /** Opens the exact recovery record, or returns why it cannot. */
    onOpenRecoveryRecord?: (
      attemptId: string,
      recoveryId: string,
    ) => string | undefined;
  },
) {
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [routeNotice, setRouteNotice] = useState("");
  const setupProblem = isSingleAgentTask(task)
    ? undefined
    : projectSetupProblem(
      project,
      decision?.primary_blocker?.code,
    );
  const operationStorageKey = `llmrelay.workflow.operations.${task.id}`;
  const commandIdentities = useRef(
    new Map<string, { body: string; id: string }>(
      (() => {
        try {
          const value = JSON.parse(
            localStorage.getItem(operationStorageKey) || "[]",
          );
          return Array.isArray(value) ? value : [];
        } catch {
          return [];
        }
      })(),
    ),
  );
  const persistOperationIdentities = () =>
    localStorage.setItem(
      operationStorageKey,
      JSON.stringify([...commandIdentities.current]),
    );
  const send = async (kind: string, body: Record<string, unknown> = {}) => {
    const intent = {
      kind,
      task_id: task.id,
      ...body,
    };
    const key = `command:${operationIntent(intent)}`;
    const stable = reuseOperationIdentity(commandIdentities.current.get(key), {
      ...intent,
      expected_version: task.version,
    });
    const { id, request } = stable;
    commandIdentities.current.set(key, { body: JSON.stringify(request), id });
    persistOperationIdentities();
    setBusy(key);
    setError("");
    try {
      await command({
        ...request,
        operation_id: id,
      } as never);
      commandIdentities.current.delete(key);
      persistOperationIdentities();
      onChanged();
    } catch (cause) {
      if (!(cause instanceof ApiError && cause.ambiguous)) {
        commandIdentities.current.delete(key);
        persistOperationIdentities();
      }
      setError(
        cause instanceof ApiError && cause.ambiguous
          ? `${cause.message} The operation ID is retained; refresh and reconcile before retrying.`
          : cause instanceof Error
          ? cause.message
          : String(cause),
      );
      onChanged();
    } finally {
      setBusy("");
    }
  };
  const control = (action: string, payload: Record<string, unknown> = {}) =>
    send("control", { action, payload });
  const sendBrowserOperation = async (
    kind: string,
    body: Record<string, unknown>,
  ) => {
    const request = { kind, ...body };
    const key = `operation:${operationIntent(request)}`;
    const stable = reuseOperationIdentity(
      commandIdentities.current.get(key),
      request,
    );
    const { id, request: stableRequest } = stable;
    commandIdentities.current.set(key, {
      body: JSON.stringify(stableRequest),
      id,
    });
    persistOperationIdentities();
    setBusy(key);
    setError("");
    try {
      await operation({ ...stableRequest, operation_id: id } as never);
      commandIdentities.current.delete(key);
      persistOperationIdentities();
      onChanged();
    } catch (cause) {
      if (!(cause instanceof ApiError && cause.ambiguous)) {
        commandIdentities.current.delete(key);
        persistOperationIdentities();
      }
      setError(
        cause instanceof ApiError && cause.ambiguous
          ? `${cause.message} The operation ID is retained; refresh and reconcile before retrying.`
          : cause instanceof Error
          ? cause.message
          : String(cause),
      );
      onChanged();
    } finally {
      setBusy("");
    }
  };
  const terminal = task.archived ||
    ["done", "cancelled"].includes(task.lifecycle);
  const attemptId = task.active_attempt?.id;
  const managerControl = controls.find((value) =>
    value.attempt_id === attemptId &&
    ["manager_stop", "manager_change"].includes(value.kind) &&
    !["finished", "cancelled", "superseded", "rejected"].includes(value.state)
  );
  const managerRevisions = task.role_settings.filter((value) =>
    value.role === "manager"
  ).sort((left, right) => right.revision - left.revision);
  const requestedManager = managerRevisions[0];
  const effectiveManager = managerRevisions.find((value) =>
    value.effective_generation_id
  );
  const managerSession = sessions.find((value) =>
    value.role_generation_id === effectiveManager?.effective_generation_id
  );
  const managerSwitch = switches.find((value) =>
    value.attempt_id === attemptId && value.role === "manager" &&
    value.old_generation_id === effectiveManager?.effective_generation_id
  );
  const singleAgent = isSingleAgentTask(task);
  const failedProbe = !!task.workflow?.run?.probe_admission_id &&
    task.workflow?.run?.stage_reason.startsWith(
        "The probe did not prove the profile:",
      ) === true;
  const pendingManagerChange = !!requestedManager && !!effectiveManager &&
    requestedManager.revision !== effectiveManager.revision;
  // The service also accepts a stopping manager whose earlier switch needs
  // recovery; any other unsettled stop would race the old manager.
  const managerReplaceable = generationSettled(managerSession) ||
    (managerSession?.generation_status === "stopping" &&
      managerSwitch?.state === "recovery_required");
  const managerControlCanBeReplaced = !managerControl ||
    managerControl.state === "failed";
  const managerStopQuiescent = managerControl?.kind === "manager_stop" &&
    managerControl.payload.quiescent === true;
  const managerChangeCanStart = managerControlCanBeReplaced ||
    managerStopQuiescent;
  const genericControlsBlocked = !!managerControl;
  const managerControlsAllowed = task.lifecycle !== "awaiting_review";
  const attemptAction = (action: ContinuationAction) =>
    action.binding.task_id === task.id ||
    (typeof action.binding.attempt_id === "string" &&
      action.binding.attempt_id === attemptId);
  const projected = actions.filter(attemptAction);
  // recover_failed_step is absent from both lists so a held step never disables
  // Pause or Cancel here; the service itself refuses Continue for that hold.
  const exactGate = projected.some((action) =>
    [
      "exact_resume",
      "authorize_implementation",
      "migrate_attempt",
      "authorize_additional_explorer",
      "recover_workspace_reservation",
      "retry_graceful_stop",
      "force_stop_exact_process",
      "fresh_accounted_retry",
      "replace_stale_authority",
      "prepare_corrected_runtime",
      "continue_fresh_dispatch",
      "start_managed_legacy_attempt",
      "authorization_required",
    ].includes(action.kind)
  );
  const projectedControlsBlocked = genericControlsBlocked || exactGate ||
    projected.some((action) =>
      [
        "wait_for_exit",
        "wait_for_capacity",
        "wait_for_service",
        "recover_ownership",
      ].includes(action.kind)
    );
  const allows = (controlName: string) =>
    decision
      ? decision.control_policy.allowed_controls.includes(controlName)
      : !projectedControlsBlocked;
  const taskControls: Array<[string, string]> = [
    ["continue", "Continue"],
    ["run_next", task.lifecycle === "ready" ? "Prepare task" : "Run next"],
    ["pause_after_role", "Pause after this step"],
    ["pause_now", "Pause now"],
    ["retry", "Retry"],
    ["cancel", "Cancel"],
  ];
  const unavailableControls = taskControls.filter(([name]) => !allows(name))
    .map(([, label]) => label);
  const actionValue = (action: ContinuationAction, key: string) => {
    const value = action.binding[key];
    return typeof value === "string" ? value : undefined;
  };
  const actionIdentity = (action: ContinuationAction) => {
    const value = action.binding.process_identity;
    return value && typeof value === "object"
      ? value as Record<string, unknown>
      : undefined;
  };
  const actionRole = (action: ContinuationAction) => {
    const role = actionValue(action, "role") ||
      sessions.find((session) =>
        session.id === actionValue(action, "session_id")
      )
        ?.role;
    return role ? roleLabel(role as Session["role"]) : undefined;
  };
  const actionButton = (action: ContinuationAction) => {
    const attemptId = actionValue(action, "attempt_id");
    const recoveryId = actionValue(action, "recovery_id");
    const workspaceId = actionValue(action, "workspace_id");
    const sessionId = actionValue(action, "session_id");
    const runtimeAdmissionId = actionValue(action, "runtime_admission_id");
    const role = actionValue(action, "role");
    const generationId = actionValue(action, "role_generation_id");
    const transcriptEpoch = actionValue(action, "transcript_epoch");
    const processIdentity = actionIdentity(action);
    switch (action.kind) {
      case "authorization_required": {
        const reviewKind = actionValue(action, "review_kind");
        if (action.operation !== "extend_review_budget") return null;
        return (
          <button
            disabled={!action.enabled || !attemptId ||
              (reviewKind !== "plan" && reviewKind !== "code") || !!busy}
            onClick={() =>
              void send("extend_review_budget", {
                attempt_id: attemptId,
                review_kind: reviewKind,
                additional: 1,
              })}
          >
            Allow one more {reviewKind} review
          </button>
        );
      }
      case "exact_resume":
        const restartResume = action.operation === "restart_resume";
        return (
          <button
            className="primary compact"
            disabled={!action.enabled || !sessionId ||
              (!restartResume && action.operation === "runtime_probe_resume" &&
                (!runtimeAdmissionId || !role)) ||
              !!busy}
            onClick={() =>
              void sendBrowserOperation(
                restartResume
                  ? "restart_resume"
                  : action.operation === "runtime_probe_resume"
                  ? "runtime_probe_resume"
                  : "role_resume",
                restartResume
                  ? { session_ids: [sessionId] }
                  : action.operation === "runtime_probe_resume"
                  ? { admission_id: runtimeAdmissionId, role }
                  : { session_id: sessionId, prompt: "" },
              )}
          >
            {restartResume
              ? "Resume after restart"
              : action.operation === "runtime_probe_resume"
              ? "Resume the verification"
              : "Resume the stopped session"}
          </button>
        );
      case "recover_workspace_reservation":
        return (
          <div className="button-row">
            <button
              disabled={!action.enabled || !attemptId || !workspaceId || !!busy}
              onClick={() =>
                void send("retry_workspace_reservation", {
                  attempt_id: attemptId,
                  workspace_id: workspaceId,
                })}
            >
              Inspect and retry workspace reservation
            </button>
            <button
              className="danger"
              disabled={!action.enabled || !attemptId || !workspaceId || !!busy}
              onClick={() =>
                void send("cancel_workspace_reservation", {
                  attempt_id: attemptId,
                  workspace_id: workspaceId,
                })}
            >
              Verify and cancel reservation
            </button>
          </div>
        );
      case "retry_graceful_stop":
      case "force_stop_exact_process":
        return (
          <button
            className={action.kind === "force_stop_exact_process"
              ? "danger"
              : undefined}
            disabled={!action.enabled || !sessionId || !generationId ||
              !transcriptEpoch || !processIdentity || !!busy}
            onClick={() =>
              void send(action.kind, {
                session_id: sessionId,
                role_generation_id: generationId,
                transcript_epoch: transcriptEpoch,
                process_identity: processIdentity,
              })}
          >
            {action.kind === "retry_graceful_stop"
              ? "Ask it to stop again"
              : "Force stop this process"}
          </button>
        );
      case "continue_fresh_dispatch":
        return (
          <button
            disabled={!action.enabled || !!busy}
            onClick={() => void control("continue")}
          >
            Continue with a new session
          </button>
        );
      case "fresh_accounted_retry":
        return (
          <button
            className="primary compact"
            disabled={!action.enabled || !!busy ||
              (action.operation === "trip_setup_dispatch" &&
                (!attemptId || !role))}
            onClick={() =>
              void (
                action.operation === "trip_setup_dispatch"
                  ? sendBrowserOperation("trip_setup_dispatch", {
                    attempt_id: attemptId,
                    role,
                    fresh_resume_rejection: action.binding,
                  })
                  : control("continue", { resume_rejection: action.binding })
              )}
          >
            {action.operation === "trip_setup_dispatch"
              ? "Start a fresh setup session"
              : "Start a fresh session"}
          </button>
        );
      case "replace_stale_authority":
        return (
          <button onClick={() => onOpenSetup(task.project_id)}>
            Review agent settings
          </button>
        );
      case "prepare_corrected_runtime":
        return (
          <button
            disabled={!action.enabled}
            onClick={() => onOpenSetup(task.project_id)}
          >
            Prepare a new verification
          </button>
        );
      case "start_managed_legacy_attempt":
        return (
          <button
            className="primary compact"
            disabled={!action.enabled || !!busy}
            onClick={() => void send("normalize_legacy_task")}
          >
            Start a fresh attempt
          </button>
        );
      case "refresh_and_reconcile":
        return (
          <button
            disabled={!action.enabled || !!busy}
            onClick={onChanged}
          >
            Refresh to see the current state
          </button>
        );
      // The failed step's form lives only in its exact recovery panel.
      case "recover_failed_step":
        return (
          <button
            disabled={!action.enabled || !attemptId || !recoveryId ||
              !onOpenRecoveryRecord}
            onClick={() => {
              if (attemptId && recoveryId && onOpenRecoveryRecord) {
                setRouteNotice(
                  onOpenRecoveryRecord(attemptId, recoveryId) ?? "",
                );
              }
            }}
          >
            Review the failed step
          </button>
        );
      default:
        return null;
    }
  };

  return (
    <section
      className="panel controls"
      data-attention-target={attemptId ? `attempt:${attemptId}` : undefined}
      tabIndex={-1}
    >
      <header>
        <h3>Controls</h3>
      </header>
      {task.lifecycle === "ready" && (
        <p className="hint">
          Prepare task creates its workspace and pauses before starting an
          agent. Then choose Continue to start this task. Other queued tasks
          stay paused if project pickup is paused.
        </p>
      )}
      {decision && decision.primary_blocker?.message && (
        <p className="hint decision-summary">
          {failedProbe
            ? "This profile probe did not establish admission evidence. Review the recovery steps in its run panel, then choose Cancel to keep its history."
            : setupProblem || decision.primary_blocker.message}{" "}
          <span className="owner">
            Waiting on: {ownerLabel(decision.primary_blocker.owner)}.
          </span>
        </p>
      )}
      {decision && (
        <TechnicalDetails>
          <p>
            {decision.reason_code} · {decision.disposition} · owner{" "}
            {decision.ownership.owner} · {decision.next_action
              ? `next ${decision.next_action.operation}`
              : "no current action"}
          </p>
          {failedProbe && <p>{decision.primary_blocker?.message}</p>}
          {setupProblem && (
            <p>
              {decision.primary_blocker?.message} · project readiness{" "}
              {project?.trip?.readiness ?? "not reported"}
            </p>
          )}
        </TechnicalDetails>
      )}
      {terminal
        ? (
          <p className="hint">
            This task is finished; its history is read-only.
          </p>
        )
        : task.lifecycle === "backlog"
        ? (
          <div className="button-row">
            <button
              className="primary"
              disabled={!!busy || !allows("make_ready")}
              title={decision?.control_policy.disabled_reason_code
                ? setupProblem || decision.primary_blocker?.message ||
                  decision.control_policy.disabled_reason_code
                : !singleAgent && project?.trip?.readiness !== "ready"
                ? projectSetupProblem(project, "task.project_ready")
                : undefined}
              onClick={() => send("make_ready")}
            >
              Make Ready
            </button>
            {project &&
              ((!singleAgent && project.trip?.readiness !== "ready") ||
                decision?.next_action?.operation === "inspect_project") &&
              (
                <button
                  onClick={() =>
                    onOpenSetup(
                      decision?.next_action?.binding.project_id || project.id,
                    )}
                >
                  Open project setup
                </button>
              )}
          </div>
        )
        : (
          <div className="button-row">
            <button
              disabled={!!busy || !allows("continue")}
              onClick={() => control("continue")}
            >
              Continue
            </button>
            <button
              disabled={!!busy || !allows("run_next")}
              onClick={() => control("run_next")}
            >
              {task.lifecycle === "ready" ? "Prepare task" : "Run next"}
            </button>
            <button
              disabled={!!busy || !allows("pause_after_role")}
              onClick={() => control("pause_after_role")}
            >
              Pause after this step
            </button>
            <button
              disabled={!!busy || !allows("pause_now")}
              onClick={() => control("pause_now")}
            >
              Pause now
            </button>
            <button
              disabled={!!busy || !allows("retry")}
              onClick={() => control("retry")}
            >
              Retry
            </button>
            <button
              className="danger"
              disabled={!!busy || !allows("cancel")}
              onClick={() => control("cancel")}
            >
              Cancel
            </button>
          </div>
        )}
      {decision && !terminal && task.lifecycle !== "backlog" &&
        unavailableControls.length > 0 && (
        <p className="hint">
          {unavailableControls.length === taskControls.length
            ? "None of these controls is available right now"
            : `Not available right now: ${unavailableControls.join(", ")}`}
          {decision.primary_blocker?.message
            ? ", because of what the task is waiting on above."
            : ". LLMRelay does not offer them in the task's current state; refresh if this looks out of date."}
        </p>
      )}
      {!terminal && task.lifecycle !== "backlog" && !singleAgent && (
        <div className="inline-form">
          <strong>Manager</strong>
          <small>
            {effectiveManager
              ? `Active: ${effectiveManager.config.provider} · ${effectiveManager.config.model} · ${effectiveManager.config.effort}`
              : "No manager is active yet"}
            {pendingManagerChange && requestedManager
              ? ` · Requested change (not active yet): ${requestedManager.config.provider} · ${requestedManager.config.model} · ${requestedManager.config.effort}`
              : ""}
          </small>
          <TechnicalDetails>
            <p>
              requested revision {requestedManager?.revision ?? "none"}{" "}
              · effective revision {effectiveManager?.revision ?? "none"}
              {managerSession ? ` · session ${managerSession.status}` : ""}
            </p>
          </TechnicalDetails>
          {managerControl && (
            <small
              className={managerControl.state === "failed"
                ? "error"
                : "badge waiting"}
            >
              {managerControl.kind === "manager_stop"
                ? "Manager stop"
                : "Manager change"} ·{" "}
              {managerControl.state.replaceAll("_", " ")}
              {typeof managerControl.payload.next_action === "string"
                ? ` · ${managerControl.payload.next_action}`
                : ""}
            </small>
          )}
          {managerSwitch && (
            <small className="badge waiting">
              Manager replacement: {managerSwitch.state.replaceAll("_", " ")}.
            </small>
          )}
          {task.lifecycle === "awaiting_review" && (
            <small className="badge waiting">
              Your review decides this result. A requested manager change
              applies only to future work; it cannot restart or replace the
              manager, or rerun final review from here.
            </small>
          )}
          <div className="button-row">
            {managerControlsAllowed && managerControlCanBeReplaced &&
              managerSession && (
              <button
                disabled={!!busy}
                onClick={() => control("stop_manager")}
              >
                Stop manager
              </button>
            )}
            {managerControlsAllowed &&
              managerControl?.kind === "manager_stop" &&
              managerControl.state === "held" && (
              <button
                disabled={!!busy}
                onClick={() => control("continue_manager")}
              >
                Continue manager
              </button>
            )}
            {managerControlsAllowed && managerChangeCanStart &&
              pendingManagerChange && requestedManager && !managerReplaceable &&
              (
                <small className="hint">
                  The manager is still stopping. Changing it becomes available
                  once LLMRelay confirms the current manager has fully stopped.
                </small>
              )}
            {managerControlsAllowed && managerChangeCanStart &&
              pendingManagerChange && requestedManager && managerReplaceable &&
              (
                <>
                  <button
                    disabled={!!busy}
                    onClick={() =>
                      control("change_manager_safe_boundary", {
                        settings_revision: requestedManager.revision,
                      })}
                  >
                    Change manager at safe boundary
                  </button>
                  <button
                    className="danger"
                    disabled={!!busy}
                    onClick={() =>
                      control("change_manager_interrupt", {
                        settings_revision: requestedManager.revision,
                      })}
                  >
                    Interrupt and change manager
                  </button>
                </>
              )}
          </div>
        </div>
      )}
      {projected.length > 0 && (
        <section
          className="continuation-actions"
          aria-label="Current recovery actions"
        >
          <h4>What can happen next</h4>
          {projected.map((action, index) => (
            <article
              className="review-row"
              key={`${action.kind}:${action.operation}:${index}`}
              data-attention-target={typeof action.binding.session_id ===
                  "string"
                ? `session:${action.binding.session_id}`
                : undefined}
              tabIndex={-1}
            >
              <div className="continuation-copy">
                <strong>
                  {action.kind === "exact_resume" &&
                      action.operation === "runtime_probe_resume"
                    ? "Verification is ready to finish"
                    : actionTitles[action.kind] || "Next step"}
                  {actionRole(action) ? ` · ${actionRole(action)}` : ""}
                </strong>
                <span>{action.reason}</span>
                {action.waiting_for && (
                  <small>Waiting for {action.waiting_for}.</small>
                )}
                {action.deadline_at && (
                  <small>
                    Stops being retried automatically at{" "}
                    {new Date(action.deadline_at).toLocaleString()}.
                  </small>
                )}
                {action.accounting_note && (
                  <small>{action.accounting_note}</small>
                )}
                {!action.enabled && action.owner !== "service" && (
                  <small>
                    Not available right now. Waiting on:{" "}
                    {ownerLabel(action.owner)}.
                  </small>
                )}
              </div>
              <div className="continuation-action">{actionButton(action)}</div>
            </article>
          ))}
        </section>
      )}
      {routeNotice && <p className="hint" role="status">{routeNotice}</p>}
      {error && <ErrorNotice error={error} />}
      <small className="hint">
        {terminal
          ? "Completed and cancelled tasks cannot be resumed or changed."
          : decision?.reason_code === "workflow.review_budget_exhausted"
          ? "The saved candidate still needs code review, and this attempt has reached its review limit. Leave it held to inspect the work, or choose Cancel to stop the task and keep its history."
          : decision?.reason_code === "workflow.review_session_exited"
          ? "Use Resume the stopped session above to continue the existing review. Continue and Retry do not recover this session."
          : task.lifecycle === "backlog" && singleAgent
          ? "Make Ready checks this task's Single agent selection and that its agent profile is admitted for the project, then queues it."
          : singleAgent
          ? "Continue resumes automatic work. Cancel stops this Single agent run and keeps its history. Accepting or reworking a result happens in the run panel above."
          : task.lifecycle === "backlog"
          ? project?.trip?.readiness === "ready"
            ? "Make Ready checks the task and all six agent roles, then queues it. Queued tasks start automatically when an agent slot is free."
            : projectSetupProblem(project, "task.project_ready")
          : "Continue resumes automatic work. Pause after this step lets the current agent finish first. Cancel stops the task. Controls may take a moment while LLMRelay confirms each agent has stopped."}
      </small>
    </section>
  );
}
