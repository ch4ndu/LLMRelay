import { type FormEvent, useEffect, useRef, useState } from "react";
import { command, operationId } from "../api";
import type { Project } from "../types";
export function ProjectPicker(
  { projects, selected, onSelect, onChanged, onAdded }: {
    projects: Project[];
    selected?: string;
    onSelect: (id: string | undefined) => void;
    onChanged: () => Promise<void> | void;
    onAdded?: (id: string) => void;
  },
) {
  const [adding, setAdding] = useState(false);
  const [path, setPath] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [pendingSelection, setPendingSelection] = useState<string>();
  const submitGuard = useRef(false);
  const pendingOperation = useRef<{ body: string; id: string } | undefined>(
    (() => {
      try {
        return JSON.parse(
          localStorage.getItem("llmrelay.add-project.operation") || "null",
        ) || undefined;
      } catch {
        return undefined;
      }
    })(),
  );
  useEffect(() => {
    if (
      pendingSelection &&
      projects.some((project) => project.id === pendingSelection)
    ) {
      onSelect(pendingSelection);
      setPendingSelection(undefined);
    }
  }, [onSelect, pendingSelection, projects]);
  const closeForm = () => {
    if (submitGuard.current) return;
    setAdding(false);
    setError("");
  };
  const add = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (submitGuard.current) return;
    const displayName = name.trim();
    const repositoryPath = path.trim();
    if (!displayName) {
      setError("Project name is required.");
      return;
    }
    if (!repositoryPath) {
      setError("Repository folder is required.");
      return;
    }
    if (!repositoryPath.startsWith("/")) {
      setError("Repository folder must be an absolute path.");
      return;
    }
    submitGuard.current = true;
    setSubmitting(true);
    setError("");
    try {
      const body = JSON.stringify({
        path: repositoryPath,
        display_name: displayName,
      });
      const operation = pendingOperation.current?.body === body
        ? pendingOperation.current.id
        : operationId();
      pendingOperation.current = { body, id: operation };
      localStorage.setItem(
        "llmrelay.add-project.operation",
        JSON.stringify(pendingOperation.current),
      );
      const response = await command({
        kind: "add_project",
        operation_id: operation,
        path: repositoryPath,
        display_name: displayName,
      });
      const entityId = typeof response.result.entity_id === "string"
        ? response.result.entity_id
        : undefined;
      setPendingSelection(entityId);
      await onChanged();
      pendingOperation.current = undefined;
      localStorage.removeItem("llmrelay.add-project.operation");
      if (entityId) onAdded?.(entityId);
      setAdding(false);
      setPath("");
      setName("");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      submitGuard.current = false;
      setSubmitting(false);
    }
  };
  return (
    <section className="projects">
      <div className="section-title">
        <span>Projects</span>
        <button
          className="add-project-toggle"
          aria-expanded={adding}
          aria-controls="add-project-form"
          onClick={() => adding ? closeForm() : setAdding(true)}
        >
          {adding ? "Close" : "Add project"}
        </button>
      </div>
      <button
        className={!selected ? "project selected" : "project"}
        onClick={() => onSelect(undefined)}
      >
        <span>All projects</span>
        <small>{projects.length} repositories</small>
      </button>
      {projects.map((project) => (
        <button
          key={project.id}
          className={selected === project.id ? "project selected" : "project"}
          onClick={() => onSelect(project.id)}
        >
          <span>{project.display_name}</span>
          <small>
            {(project.trip?.readiness || "not_initialized").replaceAll(
              "_",
              " ",
            )} · {project.queue_paused
              ? "queue paused"
              : project.base_revision.slice(0, 8)}
          </small>
        </button>
      ))}
      {adding && (
        <form
          className="inline-form"
          id="add-project-form"
          aria-busy={submitting}
          onSubmit={add}
        >
          <label>
            Project name
            <input
              required
              placeholder="e.g. JellyScope"
              value={name}
              disabled={submitting}
              onChange={(event) => {
                setName(event.target.value);
                setError("");
              }}
            />
          </label>
          <label>
            Repository folder
            <input
              required
              aria-describedby="repository-folder-hint"
              placeholder="/absolute/path/to/repository"
              value={path}
              disabled={submitting}
              onChange={(event) => {
                setPath(event.target.value);
                setError("");
              }}
            />
          </label>
          <small className="field-hint" id="repository-folder-hint">
            Use the absolute path to an existing local Git folder.
          </small>
          {path && (
            <output className="path-preview" title={path}>
              <span>Full path:</span> {path}
            </output>
          )}
          {error && <small className="error" role="alert">{error}</small>}
          <div className="inline-form-actions">
            <button type="button" disabled={submitting} onClick={closeForm}>
              Cancel
            </button>
            <button className="primary" type="submit" disabled={submitting}>
              {submitting ? "Checking repository…" : "Add project"}
            </button>
          </div>
          {submitting && (
            <small className="field-hint">
              Checking the folder before adding it.
            </small>
          )}
        </form>
      )}
    </section>
  );
}
