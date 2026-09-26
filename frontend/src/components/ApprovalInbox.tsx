import { useMemo, useRef, useState } from "react";
import { ApiError, command, reuseOperationIdentity } from "../api";
import type {
  AppState,
  PermissionRequest,
  PermissionScopePreview,
  Task,
  TripTaskVerification,
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
      <pre>{JSON.stringify(selection.command, null, 2)}</pre>
      <small className="hint">
        Source: service-owned selected check. This is distinct from native agent
        sandbox rules; selection, candidate, inputs, worktree, freshness, and
        build ownership remain mandatory.
      </small>
      {permission.family_preview && (
        <details>
          <summary>Reusable executable-family scope</summary>
          <pre>{JSON.stringify(permission.family_preview, null, 2)}</pre>
        </details>
      )}
      {permission.family_unavailable_reason && (
        <small className="hint">
          Reusable family unavailable: {permission.family_unavailable_reason}
        </small>
      )}
      {permission.inactive_reason && (
        <small className="hint">{permission.inactive_reason}</small>
      )}
      <div className="approval-actions">
        <button
          disabled={busy || !selection.scope_hash || !canApprove}
          onClick={() => onDecision("approved", "once")}
        >
          {rerun ? "Approve once to rerun" : "Approve once"}
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

export function ApprovalInbox(
  { state, onSelect, onChanged }: {
    state: AppState;
    onSelect: (task: Task, sessionId?: string) => void;
    onChanged: () => void;
  },
) {
  const pending = useMemo(
    () =>
      state.permission_requests.filter((request) =>
        request.state === "pending"
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
            ? `${cause.message} Refresh and reconcile before retrying.`
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

  return (
    <section className="panel approval-inbox">
      <header>
        <h3>Approval inbox</h3>
        <span>{pending.length + pendingChecks.length}</span>
      </header>
      <p className="hint">
        Native CLI requests and service-owned selected-check decisions are shown
        together, but their rules and authority remain separate. Codex may reuse
        an approval already granted natively without creating a new inbox item.
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
            <button
              className="approval-target"
              onClick={() => task && onSelect(task)}
            >
              <strong>
                {task?.id || selection.attempt_id} ·{" "}
                {check?.check_key || selection.check_id}
              </strong>
              <small>service check · current reviewed selection</small>
            </button>
            <ServiceCheckPermissionActions
              selection={selection}
              busy={busy === selection.check_id}
              onDecision={(decision, lifetime) =>
                decideCheck(selection, decision, lifetime)}
            />
          </article>
        );
      })}
      {!state.permission_requests.length && (
        <p className="empty">No native permission request has been recorded.</p>
      )}
      {state.permission_requests.map((request) => {
        const project = state.projects.find((item) =>
          item.id === request.project_id
        );
        const task = state.tasks.find((item) => item.id === request.task_id);
        return (
          <article
            key={request.id}
            className="approval-request"
            data-attention-target={`permission_request:${request.id}`}
            tabIndex={-1}
          >
            <button
              className="approval-target"
              onClick={() => task && onSelect(task, request.session_id)}
            >
              <strong>
                {project?.display_name || request.project_id} ·{" "}
                {task?.id || request.task_id}
              </strong>
              <small>
                {request.role.replaceAll("_", " ")} · {request.provider} ·{" "}
                {request.native_session_id.slice(0, 8) || "native ID unknown"}
              </small>
            </button>
            <dl>
              <div>
                <dt>Action</dt>
                <dd>{request.tool_name}</dd>
              </div>
              <div>
                <dt>Age</dt>
                <dd>{age(request.created_at)}</dd>
              </div>
              <div>
                <dt>Status</dt>
                <dd>
                  {request.state.replaceAll("_", " ")} · response {request
                    .delivery_state.replaceAll("_", " ")}
                </dd>
              </div>
              <div>
                <dt>Requested access</dt>
                <dd>
                  {request.requested_access == null
                    ? "Unknown — provider supplied no access field"
                    : JSON.stringify(request.requested_access)}
                </dd>
              </div>
              {request.reason && (
                <div>
                  <dt>Reason</dt>
                  <dd>{request.reason}</dd>
                </div>
              )}
            </dl>
            <pre>{shownInput(request)}</pre>
            {request.command_display && (
              <details>
                <summary>Complete structured input</summary>
                <pre>{structuredInput(request)}</pre>
              </details>
            )}
            {request.state === "pending" && (
              <div className="approval-actions">
                <button
                  disabled={busy === request.id}
                  onClick={() => decide(request, "approve_once")}
                >
                  Approve once
                </button>
                <button
                  disabled={busy === request.id ||
                    (!request.family_preview?.project &&
                      !request.family_preview?.session)}
                  onClick={() => {
                    setScope(
                      request.family_preview?.project ? "project" : "session",
                    );
                    setPreview(request);
                  }}
                >
                  Always approve matching actions
                </button>
                <button
                  className="danger"
                  disabled={busy === request.id}
                  onClick={() => decide(request, "deny")}
                >
                  Deny
                </button>
              </div>
            )}
            {request.state === "pending" && request.family_unavailable_reason &&
              (
                <small className="hint">
                  {request.family_unavailable_reason}. Approve this exact
                  request once, deny it, or answer the faithful prompt in its
                  native terminal.
                </small>
              )}
            {request.state !== "pending" && request.decision_reason && (
              <small className="hint">{request.decision_reason}</small>
            )}
            <details>
              <summary>Permission audit</summary>
              <dl>
                <div>
                  <dt>Original decision</dt>
                  <dd>
                    {request.decision_kind?.replaceAll("_", " ") ||
                      "Awaiting a decision"}
                  </dd>
                </div>
                <div>
                  <dt>Decision actor</dt>
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
                      ? ` ${
                        new Date(request.delivery_unknown_at).toLocaleString()
                      }`
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
                A delivered response means only that bytes were written and
                flushed to the authenticated local hook connection. It does not
                prove the native command executed.
              </small>
            </details>
          </article>
        );
      })}
      {error && <p className="error" role="alert">{error}</p>}
      <details className="permission-rules">
        <summary>
          Reusable permission rules ({state.permission_rules.length})
        </summary>
        <small className="hint">
          These are LLMRelay-owned rules. Revoke affects their future request
          decisions only; it does not revoke native Codex approvals or undo an
          already dispatched command.
        </small>
        {state.permission_rules.map((rule) => {
          const project = state.projects.find((item) =>
            item.id === rule.project_id
          );
          return (
            <article key={rule.id}>
              <strong>{rule.display_family}</strong>
              <small>
                {rule.provider} · {rule.role.replaceAll("_", " ")} ·{" "}
                {rule.lifetime} · authorized {rule.use_count} reserved responses
              </small>
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
              <small>
                {rule.revoked_at
                  ? "Revoked " + new Date(rule.revoked_at).toLocaleString()
                  : rule.last_used_at
                  ? "Last authorization reserved " +
                    new Date(rule.last_used_at).toLocaleString()
                  : "No authorization reserved after creation"}
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
          <p className="empty">No reusable rules have been granted.</p>
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
          <h2 id="permission-preview-title">Always approval scope</h2>
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
        <small>Request {request.id.slice(0, 8)}</small>
      </section>
    </div>
  );
}
