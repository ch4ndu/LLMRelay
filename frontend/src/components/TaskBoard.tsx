import { ErrorNotice } from "./ErrorNotice";
import { useMemo, useState } from "react";
import { command, operationId } from "../api";
import {
  type PermissionRequest,
  type Project,
  roleLabel,
  type Session,
  type Task,
  type TaskAction,
} from "../types";
import { nativePromptLabel, nativeTurnFailureLabel } from "./SessionTree";

export type StatusTone =
  | "neutral"
  | "draft"
  | "active"
  | "waiting"
  | "attention"
  | "danger"
  | "done";
export interface TaskStatus {
  label: string;
  tone: StatusTone;
  /** One plain sentence explaining what the status means now. */
  detail: string;
  completed: boolean;
}

const phaseStatus: Record<string, Omit<TaskStatus, "completed">> = {
  planning: {
    label: "Planning",
    tone: "active",
    detail: "The manager is writing a plan for this task.",
  },
  plan_review: {
    label: "Plan review",
    tone: "active",
    detail: "An independent reviewer is checking the plan.",
  },
  awaiting_plan_approval: {
    label: "Plan awaiting your approval",
    tone: "attention",
    detail: "The plan passed review. Read it and approve it to continue.",
  },
  awaiting_implementation_authorization: {
    label: "Waiting for you to start implementation",
    tone: "attention",
    detail: "Implementation starts only after you allow it for this plan.",
  },
  implementation: {
    label: "Implementing",
    tone: "active",
    detail: "The implementer is making the planned changes.",
  },
  code_review: {
    label: "Code review",
    tone: "active",
    detail: "An independent reviewer is checking the changes.",
  },
  checks: {
    label: "Running checks",
    tone: "active",
    detail: "The selected verification checks are running.",
  },
  manager_handoff: {
    label: "Preparing the result",
    tone: "active",
    detail: "The manager is confirming the finished work matches the plan.",
  },
  final_review: {
    label: "Final verification",
    tone: "active",
    detail: "A fresh verifier is checking the finished result.",
  },
  awaiting_human_review: {
    label: "Awaiting your review",
    tone: "attention",
    detail:
      "Checks and final review passed. Review the result, then accept it or request rework.",
  },
};

const attentionStatus: Record<string, Omit<TaskStatus, "completed">> = {
  needs_input: {
    label: "Needs your action",
    tone: "attention",
    detail: "The task is waiting for you. Open it to see what it needs.",
  },
  needs_review_budget: {
    label: "Needs your action",
    tone: "attention",
    detail:
      "Every allowed review for this attempt was used. Open the task to decide how to continue.",
  },
  needs_recovery: {
    label: "Manual action needed",
    tone: "danger",
    detail:
      "Automatic work stopped until you confirm what happened to this task's agents.",
  },
  resume_failed: {
    label: "Resume needs recovery",
    tone: "danger",
    detail: "An agent could not resume. Open the task to review the failure and its recovery action.",
  },
  restart_parked: {
    label: "Paused after restart",
    tone: "attention",
    detail:
      "LLMRelay restarted while this task was running. Open it to resume or continue.",
  },
  paused: {
    label: "Paused",
    tone: "waiting",
    detail: "Automatic progress is paused. Choose Continue when it should go on.",
  },
  pause_requested: {
    label: "Pausing",
    tone: "waiting",
    detail: "The task pauses when the current step finishes.",
  },
  queued_capacity: {
    label: "Waiting for a free slot",
    tone: "waiting",
    detail: "All agent slots are busy. The task continues automatically.",
  },
  blocked: {
    label: "Blocked",
    tone: "danger",
    detail:
      "The task cannot continue with its current settings. Open it to see why.",
  },
  needs_human_review: {
    label: "Awaiting your review",
    tone: "attention",
    detail:
      "Checks and final review passed. Review the result, then accept it or request rework.",
  },
};

export const isCompletedTask = (task: Task) =>
  task.archived || task.lifecycle === "done" || task.lifecycle === "cancelled";

/**
 * The plain status of a task. Ready always means queued and eligible to
 * start; it never means ready for your review.
 */
export function taskStatus(
  task: Task,
  context: { project?: Project; sessions?: Session[] } = {},
): TaskStatus {
  const completed = isCompletedTask(task);
  if (task.archived) {
    return {
      label: "Archived",
      tone: "neutral",
      detail: "Hidden from active work. Restore it from History to use it again.",
      completed,
    };
  }
  if (task.lifecycle === "done") {
    return {
      label: "Completed",
      tone: "done",
      detail: "You accepted the result. The task is finished.",
      completed,
    };
  }
  if (task.lifecycle === "cancelled") {
    return {
      label: "Cancelled",
      tone: "neutral",
      detail: "This task was stopped and will not continue.",
      completed,
    };
  }
  if (task.lifecycle === "backlog") {
    return task.attention === "needs_input"
      ? {
        label: "Draft · needs your action",
        tone: "attention",
        detail:
          "It was moved back to drafts because something must be fixed before it can start. Open it to see what is needed.",
        completed,
      }
      : {
        label: "Draft",
        tone: "draft",
        detail: "Not queued yet. Choose Make Ready when it should start.",
        completed,
      };
  }
  if (task.lifecycle === "ready") {
    if (task.attention === "needs_input") {
      return {
        label: "Ready · needs your action",
        tone: "attention",
        detail:
          "Queued, but something must be fixed before it can start. Open it to see what is needed.",
        completed,
      };
    }
    if (task.attention === "run_next_requested") {
      return {
        label: "Ready · starting next",
        tone: "active",
        detail: "You chose Run next, so this task starts as soon as possible.",
        completed,
      };
    }
    if (task.attention === "queued_capacity") {
      return { ...attentionStatus.queued_capacity, completed };
    }
    return {
      label: "Ready",
      tone: "waiting",
      detail: context.project?.queue_paused
        ? "Queued and eligible to start, but pickup is paused for this project. Choose Resume pickup, or Run next to start only this task."
        : "Queued and eligible to start. It starts automatically when an agent slot is free.",
      completed,
    };
  }
  const attention = attentionStatus[task.attention];
  if (attention) return { ...attention, completed };
  if (task.permission_waiting) {
    return {
      label: "Waiting for your approval",
      tone: "attention",
      detail: "An agent asked for permission. Review the request in Approvals.",
      completed,
    };
  }
  const attempt = task.active_attempt;
  const running = (context.sessions ?? []).filter((session) =>
    session.attempt_id === attempt?.id && session.status === "running"
  );
  const prompted = running.find((session) => session.native_prompt && !session.native_prompt.dismissed);
  const promptDetail = prompted?.native_prompt &&
    `The ${roleLabel(prompted.role)} is ${
      nativePromptLabel[prompted.native_prompt.kind]
    }. LLMRelay cannot answer it; open the agent's output to answer it there.`;
  // A later prompt never hides a failed turn; it stays as a secondary detail.
  const failed = running.find((session) => session.native_turn?.failure);
  if (failed?.native_turn?.failure) {
    return {
      label: "Agent turn stopped",
      tone: "danger",
      detail: `The ${roleLabel(failed.role)}'s latest turn stopped because ${
        nativeTurnFailureLabel[failed.native_turn.failure.kind]
      }. Its session is still open. Open the agent's output to decide how to continue. LLMRelay does not retry or switch models in response to this notice.${
        promptDetail ? ` ${promptDetail}` : ""
      }`,
      completed,
    };
  }
  if (promptDetail) {
    return {
      label: "Waiting in the agent's terminal",
      tone: "attention",
      detail: promptDetail,
      completed,
    };
  }
  const startup = running.some((session) => session.readiness === "unknown");
  if (startup) {
    return {
      label: "Waiting for startup",
      tone: "waiting",
      detail:
        "An agent is starting and may be waiting for an answer to a startup prompt. Open its output to check.",
      completed,
    };
  }
  const phase = attempt && phaseStatus[attempt.phase];
  if (attempt?.phase === "implementation" &&
    !running.some((session) => session.role === "implementer")) {
    if (attempt.candidate_hash) {
      return {
        label: "Waiting for code review",
        tone: "waiting",
        detail: "The implementation candidate is frozen. The manager must request code review; code review has not started yet.",
        completed,
      };
    }
    return {
      label: "Waiting between implementation steps",
      tone: "waiting",
      detail: task.progress?.waiting_reason ||
        "No implementer is running. LLMRelay is preparing the next implementation step.",
      completed,
    };
  }
  if (phase) return { ...phase, completed };
  return {
    label: lifecycleLabels[task.lifecycle] || "In progress",
    tone: "active",
    detail: "Agents are working on this task.",
    completed,
  };
}

export const PHASE_STEPS = ["Plan", "Build", "Verify", "Review"] as const;

/** Index of the current step in `PHASE_STEPS`, 4 when finished, -1 before planning. */
export function phaseStep(task: Task): number {
  if (task.lifecycle === "done") return PHASE_STEPS.length;
  switch (task.active_attempt?.phase) {
    case "planning":
    case "plan_review":
    case "awaiting_plan_approval":
    case "awaiting_implementation_authorization":
      return 0;
    case "implementation":
    case "code_review":
      return 1;
    case "checks":
    case "manager_handoff":
    case "final_review":
      return 2;
    case "awaiting_human_review":
      return 3;
    default:
      return -1;
  }
}

export function StatusBadge({ status }: { status: TaskStatus }) {
  return <span className={`status-badge tone-${status.tone}`}>{status.label}</span>;
}

const activeLanes = [
  "awaiting_review",
  "backlog",
  "ready",
  "in_progress",
  "validation",
];
const completedLanes = ["done", "cancelled"];
const reviewFirst = (left: Task, right: Task) =>
  Number(right.lifecycle === "awaiting_review") -
  Number(left.lifecycle === "awaiting_review");
export const lifecycleLabels: Record<string, string> = {
  backlog: "Drafts",
  ready: "Ready (queued)",
  in_progress: "In progress",
  validation: "Validation",
  awaiting_review: "Awaiting your review",
  done: "Completed",
  cancelled: "Cancelled",
};
const priorityLabel = (task: Task) =>
  task.priority > 0 ? "High priority" : task.priority < 0 ? "Low priority" : "";

export type TaskSection = "active" | "completed";
const savedSection = (key: string): TaskSection =>
  localStorage.getItem(key) === "completed" ? "completed" : "active";

/** Active and Completed switch shared by the board and the Workspace list. */
export function SectionSwitch(
  { value, onChange, counts, label }: {
    value: TaskSection;
    onChange: (value: TaskSection) => void;
    counts: Record<TaskSection, number>;
    label: string;
  },
) {
  return (
    <div className="section-switch" role="group" aria-label={label}>
      {(["active", "completed"] as TaskSection[]).map((section) => (
        <button
          key={section}
          type="button"
          aria-pressed={value === section}
          className={value === section ? "active" : ""}
          onClick={() => onChange(section)}
        >
          {section === "active" ? "Active" : "Completed"}
          <span className="count">{counts[section]}</span>
        </button>
      ))}
    </div>
  );
}

/**
 * The task's next step from the host, shown as a button beside the card or
 * row. It names the task for screen readers because several can be visible.
 */
function TaskActionButton(
  { task, action, onAction }: {
    task: Task;
    action?: TaskAction;
    onAction?: (action: TaskAction) => void;
  },
) {
  if (!action || !onAction) return null;
  const more = action.item_ids.length - 1;
  return (
    <button className="task-next-action" onClick={() => onAction(action)}>
      {action.action.label}
      <span className="visually-hidden">: {task.title}</span>
      {more > 0 && (
        <small className="task-next-more">
          {more === 1 ? " +1 more" : ` +${more} more`}
        </small>
      )}
    </button>
  );
}

const actionFor = (actions: TaskAction[] | undefined, task: Task) =>
  actions?.find((action) => action.task_id === task.id);

// Sits beside the host's next step so an independent hold never hides a permission wait.
function PermissionActionButton(
  { task, requests, action, onOpen }: {
    task: Task;
    requests: PermissionRequest[];
    action?: TaskAction;
    onOpen?: (request: PermissionRequest) => void;
  },
) {
  if (!onOpen || action?.action.kind === "review_request") return null;
  const waiting = requests.filter((request) =>
    request.actionable && request.task_id === task.id
  ).sort((left, right) => left.created_at.localeCompare(right.created_at));
  const [first] = waiting;
  if (!first) return null;
  return (
    <button className="task-permission-action" onClick={() => onOpen(first)}>
      <span>Review approval request</span>
      <small>
        {roleLabel(first.role)} wants to use {first.tool_name}
        {waiting.length > 1 && ` · +${waiting.length - 1} more`}
      </small>
      <span className="visually-hidden">: {task.title}</span>
    </button>
  );
}

export function TaskBoard(
  {
    tasks,
    projects,
    sessions = [],
    taskActions,
    permissionRequests = [],
    onOpen,
    onEdit,
    onChanged,
    onTaskAction,
    onOpenPermission,
  }: {
    tasks: Task[];
    projects: Project[];
    sessions?: Session[];
    taskActions?: TaskAction[];
    permissionRequests?: PermissionRequest[];
    onOpen: (task: Task) => void;
    onEdit: (task: Task) => void;
    onChanged: () => void;
    onTaskAction?: (action: TaskAction) => void;
    onOpenPermission?: (request: PermissionRequest) => void;
  },
) {
  const [query, setQuery] = useState("");
  const [section, setSection] = useState<TaskSection>(() =>
    savedSection("llmrelay.board.section")
  );
  const [view, setView] = useState<"board" | "list">(() =>
    localStorage.getItem("agenticjira.board.view") === "list" ? "list" : "board"
  );
  const [error, setError] = useState("");
  const matching = useMemo(
    () =>
      tasks.filter((task) =>
        !task.archived &&
        `${task.id} ${task.title} ${task.description}`.toLowerCase().includes(
          query.toLowerCase(),
        )
      ),
    [tasks, query],
  );
  const counts = {
    active: matching.filter((task) => !isCompletedTask(task)).length,
    completed: matching.filter(isCompletedTask).length,
  };
  const filtered = matching.filter((task) =>
    (section === "completed") === isCompletedTask(task)
  );
  const lanes = section === "active" ? activeLanes : completedLanes;
  const sorted = (items: Task[]) =>
    [...items].sort((left, right) =>
      right.priority - left.priority || left.manual_order - right.manual_order
    );
  const projectFor = (id: string) => projects.find((value) => value.id === id);
  const chooseView = (value: "board" | "list") => {
    setView(value);
    localStorage.setItem("agenticjira.board.view", value);
  };
  const chooseSection = (value: TaskSection) => {
    setSection(value);
    localStorage.setItem("llmrelay.board.section", value);
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

  const card = (task: Task) => {
    const project = projectFor(task.project_id);
    const status = taskStatus(task, { project, sessions });
    return (
      <article className={`task-card tone-${status.tone}`} key={task.id}>
        <button className="card-body" onClick={() => onOpen(task)}>
          <strong>{task.title}</strong>
          <StatusBadge status={status} />
          <small>
            {project?.display_name || "Unknown project"}
            {priorityLabel(task) ? ` · ${priorityLabel(task)}` : ""}
          </small>
          {task.recipe_provenance && (
            <small>
              {task.recipe_provenance.schedule_id
                ? "Scheduled draft"
                : "Recipe draft"} · {task.recipe_provenance.recipe_name}{" "}
              revision {task.recipe_provenance.recipe_revision}
            </small>
          )}
        </button>
        <TaskActionButton
          task={task}
          action={actionFor(taskActions, task)}
          onAction={onTaskAction}
        />
        <PermissionActionButton
          task={task}
          requests={permissionRequests}
          action={actionFor(taskActions, task)}
          onOpen={onOpenPermission}
        />
        {["backlog", "ready"].includes(task.lifecycle) && (
          <div className="card-actions">
            <button
              aria-label={`Move ${task.title} earlier`}
              onClick={() => move(task, -1)}
            >
              ↑
            </button>
            <button
              aria-label={`Move ${task.title} later`}
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
  };

  return (
    <>
      <div className="toolbar">
        <label className="search">
          <span aria-hidden="true">⌕</span>
          <input
            aria-label="Filter tasks"
            placeholder="Filter tasks"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
        <SectionSwitch
          label="Task section"
          value={section}
          onChange={chooseSection}
          counts={counts}
        />
        <div className="section-switch" role="group" aria-label="Task view">
          <button
            type="button"
            aria-pressed={view === "board"}
            className={view === "board" ? "active" : ""}
            onClick={() => chooseView("board")}
          >
            Board
          </button>
          <button
            type="button"
            aria-pressed={view === "list"}
            className={view === "list" ? "active" : ""}
            onClick={() => chooseView("list")}
          >
            List
          </button>
        </div>
      </div>
      {error && <ErrorNotice error={error} />}
      {view === "board"
        ? (
          <div className={`board lanes-${lanes.length}`}>
            {lanes.map((lane) => {
              const items = sorted(
                filtered.filter((task) => task.lifecycle === lane),
              );
              return (
                <section
                  className={`lane lane-${lane}`}
                  key={lane}
                  aria-label={lifecycleLabels[lane]}
                >
                  <header>
                    <h2>{lifecycleLabels[lane]}</h2>
                    <span className="count">{items.length}</span>
                  </header>
                  {items.length === 0 && <p className="empty">No tasks</p>}
                  {items.map(card)}
                </section>
              );
            })}
          </div>
        )
        : (
          <div className="task-list">
            {sorted(filtered).sort(reviewFirst).map((task) => {
              const project = projectFor(task.project_id);
              const status = taskStatus(task, { project, sessions });
              return (
                <article key={task.id}>
                  <button onClick={() => onOpen(task)}>
                    <strong>{task.title}</strong>
                    <StatusBadge status={status} />
                    <small>
                      {project?.display_name || "Unknown project"} ·{" "}
                      {lifecycleLabels[task.lifecycle] || task.lifecycle}
                      {task.recipe_provenance &&
                        ` · ${
                          task.recipe_provenance.schedule_id
                            ? "Scheduled draft"
                            : "Recipe draft"
                        } from ${task.recipe_provenance.recipe_name}`}
                    </small>
                  </button>
                  <PermissionActionButton
                    task={task}
                    requests={permissionRequests}
                    action={actionFor(taskActions, task)}
                    onOpen={onOpenPermission}
                  />
                  {["backlog", "ready"].includes(task.lifecycle) && (
                    <div className="button-row">
                      <button onClick={() => move(task, -1)}>
                        Move earlier
                      </button>
                      <button onClick={() => move(task, 1)}>Move later</button>
                      <button onClick={() => onEdit(task)}>Edit</button>
                    </div>
                  )}
                </article>
              );
            })}
            {!filtered.length && (
              <p className="empty">
                {section === "active"
                  ? "No active tasks. Create a task to get started."
                  : "No completed tasks yet."}
              </p>
            )}
          </div>
        )}
    </>
  );
}

/**
 * Compact Active or Completed list for the Workspace. Completed tasks stay out
 * of the active list so current work is easy to scan.
 */
export function TaskSections(
  {
    tasks,
    projects,
    sessions,
    taskActions,
    permissionRequests = [],
    onOpen,
    onTaskAction,
    onOpenPermission,
  }: {
    tasks: Task[];
    projects: Project[];
    sessions: Session[];
    taskActions?: TaskAction[];
    permissionRequests?: PermissionRequest[];
    onOpen: (task: Task) => void;
    onTaskAction?: (action: TaskAction) => void;
    onOpenPermission?: (request: PermissionRequest) => void;
  },
) {
  const [section, setSection] = useState<TaskSection>(() =>
    savedSection("llmrelay.workspace.section")
  );
  const visible = tasks.filter((task) => !task.archived);
  const active = visible.filter((task) => !isCompletedTask(task));
  const completed = visible.filter(isCompletedTask);
  const shown = section === "active" ? [...active].sort(reviewFirst) : completed;
  const choose = (value: TaskSection) => {
    setSection(value);
    localStorage.setItem("llmrelay.workspace.section", value);
  };
  return (
    <section className="panel task-sections" id="workspace-tasks" aria-labelledby="workspace-tasks-title">
      <header>
        <h2 id="workspace-tasks-title">Tasks</h2>
        <SectionSwitch
          label="Workspace task section"
          value={section}
          onChange={choose}
          counts={{ active: active.length, completed: completed.length }}
        />
      </header>
      <ul className="task-rows">
        {shown.map((task) => {
          const project = projects.find((item) => item.id === task.project_id);
          const status = taskStatus(task, { project, sessions });
          return (
            <li key={task.id}>
              <button className={`task-row tone-${status.tone}`} onClick={() => onOpen(task)}>
                <span className="task-row-title">
                  <strong>{task.title}</strong>
                  <small>{project?.display_name || "Unknown project"}</small>
                </span>
                <span className="task-row-status">
                  <StatusBadge status={status} />
                  <small>{status.detail}</small>
                </span>
              </button>
              <TaskActionButton
                task={task}
                action={actionFor(taskActions, task)}
                onAction={onTaskAction}
              />
              <PermissionActionButton
                task={task}
                requests={permissionRequests}
                action={actionFor(taskActions, task)}
                onOpen={onOpenPermission}
              />
            </li>
          );
        })}
      </ul>
      {!shown.length && (
        <p className="empty">
          {section === "active"
            ? "No active tasks. Choose New task to create one."
            : "No completed tasks yet. Accepted and cancelled tasks appear here."}
        </p>
      )}
    </section>
  );
}
