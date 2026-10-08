import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { useMemo, useRef, useState } from "react";
import { ApiError, command, reuseOperationIdentity } from "../api";
import {
  type AppState,
  type PermissionNativeResolution,
  type PermissionRequest,
  type PermissionScopePreview,
  roleLabel,
  type Task,
  type TripTaskVerification,
} from "../types";

type Scope = "session" | "project";

export function ServiceCheckPermissionActions(
  { selection, busy = false, onDecision, onRevoke }: {
    selection: TripTaskVerification;
    busy?: boolean;
    onDecision: (
      decision: "approved" | "denied",
      lifetime: "once" | "family",
    ) => void;
    onRevoke?: (ruleId: string, revision: number) => void;
  },
) {
  const permission = selection.authorization;
  const canApprove = permission.action_state === "actionable" ||
    permission.action_state === "current_receipt";
  const rerun = permission.action_state === "current_receipt";
  return (
    <div className="service-check-permission">
      <pre className="approval-command">
        {selection.command.shell ||
          [selection.command.executable, ...(selection.command.arguments || [])]
            .filter(Boolean).join(" ")}
      </pre>
      <small className="hint">
        LLMRelay runs this check itself in the task's workspace. Approving it
        does not change what agents are allowed to do.
      </small>
      <TechnicalDetails>
        <pre>{JSON.stringify(selection.command, null, 2)}</pre>
        <p>
          Service-owned selected check, separate from agent sandbox rules. The
          selection, candidate, inputs, worktree, freshness and build ownership
          are still checked before it runs.
        </p>
        {permission.family_preview && (
          <>
            <h4>Reusable executable-family scope</h4>
            <pre>{JSON.stringify(permission.family_preview, null, 2)}</pre>
          </>
        )}
      </TechnicalDetails>
      {permission.family_unavailable_reason && (
        <small className="hint">
          “Always approve” is not available: {permission.family_unavailable_reason}
        </small>
      )}
      {permission.inactive_reason && (
        <small className="hint">{permission.inactive_reason}</small>
      )}
      <div className="approval-actions">
        <button
          disabled={busy || !selection.scope_hash || !canApprove ||
            (permission.authorized && !rerun)}
          onClick={() => onDecision("approved", "once")}
        >
          {rerun ? "Approve once to rerun" : permission.authorized
            ? "Approved for this candidate" : "Approve once"}
        </button>
        <button
          disabled={busy || !selection.scope_hash ||
            !permission.family_preview ||
            permission.family ||
            !canApprove}
          onClick={() => onDecision("approved", "family")}
        >
          Always approve matching actions
        </button>
        <button
          className="danger"
          disabled={busy || !selection.scope_hash ||
            permission.state === "denied" ||
            permission.action_state !== "actionable"}
          onClick={() => onDecision("denied", "once")}
        >
          Deny
        </button>
        {permission.matching_rule && onRevoke && (
          <button
            disabled={busy}
            onClick={() =>
              onRevoke(
                permission.matching_rule!.id,
                permission.matching_rule!.revision,
              )}
          >
            Revoke service-check rule
          </button>
        )}
      </div>
    </div>
  );
}

const age = (created: string) => {
  const seconds = Math.max(
    0,
    Math.floor((Date.now() - new Date(created).getTime()) / 1000),
  );
  if (seconds < 60) return String(seconds) + "s";
  if (seconds < 3600) return String(Math.floor(seconds / 60)) + "m";
  return String(Math.floor(seconds / 3600)) + "h";
};

const structuredInput = (request: PermissionRequest) =>
  JSON.stringify(request.input, null, 2) ||
  "(provider supplied no structured input)";

const shownInput = (request: PermissionRequest) =>
  request.command_display || structuredInput(request);

const shownScopeValue = (value: unknown) => {
  if (typeof value === "string" && value.length > 0) return value;
  if (value == null || value === "") return "Unavailable — not provided";
  return JSON.stringify(value) || "Unavailable — not provided";
};

/** Actionable native permission requests plus actionable service-check approvals. */
export const pendingApprovalCount = (state: AppState) =>
  state.permission_requests.filter((request) => request.actionable).length +
  (state.trip_task_verification || []).filter((check) =>
    check.authorization.state === "pending" &&
    check.authorization.action_state === "actionable"
  ).length;

export function ApprovalInbox(
  { state, onSelect, onChanged }: {
    state: AppState;
    onSelect: (task: Task, sessionId?: string) => void;
    onChanged: () => void;
  },
) {
  const pending = useMemo(
    () => state.permission_requests.filter((request) => request.actionable),
    [state.permission_requests],
  );
  const answeredInAgent = useMemo(
    () =>
      state.permission_requests.filter((request) =>
        !request.actionable && request.native_resolution
      ),
    [state.permission_requests],
  );
  const decided = useMemo(
    () =>
      state.permission_requests.filter((request) =>
        !request.actionable && !request.native_resolution
      ),
    [state.permission_requests],
  );
  const pendingChecks = useMemo(
    () =>
      (state.trip_task_verification || []).filter((check) =>
        check.authorization.state === "pending" &&
        check.authorization.action_state === "actionable"
      ),
    [state.trip_task_verification],
  );
  const [preview, setPreview] = useState<PermissionRequest>();
  const [scope, setScope] = useState<Scope>("session");
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const operationStorageKey = "llmrelay.approval.operations";
  const operationIdentities = useRef(
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
  const execute = async (key: string, body: Record<string, unknown>) => {
    const stable = reuseOperationIdentity(
      operationIdentities.current.get(key),
      body,
    );
    const { id, request } = stable;
    operationIdentities.current.set(key, { body: JSON.stringify(request), id });
    localStorage.setItem(
      operationStorageKey,
      JSON.stringify([...operationIdentities.current]),
    );
    try {
      const result = await command({ ...request, operation_id: id } as never);
      operationIdentities.current.delete(key);
      localStorage.setItem(
        operationStorageKey,
        JSON.stringify([...operationIdentities.current]),
      );
      return result;
    } catch (cause) {
      if (!(cause instanceof ApiError && cause.ambiguous)) {
        operationIdentities.current.delete(key);
        localStorage.setItem(
          operationStorageKey,
          JSON.stringify([...operationIdentities.current]),
        );
      }
      throw cause;
    }
  };

  const decide = async (
    request: PermissionRequest,
    decision: "approve_once" | "always_approve" | "deny",
    lifetime?: Scope,
  ) => {
    setBusy(request.id);
    setError("");
    try {
      await execute(`permission:${request.id}`, {
        kind: "decide_permission",
        request_id: request.id,
        expected_revision: request.revision,
        decision,
        ...(lifetime ? { lifetime } : {}),
        reason: "",
      });
      setPreview(undefined);
      onChanged();
    } catch (cause) {
      if (
        cause instanceof ApiError && (cause.status === 409 || cause.ambiguous)
      ) {
        setError(
          cause.ambiguous
            ? `${cause.message} Your decision may already be recorded, so it was not sent again. The latest state was requested; check whether this request is still waiting before choosing again.`
            : "The request changed in another view. State was refreshed.",
        );
        onChanged();
      } else {
        setError(cause instanceof Error ? cause.message : String(cause));
      }
    } finally {
      setBusy("");
    }
  };

  const revoke = async (id: string, revision: number) => {
    setBusy(id);
    setError("");
    try {
      await execute(`permission-rule:${id}`, {
        kind: "revoke_permission_rule",
        rule_id: id,
        expected_revision: revision,
        reason: "",
      });
      onChanged();
    } catch (cause) {
      if (
        cause instanceof ApiError && (cause.status === 409 || cause.ambiguous)
      ) onChanged();
      setError(
        cause instanceof ApiError && cause.ambiguous
          ? `${cause.message} Refresh and reconcile before retrying.`
          : cause instanceof Error
          ? cause.message
          : String(cause),
      );
    } finally {
      setBusy("");
    }
  };

  const decideCheck = async (
    selection: TripTaskVerification,
    decision: "approved" | "denied",
    lifetime: "once" | "family",
  ) => {
    setBusy(selection.check_id);
    setError("");
    try {
      await execute(`check:${selection.attempt_id}:${selection.check_id}`, {
        kind: "trip",
        action: "authorize_check",
        attempt_id: selection.attempt_id,
        check_id: selection.check_id,
        selected_revision: selection.selected_revision,
        exact_command_hash: selection.exact_command_hash,
        scope_hash: selection.scope_hash!,
        decision,
        lifetime,
      });
      onChanged();
    } catch (cause) {
      if (cause instanceof ApiError && cause.ambiguous) onChanged();
      setError(
        cause instanceof ApiError && cause.ambiguous
          ? `${cause.message} Refresh and reconcile before retrying.`
          : cause instanceof Error
          ? cause.message
          : String(cause),
      );
    } finally {
      setBusy("");
    }
  };

  const selectedPreview = preview?.family_preview?.[scope];
  const taskFor = (taskId: string) =>
    state.tasks.find((item) => item.id === taskId);
  const subject = (request: PermissionRequest) =>
    [
      taskFor(request.task_id)?.title,
      state.projects.find((item) => item.id === request.project_id)
        ?.display_name,
    ].filter(Boolean).join(" · ");
  const count = pending.length + pendingChecks.length;

  return (
    <section
      className="panel approval-inbox"
      id="workspace-approvals"
      aria-labelledby="approvals-title"
      tabIndex={-1}
    >
      <header>
        <h2 id="approvals-title">Approvals</h2>
        <span className="count" aria-label={`${count} waiting`}>{count}</span>
      </header>
      <p className="hint">
        Agents ask here before running a command or using a tool that needs
        your permission. Nothing runs until you decide. An agent may reuse an
        approval it already has, so not every action appears here.
      </p>
      {pendingChecks.map((selection) => {
        const task = state.tasks.find((item) =>
          item.active_attempt?.id === selection.attempt_id
        );
        const check = (state.trip_checks || []).find((item) =>
          item.id === selection.check_id
        );
        return (
          <article
            key={`service:${selection.attempt_id}:${selection.check_id}`}
            className="approval-request"
          >
            <div className="approval-heading">
              <strong>
                Verification check waiting to run:{" "}
                {check?.original_text || check?.check_key || "selected check"}
              </strong>
              <small>{task?.title || "Current task"}</small>
            </div>
            {task && (
              <button className="link-button" onClick={() => onSelect(task)}>
                Open task<span className="visually-hidden">: {task.title}</span>
              </button>
            )}
            <ServiceCheckPermissionActions
              selection={selection}
              busy={busy === selection.check_id}
              onDecision={(decision, lifetime) =>
                decideCheck(selection, decision, lifetime)}
            />
          </article>
        );
      })}
      {pending.map((request) => (
        <PermissionRequestCard
          key={request.id}
          request={request}
          subject={subject(request)}
          busy={busy === request.id}
          onOpenTask={taskFor(request.task_id)
            ? () => onSelect(taskFor(request.task_id)!, request.session_id)
            : undefined}
          onDecide={(decision) => decide(request, decision)}
          onAlways={() => {
            setScope(request.family_preview?.project ? "project" : "session");
            setPreview(request);
          }}
        />
      ))}
      {!count && <p className="empty">No approvals are waiting.</p>}
      {error && <ErrorNotice error={error} />}
      {answeredInAgent.length > 0 && (
        <details className="decided-requests">
          <summary>Answered in the agent ({answeredInAgent.length})</summary>
          <small className="hint">
            These were answered in the agent's own terminal, not in LLMRelay.
            They stay here as history and can no longer be approved or denied.
          </small>
          {answeredInAgent.map((request) => (
            <PermissionRequestCard
              key={request.id}
              request={request}
              subject={subject(request)}
              busy={false}
              onDecide={() => {}}
              onAlways={() => {}}
            />
          ))}
        </details>
      )}
      {decided.length > 0 && (
        <details className="decided-requests">
          <summary>Recent decisions ({decided.length})</summary>
          {decided.map((request) => (
            <PermissionRequestCard
              key={request.id}
              request={request}
              subject={subject(request)}
              busy={false}
              onDecide={() => {}}
              onAlways={() => {}}
            />
          ))}
        </details>
      )}
      <details className="permission-rules">
        <summary>
          Saved approval rules ({state.permission_rules.length})
        </summary>
        <small className="hint">
          These rules belong to LLMRelay. Revoking one affects only future
          requests; it does not undo a command that already ran or change the
          agent's own approvals.
        </small>
        {state.permission_rules.map((rule) => {
          const project = state.projects.find((item) =>
            item.id === rule.project_id
          );
          return (
            <article key={rule.id}>
              <strong>{rule.display_family}</strong>
              <small>
                {roleLabel(rule.role)} ·{" "}
                {rule.provider === "claude" ? "Claude" : "Codex"} ·{" "}
                {rule.lifetime === "project" ? "whole project" : "one session"}{" "}
                · used {rule.use_count} time{rule.use_count === 1 ? "" : "s"}
              </small>
              <TechnicalDetails>
                <dl>
                  <div>
                    <dt>Project</dt>
                    <dd>
                      {project?.display_name ||
                        "Unavailable — registered project not present"}
                    </dd>
                  </div>
                  <div>
                    <dt>Project root</dt>
                    <dd>{shownScopeValue(rule.scope.registered_root)}</dd>
                  </div>
                  {rule.lifetime === "session" && (
                    <>
                      <div>
                        <dt>Native session</dt>
                        <dd>{shownScopeValue(rule.scope.native_session)}</dd>
                      </div>
                      <div>
                        <dt>Worktree</dt>
                        <dd>{shownScopeValue(rule.scope.worktree)}</dd>
                      </div>
                    </>
                  )}
                  <div>
                    <dt>Coverage</dt>
                    <dd>{shownScopeValue(rule.scope.coverage)}</dd>
                  </div>
                  <div>
                    <dt>Configuration binding</dt>
                    <dd>{shownScopeValue(rule.scope.configuration_binding)}</dd>
                  </div>
                </dl>
              </TechnicalDetails>
              <small>
                {rule.revoked_at
                  ? "Revoked " + new Date(rule.revoked_at).toLocaleString()
                  : rule.last_used_at
                  ? "Last used " + new Date(rule.last_used_at).toLocaleString()
                  : "Not used yet"}
              </small>
              {!rule.revoked_at && (
                <button
                  disabled={busy === rule.id}
                  onClick={() => revoke(rule.id, rule.revision)}
                >
                  Revoke
                </button>
              )}
            </article>
          );
        })}
        {!state.permission_rules.length && (
          <p className="empty">No saved approval rules.</p>
        )}
      </details>
      {preview && selectedPreview && (
        <ScopeDialog
          request={preview}
          scope={scope}
          preview={selectedPreview}
          busy={busy === preview.id}
          onScope={setScope}
          onCancel={() => setPreview(undefined)}
          onConfirm={() => decide(preview, "always_approve", scope)}
        />
      )}
    </section>
  );
}

function PermissionRequestCard(
  { request, subject, busy, onOpenTask, onDecide, onAlways }: {
    request: PermissionRequest;
    subject: string;
    busy: boolean;
    onOpenTask?: () => void;
    onDecide: (decision: "approve_once" | "deny") => void;
    onAlways: () => void;
  },
) {
  const isPending = request.actionable;
  const resolution = request.native_resolution;
  return (
    <article
      className={`approval-request ${isPending ? "pending" : "decided"}`}
      data-attention-target={isPending
        ? `permission_request:${request.id}`
        : undefined}
      tabIndex={-1}
    >
      <div className="approval-heading">
        <strong>
          {roleLabel(request.role)} wants to use {request.tool_name}
        </strong>
        <small>
          {subject || "Task not shown"} · asked {age(request.created_at)} ago
          {!isPending && ` · ${
            resolution
              ? nativeResolutionLabel[resolution.kind]
              : `${decisionLabel(request)} · ${responseLabel(request)}`
          }`}
        </small>
      </div>
      {onOpenTask && (
        <button className="link-button" onClick={onOpenTask}>
          Open task
          <span className="visually-hidden">
            : {subject.split(" · ")[0] || "for this request"}
          </span>
        </button>
      )}
      <pre className="approval-command">{shownInput(request)}</pre>
      {request.reason && <p className="approval-reason">{request.reason}</p>}
      {isPending && (
        <div className="approval-actions">
          <button className="primary" disabled={busy} onClick={() => onDecide("approve_once")}>
            Approve once
          </button>
          <button
            disabled={busy ||
              (!request.family_preview?.project &&
                !request.family_preview?.session)}
            onClick={onAlways}
          >
            Always approve matching actions
          </button>
          <button
            className="danger"
            disabled={busy}
            onClick={() => onDecide("deny")}
          >
            Deny
          </button>
        </div>
      )}
      {isPending && request.family_unavailable_reason && (
        <small className="hint">
          {request.family_unavailable_reason}. Approve this exact request once,
          deny it, or answer the prompt in the agent's own terminal.
        </small>
      )}
      {isPending && !request.native_correlation_available && (
        <small className="hint">
          If you answer this in the agent's own terminal instead, LLMRelay
          cannot see that answer, so this request stays listed until it
          expires.
        </small>
      )}
      {!isPending && request.decision_reason && (
        <small className="hint">{request.decision_reason}</small>
      )}
      <details>
        <summary>Technical details</summary>
        {request.command_display && (
          <>
            <h4>Complete structured input</h4>
            <pre>{structuredInput(request)}</pre>
          </>
        )}
        <dl>
          <div>
            <dt>Requested access</dt>
            <dd>
              {request.requested_access == null
                ? "Unknown — the agent supplied no access field"
                : JSON.stringify(request.requested_access)}
            </dd>
          </div>
          <div>
            <dt>Status</dt>
            <dd>
              {request.state.replaceAll("_", " ")} · response{" "}
              {request.delivery_state.replaceAll("_", " ")}
            </dd>
          </div>
          <div>
            <dt>Agent</dt>
            <dd>
              {request.provider} · session{" "}
              {request.native_session_id.slice(0, 8) || "unknown"}
            </dd>
          </div>
        </dl>
        <h4>Permission audit</h4>
        <dl>
          <div>
            <dt>Original decision</dt>
            <dd>
              {request.decision_kind?.replaceAll("_", " ") ||
                (resolution ? "None in LLMRelay" : "Awaiting a decision")}
            </dd>
          </div>
          {resolution && (
            <div>
              <dt>Answered in the agent</dt>
              <dd>
                {resolution.kind.replaceAll("_", " ")} ·{" "}
                {new Date(resolution.observed_at).toLocaleString()} · tool call
                {" "}
                {resolution.tool_use_id} · hook event {resolution.hook_event_id}
              </dd>
            </div>
          )}
          <div>
            <dt>Decided by</dt>
            <dd>{request.decision_actor || "Not decided"}</dd>
          </div>
          <div>
            <dt>Decision time</dt>
            <dd>
              {request.decided_at
                ? new Date(request.decided_at).toLocaleString()
                : "Not decided"}
            </dd>
          </div>
          <div>
            <dt>Matching rule</dt>
            <dd>{request.matching_rule_id || "None"}</dd>
          </div>
          <div>
            <dt>One-shot reservation</dt>
            <dd>
              {request.delivery_reserved_at
                ? `${request.reserved_behavior || "response"} reserved ${
                  new Date(request.delivery_reserved_at).toLocaleString()
                }`
                : "Not reserved"}
            </dd>
          </div>
          <div>
            <dt>Local response delivery</dt>
            <dd>
              {request.delivery_state.replaceAll("_", " ")}
              {request.delivered_at
                ? ` ${new Date(request.delivered_at).toLocaleString()}`
                : request.delivery_unknown_at
                ? ` ${new Date(request.delivery_unknown_at).toLocaleString()}`
                : ""}
            </dd>
          </div>
          {request.delivery_reason && (
            <div>
              <dt>Delivery evidence</dt>
              <dd>{request.delivery_reason}</dd>
            </div>
          )}
        </dl>
        <small className="hint">
          A delivered response means LLMRelay sent your decision to the agent.
          It does not prove the command ran.
        </small>
      </details>
    </article>
  );
}

const decisionLabel = (request: PermissionRequest) => {
  switch (request.decision_kind) {
    case "approve_once":
      return "approved once";
    case "always_approve":
      return "approved by a saved rule";
    case "deny":
      return "denied";
    default:
      return request.state.replaceAll("_", " ");
  }
};

const responseLabel = (request: PermissionRequest) => {
  switch (request.delivery_state) {
    case "not_reserved":
      return "not sent to the agent yet";
    case "reserved":
      return "waiting to be sent to the agent";
    case "delivered":
      return "sent to the agent, which does not show the command ran";
    case "unknown":
      return "sending was not confirmed and it will not be resent; check the agent's output";
    default:
      return `response ${request.delivery_state.replaceAll("_", " ")}`;
  }
};

const nativeResolutionLabel: Record<
  PermissionNativeResolution["kind"],
  string
> = {
  tool_finished: "answered in the agent's terminal; the agent reported the tool finished",
  tool_failed: "answered in the agent's terminal; the agent reported the tool failed",
  native_denied: "denied in the agent's terminal",
};

function ScopeDialog(
  { request, scope, preview, busy, onScope, onCancel, onConfirm }: {
    request: PermissionRequest;
    scope: Scope;
    preview: PermissionScopePreview;
    busy: boolean;
    onScope: (scope: Scope) => void;
    onCancel: () => void;
    onConfirm: () => void;
  },
) {
  return (
    <div className="modal-backdrop" role="presentation">
      <section
        className="modal permission-preview"
        role="dialog"
        aria-modal="true"
        aria-labelledby="permission-preview-title"
      >
        <header>
          <h2 id="permission-preview-title">Always approve matching actions</h2>
        </header>
        <label>
          Lifetime
          <select
            aria-label="Always approval lifetime"
            value={scope}
            onChange={(event) => onScope(event.target.value as Scope)}
          >
            {request.family_preview?.project && (
              <option value="project">Project (recommended)</option>
            )}
            {request.family_preview?.session && (
              <option value="session">Session</option>
            )}
          </select>
        </label>
        <dl>
          <div>
            <dt>Command family</dt>
            <dd>{preview.command_family}</dd>
          </div>
          <div>
            <dt>Arguments</dt>
            <dd>{preview.arguments}</dd>
          </div>
          <div>
            <dt>Provider / role</dt>
            <dd>{preview.provider} / {preview.role}</dd>
          </div>
          <div>
            <dt>Coverage</dt>
            <dd>{preview.coverage}</dd>
          </div>
          <div>
            <dt>Configuration binding</dt>
            <dd>{preview.configuration_binding}</dd>
          </div>
          {preview.worktree && (
            <div>
              <dt>Worktree</dt>
              <dd>{preview.worktree}</dd>
            </div>
          )}
          {preview.registered_root && (
            <div>
              <dt>Registered root</dt>
              <dd>{preview.registered_root}</dd>
            </div>
          )}
        </dl>
        <p className="warning">{preview.warning}</p>
        <p>
          This grants all current and future arguments for the displayed
          executable family, including changed filenames, counts, flags, and
          configuration values. It does not grant compound commands,
          substitutions, redirects, another provider or role, or blanket agent
          authority.
        </p>
        <footer>
          <button onClick={onCancel}>Cancel</button>
          <button className="primary" disabled={busy} onClick={onConfirm}>
            Confirm always approval
          </button>
        </footer>
        <TechnicalDetails>
          <p>Request {request.id}</p>
        </TechnicalDetails>
      </section>
    </div>
  );
}
