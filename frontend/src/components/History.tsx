import { ErrorNotice } from "./ErrorNotice";
import { useRef, useState } from "react";
import { command, operationId } from "../api";
import type { Project, Task } from "../types";
import { HistorySearch } from "./HistorySearch";
import { UsageComparisons } from "./UsageComparisons";
export function History(
  { tasks, projects, projectId, incarnation, onOpen, onChanged }: {
    tasks: Task[];
    projects: Project[];
    projectId?: string;
    incarnation: string;
    onOpen: (task: Task) => void;
    onChanged: () => void;
  },
) {
  const [showArchived, setShowArchived] = useState(false);
  const [view, setView] = useState<"tasks" | "search" | "usage">("tasks");
  const [error, setError] = useState("");
  const operationIds = useRef(new Map<string, string>());
  const insightsKey = JSON.stringify([incarnation, projectId]);
  const toggle = async (task: Task) => {
    const key = `${task.archived ? "restore" : "archive"}:${task.id}:${task.version}`;
    const id = operationIds.current.get(key) || operationId();
    operationIds.current.set(key, id);
    try {
      await command({
        kind: task.archived ? "restore" : "archive",
        operation_id: id,
        task_id: task.id,
        expected_version: task.version,
      });
      operationIds.current.delete(key);
      onChanged();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  const rows = tasks.filter((t) =>
    (["done", "cancelled"].includes(t.lifecycle) &&
      (showArchived || !t.archived)) ||
    (t.lifecycle === "backlog" && t.archived)
  );
  return (
    <section className="history">
      <header className="page-heading">
        <div>
          <span className="eyebrow">History</span>
          <h1>{view === "tasks" ? "Completed and archived tasks" : view === "search" ? "Search history" : "Workflow and provider comparisons"}</h1>
          <p>
            {view === "tasks"
              ? "Accepted, cancelled and archived tasks. Their results and evidence stay available here."
              : view === "search"
              ? "Find saved text across projects, tasks and attempts. Open an exact source to save a knowledge draft with its original provenance."
              : "Compare saved task and run outcomes, execution evidence and observed session usage."}
          </p>
        </div>
        {view === "tasks" && <label className="toggle">
          <input
            type="checkbox"
            checked={showArchived}
            onChange={(e) => setShowArchived(e.target.checked)}
          />Show archived
        </label>}
      </header>
      <nav className="history-views" aria-label="History views">
        <button aria-pressed={view === "tasks"} onClick={() => setView("tasks")}>Tasks</button>
        <button aria-pressed={view === "search"} onClick={() => setView("search")}>Search</button>
        <button aria-pressed={view === "usage"} onClick={() => setView("usage")}>Observed usage and comparisons</button>
      </nav>
      {view === "search" && <HistorySearch key={insightsKey} projects={projects} initialProjectId={projectId} tasks={tasks} onOpen={onOpen} />}
      {view === "usage" && <UsageComparisons key={insightsKey} projects={projects} initialProjectId={projectId} tasks={tasks} onOpen={onOpen} />}
      {error && <ErrorNotice error={error} />}
      {view === "tasks" && <div className="history-list">
        {!rows.length && (
          <p className="empty">No completed or archived tasks yet.</p>
        )}
        {rows.map((task) => (
          <article key={task.id}>
            <button onClick={() => onOpen(task)}>
              <strong>{task.title}</strong>
              <small>
                {projects.find((p) => p.id === task.project_id)?.display_name}
                {" · "}
                {task.lifecycle === "done"
                  ? "Completed"
                  : task.lifecycle === "cancelled"
                  ? "Cancelled"
                  : "Draft"}
              </small>
              <small className="task-key">{task.id}</small>
            </button>
            <span>{task.archived ? "Archived" : "Shown in Completed"}</span>
            <button disabled={!task.archived && !task.can_archive} onClick={() => toggle(task)}>
              {task.archived ? "Restore" : "Archive"}
            </button>
          </article>
        ))}
      </div>}
    </section>
  );
}
