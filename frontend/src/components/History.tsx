import { ErrorNotice } from "./ErrorNotice";
import { useRef, useState } from "react";
import { command, operationId } from "../api";
import type { Project, Task } from "../types";
export function History(
  { tasks, projects, onOpen, onChanged }: {
    tasks: Task[];
    projects: Project[];
    onOpen: (task: Task) => void;
    onChanged: () => void;
  },
) {
  const [showArchived, setShowArchived] = useState(false);
  const [error, setError] = useState("");
  const operationIds = useRef(new Map<string, string>());
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
    (t.lifecycle === "done" && (showArchived || !t.archived)) ||
    (t.lifecycle === "backlog" && t.archived)
  );
  return (
    <section className="history">
      <header className="page-heading">
        <div>
          <span className="eyebrow">Completed tasks and archived drafts</span>
          <h1>History and archive</h1>
          <p>Accepted results and evidence stay available when hidden.</p>
        </div>
        <label className="toggle">
          <input
            type="checkbox"
            checked={showArchived}
            onChange={(e) => setShowArchived(e.target.checked)}
          />Show archived
        </label>
      </header>
      {error && <ErrorNotice error={error} />}
      <div className="history-list">
        {rows.map((task) => (
          <article key={task.id}>
            <button onClick={() => onOpen(task)}>
              <strong>{task.id} · {task.title}</strong>
              <small>
                {projects.find((p) => p.id === task.project_id)?.display_name} ·
                {" "}
                {task.active_attempt?.candidate_hash?.slice(0, 12)}
              </small>
            </button>
            <span>{task.archived ? "Archived" : "Visible"}</span>
            <button disabled={!task.archived && !task.can_archive} onClick={() => toggle(task)}>
              {task.archived ? "Restore" : "Archive"}
            </button>
          </article>
        ))}
      </div>
    </section>
  );
}
