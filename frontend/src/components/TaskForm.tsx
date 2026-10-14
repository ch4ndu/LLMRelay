import { ErrorNotice } from "./ErrorNotice";
import { type FormEvent, useEffect, useRef, useState } from "react";
import { ApiError, command, operationId, submitWorkflowCommand } from "../api";
import { ModelSelector } from "./ModelSelector";
import { UncertainRequestNotice, useWorkflowCommands } from "./WorkflowControls";
import {
  emptySingleAgentSelection,
  type SingleAgentSelection,
  SingleAgentSelectionFields,
} from "./RoleSettings";
import {
  type Project,
  type ProjectExecution,
  type Provider,
  type Role,
  type RoleConfig,
  roleLabel,
  ROLES,
  type Task,
  type WorkflowFamily,
  type WorkspaceMode,
} from "../types";

const efforts = ["low", "medium", "high", "xhigh", "max", "ultra"];

export interface TaskDraft {
  project_id: string;
  title: string;
  description: string;
  criteria: string;
  priority: number;
  ready: boolean;
  workspace_mode: WorkspaceMode;
  roles: Partial<Record<Role, TaskRoleDraft>>;
  roleOverrides: Partial<Record<Role, boolean>>;
}

export type TaskRoleDraft = Omit<RoleConfig, "provider"> & {
  provider: Provider | "";
};

type PendingTaskOperation = { body: string; id: string };

const removeStoredValue = (key: string, expectedValue: string | undefined) => {
  if (expectedValue !== undefined && localStorage.getItem(key) === expectedValue) {
    localStorage.removeItem(key);
  }
};

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
type WorkflowCreateRequest = {
  kind: "create_task";
  project_id: string;
  title: string;
  description: string;
  acceptance_criteria: string[];
  priority: number;
  selection:
    | ({ family: "single_agent"; workspace_mode?: WorkspaceMode } & SingleAgentSelection)
    | { family: "trip"; workspace_mode: "main_checkout" };
};

const blank = (project = ""): TaskDraft => ({
  project_id: project,
  title: "",
  description: "",
  criteria: "",
  priority: 0,
  ready: false,
  workspace_mode: "isolated_worktree",
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

const isStringList = (value: unknown): value is string[] =>
  Array.isArray(value) && value.every((item) => typeof item === "string");

const pendingWorkflowCreate = (
  value: Record<string, unknown>,
): WorkflowCreateRequest | undefined => {
  const selection = value.selection;
  if (
    value.kind !== "create_task" ||
    Object.keys(value).sort().join() !==
      "acceptance_criteria,description,kind,priority,project_id,selection,title" ||
    typeof value.project_id !== "string" || typeof value.title !== "string" ||
    typeof value.description !== "string" ||
    !isStringList(value.acceptance_criteria) ||
    typeof value.priority !== "number" || !Number.isFinite(value.priority) ||
    !isRecord(selection)
  ) return undefined;
  if (
    selection.workspace_mode !== undefined &&
    selection.workspace_mode !== "isolated_worktree" &&
    selection.workspace_mode !== "main_checkout"
  ) return undefined;
  let parsedSelection: WorkflowCreateRequest["selection"];
  if (selection.family === "trip") {
    if (selection.workspace_mode !== "main_checkout" ||
      Object.keys(selection).sort().join() !== "family,workspace_mode") return undefined;
    parsedSelection = { family: "trip", workspace_mode: "main_checkout" };
  } else if (selection.family === "single_agent" &&
    Object.keys(selection).sort().join() ===
      (selection.workspace_mode === undefined
        ? "check_ids,execution_config_revision_id,family,profile_revision_id"
        : "check_ids,execution_config_revision_id,family,profile_revision_id,workspace_mode") &&
    typeof selection.execution_config_revision_id === "string" &&
    typeof selection.profile_revision_id === "string" &&
    isStringList(selection.check_ids)) {
    parsedSelection = {
      family: "single_agent",
      execution_config_revision_id: selection.execution_config_revision_id,
      profile_revision_id: selection.profile_revision_id,
      check_ids: selection.check_ids,
      ...(selection.workspace_mode === undefined ? {} : { workspace_mode: selection.workspace_mode }),
    };
  } else return undefined;
  return {
    kind: "create_task",
    project_id: value.project_id,
    title: value.title,
    description: value.description,
    acceptance_criteria: value.acceptance_criteria,
    priority: value.priority,
    selection: parsedSelection,
  };
};

const pendingCreateRequest = (
  pending: PendingTaskOperation | undefined,
): TaskCreateRequest | WorkflowCreateRequest | undefined => {
  if (!pending) return undefined;
  try {
    const value: unknown = JSON.parse(pending.body);
    if (isRecord(value) && "selection" in value) {
      return pendingWorkflowCreate(value);
    }
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
  if (value.workspace_mode !== undefined && value.workspace_mode !== "isolated_worktree" &&
    value.workspace_mode !== "main_checkout") return undefined;
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
    workspace_mode: value.workspace_mode === "main_checkout" ? "main_checkout" : "isolated_worktree",
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
    workflowExecution,
  }: {
    projects: Project[];
    selectedProjectId?: string;
    editing?: Task;
    onSaved: () => void;
    onClose: () => void;
    onOpenSetup?: (projectId: string) => void;
    /**
     * Each project's Single agent setup and default workflow. Without it the
     * form creates TRIP tasks only, as older services do.
     */
    workflowExecution?: ProjectExecution[];
  },
) {
  const key = editing
    ? `agenticjira.edit.${editing.id}`
    : "agenticjira.new-task";
  const [initial] = useState<{ draft: TaskDraft; restored: boolean }>(() => {
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
              ...(editing ? { workspace_mode: editing.workflow?.workspace_mode ?? "isolated_worktree" } : {}),
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
          workspace_mode: editing.workflow?.workspace_mode ?? "isolated_worktree",
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
  const workspaceCommands = useWorkflowCommands(`llmrelay.checkout.operations.${editing?.id ?? "new"}`, () => {});
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
  const execution = workflowExecution?.find((item) =>
    item.project_id === draft.project_id
  );
  // Undefined follows the project's default; a choice is reset per project.
  const [chosenFamily, setChosenFamily] = useState<
    { project_id: string; family: WorkflowFamily } | undefined
  >(undefined);
  const family: WorkflowFamily = editing || !workflowExecution
    ? "trip"
    : chosenFamily?.project_id === draft.project_id
    ? chosenFamily.family
    : execution?.default_family ?? "trip";
  const singleAgent = family === "single_agent";
  const mainCheckout = draft.workspace_mode === "main_checkout";
  const [singleAgentChoice, setSingleAgentChoice] = useState<
    { project_id: string; selection: SingleAgentSelection } | undefined
  >(undefined);
  const singleAgentSelection =
    singleAgentChoice?.project_id === draft.project_id
      ? singleAgentChoice.selection
      : emptySingleAgentSelection(execution);
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
  const missingRole = !editing && !singleAgent
    ? ROLES.find((role) => {
      if (!draft.roleOverrides[role]) return false;
      const config = draft.roles[role];
      return !config || !config.provider || !config.model.trim() ||
        !config.effort.trim();
    })
    : undefined;
  const unactivatedOverride = !editing && !singleAgent
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
    if (editing && draft.workspace_mode !== (editing.workflow?.workspace_mode ?? "isolated_worktree")) {
      return "Save workspace mode only before saving task fields, or restore the current workspace choice.";
    }
    if (missingRole) {
      return `Choose a provider, model, and effort for ${
        roleLabel(missingRole)
      }.`;
    }
    if (!editing && mainCheckout && !singleAgent &&
      ROLES.some((role) => draft.roleOverrides[role])) {
      return "Main checkout drafts use the project role settings. Turn off these overrides, then configure task-specific roles in the saved task's Roles tab.";
    }
    if (
      singleAgent &&
      (!singleAgentSelection.execution_config_revision_id ||
        !singleAgentSelection.profile_revision_id)
    ) {
      return "Choose an execution configuration and an agent profile for the Single agent.";
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
  const workflowCreate = (criteria: string[]): WorkflowCreateRequest => ({
    kind: "create_task",
    project_id: draft.project_id,
    title: draft.title,
    description: draft.description,
    acceptance_criteria: criteria,
    priority: draft.priority,
    selection: singleAgent
      ? { family: "single_agent", ...singleAgentSelection,
        ...(mainCheckout ? { workspace_mode: "main_checkout" as const } : {}) }
      : { family: "trip", workspace_mode: "main_checkout" },
  });

  const submit = async (requestedReady: boolean) => {
    if (saveGuard.current || workspaceCommands.busy) return;
    const ready = requestedReady && !singleAgent && !mainCheckout;
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
    let submittedOperation: PendingTaskOperation | undefined;
    let submittedOperationRecord: string | undefined;
    try {
      const submittedDraft = JSON.stringify(draft);
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
        submittedOperation = { body: serialized, id };
        pendingOperation.current = submittedOperation;
        submittedOperationRecord = JSON.stringify(submittedOperation);
        localStorage.setItem(
          `${key}.operation`,
          submittedOperationRecord,
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
        const body: TaskCreateRequest | WorkflowCreateRequest = singleAgent || mainCheckout
          ? workflowCreate(criteria)
          : {
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
        submittedOperation = { body: serialized, id };
        pendingOperation.current = submittedOperation;
        submittedCreate = true;
        submittedOperationRecord = JSON.stringify(submittedOperation);
        localStorage.setItem(
          `${key}.operation`,
          submittedOperationRecord,
        );
        if ("selection" in body) {
          await submitWorkflowCommand({ ...body, operation_id: id });
        } else {
          await command({ ...body, operation_id: id });
        }
      }
      pendingOperation.current = undefined;
      setUnresolvedCreate(undefined);
      removeStoredValue(`${key}.operation`, submittedOperationRecord);
      removeStoredValue(key, submittedDraft);
      onSaved();
    } catch (cause) {
      const ambiguous = cause instanceof ApiError && cause.ambiguous;
      if (submittedCreate) {
        if (ambiguous) {
          setUnresolvedCreate(submittedOperation);
        } else {
          pendingOperation.current = undefined;
          setUnresolvedCreate(undefined);
          removeStoredValue(`${key}.operation`, submittedOperationRecord);
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
    const submittedOperation = unresolvedCreate;
    const request = pendingCreateRequest(submittedOperation);
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
    const currentCriteria = draft.criteria.split("\n").map((value) =>
      value.trim()
    ).filter(Boolean);
    const currentRequest = "selection" in request
      ? workflowCreate(currentCriteria)
      : {
        kind: "create_task",
        project_id: draft.project_id,
        title: draft.title,
        description: draft.description,
        acceptance_criteria: currentCriteria,
        priority: draft.priority,
        ready: request.ready,
        role_overrides: currentRoleOverrides,
      };
    const preserveEditedDraft = JSON.stringify(currentRequest) !==
      submittedOperation.body;
    let submittedOperationRecord: string | undefined;
    try {
      const submittedDraft = JSON.stringify(draft);
      submittedOperationRecord = JSON.stringify(submittedOperation);
      if ("selection" in request) {
        await submitWorkflowCommand({
          ...request,
          operation_id: submittedOperation.id,
        });
      } else {
        await command({ ...request, operation_id: submittedOperation.id });
      }
      pendingOperation.current = undefined;
      setUnresolvedCreate(undefined);
      removeStoredValue(`${key}.operation`, submittedOperationRecord);
      if (!preserveEditedDraft) removeStoredValue(key, submittedDraft);
      onSaved();
    } catch (cause) {
      const ambiguous = cause instanceof ApiError && cause.ambiguous;
      if (!ambiguous) {
        pendingOperation.current = undefined;
        setUnresolvedCreate(undefined);
        removeStoredValue(`${key}.operation`, submittedOperationRecord);
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
  const saveWorkspace = async () => {
    if (!editing) return;
    const result = await workspaceCommands.submit("workspace", (operation_id) => ({
      kind: "set_task_workflow", operation_id, task_id: editing.id,
      expected_version: editing.version,
      selection: {
        family: editing.workflow?.family ?? "trip",
        workspace_mode: draft.workspace_mode,
        ...(editing.workflow?.family === "single_agent" ? {
          execution_config_revision_id: editing.workflow.execution_config_revision_id,
          profile_revision_id: editing.workflow.profile_revision_id,
          check_ids: editing.workflow.check_ids,
        } : {}),
      },
    }));
    if (result) onSaved();
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
        {!editing && workflowExecution && (
          <fieldset className="workflow-choice" disabled={saving}>
            <legend>Workflow</legend>
            <div role="radiogroup" aria-label="Workflow">
              {(["trip", "single_agent"] as const).map((option) => (
                <label className="toggle" key={option}>
                  <input
                    type="radio"
                    name="task-workflow"
                    checked={family === option}
                    onChange={() =>
                      setChosenFamily({
                        project_id: draft.project_id,
                        family: option,
                      })}
                  />
                  {option === "trip"
                    ? "TRIP (manager and five delegated roles)"
                    : "Single agent (one agent writes the whole task)"}
                </label>
              ))}
            </div>
            <small>
              Project default:{" "}
              {execution?.default_family === "single_agent"
                ? "Single agent"
                : "TRIP"}
            </small>
          </fieldset>
        )}
        {workflowExecution && (!editing || (!editing.active_attempt && ["backlog", "ready"].includes(editing.lifecycle))) && (
          <fieldset disabled={saving || workspaceCommands.busy}>
            <legend>Workspace</legend>
            <div role="radiogroup" aria-label="Workspace">
              {(["isolated_worktree", "main_checkout"] as const).map((mode) => (
                <label className="toggle" key={mode}>
                  <input
                    type="radio"
                    name="task-workspace"
                    checked={draft.workspace_mode === mode}
                    onChange={() => update("workspace_mode", mode)}
                  />
                  {mode === "main_checkout" ? "Main checkout" : "Isolated worktree"}
                </label>
              ))}
            </div>
            <p className="hint">
              Main checkout lets the writer edit your selected checkout, including
              existing files and uncommitted work. Isolated worktree uses a separate
              app-owned checkout. Main tasks start as drafts; review the canonical
              path and authorize access with Make Ready in the task's Overview.
            </p>
            {editing && <>
              <button type="button" disabled={draft.workspace_mode === (editing.workflow?.workspace_mode ?? "isolated_worktree")}
                onClick={() => void saveWorkspace()}>Save workspace mode only</button>
              <small>This saves the workspace choice at the task's current version and returns a Ready task to drafts. Task field edits stay in this form's saved draft.</small>
              {workspaceCommands.uncertain.has("workspace") && <UncertainRequestNotice changed={workspaceCommands.reviewed.has("workspace")} />}
            </>}
          </fieldset>
        )}
        {editing
          ? (
            <p className="hint">
              Change the workflow, provider, model, or effort in the task's
              Roles tab. Saving this form changes only the queued task fields
              above. The project stays fixed for this versioned update.
              {" "}Workspace: {draft.workspace_mode === "main_checkout" ? "Main checkout" : "Isolated worktree"}.
              Change it above or in Overview before an attempt starts; that change requires
              Make Ready again.
              {editing.workflow?.family === "single_agent" &&
                editing.lifecycle === "ready" &&
                " Saving changes to a Ready Single agent task moves it back to the drafts; choose Make Ready again afterwards."}
            </p>
          )
          : singleAgent
          ? (
            <SingleAgentSelectionFields
              execution={execution}
              value={singleAgentSelection}
              onChange={(selection) =>
                setSingleAgentChoice({ project_id: draft.project_id, selection })}
              disabled={saving}
            />
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
        {workspaceCommands.error && <ErrorNotice error={workspaceCommands.error} />}
        <p
          className={`form-status${validationReason ? " invalid" : ""}`}
          id="task-form-status"
        >
          {saving
            ? "Saving the task…"
            : editing
            ? validationReason || "Save changes updates task fields at the current version and preserves its saved workspace mode."
            : mainCheckout
            ? validationReason || "Save this Main checkout task as a draft, then authorize its current checkout in Overview."
            : singleAgent
            ? validationReason ||
              "Single agent tasks are saved as drafts. Make one Ready from the task once its agent profile is admitted."
            : readyReason || "Ready uses the activated project defaults or exact task-only profiles explicitly activated with current ordinary capability evidence."}
        </p>
        <footer>
          <button type="button" disabled={saving} onClick={onClose}>
            Cancel
          </button>
          {!editing && (
            <button
              type="button"
              className={singleAgent || mainCheckout ? "primary" : undefined}
              disabled={saving || ((singleAgent || mainCheckout) && !!validationReason)}
              aria-describedby="task-form-status"
              onClick={() => void submit(false)}
            >
              Save draft
            </button>
          )}
          {!editing && !singleAgent && selectedProject &&
            selectedProject.trip?.readiness !== "ready" && (
            <button
              type="button"
              disabled={saving}
              onClick={() => onOpenSetup(selectedProject.id)}
            >
              Open project setup
            </button>
          )}
          {(editing || (!singleAgent && !mainCheckout)) && (
            <button
              className="primary"
              type="submit"
              aria-describedby="task-form-status"
              disabled={saving || workspaceCommands.busy || !!(editing ? validationReason : readyReason)}
              title={(editing ? validationReason : readyReason) || undefined}
            >
              {saving
                ? "Saving…"
                : editing
                ? "Save changes"
                : "Create Ready task"}
            </button>
          )}
        </footer>
      </form>
    </div>
  );
}
