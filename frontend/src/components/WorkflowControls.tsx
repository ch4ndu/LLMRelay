import { ErrorNotice } from "./ErrorNotice";
import { useRef, useState } from "react";
import {
  ApiError,
  command,
  operation,
  operationIntent,
  reuseOperationIdentity,
} from "../api";
import type {
  ContinuationAction,
  DecisionExplanation,
  Project,
  Session,
  SwitchIntent,
  Task,
  WorkflowControl,
} from "../types";

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
  },
) {
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
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
  const actionButton = (action: ContinuationAction) => {
    const attemptId = actionValue(action, "attempt_id");
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
            className="primary"
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
              ? "Resume retained restart session"
              : action.operation === "runtime_probe_resume"
              ? "Resume retained runtime probe"
              : "Resume retained session"}
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
              ? "Retry graceful stop"
              : "Force stop exact managed process"}
          </button>
        );
      case "continue_fresh_dispatch":
        return (
          <button
            disabled={!action.enabled || !!busy}
            onClick={() => void control("continue")}
          >
            Continue with fresh dispatch
          </button>
        );
      case "fresh_accounted_retry":
        return (
          <button
            className="primary"
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
              ? "Start fresh accounted setup session"
              : "Start fresh accounted session"}
          </button>
        );
      case "replace_stale_authority":
        return (
          <button onClick={() => onOpenSetup(task.project_id)}>
            Review current role authority
          </button>
        );
      case "prepare_corrected_runtime":
        return (
          <button
            disabled={!action.enabled}
            onClick={() => onOpenSetup(task.project_id)}
          >
            Prepare corrected runtime verification
          </button>
        );
      case "start_managed_legacy_attempt":
        return (
          <button
            className="primary"
            disabled={!action.enabled || !!busy}
            onClick={() => void send("normalize_legacy_task")}
          >
            Start a fresh managed attempt
          </button>
        );
      case "refresh_and_reconcile":
        return (
          <button
            disabled={!action.enabled || !!busy}
            onClick={onChanged}
          >
            Refresh and review corrected control
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
        <h3>Workflow controls</h3>
        <span>
          {task.active_attempt?.phase?.replaceAll("_", " ") || task.lifecycle}
        </span>
      </header>
      {decision && (
        <p className="hint">
          {decision.primary_blocker?.message ||
            decision.reason_code.replaceAll("_", " ")}. Owner:{" "}
          {decision.ownership.owner}. {decision.next_action
            ? `Next: ${decision.next_action.operation.replaceAll("_", " ")}.`
            : "No current action is offered."}
        </p>
      )}
      {terminal
        ? (
          <p className="hint">
            This task is terminal; its workflow history is read-only.
          </p>
        )
        : task.lifecycle === "backlog"
        ? (
          <div className="button-row">
            <button
              className="primary"
              disabled={!!busy || !allows("make_ready")}
              title={decision?.control_policy.disabled_reason_code
                ? decision.primary_blocker?.message ||
                  decision.control_policy.disabled_reason_code
                : project?.trip?.readiness !== "ready"
                ? project?.trip?.reason
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
              Pause after role
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
          <strong>Manager authority</strong>
          <small>
            requested {requestedManager
              ? `${requestedManager.config.provider} · ${requestedManager.config.model} · ${requestedManager.config.effort} (rev ${requestedManager.revision})`
              : "not configured"}
            {" · "}effective {effectiveManager
              ? `${effectiveManager.config.provider} · ${effectiveManager.config.model} · ${effectiveManager.config.effort} (rev ${effectiveManager.revision})`
              : "not active"}
            {managerSession ? ` · ${managerSession.status}` : ""}
          </small>
          {managerControl && (
            <small
              className={managerControl.state === "failed"
                ? "error"
                : "badge waiting"}
            >
              {managerControl.kind.replaceAll("_", " ")} ·{" "}
              {managerControl.state.replaceAll("_", " ")}
              {typeof managerControl.payload.next_action === "string"
                ? ` · ${managerControl.payload.next_action}`
                : ""}
            </small>
          )}
          {managerSwitch && (
            <small className="badge waiting">
              Replacement switch {managerSwitch.state.replaceAll("_", " ")}.
            </small>
          )}
          {task.lifecycle === "awaiting_review" && (
            <small className="badge waiting">
              Human review is still authoritative. The requested manager profile
              is future configuration only; it cannot dispatch, resume, replace
              the manager, or rerun final review here.
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
          <h4>Current recovery and continuation</h4>
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
              <strong>{action.kind.replaceAll("_", " ")}</strong>
              <span>{action.reason}</span>
              {action.waiting_for && (
                <small>Waiting for: {action.waiting_for}</small>
              )}
              {action.deadline_at && (
                <small>
                  Deadline: {new Date(action.deadline_at).toLocaleString()}
                </small>
              )}
              {action.accounting_note && (
                <small>{action.accounting_note}</small>
              )}
              {actionButton(action)}
            </article>
          ))}
        </section>
      )}
      {error && <ErrorNotice error={error} />}
      <small>
        {terminal
          ? "Accepted and cancelled work cannot be resumed or changed from terminal history."
          : task.lifecycle === "backlog"
          ? project?.trip?.readiness === "ready"
            ? "Ready validates the task and the host manager plus five delegated role settings before queue pickup."
            : `Ready is blocked: ${
              project?.trip?.reason || "the project is not initialized"
            }. The draft remains available.`
          : "Control requests are versioned and may remain draining until every owned process is reconciled."}
      </small>
    </section>
  );
}
