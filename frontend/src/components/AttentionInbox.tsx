import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { useMemo, useState } from "react";
import { command, operationId } from "../api";
import type {
  AppState,
  AttentionCategory,
  AttentionItem,
  AttentionTarget,
} from "../types";

const attentionGroups: Array<{ category: AttentionCategory; label: string }> = [
  { category: "permission", label: "Waiting for your approval" },
  { category: "decision", label: "Waiting for your decision" },
  { category: "recovery", label: "Manual action needed" },
  { category: "compatibility", label: "Setup needs attention" },
  { category: "blocked", label: "Can't continue yet" },
  { category: "awaiting_acceptance", label: "Ready for your review" },
];

const sameTarget = (left: AttentionTarget, right: AttentionTarget) => {
  const entries = Object.entries(left);
  return entries.length === Object.keys(right).length &&
    entries.every(([key, value]) => Reflect.get(right, key) === value);
};

function unresolvedTarget(
  state: AppState,
  target: AttentionTarget,
): string | undefined {
  if (target.kind === "permission_request") {
    const request = state.permission_requests.find((candidate) =>
      candidate.id === target.request_id
    );
    return request?.actionable &&
        request.revision === target.request_revision
      ? undefined
      : "the permission request was already decided, answered in the agent, or changed.";
  }
  if (target.kind === "diagnostics") return undefined;
  if (target.kind === "project_setup") {
    const project = state.projects.find((candidate) =>
      candidate.id === target.project_id
    );
    if (!project) return "the project is no longer registered.";
    return (project.trip?.setup_operation_id ?? null) ===
        target.setup_operation_id
      ? undefined
      : "a different setup operation is now current.";
  }
  const task = state.tasks.find((candidate) =>
    candidate.id === target.task_id &&
    candidate.project_id === target.project_id
  );
  if (!task) return "the task is no longer shown.";
  const attempt = task.active_attempt;
  switch (target.kind) {
    case "task":
      return task.version === target.task_version
        ? undefined
        : "the task changed.";
    case "attempt":
      return attempt?.id === target.attempt_id &&
          attempt.phase === target.phase &&
          (attempt.plan_hash ?? null) === target.plan_hash &&
          (attempt.candidate_hash ?? null) === target.candidate_hash
        ? undefined
        : "a newer attempt or decision replaced it.";
    case "session": {
      const session = state.active_sessions.find((candidate) =>
        candidate.id === target.session_id
      );
      return session?.task_id === target.task_id &&
          session.attempt_id === target.attempt_id &&
          session.role_generation_id === target.role_generation_id
        ? undefined
        : "the session was replaced.";
    }
    case "recovery_record":
      return attempt?.id === target.attempt_id &&
          state.recovery.some((record) =>
            record.id === target.recovery_id &&
            record.attempt_id === target.attempt_id &&
            record.state === "attention_required"
          )
        ? undefined
        : "the recovery record was resolved or its attempt was superseded.";
    case "role_settings":
      return task.role_settings.some((setting) =>
          setting.role === target.role &&
          setting.revision === target.settings_revision
        )
        ? undefined
        : "the agent settings changed.";
  }
}

/**
 * Why `target`, offered by `item`, cannot be opened exactly in `state`; an
 * old binding is refused rather than retargeted to a newer entity.
 */
export function attentionTargetProblem(
  state: AppState,
  item: AttentionItem,
  target: AttentionTarget,
): string | undefined {
  const current = state.attention.find((candidate) => candidate.id === item.id);
  if (!current) return "it is no longer in the attention list.";
  const offered = [
    current.target,
    ...current.held_tasks.map((held): AttentionTarget => ({
      kind: "task",
      ...held,
    })),
  ];
  if (
    !offered.some((candidate) =>
      candidate !== null && sameTarget(candidate, target)
    )
  ) return "its target was replaced.";
  return unresolvedTarget(state, target);
}

// Panels mark their focus container with `data-attention-target="<kind>:<id>"`.
const attentionMarker = (target: AttentionTarget) => {
  switch (target.kind) {
    case "task":
      return `task:${target.task_id}`;
    case "attempt":
      return `attempt:${target.attempt_id}`;
    case "session":
      return `session:${target.session_id}`;
    case "permission_request":
      return `permission_request:${target.request_id}`;
    case "recovery_record":
      return `recovery_record:${target.recovery_id}`;
    case "project_setup":
      return `project_setup:${target.project_id}`;
    case "role_settings":
      return `role_settings:${target.task_id}:${target.role}`;
    case "diagnostics":
      return "diagnostics";
  }
};

/**
 * The rendered element marked for exactly `target`; no other panel stands in.
 * Content behind an open dialog is inert and never receives the focus.
 */
export function attentionFocusElement(
  target: AttentionTarget,
): HTMLElement | undefined {
  const marker = attentionMarker(target);
  return [...document.querySelectorAll<HTMLElement>("[data-attention-target]")]
    .find((element) =>
      element.dataset.attentionTarget === marker && !element.closest("[inert]")
    );
}

export function AttentionInbox(
  { state, onNavigate, onChanged }: {
    state: AppState;
    onNavigate: (
      item: AttentionItem,
      target: AttentionTarget,
    ) => string | undefined;
    onChanged: () => void;
  },
) {
  const [message, setMessage] = useState("");
  const [target, setTarget] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const open = (item: AttentionItem, destination: AttentionTarget) =>
    setNotice(onNavigate(item, destination) ?? "");
  const projectName = (projectId: string) =>
    state.projects.find((project) => project.id === projectId)?.display_name;
  const taskTitle = (taskId: string) =>
    state.tasks.find((task) => task.id === taskId)?.title;
  const targets = useMemo(() =>
    state.active_sessions
      .filter((session) =>
        session.role === "manager" && session.status === "running"
      )
      .flatMap((session) => {
        const task = state.tasks.find((item) =>
          item.id === session.task_id &&
          item.active_attempt?.id === session.attempt_id
        );
        return task ? [{ task, session }] : [];
      }), [state.active_sessions, state.tasks]);

  const send = async () => {
    const selected = targets.find((item) =>
      item.session.role_generation_id === target
    );
    if (!selected) {
      setError(
        "The service has not exposed a task-bound running manager target.",
      );
      return;
    }
    try {
      await command({
        kind: "guidance",
        operation_id: operationId(),
        task_id: selected.task.id,
        role_generation_id: target,
        expected_version: selected.task.version,
        body: message,
      });
      setMessage("");
      setError("");
      onChanged();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  return (
    <section
      className="panel inbox"
      id="workspace-attention"
      aria-labelledby="attention-title"
      tabIndex={-1}
    >
      <header>
        <h2 id="attention-title">Needs your attention</h2>
        <span className="count" aria-label={`${state.attention.length} waiting`}>
          {state.attention.length}
        </span>
      </header>
      {notice && (
        <p className="attention-notice" role="status">
          {notice}
          <button type="button" onClick={() => setNotice("")}>Dismiss</button>
        </p>
      )}
      {attentionGroups.map(({ category, label }) => {
        const items = state.attention.filter((item) =>
          item.category === category
        );
        if (!items.length) return null;
        // One explanation shared by every item is shown once for the group.
        const sharedReason = items.length > 1 &&
            items.every((item) => item.reason === items[0].reason)
          ? items[0].reason
          : undefined;
        return (
          <div
            key={category}
            className="attention-group"
            role="group"
            aria-label={`${label} (${items.length})`}
          >
            <h3>
              {label} <span className="count">{items.length}</span>
            </h3>
            {sharedReason && <p className="hint">{sharedReason}</p>}
            {items.map((item) => {
              const { target: destination } = item;
              const project = destination && "project_id" in destination
                ? projectName(destination.project_id)
                : undefined;
              const subject = [item.task_title, project].filter(Boolean).join(
                " · ",
              );
              return (
                <article
                  key={item.id}
                  className="attention-item"
                  data-attention-id={item.id}
                >
                  <div className="attention-copy">
                    <strong>{item.title}</strong>
                    {subject && <small className="attention-subject">{subject}</small>}
                    {!sharedReason && item.reason && <p>{item.reason}</p>}
                    {item.details && (
                      <TechnicalDetails>
                        <pre>{item.details}</pre>
                      </TechnicalDetails>
                    )}
                  </div>
                  {destination
                    ? (
                      <button
                        type="button"
                        className="attention-open"
                        onClick={() => open(item, destination)}
                      >
                        {item.action?.label || "Open"}
                        <span className="visually-hidden">: {item.title}</span>
                      </button>
                    )
                    : item.held_tasks.length > 0
                    ? (
                      <div className="held-tasks">
                        {item.held_tasks.map((held) => (
                          <button
                            key={held.task_id}
                            type="button"
                            onClick={() => open(item, { kind: "task", ...held })}
                          >
                            Open {taskTitle(held.task_id) || "task"}
                          </button>
                        ))}
                      </div>
                    )
                    : (
                      // No current page can act on this item, so the only
                      // honest action is to fetch the latest state.
                      <div className="attention-refresh">
                        <small className="hint">
                          Nothing here can be opened directly. Refresh to see
                          whether it has been resolved.
                        </small>
                        <button type="button" onClick={onChanged}>
                          Refresh
                          <span className="visually-hidden">: {item.title}</span>
                        </button>
                      </div>
                    )}
                </article>
              );
            })}
          </div>
        );
      })}
      {!state.attention.length && (
        <p className="empty">Nothing is waiting for you.</p>
      )}
      <details className="guidance">
        <summary>Send guidance to a running manager</summary>
        <label>
          Manager
          <select
            aria-label="Guidance target"
            value={target}
            onChange={(event) => setTarget(event.target.value)}
          >
            <option value="">Choose a running manager</option>
            {targets.map(({ task, session }) => (
              <option
                key={session.role_generation_id}
                value={session.role_generation_id}
              >
                {task.title} · {session.provider === "claude" ? "Claude" : "Codex"}
              </option>
            ))}
          </select>
        </label>
        {!targets.length && (
          <small className="hint">
            Guidance can be sent while a task's manager is running.
          </small>
        )}
        <textarea
          aria-label="Guidance message"
          value={message}
          onChange={(event) => setMessage(event.target.value)}
          placeholder="The manager receives this when it next pauses between steps."
        />
        <button disabled={!target || !message.trim()} onClick={send}>
          Queue guidance
        </button>
        {error && <ErrorNotice error={error} />}
        {state.guidance.length > 0 && (
          <ul>
            {state.guidance.slice(0, 8).map((item) => (
              <li key={String(item.id)}>
                <span>{String(item.body)}</span>
                <em>{guidanceState(String(item.state))}</em>
              </li>
            ))}
          </ul>
        )}
        <TechnicalDetails>
          <p>
            Guidance waits for a safe pause between the manager's steps. If
            delivery cannot be confirmed it is not sent again automatically.
          </p>
        </TechnicalDetails>
      </details>
    </section>
  );
}

export function guidanceState(state: string): string {
  switch (state) {
    case "queued":
    case "delivery_reserved":
      return "Waiting for the manager to pause";
    case "written_awaiting_submit":
      return "Typed into the manager's input; not submitted yet";
    case "submitted":
      return "Sent to the manager; not yet confirmed as accepted";
    case "acknowledged":
      return "Received by the manager";
    case "delivery_unknown":
      return "Delivery not confirmed — check the manager's output before sending again";
    default:
      return state.replaceAll("_", " ");
  }
}
