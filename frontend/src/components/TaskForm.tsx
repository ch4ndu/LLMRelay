import { ErrorNotice } from "./ErrorNotice";
import { type FormEvent, useEffect, useRef, useState } from "react";
import { ApiError, command, operationId, submitWorkflowCommand } from "../api";
import { ModelSelector } from "./ModelSelector";
import { useDialogFocus } from "../useDialogFocus";
import { UncertainRequestNotice, useWorkflowCommands } from "./WorkflowControls";
import { StorageLockUnavailableError, withStorageLock } from "../storageCoordination";
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
  workflowChoice?: { project_id: string; family: WorkflowFamily };
  singleAgentChoice?: { project_id: string; selection: SingleAgentSelection };
}

export type TaskRoleDraft = Omit<RoleConfig, "provider"> & {
  provider: Provider | "";
};

type PendingTaskOperation = { body: string; id: string };
type CompletedTaskSave = {
  operation?: PendingTaskOperation;
  draft: TaskDraft;
  storedDraft?: string;
  preserveDraft: boolean;
};
type DraftRecovery =
  | { status: "unread" }
  | { status: "loaded"; raw: string; draft: TaskDraft };

function DraftText({ draft }: { draft: TaskDraft }) {
  return (
    <dl className="draft-text-preview">
      <dt>Title</dt>
      <dd>{draft.title || "Untitled"}</dd>
      <dt>Description</dt>
      <dd>{draft.description || "Empty"}</dd>
      <dt>Acceptance criteria</dt>
      <dd>{draft.criteria || "Empty"}</dd>
    </dl>
  );
}

const removeStoredValue = (key: string, expectedValue: string | undefined): boolean => {
  try {
    const stored = localStorage.getItem(key);
    if (stored === null) return true;
    if (expectedValue !== undefined && stored === expectedValue) {
      localStorage.removeItem(key);
      return true;
    }
  } catch { /* A retained operation remains safe to reconcile after successful dispatch. */ }
  return false;
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
type TaskEditRequest = {
  kind: "update_task";
  task_id: string;
  expected_version: number;
  title: string;
  description: string;
  acceptance_criteria: string[];
  priority: number;
  manual_order: number;
  role_overrides: null;
};
type TaskRequest = TaskCreateRequest | WorkflowCreateRequest | TaskEditRequest;
type PublishedTaskRequest = {
  operation: PendingTaskOperation;
  replayingUnresolved: boolean;
  storedDraft?: string;
};

const unsentLockMessage =
  "This browser could not coordinate browser storage with your other open LLMRelay windows, so nothing was saved and no new request was sent. Keep this form open, copy your text if you need it, and try again.";
const recoveryLockMessage =
  "This browser could not coordinate browser storage with your other open LLMRelay windows, so the saved draft was left unchanged. Your current edits are still here; copy them if you need them and try again.";
const retainedCreateNote =
  "The exact create request is retained for explicit reconciliation; edited fields and a different Ready mode cannot be submitted yet.";

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

const matchesStoredOperation = (raw: string, operation: PendingTaskOperation): boolean => {
  try {
    const value: unknown = JSON.parse(raw);
    return isRecord(value) && Object.keys(value).sort().join() === "body,id" &&
      value.body === operation.body && value.id === operation.id;
  } catch {
    return false;
  }
};

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

const pendingEditRequest = (
  pending: PendingTaskOperation,
  taskId: string,
): boolean => {
  try {
    const value: unknown = JSON.parse(pending.body);
    return isRecord(value) && value.kind === "update_task" &&
      Object.keys(value).sort().join() ===
        "acceptance_criteria,description,expected_version,kind,manual_order,priority,role_overrides,task_id,title" &&
      value.task_id === taskId && typeof value.title === "string" &&
      typeof value.description === "string" &&
      isStringList(value.acceptance_criteria) &&
      typeof value.expected_version === "number" &&
      Number.isFinite(value.expected_version) &&
      typeof value.priority === "number" && Number.isFinite(value.priority) &&
      typeof value.manual_order === "number" &&
      Number.isFinite(value.manual_order) &&
      value.role_overrides === null;
  } catch {
    return false;
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
    ...(isRecord(value.workflowChoice) && typeof value.workflowChoice.project_id === "string" &&
        (value.workflowChoice.family === "trip" || value.workflowChoice.family === "single_agent")
      ? { workflowChoice: { project_id: value.workflowChoice.project_id, family: value.workflowChoice.family } }
      : {}),
    ...(isRecord(value.singleAgentChoice) && typeof value.singleAgentChoice.project_id === "string" &&
        isRecord(value.singleAgentChoice.selection) &&
        typeof value.singleAgentChoice.selection.execution_config_revision_id === "string" &&
        typeof value.singleAgentChoice.selection.profile_revision_id === "string" &&
        isStringList(value.singleAgentChoice.selection.check_ids)
      ? { singleAgentChoice: { project_id: value.singleAgentChoice.project_id, selection: {
        execution_config_revision_id: value.singleAgentChoice.selection.execution_config_revision_id,
        profile_revision_id: value.singleAgentChoice.selection.profile_revision_id,
        check_ids: value.singleAgentChoice.selection.check_ids,
      } } }
      : {}),
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
  const operationKey = `${key}.operation`;
  const retainedOperationKey = (id: string) => `${operationKey}.${id}`;
  const parseOperation = (raw: string): PendingTaskOperation => {
    const value: unknown = JSON.parse(raw);
    if (!isRecord(value) || Object.keys(value).sort().join() !== "body,id" ||
      typeof value.body !== "string" || typeof value.id !== "string" || !value.id.trim()) {
      throw new Error("The saved task request is invalid.");
    }
    const pending = { body: value.body, id: value.id };
    if (!(editing ? pendingEditRequest(pending, editing.id) : pendingCreateRequest(pending))) {
      throw new Error("The saved task request is invalid.");
    }
    return pending;
  };
  const readOperations = (): PendingTaskOperation[] => {
    const operations = new Map<string, PendingTaskOperation>();
    const add = (raw: string) => {
      const operation = parseOperation(raw);
      const previous = operations.get(operation.id);
      if (previous && previous.body !== operation.body) throw new Error("Saved task requests disagree.");
      operations.set(operation.id, operation);
    };
    const current = localStorage.getItem(operationKey);
    if (current !== null) add(current);
    if (!editing) {
      // Separate records keep concurrent creates recoverable even when their
      // shared current-request pointer is replaced by another window.
      for (let index = 0; index < localStorage.length; index++) {
        const storedKey = localStorage.key(index);
        if (!storedKey?.startsWith(`${operationKey}.`)) continue;
        const raw = localStorage.getItem(storedKey);
        if (raw === null) continue;
        const operation = parseOperation(raw);
        if (storedKey !== retainedOperationKey(operation.id)) throw new Error("A saved task request has the wrong identity.");
        add(raw);
      }
    }
    return [...operations.values()];
  };
  const parseSavedDraft = (stored: string): TaskDraft => {
    const restored = normalizeDraft(JSON.parse(stored));
    if (!restored) throw new Error("The saved draft could not be read.");
    const selectedIsValid = projects.some((item) => item.id === selectedProjectId);
    return {
      ...restored,
      project_id: editing ? editing.project_id : restored.project_id ||
        (selectedIsValid ? selectedProjectId! : ""),
      ...(editing ? { workspace_mode: editing.workflow?.workspace_mode ?? "isolated_worktree" } : {}),
    };
  };
  const [initial] = useState<{ draft: TaskDraft; restored: boolean; raw?: string; recovery?: DraftRecovery }>(() => {
    let recovery: DraftRecovery | undefined;
    try {
      const stored = localStorage.getItem(key);
      if (stored !== null) {
        return { draft: parseSavedDraft(stored), restored: true, raw: stored };
      }
    } catch {
      recovery = { status: "unread" };
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
        recovery,
      };
    }
    const selectedIsValid = projects.some((item) =>
      item.id === selectedProjectId
    );
    return {
      draft: blank(selectedIsValid ? selectedProjectId : projects[0]?.id),
      restored: false,
      recovery,
    };
  });
  const restoredDraft = useRef(initial.restored);
  const saveGuard = useRef(false);
  const lifecycle = useRef(new AbortController());
  const [initialOperation] = useState(() => {
    try {
      return { pending: readOperations()[0], fault: "" };
    } catch {
      return { pending: undefined, fault: "Saved task requests could not be read safely. They have been kept unchanged. Refresh and check the task board, then restore browser storage or the saved request before saving; nothing new was sent." };
    }
  });
  const [operationFault, setOperationFault] = useState(initialOperation.fault);
  const currentOperationFault = useRef(initialOperation.fault);
  const pendingOperation = useRef<PendingTaskOperation | undefined>(initialOperation.pending ?? undefined);
  const refusedCreate = useRef<PendingTaskOperation | undefined>(undefined);
  const [draft, setDraft] = useState<TaskDraft>(initial.draft);
  const [draftRecovery, setDraftRecovery] = useState(initial.recovery);
  const currentRecovery = useRef(initial.recovery);
  const [newerDraft, setNewerDraft] = useState<TaskDraft>();
  const [completedSave, setCompletedSave] = useState<CompletedTaskSave>();
  const currentCompletion = useRef<CompletedTaskSave | undefined>(undefined);
  const currentDraft = useRef(draft);
  currentDraft.current = draft;
  const currentNewerDraft = useRef(newerDraft);
  currentNewerDraft.current = newerDraft;
  const lastStoredDraft = useRef(initial.raw);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const [storageWarning, setStorageWarning] = useState("");
  const dialog = useRef<HTMLFormElement>(null);
  const titleInput = useRef<HTMLInputElement>(null);
  const workspaceCommands = useWorkflowCommands(`llmrelay.checkout.operations.${editing?.id ?? "new"}`, () => {});
  const [unresolvedCreate, setUnresolvedCreate] = useState<
    PendingTaskOperation | undefined
  >(() =>
    !editing && pendingCreateRequest(pendingOperation.current)
      ? pendingOperation.current
      : undefined
  );
  const currentUnresolved = useRef(unresolvedCreate);

  // Storage lock sections run after the render that queued them, so they read
  // these refs, which change synchronously with each state transition.
  const changeRecovery = (next: DraftRecovery | undefined) => {
    currentRecovery.current = next;
    setDraftRecovery(next);
  };
  const changeUnresolved = (next: PendingTaskOperation | undefined) => {
    currentUnresolved.current = next;
    setUnresolvedCreate(next);
  };
  const changeOperationFault = (message: string) => {
    currentOperationFault.current = message;
    setOperationFault(message);
  };
  const markCompleted = (completed: CompletedTaskSave) => {
    currentCompletion.current = completed;
    setCompletedSave(completed);
  };
  // Aborted when the form closes or its storage key changes. Queued actions
  // then never run; settlement of a request already sent still completes,
  // without UI callbacks.
  useEffect(() => {
    const current = new AbortController();
    lifecycle.current = current;
    return () => current.abort();
  }, [key]);

  const holdSavedDraft = (raw: string | null) => {
    try {
      changeRecovery(raw === null ? { status: "unread" } : {
        status: "loaded", raw, draft: parseSavedDraft(raw),
      });
    } catch {
      changeRecovery({ status: "unread" });
    }
  };
  const retainPendingCreate = () => {
    const pending = pendingOperation.current;
    if (editing || !pending) return;
    const retainedKey = retainedOperationKey(pending.id);
    const retained = localStorage.getItem(retainedKey);
    if (retained !== null && !matchesStoredOperation(retained, pending)) {
      throw new Error("The retained task request has a different body or identity.");
    }
    // A peer can settle and delete this request while this form still holds
    // submitted fields. Keep their original identity before resaving them.
    if (retained === null) localStorage.setItem(retainedKey, JSON.stringify(pending));
  };
  const forgetRefusedCreate = () => {
    const refused = refusedCreate.current;
    if (!refused) return;
    const record = JSON.stringify(refused);
    try {
      for (const storedKey of [operationKey, retainedOperationKey(refused.id)]) {
        if (localStorage.getItem(storedKey) === record) localStorage.removeItem(storedKey);
      }
      refusedCreate.current = undefined;
    } catch { /* The next locked ownership check retries; until then this form ignores the record. */ }
  };
  const checkOperationOwnership = (): boolean => {
    forgetRefusedCreate();
    try {
      const operations = readOperations().filter((operation) => operation.id !== refusedCreate.current?.id);
      if (operations.some((operation) => operation.id === pendingOperation.current?.id &&
        operation.body === pendingOperation.current.body)) return true;
      const foreign = operations[0];
      if (!foreign) return true;
      // Never make a different create replace an observed unresolved request.
      // Recovery keeps this form's fields while replaying the saved identity.
      pendingOperation.current = foreign;
      if (!editing) changeUnresolved(foreign);
      holdSavedDraft(localStorage.getItem(key));
      setError("Another window saved a task request that still needs reconciliation. Your current edits are kept here. Choose which draft to keep, then reconcile the saved request before creating another task.");
      return false;
    } catch {
      changeOperationFault("Saved task requests could not be read safely. They have been kept unchanged. Refresh and check the task board, then restore browser storage or the saved request before saving; nothing new was sent.");
      return false;
    }
  };
  const checkDraftOwnership = (): boolean => {
    try {
      retainPendingCreate();
      if (!currentOperationFault.current && !checkOperationOwnership()) return false;
      const stored = localStorage.getItem(key);
      if (stored !== (lastStoredDraft.current ?? null) && stored !== JSON.stringify(currentDraft.current)) {
        holdSavedDraft(stored);
        setError("Another window changed the saved draft. Both copies have been kept. Choose which copy to use before saving or submitting this form.");
        return false;
      }
      return true;
    } catch {
      changeRecovery({ status: "unread" });
      setError("The saved draft or earlier task request could not be kept safely. Your current edits are kept here. Restore browser storage and use Retry loading saved draft before saving.");
      return false;
    }
  };

  const autosaveCurrentDraft = () => {
    if (currentRecovery.current || currentCompletion.current || !checkDraftOwnership()) return;
    try {
      const serialized = JSON.stringify(currentDraft.current);
      localStorage.setItem(key, serialized);
      lastStoredDraft.current = serialized;
      setStorageWarning("");
    } catch {
      setStorageWarning("This browser could not save your draft locally. Keep this form open and copy your text before refreshing or closing it.");
    }
  };
  useEffect(() => {
    if (draftRecovery || completedSave) return;
    // Newer edits, recovery, completion, a different key or closing the form
    // each make a queued autosave obsolete before it is granted.
    const obsolete = new AbortController();
    withStorageLock(key, autosaveCurrentDraft, obsolete.signal).catch(() =>
      setStorageWarning("This browser could not coordinate saving your draft with other open windows, so it was not saved. Keep this form open and copy your text before refreshing or closing it.")
    );
    return () => obsolete.abort();
  }, [draft, key, draftRecovery, completedSave, operationFault]);
  const retryDraftRead = () => {
    if (!currentRecovery.current) return;
    try {
      const raw = localStorage.getItem(key);
      if (raw === null) {
        retainPendingCreate();
        lastStoredDraft.current = undefined;
      }
      changeRecovery(raw === null ? undefined : {
        status: "loaded",
        raw,
        draft: parseSavedDraft(raw),
      });
      setError("");
    } catch {
      changeRecovery({ status: "unread" });
      setError("The saved draft or earlier task request still could not be recovered safely. Restore browser storage and use Retry loading saved draft again, or copy your current text before closing.");
    }
  };
  const restoreSavedDraft = () => {
    const recovery = currentRecovery.current;
    if (recovery?.status !== "loaded") return;
    if (currentNewerDraft.current) {
      setError("Restore the newer edits already kept below before loading another saved copy. Both copies remain available.");
      return;
    }
    try {
      retainPendingCreate();
      if (!currentOperationFault.current && !checkOperationOwnership()) return;
      if (localStorage.getItem(key) !== recovery.raw) {
        changeRecovery({ status: "unread" });
        setError("The saved draft changed. Use Retry loading saved draft to read the latest copy before choosing.");
        return;
      }
      currentNewerDraft.current = currentDraft.current;
      setNewerDraft(currentDraft.current);
      currentDraft.current = recovery.draft;
      setDraft(recovery.draft);
      lastStoredDraft.current = recovery.raw;
      restoredDraft.current = true;
      changeRecovery(undefined);
      setError("");
    } catch {
      changeRecovery({ status: "unread" });
      setError("The saved draft could not be checked again. Both copies have been kept. Restore browser storage and use Retry loading saved draft.");
    }
  };
  const replaceSavedDraft = () => {
    const recovery = currentRecovery.current;
    if (!recovery) return;
    try {
      retainPendingCreate();
      if (!currentOperationFault.current && !checkOperationOwnership()) return;
      if (recovery.status === "loaded" && localStorage.getItem(key) !== recovery.raw) {
        changeRecovery({ status: "unread" });
        setError("The saved draft changed again. Use Retry loading saved draft before replacing it.");
        return;
      }
      const serialized = JSON.stringify(currentDraft.current);
      localStorage.setItem(key, serialized);
      lastStoredDraft.current = serialized;
      changeRecovery(undefined);
      setStorageWarning("");
      setError("");
    } catch {
      setError("This browser could not replace the saved draft. Your current edits are still here. Restore browser storage and try again, or copy your text before closing.");
    }
  };
  const runGuarded = async (action: (signal: AbortSignal) => Promise<void>) => {
    if (saveGuard.current) return;
    saveGuard.current = true;
    setSaving(true);
    try {
      await action(lifecycle.current.signal);
    } finally {
      saveGuard.current = false;
      setSaving(false);
    }
  };
  const chooseSavedDraft = (choice: () => void) =>
    void runGuarded(async (signal) => {
      try {
        await withStorageLock(key, choice, signal);
      } catch {
        setError(recoveryLockMessage);
      }
    });
  const close = () => {
    if (!saveGuard.current && !workspaceCommands.busy) onClose();
  };
  const onDialogKeyDown = useDialogFocus(dialog, titleInput, close);

  const settleCompletedSave = (completed: CompletedTaskSave): boolean => {
    if (currentRecovery.current) {
      setError("The change is already saved. Choose which saved draft to use below before finishing locally.");
      return false;
    }
    if (currentNewerDraft.current) {
      setError("The change is already saved. Restore the newer edits below to keep them as your draft before finishing, or copy them before closing this form.");
      return false;
    }
    const serialized = JSON.stringify(currentDraft.current);
    const preserve = completed.preserveDraft || serialized !== JSON.stringify(completed.draft);
    const recoverChangedDraft = (raw: string) => {
      try {
        changeRecovery({ status: "loaded", raw, draft: parseSavedDraft(raw) });
      } catch {
        changeRecovery({ status: "unread" });
      }
      setError("The change is already saved, but another window saved a different draft. Both copies have been kept. Choose which copy to use below before finishing locally.");
    };
    try {
      const stored = localStorage.getItem(key);
      const operationRecord = completed.operation && JSON.stringify(completed.operation);
      const storedOperation = operationRecord && localStorage.getItem(`${key}.operation`);
      const superseded = storedOperation && completed.operation &&
        !matchesStoredOperation(storedOperation, completed.operation);
      if (superseded) {
        if (preserve && stored !== serialized) {
          if (stored !== null) recoverChangedDraft(stored);
          else setError("The change is already saved, but another window changed its saved request. Keep this form open and copy your newer edits before closing it.");
          return false;
        }
      } else if (preserve) {
        if (stored !== null && stored !== serialized &&
          stored !== completed.storedDraft && stored !== lastStoredDraft.current) {
          recoverChangedDraft(stored);
          return false;
        }
        if (stored !== serialized) localStorage.setItem(key, serialized);
        lastStoredDraft.current = serialized;
      } else if (stored !== null) {
        if (stored !== serialized && stored !== completed.storedDraft) {
          recoverChangedDraft(stored);
          return false;
        }
        localStorage.removeItem(key);
      }
      // A retained submitted draft must never outlive its replay identity.
      if (!superseded && operationRecord && !removeStoredValue(`${key}.operation`, storedOperation ?? operationRecord)) {
        throw new Error("The saved request could not be cleared.");
      }
      // A peer's pointer leaves the shared draft intact. Keep this create's
      // identity until a later reconciliation can settle its draft as well.
      if (!editing && completed.operation && !superseded &&
        !removeStoredValue(retainedOperationKey(completed.operation.id), operationRecord)) {
        throw new Error("The retained request could not be cleared.");
      }
    } catch {
      setError("The change is already saved, but this browser could not finish saving your draft or clearing the saved request. Keep this form open. Restore browser storage, then choose Finish saving locally; that button sends no new task request.");
      return false;
    }
    pendingOperation.current = undefined;
    setStorageWarning("");
    setError("");
    return true;
  };
  /**
   * Settles a completed save; onSaved is withheld once `form` is aborted.
   * Settlement after a dispatched request omits `queued` so it still runs
   * after the form closes; an explicit retry passes it to be cancellable.
   */
  const finishSavingLocally = async (
    completed: CompletedTaskSave,
    form: AbortSignal,
    queued?: AbortSignal,
  ) => {
    markCompleted(completed);
    changeUnresolved(undefined);
    try {
      const settled = await withStorageLock(key, () => settleCompletedSave(completed), queued);
      if (settled && !form.aborted) onSaved();
    } catch {
      setError("The change is already saved, but this browser could not coordinate browser storage with your other open LLMRelay windows to finish it. Keep this form open and copy any newer text, then choose Finish saving locally to try again; that button sends no new task request.");
    }
  };
  const retryFinishSavingLocally = (completed: CompletedTaskSave) =>
    void runGuarded((signal) => finishSavingLocally(completed, signal, signal));

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
  const chosenFamily = draft.workflowChoice;
  const family: WorkflowFamily = editing || !workflowExecution
    ? "trip"
    : chosenFamily?.project_id === draft.project_id
    ? chosenFamily.family
    : execution?.default_family ?? "trip";
  const singleAgent = family === "single_agent";
  const mainCheckout = draft.workspace_mode === "main_checkout";
  const singleAgentChoice = draft.singleAgentChoice;
  const singleAgentSelection =
    singleAgentChoice?.project_id === draft.project_id
      ? singleAgentChoice.selection
      : emptySingleAgentSelection(execution);
  useEffect(() => {
    if (editing || !execution) return;
    setDraft((current) => {
      if (current.workflowChoice?.project_id === current.project_id &&
          current.singleAgentChoice?.project_id === current.project_id) return current;
      return {
        ...current,
        workflowChoice: current.workflowChoice?.project_id === current.project_id
          ? current.workflowChoice : { project_id: current.project_id, family: execution.default_family },
        singleAgentChoice: current.singleAgentChoice?.project_id === current.project_id
          ? current.singleAgentChoice : { project_id: current.project_id, selection: emptySingleAgentSelection(execution) },
      };
    });
  }, [editing, execution]);
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
    if (completedSave) return "The change is already saved. Finish saving locally before another action.";
    if (draftRecovery) return "Recover or explicitly replace the saved draft before saving this task.";
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
    if (singleAgent) {
      const configuration = execution?.configurations.find((item) => item.id === singleAgentSelection.execution_config_revision_id);
      if (!configuration) return "The saved execution configuration is no longer available. Choose a current configuration.";
      if (!execution?.profiles.some((item) => item.current_revision?.id === singleAgentSelection.profile_revision_id)) {
        return "The saved agent profile revision is no longer available. Choose a current profile.";
      }
      if (singleAgentSelection.check_ids.some((id) => !configuration.checks.some((check) => check.id === id))) {
        return "Some saved checks are no longer available. Review the selected checks before saving.";
      }
    }
    return operationFault;
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
      } differs from the project default. Save this as a draft, run functional qualification for its exact settings, and explicitly activate the task-only profile before making it Ready.`
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
  const createRequest = (ready: boolean): TaskCreateRequest | WorkflowCreateRequest => {
    const criteria = draft.criteria.split("\n").map((value) => value.trim()).filter(Boolean);
    return singleAgent || mainCheckout ? workflowCreate(criteria) : {
      kind: "create_task",
      project_id: draft.project_id,
      title: draft.title,
      description: draft.description,
      acceptance_criteria: criteria,
      priority: draft.priority,
      ready,
      role_overrides: Object.fromEntries(ROLES.flatMap((role) =>
        draft.roleOverrides[role] && draft.roles[role] ? [[role, draft.roles[role]]] : []
      )),
    };
  };

  const publishTaskRequest = (body: TaskRequest): PublishedTaskRequest | undefined => {
    if (currentOperationFault.current) {
      setError(currentOperationFault.current);
      return undefined;
    }
    if (currentRecovery.current || currentCompletion.current || !checkDraftOwnership()) return undefined;
    const serialized = JSON.stringify(body);
    const unresolved = currentUnresolved.current;
    if (!editing && unresolved && unresolved.body !== serialized) {
      setError(
        "An earlier task create still has an unknown result. Refresh the board to inspect it or use Retry exact unresolved create before submitting edited fields or a different Ready mode.",
      );
      return undefined;
    }
    const id = pendingOperation.current?.body === serialized
      ? pendingOperation.current.id
      : operationId();
    const operation = { body: serialized, id };
    const record = JSON.stringify(operation);
    pendingOperation.current = operation;
    try {
      if (editing) {
        localStorage.setItem(operationKey, record);
      } else {
        localStorage.setItem(retainedOperationKey(id), record);
        const currentOperation = localStorage.getItem(operationKey);
        if (currentOperation === null || matchesStoredOperation(currentOperation, operation)) {
          localStorage.setItem(operationKey, record);
        }
      }
    } catch {
      const message = "The task request could not be saved safely in this browser. Nothing new was sent. Restore browser storage and try again.";
      if (!editing && unresolved) {
        changeUnresolved(operation);
        setError(`${message} ${retainedCreateNote}`);
        return undefined;
      }
      if (!editing) {
        pendingOperation.current = undefined;
        removeStoredValue(operationKey, record);
        removeStoredValue(retainedOperationKey(id), record);
      }
      setError(message);
      return undefined;
    }
    return { operation, replayingUnresolved: !!unresolved, storedDraft: lastStoredDraft.current };
  };
  const sendTaskRequest = async (
    body: TaskRequest,
    published: PublishedTaskRequest,
    submittedDraft: TaskDraft,
    form: AbortSignal,
  ) => {
    const { operation } = published;
    try {
      if ("selection" in body) {
        await submitWorkflowCommand({ ...body, operation_id: operation.id });
      } else {
        await command({ ...body, operation_id: operation.id });
      }
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      if (editing) {
        setError(message);
      } else if (published.replayingUnresolved || (cause instanceof ApiError && cause.ambiguous)) {
        changeUnresolved(operation);
        setError(`${message} ${retainedCreateNote}`);
      } else {
        refusedCreate.current = operation;
        pendingOperation.current = undefined;
        changeUnresolved(undefined);
        setError(message);
        await withStorageLock(key, forgetRefusedCreate).catch(() => {
          // The refused record stays ignored until a later locked check removes it.
        });
      }
      return;
    }
    await finishSavingLocally({
      operation,
      draft: submittedDraft,
      storedDraft: published.storedDraft,
      preserveDraft: false,
    }, form);
  };
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
    const { signal } = lifecycle.current;
    const submittedDraft = draft;
    const body: TaskRequest = editing
      ? {
        kind: "update_task",
        task_id: editing.id,
        expected_version: editing.version,
        title: draft.title,
        description: draft.description,
        acceptance_criteria: draft.criteria.split("\n").map((value) => value.trim()).filter(Boolean),
        priority: draft.priority,
        manual_order: editing.manual_order,
        role_overrides: null,
      }
      : createRequest(ready);
    try {
      const published = await withStorageLock(key, () => publishTaskRequest(body), signal).catch(() => {
        setError(unsentLockMessage);
        return undefined;
      });
      // A request published just before the form closed stays retained and
      // unsent; it is recovered later through its exact identity.
      if (published && !signal.aborted) await sendTaskRequest(body, published, submittedDraft, signal);
    } finally {
      saveGuard.current = false;
      setSaving(false);
    }
  };
  const retainReplay = (operation: PendingTaskOperation): { storedDraft?: string } | undefined => {
    if (
      currentRecovery.current || currentCompletion.current || currentOperationFault.current ||
      currentUnresolved.current?.id !== operation.id || !checkDraftOwnership()
    ) return undefined;
    const storedDraft = lastStoredDraft.current;
    localStorage.setItem(retainedOperationKey(operation.id), JSON.stringify(operation));
    return { storedDraft };
  };
  const retryUnresolvedCreate = async () => {
    if (saveGuard.current || draftRecovery || operationFault || !unresolvedCreate) return;
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
    const { signal } = lifecycle.current;
    const submittedDraft = draft;
    const currentRequest = createRequest("ready" in request && request.ready);
    const preserveEditedDraft = JSON.stringify(currentRequest) !==
      submittedOperation.body;
    try {
      const retained = await withStorageLock(key, () => retainReplay(submittedOperation), signal);
      if (!retained || signal.aborted) return;
      if ("selection" in request) {
        await submitWorkflowCommand({
          ...request,
          operation_id: submittedOperation.id,
        });
      } else {
        await command({ ...request, operation_id: submittedOperation.id });
      }
      await finishSavingLocally({
        operation: submittedOperation,
        draft: submittedDraft,
        storedDraft: retained.storedDraft,
        preserveDraft: preserveEditedDraft,
      }, signal);
    } catch (cause) {
      setError(
        cause instanceof StorageLockUnavailableError
          ? "This browser could not coordinate browser storage with your other open LLMRelay windows, so the earlier create was not retried and nothing new was sent. Its result remains unknown. Keep this form open, refresh and inspect the board, then try again."
          : `${cause instanceof Error ? cause.message : String(cause)} The original create result remains unknown. Refresh and inspect the board; its exact request is retained for reconciliation.`,
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
  const confirmWorkspaceOwnership = () =>
    !currentRecovery.current && !currentCompletion.current && checkDraftOwnership()
      ? { storedDraft: lastStoredDraft.current }
      : undefined;
  const saveWorkspace = async () => {
    if (!editing || draftRecovery || completedSave || saveGuard.current) return;
    saveGuard.current = true;
    const { signal } = lifecycle.current;
    const submittedDraft = draft;
    try {
      // The draft lock is released before the journal lock is requested.
      const owned = await withStorageLock(key, confirmWorkspaceOwnership, signal).catch(() => {
        setError(unsentLockMessage);
        return undefined;
      });
      if (!owned || signal.aborted) return;
      const result = await workspaceCommands.submit("workspace", (operation_id) => ({
        kind: "set_task_workflow", operation_id, task_id: editing.id,
        expected_version: editing.version,
        selection: {
          family: editing.workflow?.family ?? "trip",
          workspace_mode: submittedDraft.workspace_mode,
          ...(editing.workflow?.family === "single_agent" ? {
            execution_config_revision_id: editing.workflow.execution_config_revision_id,
            profile_revision_id: editing.workflow.profile_revision_id,
            check_ids: editing.workflow.check_ids,
          } : {}),
        },
      }));
      if (result) {
        await finishSavingLocally({ draft: submittedDraft, storedDraft: owned.storedDraft, preserveDraft: true }, signal);
      }
    } finally {
      saveGuard.current = false;
    }
  };

  return (
    <div className="modal-backdrop">
      <form
        ref={dialog}
        className="modal"
        role="dialog"
        aria-modal="true"
        tabIndex={-1}
        onKeyDown={onDialogKeyDown}
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
            disabled={saving || workspaceCommands.busy}
            onClick={close}
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
            ref={titleInput}
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
                      update("workflowChoice", {
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
              <button type="button" disabled={!!draftRecovery || !!completedSave || draft.workspace_mode === (editing.workflow?.workspace_mode ?? "isolated_worktree")}
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
                update("singleAgentChoice", { project_id: draft.project_id, selection })}
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
            operation identity. {draftRecovery
              ? "Current edits stay in this form while saved draft recovery is pending."
              : "Current form edits remain saved separately."}
            <button
              type="button"
              disabled={saving || !!draftRecovery}
              onClick={() => void retryUnresolvedCreate()}
            >
              Retry exact unresolved create
            </button>
          </div>
        )}
        {draftRecovery && (
          <section className="warning" aria-label="Saved draft recovery">
            <p role="status">
              {draftRecovery.status === "unread"
                ? "This browser could not read your saved draft. The saved copy has been left unchanged."
                : "Your saved draft is available. Choose which copy to use; your current form has not changed."}
              {" "}Automatic saving is paused. Your current edits stay in this form;
              copy them before refreshing or closing it.
            </p>
            <button type="button" disabled={saving} onClick={() => chooseSavedDraft(retryDraftRead)}>
              Retry loading saved draft
            </button>
            {draftRecovery.status === "loaded" && (
              <>
                <details>
                  <summary>Saved draft text</summary>
                  <DraftText draft={draftRecovery.draft} />
                </details>
                <button type="button" disabled={saving} onClick={() => chooseSavedDraft(restoreSavedDraft)}>
                  Restore saved draft
                </button>
                <p>Your newer edits will stay available separately in this form.</p>
              </>
            )}
            <button type="button" disabled={saving} onClick={() => chooseSavedDraft(replaceSavedDraft)}>
              Replace saved draft with current form
            </button>
            <p>Replacing discards the previously saved copy and saves the fields currently in this form.</p>
          </section>
        )}
        {newerDraft && (
          <section className="warning" aria-label="Newer edits">
            <details>
              <summary>Newer edits kept in this form</summary>
              <DraftText draft={newerDraft} />
            </details>
            <p>Copy these edits before closing, or restore them to replace the current form and its saved draft.</p>
            <button
              type="button"
              disabled={saving || workspaceCommands.busy}
              onClick={() => {
                setDraft(newerDraft);
                setNewerDraft(undefined);
                setError("");
              }}
            >
              Restore newer edits
            </button>
          </section>
        )}
        {storageWarning && <p className="warning" role="status">{storageWarning}</p>}
        {completedSave && (
          <section className="warning" aria-label="Finish saved change">
            <p>The change is already saved. Keep any newer edits before finishing this form.</p>
            <button type="button" disabled={saving || workspaceCommands.busy}
              onClick={() => retryFinishSavingLocally(completedSave)}>
              Finish saving locally
            </button>
          </section>
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
            : readyReason || "Ready uses the activated project defaults or task-only profiles activated with current functional qualification. All six roles may use the same provider; each exact profile is checked separately."}
        </p>
        <footer>
          <button type="button" disabled={saving || workspaceCommands.busy} onClick={close}>
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
