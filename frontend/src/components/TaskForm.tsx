import { ErrorNotice } from "./ErrorNotice";
import { type FormEvent, useEffect, useRef, useState } from "react";
import { ApiError, command, operationId } from "../api";
import { ModelSelector } from "./ModelSelector";
import {
  type Project,
  type Provider,
  type Role,
  type RoleConfig,
  roleLabel,
  ROLES,
  type Task,
} from "../types";

const efforts = ["low", "medium", "high", "xhigh", "max", "ultra"];

export interface TaskDraft {
  project_id: string;
  title: string;
  description: string;
  criteria: string;
  priority: number;
  ready: boolean;
  roles: Partial<Record<Role, TaskRoleDraft>>;
  roleOverrides: Partial<Record<Role, boolean>>;
}

export type TaskRoleDraft = Omit<RoleConfig, "provider"> & {
  provider: Provider | "";
};

type PendingTaskOperation = { body: string; id: string };
type TaskCreateRequest = {
  kind: "create_task";
  project_id: string;
  title: string;
  description: string;
  acceptance_criteria: string[];
  priority: number;
  ready: boolean;
  role_overrides: Partial<Record<Role, RoleConfig>>;
};

const blank = (project = ""): TaskDraft => ({
  project_id: project,
  title: "",
  description: "",
  criteria: "",
  priority: 0,
  ready: false,
  roles: {},
  roleOverrides: {},
});

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const stringField = (
  value: Record<string, unknown>,
  key: string,
  fallback = "",
) => typeof value[key] === "string" ? value[key] : fallback;

const normalizeRole = (value: unknown): TaskRoleDraft | undefined => {
  if (!isRecord(value)) return undefined;
  const provider = value.provider === "claude" || value.provider === "codex" ||
      value.provider === ""
    ? value.provider
    : undefined;
  if (provider === undefined) return undefined;
  return {
    provider,
    model: typeof value.model === "string" ? value.model : "",
    effort: typeof value.effort === "string" ? value.effort : "",
  };
};

const pendingCreateRequest = (
  pending: PendingTaskOperation | undefined,
): TaskCreateRequest | undefined => {
  if (!pending) return undefined;
  try {
    const value: unknown = JSON.parse(pending.body);
    if (
      !isRecord(value) || value.kind !== "create_task" ||
      Object.keys(value).sort().join() !== [
        "kind",
        "project_id",
        "title",
        "description",
        "acceptance_criteria",
        "priority",
        "ready",
        "role_overrides",
      ].sort().join() ||
      typeof value.project_id !== "string" ||
      typeof value.title !== "string" ||
      typeof value.description !== "string" ||
      !Array.isArray(value.acceptance_criteria) ||
      value.acceptance_criteria.some((item) => typeof item !== "string") ||
      typeof value.priority !== "number" || !Number.isFinite(value.priority) ||
      typeof value.ready !== "boolean" || !isRecord(value.role_overrides) ||
      Object.entries(value.role_overrides).some(([role, config]) =>
        !ROLES.some((candidate) => candidate === role) ||
        !isRecord(config) ||
        Object.keys(config).sort().join() !== "effort,model,provider" ||
        !["codex", "claude"].includes(String(config.provider)) ||
        typeof config.model !== "string" || typeof config.effort !== "string"
      )
    ) return undefined;
    return value as TaskCreateRequest;
  } catch {
    return undefined;
  }
};

const normalizeDraft = (value: unknown): TaskDraft | undefined => {
  if (!isRecord(value)) return undefined;
  const storedRoles = isRecord(value.roles) ? value.roles : {};
  const priority = value.priority === -1 || value.priority === 0 ||
      value.priority === 1
    ? value.priority
    : 0;
  return {
    project_id: stringField(value, "project_id"),
    title: stringField(value, "title"),
    description: stringField(value, "description"),
    criteria: stringField(value, "criteria"),
    priority,
    ready: typeof value.ready === "boolean" ? value.ready : false,
    roles: Object.fromEntries(ROLES.flatMap((role) => {
      const config = normalizeRole(storedRoles[role]);
      return config ? [[role, config]] : [];
    })) as Partial<Record<Role, TaskRoleDraft>>,
    roleOverrides: (isRecord(value.roleOverrides)
      ? Object.fromEntries(
        ROLES.map((role) => [
          role,
          value.roleOverrides === undefined
            ? false
            : Boolean((value.roleOverrides as Record<string, unknown>)[role]),
        ]),
      )
      : Object.fromEntries(
        ROLES.map((role) => [role, storedRoles[role] !== undefined]),
      )) as Partial<Record<Role, boolean>>,
  };
};

export function TaskForm(
  {
    projects,
    selectedProjectId,
    editing,
    onSaved,
    onClose,
    onOpenSetup = () => {},
  }: {
    projects: Project[];
    selectedProjectId?: string;
    editing?: Task;
    onSaved: () => void;
    onClose: () => void;
    onOpenSetup?: (projectId: string) => void;
  },
) {
  const key = editing
    ? `agenticjira.edit.${editing.id}`
    : "agenticjira.new-task";
  const [initial] = useState(() => {
    const stored = localStorage.getItem(key);
    if (stored) {
      try {
        const restored = normalizeDraft(JSON.parse(stored));
        if (restored) {
          const selectedIsValid = projects.some((item) =>
            item.id === selectedProjectId
          );
          return {
            draft: {
              ...restored,
              project_id: editing ? editing.project_id : restored.project_id ||
                (selectedIsValid ? selectedProjectId! : ""),
            },
            restored: true,
          };
        }
      } catch { /* use the task or a safe new draft */ }
    }
    if (editing) {
      return {
        draft: {
          project_id: editing.project_id,
          title: editing.title,
          description: editing.description,
          criteria: editing.acceptance_criteria.join("\n"),
          priority: editing.priority,
          ready: editing.lifecycle === "ready",
          roles: Object.fromEntries(
            ROLES.flatMap((role) => {
              const config = normalizeRole(editing.role_overrides[role]);
              return config ? [[role, config]] : [];
            }),
          ) as Partial<Record<Role, TaskRoleDraft>>,
          roleOverrides: Object.fromEntries(
            ROLES.map((role) => [role, !!editing.role_overrides[role]]),
          ) as Partial<Record<Role, boolean>>,
        },
        restored: false,
      };
    }
    const selectedIsValid = projects.some((item) =>
      item.id === selectedProjectId
    );
    return {
      draft: blank(selectedIsValid ? selectedProjectId : projects[0]?.id),
      restored: false,
    };
  });
  const restoredDraft = useRef(initial.restored);
  const saveGuard = useRef(false);
  const pendingOperation = useRef<PendingTaskOperation | undefined>(
    (() => {
      try {
        const value: unknown = JSON.parse(
          localStorage.getItem(`${key}.operation`) || "null",
        );
        return isRecord(value) && typeof value.body === "string" &&
            typeof value.id === "string"
          ? { body: value.body, id: value.id }
          : undefined;
      } catch {
        return undefined;
      }
    })(),
  );
  const [draft, setDraft] = useState<TaskDraft>(initial.draft);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const [unresolvedCreate, setUnresolvedCreate] = useState<
    PendingTaskOperation | undefined
  >(() =>
    !editing && pendingCreateRequest(pendingOperation.current)
      ? pendingOperation.current
      : undefined
  );

  useEffect(() => {
    localStorage.setItem(key, JSON.stringify(draft));
  }, [draft, key]);

  useEffect(() => {
    if (editing) return;
    setDraft((current) => {
      if (current.project_id) return current;
      const selectedIsValid = projects.some((item) =>
        item.id === selectedProjectId
      );
      const nextProject = selectedIsValid
        ? selectedProjectId
        : restoredDraft.current
        ? undefined
        : projects[0]?.id;
      return nextProject ? { ...current, project_id: nextProject } : current;
    });
  }, [editing, projects, selectedProjectId]);

  const update = <K extends keyof TaskDraft>(field: K, value: TaskDraft[K]) => {
    setError("");
    setDraft((current) => ({ ...current, [field]: value }));
  };
  const updateRole = (role: Role, value: TaskRoleDraft) => {
    setError("");
    setDraft((current) => ({
      ...current,
      roles: { ...current.roles, [role]: value },
    }));
  };
  const selectedProject = projects.find((item) => item.id === draft.project_id);
  const inheritedRole = (role: Role): TaskRoleDraft | undefined => {
    const candidate = selectedProject?.settings["roles"];
    const roles = isRecord(candidate) ? candidate : {};
    return normalizeRole(roles[role]);
  };
  const knownExactModels = (provider: Provider | "") => provider
    ? [
      ...ROLES.map((role) => {
        const inherited = inheritedRole(role);
        return inherited?.provider === provider ? inherited.model : "";
      }),
      ...Object.values(draft.roles).map((role) =>
        role?.provider === provider ? role.model : ""
      ),
    ]
    : [];
  const toggleOverride = (role: Role, enabled: boolean) => {
    const inherited = inheritedRole(role);
    setDraft((current) => ({
      ...current,
      roles: enabled && !current.roles[role]
        ? {
          ...current.roles,
          [role]: inherited || { provider: "", model: "", effort: "" },
        }
        : current.roles,
      roleOverrides: { ...current.roleOverrides, [role]: enabled },
    }));
  };

  const projectIsAvailable = projects.some((item) =>
    item.id === draft.project_id
  );
  const missingRole = !editing
    ? ROLES.find((role) => {
      if (!draft.roleOverrides[role]) return false;
      const config = draft.roles[role];
      return !config || !config.provider || !config.model.trim() ||
        !config.effort.trim();
    })
    : undefined;
  const unactivatedOverride = !editing
    ? ROLES.find((role) => {
      if (!draft.roleOverrides[role]) return false;
      const requested = draft.roles[role];
      const activated = inheritedRole(role);
      return !requested || !activated ||
        requested.provider !== activated.provider ||
        requested.model !== activated.model ||
        requested.effort !== activated.effort;
    })
    : undefined;
  const validationReason = (() => {
    if (!editing && !draft.project_id) return "Choose a project.";
    if (!editing && !projectIsAvailable) {
      return "Choose an available project. The saved project is no longer available.";
    }
    if (!draft.title.trim()) return "Enter a task title.";
    if (Array.from(draft.title).length > 200) {
      return "Task titles are limited to 200 characters.";
    }
    if (missingRole) {
      return `Choose a provider, model, and effort for ${
        roleLabel(missingRole)
      }.`;
    }
    return "";
  })();
  const readyReason = validationReason ||
    (selectedProject?.trip?.readiness !== "ready"
      ? selectedProject
        ? `${selectedProject.display_name} is ${
          (selectedProject.trip?.readiness || "not_initialized").replaceAll(
            "_",
            " ",
          )
        }. Initialize or adopt TRIP Explorer before making work Ready.`
        : "Choose a project."
      : unactivatedOverride
      ? `${
        roleLabel(unactivatedOverride)
      } differs from the project default. Save this as a draft, validate its exact ordinary capability, and explicitly activate the task-only profile before making it Ready.`
      : "");
  const missingProject = draft.project_id && !projectIsAvailable;

  const submit = async (ready: boolean) => {
    if (saveGuard.current) return;
    const reason = ready ? readyReason : validationReason;
    if (reason) {
      setError(reason);
      return;
    }
    saveGuard.current = true;
    setSaving(true);
    setError("");
    const criteria = draft.criteria.split("\n").map((value) => value.trim())
      .filter(Boolean);
    let submittedCreate = false;
    try {
      if (editing) {
        const body = {
          kind: "update_task",
          task_id: editing.id,
          expected_version: editing.version,
          title: draft.title,
          description: draft.description,
          acceptance_criteria: criteria,
          priority: draft.priority,
          manual_order: editing.manual_order,
          role_overrides: null,
        };
        const serialized = JSON.stringify(body);
        const id = pendingOperation.current?.body === serialized
          ? pendingOperation.current.id
          : operationId();
        pendingOperation.current = { body: serialized, id };
        localStorage.setItem(
          `${key}.operation`,
          JSON.stringify(pendingOperation.current),
        );
        await command({ ...body, operation_id: id });
      } else {
        const roleOverrides = Object.fromEntries(
          ROLES.flatMap((role) =>
            draft.roleOverrides[role] && draft.roles[role]
              ? [[role, draft.roles[role]]]
              : []
          ),
        );
        const body = {
          kind: "create_task",
          project_id: draft.project_id,
          title: draft.title,
          description: draft.description,
          acceptance_criteria: criteria,
          priority: draft.priority,
          ready,
          role_overrides: roleOverrides,
        };
        const serialized = JSON.stringify(body);
        if (unresolvedCreate && unresolvedCreate.body !== serialized) {
          setError(
            "An earlier task create still has an unknown result. Refresh the board to inspect it or use Retry exact unresolved create before submitting edited fields or a different Ready mode.",
          );
          return;
        }
        const id = pendingOperation.current?.body === serialized
          ? pendingOperation.current.id
          : operationId();
        pendingOperation.current = { body: serialized, id };
        submittedCreate = true;
        localStorage.setItem(
          `${key}.operation`,
          JSON.stringify(pendingOperation.current),
        );
        await command({ ...body, operation_id: id });
      }
      pendingOperation.current = undefined;
      setUnresolvedCreate(undefined);
      localStorage.removeItem(`${key}.operation`);
      localStorage.removeItem(key);
      onSaved();
    } catch (cause) {
      const ambiguous = cause instanceof ApiError && cause.ambiguous;
      if (submittedCreate) {
        if (ambiguous) {
          setUnresolvedCreate(pendingOperation.current);
        } else {
          pendingOperation.current = undefined;
          setUnresolvedCreate(undefined);
          localStorage.removeItem(`${key}.operation`);
        }
      }
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(
        ambiguous && submittedCreate
          ? `${message} The exact create request is retained for explicit reconciliation; edited fields and a different Ready mode cannot be submitted yet.`
          : message,
      );
    } finally {
      saveGuard.current = false;
      setSaving(false);
    }
  };
  const retryUnresolvedCreate = async () => {
    if (saveGuard.current || !unresolvedCreate) return;
    const request = pendingCreateRequest(unresolvedCreate);
    if (!request) {
      setError(
        "The saved unresolved create request is invalid. Leave it unchanged and refresh the task board before taking another create action.",
      );
      return;
    }
    saveGuard.current = true;
    setSaving(true);
    setError("");
    const currentRoleOverrides = Object.fromEntries(
      ROLES.flatMap((role) =>
        draft.roleOverrides[role] && draft.roles[role]
          ? [[role, draft.roles[role]]]
          : []
      ),
    );
    const currentRequest = {
      kind: "create_task",
      project_id: draft.project_id,
      title: draft.title,
      description: draft.description,
      acceptance_criteria: draft.criteria.split("\n").map((value) =>
        value.trim()
      ).filter(Boolean),
      priority: draft.priority,
      ready: request.ready,
      role_overrides: currentRoleOverrides,
    };
    const preserveEditedDraft = JSON.stringify(currentRequest) !==
      unresolvedCreate.body;
    try {
      await command({ ...request, operation_id: unresolvedCreate.id });
      pendingOperation.current = undefined;
      setUnresolvedCreate(undefined);
      localStorage.removeItem(`${key}.operation`);
      if (!preserveEditedDraft) localStorage.removeItem(key);
      onSaved();
    } catch (cause) {
      const ambiguous = cause instanceof ApiError && cause.ambiguous;
      if (!ambiguous) {
        pendingOperation.current = undefined;
        setUnresolvedCreate(undefined);
        localStorage.removeItem(`${key}.operation`);
      }
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(
        ambiguous
          ? `${message} The exact create request remains unresolved and retained for another explicit reconciliation.`
          : message,
      );
    } finally {
      saveGuard.current = false;
      setSaving(false);
    }
  };
  const submitReady = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    void submit(editing ? draft.ready : true);
  };

  return (
    <div className="modal-backdrop">
      <form
        className="modal"
        aria-labelledby="task-form-title"
        aria-busy={saving}
        onSubmit={submitReady}
      >
        <header>
          <div>
            <span className="eyebrow">
              {editing ? "Edit queued task" : "New task"}
            </span>
            <h2 id="task-form-title">
              {editing ? editing.title : "Describe the work"}
            </h2>
          </div>
          <button
            className="icon"
            type="button"
            aria-label="Close task form"
            disabled={saving}
            onClick={onClose}
          >
            ×
          </button>
        </header>
        <div className="form-grid">
          <label>
            Project
            <select
              value={draft.project_id}
              disabled={!!editing || saving}
              onChange={(event) => update("project_id", event.target.value)}
            >
              <option value="">Choose a project</option>
              {missingProject && (
                <option value={draft.project_id} disabled>
                  Choose an available project (saved project unavailable)
                </option>
              )}
              {projects.map((project) => (
                <option key={project.id} value={project.id}>
                  {project.display_name}
                </option>
              ))}
            </select>
          </label>
          <label>
            Priority
            <select
              value={draft.priority}
              disabled={saving}
              onChange={(event) =>
                update("priority", Number(event.target.value))}
            >
              <option value={-1}>Low</option>
              <option value={0}>Normal</option>
              <option value={1}>High</option>
            </select>
          </label>
        </div>
        <label>
          Title
          <input
            autoFocus
            value={draft.title}
            maxLength={200}
            disabled={saving}
            onChange={(event) => update("title", event.target.value)}
          />
        </label>
        <label>
          Description
          <textarea
            rows={5}
            value={draft.description}
            disabled={saving}
            onChange={(event) => update("description", event.target.value)}
          />
        </label>
        <label>
          Acceptance criteria <small>One line per criterion</small>
          <textarea
            rows={5}
            value={draft.criteria}
            disabled={saving}
            onChange={(event) => update("criteria", event.target.value)}
          />
        </label>
        {editing
          ? (
            <p className="hint">
              Change provider, model, or effort in Role settings. Saving this
              form changes only the queued task fields above. The project stays
              fixed for this versioned update.
            </p>
          )
          : (
            <details>
              <summary>Role overrides</summary>
              <div className="role-grid">
                {ROLES.map((role) => {
                  const inherited = inheritedRole(role);
                  const config = draft.roles[role] || inherited ||
                    (draft.roleOverrides[role]
                      ? { provider: "" as const, model: "", effort: "" }
                      : undefined);
                  return (
                    <fieldset key={role} disabled={saving}>
                      <legend>
                        <label className="toggle">
                          <input
                            type="checkbox"
                            checked={!!draft.roleOverrides[role]}
                            onChange={(event) =>
                              toggleOverride(role, event.target.checked)}
                          />Override {roleLabel(role)}
                        </label>
                      </legend>
                      {!draft.roleOverrides[role] && (
                        <small>
                          {inherited
                            ? `Inherited: ${inherited.provider} · ${inherited.model} · ${inherited.effort}`
                            : "No activated project profile yet; drafts remain allowed."}
                        </small>
                      )}
                      {draft.roleOverrides[role] && config && (
                        <>
                          <select
                            aria-label={`${roleLabel(role)} provider`}
                            value={config.provider}
                            onChange={(event) =>
                              updateRole(role, {
                                ...config,
                                provider: event.target.value as
                                  | "codex"
                                  | "claude",
                              })}
                          >
                            <option value="">Choose provider</option>
                            <option value="codex">Codex</option>
                            <option value="claude">Claude</option>
                          </select>
                          <ModelSelector
                            provider={config.provider}
                            value={config.model}
                            onChange={(model) =>
                              updateRole(role, { ...config, model })}
                            label={`${roleLabel(role)} model`}
                            knownExactModels={knownExactModels(
                              config.provider,
                            )}
                            disabled={saving}
                          />
                          <select
                            aria-label={`${roleLabel(role)} effort`}
                            value={config.effort}
                            onChange={(event) =>
                              updateRole(role, {
                                ...config,
                                effort: event.target.value,
                              })}
                          >
                            {!efforts.includes(config.effort) && (
                              <option value={config.effort}>
                                {config.effort || "Choose effort"}
                              </option>
                            )}
                            {efforts.map((effort) => (
                              <option key={effort} value={effort}>
                                {effort}
                              </option>
                            ))}
                          </select>
                        </>
                      )}
                    </fieldset>
                  );
                })}
              </div>
            </details>
          )}
        {unresolvedCreate && (
          <div className="warning" role="status">
            An earlier create may already be committed. Refresh and inspect the
            task board, or reconcile only that saved request with its original
            operation identity. Current form edits remain saved separately.
            <button
              type="button"
              disabled={saving}
              onClick={() => void retryUnresolvedCreate()}
            >
              Retry exact unresolved create
            </button>
          </div>
        )}
        {error && (
          <div>
            <ErrorNotice error={error} />
            <small>Your draft is still here.</small>
          </div>
        )}
        <p
          className={`form-status${validationReason ? " invalid" : ""}`}
          id="task-form-status"
        >
          {saving
            ? "Saving the task…"
            : (editing ? validationReason : readyReason) || (editing
              ? "Save changes updates this queued task at its current version."
              : "Ready uses the activated project defaults or exact task-only profiles explicitly activated with current ordinary capability evidence.")}
        </p>
        <footer>
          <button type="button" disabled={saving} onClick={onClose}>
            Cancel
          </button>
          {!editing && (
            <button
              type="button"
              disabled={saving}
              aria-describedby="task-form-status"
              onClick={() => void submit(false)}
            >
              Save draft
            </button>
          )}
          {!editing && selectedProject &&
            selectedProject.trip?.readiness !== "ready" && (
            <button
              type="button"
              disabled={saving}
              onClick={() => onOpenSetup(selectedProject.id)}
            >
              Open project setup
            </button>
          )}
          <button
            className="primary"
            type="submit"
            aria-describedby="task-form-status"
            disabled={saving || !!(editing ? validationReason : readyReason)}
            title={(editing ? validationReason : readyReason) || undefined}
          >
            {saving
              ? "Saving…"
              : editing
              ? "Save changes"
              : "Create Ready task"}
          </button>
        </footer>
      </form>
    </div>
  );
}
