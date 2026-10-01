import { ErrorNotice } from "./ErrorNotice";
import { useRef, useState } from "react";
import {
  ApiError,
  command,
  operation,
  operationIntent,
  reuseOperationIdentity,
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
  type WorkflowControl,
} from "../types";

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
  const setupProblem = projectSetupProblem(
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
  const pendingManagerChange = !!requestedManager && !!effectiveManager &&
    requestedManager.revision !== effectiveManager.revision;
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
      sessions.find((session) => session.id === actionValue(action, "session_id"))
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
                setRouteNotice(onOpenRecoveryRecord(attemptId, recoveryId) ?? "");
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
      {decision && decision.primary_blocker?.message && (
        <p className="hint decision-summary">
          {setupProblem || decision.primary_blocker.message}{" "}
          <span className="owner">
            Waiting on: {ownerLabel(decision.primary_blocker.owner)}.
          </span>
        </p>
      )}
      {decision && (
        <TechnicalDetails>
          <p>
            {decision.reason_code} · {decision.disposition} · owner{" "}
            {decision.ownership.owner} ·{" "}
            {decision.next_action
              ? `next ${decision.next_action.operation}`
              : "no current action"}
          </p>
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
                : project?.trip?.readiness !== "ready"
                ? projectSetupProblem(project, "task.project_ready")
                : undefined}
              onClick={() => send("make_ready")}
            >
              Make Ready
            </button>
            {project && (project.trip?.readiness !== "ready" ||
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
              Run next
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
      {!terminal && task.lifecycle !== "backlog" && (
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
              requested revision {requestedManager?.revision ?? "none"} ·
              effective revision {effectiveManager?.revision ?? "none"}
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
              pendingManagerChange && requestedManager && (
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
                  {actionTitles[action.kind] || "Next step"}
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
          : task.lifecycle === "backlog"
          ? project?.trip?.readiness === "ready"
            ? "Make Ready checks the task and all six agent roles, then queues it. Queued tasks start automatically when an agent slot is free."
            : projectSetupProblem(project, "task.project_ready")
          : "Continue resumes automatic work. Pause after this step lets the current agent finish first. Cancel stops the task. Controls may take a moment while LLMRelay confirms each agent has stopped."}
      </small>
    </section>
  );
}
