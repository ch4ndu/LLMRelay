import { ErrorNotice } from "./ErrorNotice";
import { useMemo, useState } from "react";
import { command, operationId } from "../api";
import type {
  AppState,
  AttentionCategory,
  AttentionItem,
  AttentionTarget,
} from "../types";

const attentionGroups: Array<{ category: AttentionCategory; label: string }> = [
  { category: "permission", label: "Permissions" },
  { category: "decision", label: "Decisions" },
  { category: "recovery", label: "Recovery" },
  { category: "compatibility", label: "Compatibility" },
  { category: "blocked", label: "Blocked" },
  { category: "awaiting_acceptance", label: "Completed · awaiting acceptance" },
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
    return request?.state === "pending" &&
        request.revision === target.request_revision
      ? undefined
      : "the permission request was already decided or changed.";
  }
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
  }
};

/** The rendered element marked for exactly `target`; no other panel stands in. */
export function attentionFocusElement(
  target: AttentionTarget,
): HTMLElement | undefined {
  const marker = attentionMarker(target);
  return [...document.querySelectorAll<HTMLElement>("[data-attention-target]")]
    .find((element) => element.dataset.attentionTarget === marker);
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
    <section className="panel inbox" aria-label="Attention inbox">
      <header>
        <h3>Attention inbox</h3>
        <span>{state.attention.length}</span>
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
        return (
          <div
            key={category}
            className="attention-group"
            role="group"
            aria-label={`${label} (${items.length})`}
          >
            <h4>
              {label} <span>{items.length}</span>
            </h4>
            {items.map((item) => {
              const { target: destination } = item;
              const project = destination &&
                projectName(destination.project_id);
              return destination
                ? (
                  <button
                    key={item.id}
                    type="button"
                    className="attention-item"
                    data-attention-id={item.id}
                    onClick={() => open(item, destination)}
                  >
                    <strong>{item.title}</strong>
                    <small>{item.reason}</small>
                    {project && <em>{project}</em>}
                  </button>
                )
                : (
                  <div
                    key={item.id}
                    className="attention-item restore-hold-item"
                    data-attention-id={item.id}
                  >
                    <strong>{item.title}</strong>
                    <small>{item.reason}</small>
                    {item.held_tasks.map((held) => (
                      <button
                        key={held.task_id}
                        type="button"
                        onClick={() => open(item, { kind: "task", ...held })}
                      >
                        {held.task_id} · {taskTitle(held.task_id)}
                      </button>
                    ))}
                  </div>
                );
            })}
          </div>
        );
      })}
      {!state.attention.length && (
        <p className="empty">
          No task needs a human decision.
        </p>
      )}
      <div className="guidance">
        <h4>Manager guidance</h4>
        <select
          aria-label="Guidance target"
          value={target}
          onChange={(event) => setTarget(event.target.value)}
        >
          <option value="">Choose running manager</option>
          {targets.map(({ task, session }) => (
            <option
              key={session.role_generation_id}
              value={session.role_generation_id}
            >
              {task.id} · {session.provider} ·{" "}
              {session.role_generation_id.slice(0, 8)}
            </option>
          ))}
        </select>
        {!targets.length && (
          <small className="hint">
            Guidance becomes available while a task-bound manager session is
            active.
          </small>
        )}
        <textarea
          aria-label="Guidance message"
          value={message}
          onChange={(event) => setMessage(event.target.value)}
          placeholder="Submit once; delivery waits for a proven safe idle boundary."
        />
        <button disabled={!target || !message.trim()} onClick={send}>
          Queue guidance
        </button>
        {error && <ErrorNotice error={error} />}
        <ul>
          {state.guidance.slice(0, 8).map((item) => (
            <li key={String(item.id)}>
              <span>{String(item.body)}</span>
              <em>{String(item.state).replaceAll("_", " ")}</em>
            </li>
          ))}
        </ul>
      </div>
    </section>
  );
}
