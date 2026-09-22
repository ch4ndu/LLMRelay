import { useMemo, useState } from "react";
import { command, operationId } from "../api";
import type { AppState, Task } from "../types";

export function AttentionInbox(
  { state, onSelect, onChanged, onOpenSetup = () => {} }: {
    state: AppState;
    onSelect: (task: Task) => void;
    onChanged: () => void;
    onOpenSetup?: (projectId: string) => void;
  },
) {
  const [message, setMessage] = useState("");
  const [target, setTarget] = useState("");
  const [error, setError] = useState("");
  const attention = state.tasks.filter((task) =>
    task.attention !== "none" || task.permission_waiting
  );
  const setupAttention = state.projects.filter((project) =>
    project.trip && project.trip.readiness !== "ready"
  );
  const livenessItems = state.continuation_actions.filter((action) =>
    action.owner !== "service" || action.waiting_for || action.enabled
  );
  const taskForAction = (action: typeof livenessItems[number]) => {
    const taskId = action.binding.task_id;
    if (typeof taskId === "string") {
      return state.tasks.find((task) => task.id === taskId);
    }
    const attemptId = action.binding.attempt_id;
    return typeof attemptId === "string"
      ? state.tasks.find((task) => task.active_attempt?.id === attemptId)
      : undefined;
  };
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
    <section className="panel inbox">
      <header>
        <h3>Attention inbox</h3>
        <span>
          {attention.length + setupAttention.length + livenessItems.length}
        </span>
      </header>
      {attention.map((task) => (
        <button
          key={task.id}
          onClick={() => onSelect(task)}
        >
          <strong>{task.id} · {task.title}</strong>
          <small>
            {task.permission_waiting
              ? "waiting for permission"
              : task.attention.replaceAll("_", " ")}
          </small>
        </button>
      ))}
      {setupAttention.map((project) => (
        <button
          key={`setup:${project.id}`}
          onClick={() => onOpenSetup(project.id)}
        >
          <strong>{project.display_name} · project setup</strong>
          <small>{project.trip?.reason}</small>
        </button>
      ))}
      {livenessItems.map((action, index) => {
        const task = taskForAction(action);
        const projectId = action.binding.project_id;
        return (
          <button
            key={`continuation:${action.kind}:${action.operation}:${index}`}
            disabled={!task && typeof projectId !== "string"}
            onClick={() => {
              if (task) onSelect(task);
              else if (typeof projectId === "string") onOpenSetup(projectId);
            }}
          >
            <strong>{action.kind.replaceAll("_", " ")}</strong>
            <small>
              {action.reason}
              {action.waiting_for ? ` Waiting for ${action.waiting_for}.` : ""}
            </small>
          </button>
        );
      })}
      {!attention.length && !setupAttention.length && !livenessItems.length && (
        <p className="empty">No task needs a human decision.</p>
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
        {error && <small className="error">{error}</small>}
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
