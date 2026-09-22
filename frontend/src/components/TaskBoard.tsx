import { useMemo, useState } from "react";
import { command, operationId } from "../api";
import type { Project, Task } from "../types";

const lanes = [
  "backlog",
  "ready",
  "in_progress",
  "validation",
  "awaiting_review",
  "done",
  "cancelled",
];
const labels: Record<string, string> = {
  backlog: "Backlog",
  ready: "Ready",
  in_progress: "In progress",
  validation: "Validation",
  awaiting_review: "Awaiting review",
  done: "Done",
  cancelled: "Cancelled",
};

export function TaskBoard(
  { tasks, projects, onOpen, onEdit, onChanged }: {
    tasks: Task[];
    projects: Project[];
    onOpen: (task: Task) => void;
    onEdit: (task: Task) => void;
    onChanged: () => void;
  },
) {
  const [query, setQuery] = useState("");
  const [hideDone, setHideDone] = useState(false);
  const [view, setView] = useState<"board" | "list">(() =>
    localStorage.getItem("agenticjira.board.view") === "list" ? "list" : "board"
  );
  const [error, setError] = useState("");
  const filtered = useMemo(
    () =>
      tasks.filter((task) =>
        !task.archived && (!hideDone || task.lifecycle !== "done") &&
        `${task.id} ${task.title} ${task.description}`.toLowerCase().includes(
          query.toLowerCase(),
        )
      ),
    [tasks, query, hideDone],
  );
  const sorted = (items: Task[]) =>
    [...items].sort((left, right) =>
      right.priority - left.priority || left.manual_order - right.manual_order
    );
  const project = (id: string) =>
    projects.find((value) => value.id === id)?.display_name ||
    "Unknown project";
  const chooseView = (value: "board" | "list") => {
    setView(value);
    localStorage.setItem("agenticjira.board.view", value);
  };
  const move = async (task: Task, offset: number) => {
    try {
      await command({
        kind: "update_task",
        operation_id: operationId(),
        task_id: task.id,
        expected_version: task.version,
        title: task.title,
        description: task.description,
        acceptance_criteria: task.acceptance_criteria,
        priority: task.priority,
        manual_order: task.manual_order + offset,
        role_overrides: null,
      });
      setError("");
      onChanged();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  const card = (task: Task) => (
    <article className={`task-card attention-${task.attention}`} key={task.id}>
      <button
        className="card-body"
        onClick={() =>
          onOpen(task)}
      >
        <span className="task-id">
          {task.id} ·{" "}
          {task.priority > 0 ? "High" : task.priority < 0 ? "Low" : "Normal"}
        </span>
        <strong>{task.title}</strong>
        <small>{project(task.project_id)}</small>
        {task.attention !== "none" && (
          <span className="attention">
            {task.attention.replaceAll("_", " ")}
          </span>
        )}
      </button>
      {["backlog", "ready"].includes(task.lifecycle) && (
        <div className="card-actions">
          <button
            aria-label={`Move ${task.id} earlier`}
            onClick={() =>
              move(task, -1)}
          >
            ↑
          </button>
          <button
            aria-label={`Move ${task.id} later`}
            onClick={() => move(task, 1)}
          >
            ↓
          </button>
          <button className="card-edit" onClick={() => onEdit(task)}>
            Edit
          </button>
        </div>
      )}
    </article>
  );

  return (
    <>
      <div className="toolbar">
        <label className="search">
          <span>⌕</span>
          <input
            aria-label="Filter tasks"
            placeholder="Filter tasks"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
        <label className="toggle">
          <input
            type="checkbox"
            checked={hideDone}
            onChange={(event) => setHideDone(event.target.checked)}
          />Hide done
        </label>
        <div className="button-row" aria-label="Task view">
          <button
            className={view === "board" ? "active" : ""}
            onClick={() => chooseView("board")}
          >
            Board
          </button>
          <button
            className={view === "list" ? "active" : ""}
            onClick={() => chooseView("list")}
          >
            List
          </button>
        </div>
      </div>
      {error && <p className="error" role="alert">{error}</p>}
      {view === "board"
        ? (
          <div className="board">
            {lanes.filter((lane) => !hideDone || lane !== "done").map(
              (lane) => {
                const items = sorted(
                  filtered.filter((task) => task.lifecycle === lane),
                );
                return (
                  <section className="lane" key={lane}>
                    <header>
                      <h2>{labels[lane]}</h2>
                      <span>{items.length}</span>
                    </header>
                    {items.length === 0 && <p className="empty">No tasks</p>}
                    {items.map(card)}
                  </section>
                );
              },
            )}
          </div>
        )
        : (
          <div className="task-list">
            {sorted(filtered).map((task) => (
              <article key={task.id}>
                <button onClick={() => onOpen(task)}>
                  <strong>{task.id} · {task.title}</strong>
                  <small>
                    {project(task.project_id)} ·{" "}
                    {labels[task.lifecycle] || task.lifecycle} · order{" "}
                    {task.manual_order}
                  </small>
                </button>
                {["backlog", "ready"].includes(task.lifecycle) && (
                  <div className="button-row">
                    <button
                      onClick={() => move(task, -1)}
                    >
                      Move earlier
                    </button>
                    <button onClick={() => move(task, 1)}>Move later</button>
                    <button onClick={() => onEdit(task)}>Edit</button>
                  </div>
                )}
              </article>
            ))}
          </div>
        )}
    </>
  );
}
