import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import {
  type KeyboardEvent as ReactKeyboardEvent,
  type RefObject,
  useEffect,
  useRef,
  useState,
} from "react";
import {
  ApiError,
  command,
  getTaskContent,
  operation,
  reuseOperationIdentity,
} from "../api";
import {
  type AppState,
  type CmuxKeyboardControlAction,
  type CmuxKeyboardControlOutcome,
  type CmuxSessionSurface,
  type CmuxViewOutcome,
  type ContinuationAction,
  type CurrentInputSource,
  type CurrentInputUnavailableReason,
  type PermissionRequest,
  type Role,
  roleLabel,
  type Task,
  type TaskAction,
  type TaskContent,
  type TaskProgress,
} from "../types";
import {
  isSingleAgentTask,
  projectSetupProblem,
  WorkflowControls,
} from "./WorkflowControls";
import {
  currentPlanDecision,
  currentPlanView,
  type PlanView,
  ReviewPanel,
} from "./ReviewPanel";
import { RoleSettings } from "./RoleSettings";
import { RecoveryPanel } from "./RecoveryPanel";
import { GuidanceRecovery } from "./GuidanceRecovery";
import { WorkSummary } from "./WorkSummary";
import { ServiceCheckPermissionActions } from "./ApprovalInbox";
import { MarkdownContent } from "./MarkdownContent";
import { type AttemptTarget, guidanceState } from "./AttentionInbox";
import { ActivityTimeline } from "./ActivityTimeline";
import {
  ObservedUsageDetails,
  SessionTree,
  useSessionAccess,
} from "./SessionTree";
import { StatusBadge, taskStatus, taskSteps } from "./TaskBoard";

export type TaskTab = "overview" | "changes" | "checks" | "roles" | "activity";
const TABS: Array<{ id: TaskTab; label: string }> = [
  { id: "overview", label: "Overview" },
  { id: "changes", label: "Changes" },
  { id: "checks", label: "Checks" },
  { id: "roles", label: "Roles" },
  { id: "activity", label: "Activity" },
];

const focusableSelector =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), summary, [tabindex]:not([tabindex="-1"])';

// Open dialogs, innermost last. Only the innermost answers Escape, and the
// page stays scroll-locked until the last of them closes.
const openDialogs: symbol[] = [];

/**
 * Modal focus handling: focus moves into the dialog, Tab stays inside it,
 * Escape closes the innermost dialog unless something inside already handled
 * that key, and focus returns to `returnFocus` or to what opened it, or to
 * `fallbackFocus` when that is no longer shown.
 */
function useDialogFocus(
  dialog: RefObject<HTMLElement | null>,
  initial: RefObject<HTMLElement | null>,
  onClose: () => void,
  returnFocus?: RefObject<HTMLElement | null>,
  fallbackFocus?: RefObject<HTMLElement | null>,
) {
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    const token = Symbol("dialog");
    openDialogs.push(token);
    const opener = returnFocus?.current ??
      (document.activeElement instanceof HTMLElement
        ? document.activeElement
        : undefined);
    const target = initial.current ?? dialog.current;
    target?.focus({ preventScroll: true });
    if (document.activeElement !== target) {
      dialog.current?.focus({ preventScroll: true });
    }
    document.body.classList.add("dialog-open");
    const onKeyDown = (event: KeyboardEvent) => {
      if (
        event.key !== "Escape" || event.defaultPrevented ||
        openDialogs.at(-1) !== token
      ) return;
      event.preventDefault();
      close.current();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      openDialogs.splice(openDialogs.indexOf(token), 1);
      if (!openDialogs.length) document.body.classList.remove("dialog-open");
      const restore = opener?.isConnected ? opener : fallbackFocus?.current;
      restore?.focus({ preventScroll: true });
    };
  }, [dialog, initial, returnFocus, fallbackFocus]);
  return (event: ReactKeyboardEvent<HTMLElement>) => {
    if (event.key !== "Tab" || !dialog.current) return;
    const focusable = [
      ...dialog.current.querySelectorAll<HTMLElement>(focusableSelector),
    ].filter((element) =>
      element.getClientRects().length > 0 || element === document.activeElement
    );
    if (!focusable.length) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };
}

/**
 * Current-attempt plans and reports, refetched when the service reports a new
 * content revision. Only content for the current attempt and revision is kept
 * or shown: a superseded request can still resolve after a newer one, and its
 * older content or error must never replace or hide the current state.
 */
function useTaskContent(task: Task, enabled: boolean) {
  const attemptId = task.active_attempt?.id;
  // Absent from older services, which then bind content by attempt alone.
  const revision = task.active_attempt?.content_revision ?? "";
  const current = (
    value?: { attempt_id: string | null; content_revision: string },
  ) =>
    !!value && value.attempt_id === attemptId &&
    (!revision || value.content_revision === revision);
  const [content, setContent] = useState<TaskContent>();
  const [error, setError] = useState<
    { attempt_id: string | null; content_revision: string; message: string }
  >();
  const [reloads, setReloads] = useState(0);
  useEffect(() => {
    if (!enabled || !attemptId) return;
    const controller = new AbortController();
    getTaskContent(task.id, controller.signal).then((value) => {
      if (
        controller.signal.aborted || value.attempt_id !== attemptId ||
        (revision && value.content_revision !== revision)
      ) return;
      setContent(value);
      setError(undefined);
    }).catch((cause) => {
      if (controller.signal.aborted) return;
      setError({
        attempt_id: attemptId,
        content_revision: revision,
        message: cause instanceof Error ? cause.message : String(cause),
      });
    });
    return () => controller.abort();
  }, [task.id, attemptId, revision, enabled, reloads]);
  return {
    content: current(content) ? content : undefined,
    error: current(error) ? error!.message : "",
    reload: () => setReloads((count) => count + 1),
  };
}

function PhaseProgress({ task }: { task: Task }) {
  const { steps, current } = taskSteps(task);
  if (current < 0) return null;
  return (
    <ol className="phase-progress" aria-label="Progress">
      {steps.map((step, index) => {
        const state = index < current
          ? "complete"
          : index === current
          ? "current"
          : "upcoming";
        return (
          <li
            key={step}
            className={state}
            aria-current={state === "current" ? "step" : undefined}
          >
            <span className="phase-dot" aria-hidden="true" />
            <span>
              {step}
              <span className="visually-hidden">
                {state === "complete"
                  ? " (done)"
                  : state === "current"
                  ? " (current step)"
                  : " (not started)"}
              </span>
            </span>
          </li>
        );
      })}
    </ol>
  );
}

const outcomeLabel = (outcome?: string) => {
  switch (outcome) {
    case "plan_ready":
      return "Plan";
    case "approved":
      return "Approved";
    case "request_changes":
      return "Changes requested";
    case "needs_rework":
      return "Needs a different approach";
    case "candidate_ready":
      return "Work finished";
    case "needs_input":
      return "Needs your input";
    case "blocked":
      return "Blocked";
    case "evidence_ready":
      return "Findings";
    case "handoff_ready":
      return "Ready for final verification";
    default:
      return outcome ? outcome.replaceAll("_", " ") : "Report";
  }
};

function ReportList(
  { content, error, started }: {
    content?: TaskContent;
    error: string;
    /** Whether the task has an attempt; without one nothing is fetched. */
    started: boolean;
  },
) {
  if (!started) {
    return (
      <p className="empty">
        No agent has worked on this task yet, so there are no reports. Plans,
        reviews and results appear here once it starts.
      </p>
    );
  }
  if (error) return <ErrorNotice error={error} />;
  if (!content) return <p className="hint">Loading reports…</p>;
  const records = [...content.records].reverse();
  if (!records.length) {
    return (
      <p className="empty">
        No reports yet. Agents' plans, reviews and results appear here.
      </p>
    );
  }
  return (
    <div className="report-list">
      {content.truncated && (
        <p className="hint">Only the most recent reports are shown.</p>
      )}
      {records.map((record) => (
        <article className="report" key={record.id}>
          <header>
            <strong>
              {record.kind === "rework_request"
                ? "Your rework request"
                : `${record.role ? roleLabel(record.role) : "Agent"} · ${
                  outcomeLabel(record.outcome)
                }`}
            </strong>
            <time dateTime={record.created_at}>
              {new Date(record.created_at).toLocaleString()}
            </time>
          </header>
          {record.superseded_by_native_turn && (
            <small className="hint">
              Superseded: the agent accepted a later turn on{" "}
              {new Date(record.superseded_by_native_turn.superseded_at)
                .toLocaleString()}, so this report no longer holds the task.
            </small>
          )}
          <MarkdownContent text={record.summary} />
          {record.plan && record.plan !== record.summary && (
            <details>
              <summary>Plan</summary>
              <MarkdownContent text={record.plan} />
            </details>
          )}
        </article>
      ))}
    </div>
  );
}

const unavailableInput: Record<CurrentInputUnavailableReason, string> = {
  not_recorded:
    "This request was recorded before LLMRelay linked requests to the report that raised them, so its exact question cannot be shown.",
  missing:
    "The report that raised this request could not be found, so its question cannot be shown.",
  mismatched:
    "The report recorded for this request does not match it, so it is not shown.",
  superseded:
    "The agent continued working after it raised this request, so that report no longer applies.",
  generation_replaced:
    "The agent session that raised this request has since been replaced, so its report no longer applies.",
};

const inputAuthor = (source: CurrentInputSource) =>
  source.role === null
    ? "An agent"
    : source.role === "single_agent"
    ? "The agent"
    : `The ${roleLabel(source.role).toLowerCase()}`;

/**
 * The exact report behind the current attempt's input hold. When it is
 * unavailable, nothing stands in for it: the person is told why and can
 * refresh or read every report in Activity.
 */
function CurrentInputReport(
  { content, error, titleId, onRefresh, onOpenReports }: {
    content?: TaskContent;
    error: string;
    titleId: string;
    onRefresh: () => void;
    onOpenReports: () => void;
  },
) {
  const source = content?.current_input;
  const unavailable = (title: string, message: string) => (
    <>
      <h3 id={titleId}>{title}</h3>
      <p>{message}</p>
      <p className="hint">
        No other report is shown in its place. Refresh to check the current
        state, or read every report for this attempt in Activity.
      </p>
      <div className="button-row">
        <button type="button" onClick={onRefresh}>Refresh</button>
        <button type="button" onClick={onOpenReports}>Open Activity</button>
      </div>
      {source?.state === "unavailable" && (
        <TechnicalDetails>
          <p>
            {source.kind} · {source.unavailable_reason} · report{" "}
            {source.result_id ?? "not recorded"} · generation{" "}
            {source.role_generation_id ?? "not recorded"} · attempt{" "}
            {source.attempt_id}
          </p>
        </TechnicalDetails>
      )}
    </>
  );
  if (error) {
    return (
      <>
        <h3 id={titleId}>The agent's request could not be loaded</h3>
        <ErrorNotice error={error} />
        <div className="button-row">
          <button type="button" onClick={onRefresh}>Refresh</button>
        </div>
      </>
    );
  }
  if (!content) {
    return (
      <>
        <h3 id={titleId}>Waiting for your input</h3>
        <p className="hint">Loading the agent's report…</p>
      </>
    );
  }
  if (source === undefined) {
    return unavailable(
      "The agent's question is not available",
      "This LLMRelay service does not identify which report raised this request, so no question is shown here.",
    );
  }
  if (source === null) {
    return unavailable(
      "No question is linked to this wait",
      "LLMRelay has no agent question linked to this wait. Why it is waiting and the available actions show what the task needs.",
    );
  }
  if (source.state === "unavailable") {
    return unavailable(
      "The agent's question is not available",
      unavailableInput[source.unavailable_reason],
    );
  }
  const author = inputAuthor(source);
  return (
    <>
      <h3 id={titleId}>
        {source.kind === "question"
          ? `${author} needs a decision`
          : `${author} is blocked`}
      </h3>
      <article className="report current-input">
        <header>
          <small className="hint">
            {source.kind === "question"
              ? "Its question, in its own words:"
              : "What is blocking it, in its own words:"}
          </small>
          <time dateTime={source.report.created_at}>
            {new Date(source.report.created_at).toLocaleString()}
          </time>
        </header>
        <MarkdownContent text={source.report.summary} />
      </article>
      <TechnicalDetails>
        <p>
          Report {source.report.id} · generation {source.role_generation_id}
          {" "}
          · attempt {source.attempt_id}
        </p>
      </TechnicalDetails>
    </>
  );
}

const responsibleLabel = (progress: TaskProgress) => {
  switch (progress.responsible) {
    case "you":
      return "You";
    case "agent":
      return progress.responsible_role
        ? roleLabel(progress.responsible_role)
        : "The agent";
    case "external":
      return "Another task or service";
    default:
      return "LLMRelay";
  }
};

const activityLabel: Record<TaskProgress["activity"], string> = {
  no_live_agent: "No agent is running for this task.",
  agent_live_idle: "An agent is running; no newer workflow step is recorded.",
  agent_active_without_progress:
    "An agent is running and producing activity; no newer workflow step is recorded.",
};

const when = (value: string | null) => {
  if (!value) return undefined;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
};

/**
 * The complete plan the person chose to open, read-only. It shows only the
 * exact snapshot it was opened with; once the task's attempt, plan or content
 * revision moves on, the old text is withdrawn rather than shown under the
 * newer identity. A decision it was opened for is shown only while that same
 * decision still names this plan.
 */
function PlanViewer(
  { view, task, actions, returnFocus, fallbackFocus, onReopen, onClose }: {
    view: PlanView;
    task: Task;
    actions: ContinuationAction[];
    returnFocus: RefObject<HTMLElement | null>;
    fallbackFocus: RefObject<HTMLElement | null>;
    onReopen: (view: PlanView) => void;
    onClose: () => void;
  },
) {
  const dialog = useRef<HTMLElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);
  const trapTab = useDialogFocus(
    dialog,
    closeButton,
    onClose,
    returnFocus,
    fallbackFocus,
  );
  const liveDecision = currentPlanDecision(task, actions);
  const decisionEnded = !!view.decision &&
    liveDecision?.phase !== view.decision.phase;
  const current = currentPlanView(
    task,
    view.decision ? liveDecision : undefined,
  );
  const unchanged = !!current && current.task_id === view.task_id &&
    current.attempt_id === view.attempt_id &&
    current.plan_hash === view.plan_hash &&
    current.content_revision === view.content_revision &&
    current.text === view.text;
  return (
    <div className="plan-viewer-backdrop">
      <section
        ref={dialog}
        className="plan-viewer"
        role="dialog"
        aria-modal="true"
        aria-labelledby="plan-viewer-title"
        tabIndex={-1}
        onKeyDown={trapTab}
      >
        <header className="plan-viewer-header">
          <div>
            <span className="eyebrow">Plan · read only</span>
            <h2 id="plan-viewer-title">{task.title}</h2>
          </div>
          <button
            ref={closeButton}
            className="icon-button plan-viewer-close"
            aria-label="Close plan"
            onClick={onClose}
          >
            <span aria-hidden="true">×</span>
          </button>
        </header>
        <div className="plan-viewer-body">
          {unchanged
            ? (
              <>
                {view.decision && !decisionEnded
                  ? (
                    <>
                      <p>
                        <strong>Opened for:</strong> {view.decision.title}
                      </p>
                      <p className="hint">
                        This is the complete plan that decision applies to. Your
                        decision covers the whole plan.
                      </p>
                    </>
                  )
                  : decisionEnded
                  ? (
                    <p className="warning" role="status">
                      The decision you opened this plan for is no longer waiting
                      for you, so it is shown only as the task's current plan.
                      Check Your decision for what the task needs now.
                    </p>
                  )
                  : (
                    <p className="hint">
                      This is the task's current plan, shown as context. It is
                      not linked to any particular report or question; only an
                      agent's own words say which plan or part of it they mean.
                    </p>
                  )}
                <MarkdownContent className="plan-content" text={view.text} />
              </>
            )
            : (
              <div className="warning" role="status">
                <p>
                  This plan changed after you opened it, so the earlier text is
                  no longer shown. Open the current plan to read it.
                </p>
                {current && (
                  <button onClick={() => onReopen(current)}>
                    Show the current plan
                  </button>
                )}
              </div>
            )}
        </div>
        <footer className="plan-viewer-footer">
          <span className="hint">
            Reading the plan does not approve it. Decide in the task's Your
            decision panel.
          </span>
          <TechnicalDetails>
            <dl className="technical-list">
              <div>
                <dt>Task</dt>
                <dd>{view.task_id}</dd>
              </div>
              <div>
                <dt>Attempt</dt>
                <dd>{view.attempt_id}</dd>
              </div>
              <div>
                <dt>Plan</dt>
                <dd>{view.plan_hash ?? "not reported"}</dd>
              </div>
              <div>
                <dt>Content revision</dt>
                <dd>{view.content_revision ?? "not reported"}</dd>
              </div>
            </dl>
          </TechnicalDetails>
        </footer>
      </section>
    </div>
  );
}

/** Who the task waits on, since when, and whether anything is advancing. */
function ProgressFacts({ progress }: { progress: TaskProgress }) {
  return (
    <>
      <dl className="progress-facts">
        <dt>Waiting on</dt>
        <dd>{responsibleLabel(progress)}</dd>
        {progress.next_operation && (
          <>
            <dt>Next step</dt>
            <dd>{progress.next_operation}</dd>
          </>
        )}
        {progress.waiting_since && (
          <>
            <dt>Waiting since</dt>
            <dd>
              <time dateTime={progress.waiting_since}>
                {when(progress.waiting_since)}
              </time>
            </dd>
          </>
        )}
        {progress.last_meaningful_at && (
          <>
            <dt>Last recorded step</dt>
            <dd>
              <time dateTime={progress.last_meaningful_at}>
                {when(progress.last_meaningful_at)}
              </time>
            </dd>
          </>
        )}
      </dl>
      <p className="hint">{activityLabel[progress.activity]}</p>
      <TechnicalDetails>
        <pre>
          {JSON.stringify(
            {
              reason_code: progress.reason_code,
              last_event: progress.last_meaningful_event,
              last_agent_activity_at: progress.last_agent_activity_at,
            },
            null,
            2,
          )}
        </pre>
      </TechnicalDetails>
    </>
  );
}

export function TaskDetail(
  {
    task,
    state,
    selectedRecoveryId,
    settingsFocus,
    planRequest,
    tab: controlledTab,
    onTabChange,
    onClose,
    onChanged,
    onOpenSetup = () => {},
    onTaskAction,
    onOpenRecoveryRecord,
    onOpenPermission,
    onViewCmuxSession = async () => {
      throw new Error("persistent cmux presentation is not available");
    },
    onSetCmuxKeyboardControl = async () => {
      throw new Error("persistent cmux keyboard control is not available");
    },
    onDiscardCmuxSurface = async () => {
      throw new Error("persistent cmux discard is not available");
    },
  }: {
    task: Task;
    state: AppState;
    selectedRecoveryId?: string;
    /** A fresh object each time an attention item opens a role's settings. */
    settingsFocus?: { role: Role; settings_revision: number };
    /** A fresh object each time an attention item asks to open its plan. */
    planRequest?: AttemptTarget;
    tab?: TaskTab;
    onTabChange?: (tab: TaskTab) => void;
    onClose: () => void;
    onChanged: () => void;
    onOpenSetup?: (projectId: string) => void;
    /** Opens `itemId`, or the action's primary item, exactly. */
    onTaskAction?: (action: TaskAction, itemId?: string) => void;
    /** Opens this task's exact recovery record, or returns why it cannot. */
    onOpenRecoveryRecord?: (
      attemptId: string,
      recoveryId: string,
    ) => string | undefined;
    onOpenPermission?: (request: PermissionRequest) => void;
    onViewCmuxSession?: (sessionId: string) => Promise<CmuxViewOutcome>;
    onSetCmuxKeyboardControl?: (
      sessionId: string,
      surface: CmuxSessionSurface,
      action: CmuxKeyboardControlAction,
    ) => Promise<CmuxKeyboardControlOutcome>;
    onDiscardCmuxSurface?: (
      sessionId: string,
      surfaceRouteId: string,
      operationId: string,
    ) => Promise<CmuxViewOutcome>;
  },
) {
  const [localTab, setLocalTab] = useState<TaskTab>("overview");
  // A role-settings route shows Roles in the same render, so the focus that
  // follows the route finds the role; choosing any tab ends that override.
  const [rolesFocus, setRolesFocus] = useState(settingsFocus);
  const [seenFocus, setSeenFocus] = useState(settingsFocus);
  if (settingsFocus !== seenFocus) {
    setSeenFocus(settingsFocus);
    setRolesFocus(settingsFocus);
  }
  const [planView, setPlanView] = useState<PlanView>();
  const planOpener = useRef<HTMLElement | null>(null);
  const openPlan = (view: PlanView) => {
    planOpener.current = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    setPlanView(view);
  };
  const [planRequestProblem, setPlanRequestProblem] = useState("");
  // Only the plan the request names opens, and it is labelled with its
  // decision only when that decision's own request names it too. Once open,
  // PlanViewer withdraws it if the task moves on. Later renders of the same
  // request never reopen it.
  useEffect(() => {
    if (!planRequest) return;
    const attempt = task.active_attempt;
    const requested = attempt?.id === planRequest.attempt_id &&
      attempt.phase === planRequest.phase &&
      (attempt.plan_hash ?? null) === planRequest.plan_hash;
    const decision = currentPlanDecision(
      task,
      state.continuation_actions || [],
    );
    const view = requested
      ? currentPlanView(
        task,
        decision?.phase === planRequest.phase ? decision : undefined,
      )
      : undefined;
    if (view) {
      setPlanRequestProblem("");
      openPlan(view);
    } else {
      setPlanRequestProblem(
        requested
          ? "The plan for this request is not included in the current task details, so it cannot be shown. Refresh to check again."
          : "This request refers to a different plan than the task now shows. Refresh to review the current request.",
      );
    }
  }, [planRequest]);
  const nextAction = state.task_actions?.find((action) =>
    action.task_id === task.id
  );
  const waitingPermissions = state.permission_requests.filter((request) =>
    request.actionable && request.task_id === task.id
  ).sort((left, right) => left.created_at.localeCompare(right.created_at));
  const headerPermission = nextAction?.action.kind === "review_request"
    ? undefined
    : waitingPermissions[0];
  const tab = settingsFocus && rolesFocus === settingsFocus
    ? "roles"
    : controlledTab ?? localTab;
  const chooseTab = (next: TaskTab) => {
    setRolesFocus(undefined);
    if (onTabChange) onTabChange(next);
    else setLocalTab(next);
  };
  const [dependency, setDependency] = useState("");
  const [integrationRef, setIntegrationRef] = useState("");
  const [answer, setAnswer] = useState("");
  const [error, setError] = useState("");
  const [deleteVersion, setDeleteVersion] = useState<number | null>(null);
  const [queueBusy, setQueueBusy] = useState(false);
  const queueGuard = useRef(false);
  const queueRequestPending = useRef(false);
  const currentTaskVersion = useRef(task.version);
  currentTaskVersion.current = task.version;
  useEffect(() => {
    setDeleteVersion(null);
    if (!queueRequestPending.current) {
      setQueueBusy(false);
      queueGuard.current = false;
    }
  }, [task.id, task.version]);
  const dialog = useRef<HTMLElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);
  const trapTab = useDialogFocus(dialog, closeButton, onClose);
  const operationStorageKey = `llmrelay.task.operations.${task.id}`;
  const commandIdentities = useRef(
    new Map<string, { body: string; id: string }>(
      (() => {
        try {
          const value = JSON.parse(
            localStorage.getItem(operationStorageKey) || "[]",
          );
          return Array.isArray(value) ? value : [];
        } catch {
          return [];
        }
      })(),
    ),
  );
  const attempt = task.active_attempt;
  const singleAgent = isSingleAgentTask(task);
  const singleAgentRun = singleAgent ? task.workflow?.run ?? null : null;
  const project = state.projects.find((item) => item.id === task.project_id);
  const terminal = task.archived ||
    task.lifecycle === "done" || task.lifecycle === "cancelled";
  const dependenciesEditable = !terminal && !attempt &&
    ["backlog", "ready"].includes(task.lifecycle);
  const dependentTasks = state.tasks.filter((other) =>
    other.dependencies.some((item) => item.task_id === task.id)
  );
  const taskSessions = state.active_sessions.filter((session) =>
    session.task_id === task.id
  );
  const currentSessions = taskSessions.filter((session) =>
    session.attempt_id === attempt?.id
  );
  const access = useSessionAccess(taskSessions, {
    onView: onViewCmuxSession,
    onSetKeyboardControl: onSetCmuxKeyboardControl,
    onDiscard: onDiscardCmuxSurface,
    onChanged,
  });
  const liveSession = currentSessions.find((session) =>
    session.status === "running"
  );
  const status = taskStatus(task, { project, sessions: state.active_sessions });
  const failedProbe = !!task.workflow?.run?.probe_admission_id &&
    task.workflow?.run?.stage_reason.startsWith(
        "The probe did not prove the profile:",
      ) === true;
  // The reply form exists only for the host's Answer question item, and it
  // writes only to that item's exact session; a generic wait never opens it.
  const question = terminal
    ? undefined
    : state.attention.find((item) =>
      item.action?.kind === "answer_question" &&
      item.target?.kind === "session" &&
      item.target.task_id === task.id &&
      item.target.attempt_id === attempt?.id
    );
  const questionTarget = question?.target?.kind === "session"
    ? question.target
    : undefined;
  const questionSession = questionTarget &&
    state.active_sessions.find((session) =>
      session.id === questionTarget.session_id &&
      session.role_generation_id === questionTarget.role_generation_id &&
      session.status === "running"
    );
  const waitingForInput = !terminal && task.attention === "needs_input";
  const resultDecision = !!singleAgentRun?.result?.candidate_hash &&
    (singleAgentRun.stage === "awaiting_selected_checks" ||
      singleAgentRun.stage === "awaiting_human" ||
      singleAgentRun.available_actions.some((action) =>
        ["authorize_check", "accept_result", "request_rework"].includes(action)
      ));
  const needsAnswer = !failedProbe &&
    (!!question || (waitingForInput && !resultDecision));
  const { content, error: contentError, reload: reloadContent } =
    useTaskContent(
      task,
      tab === "activity" || needsAnswer,
    );
  const currentInput = content?.current_input;
  const askedBy = currentInput?.state === "available"
    ? inputAuthor(currentInput)
    : undefined;
  const apply = async (body: Record<string, unknown>) => {
    const key = body.kind === "trip"
      ? `${String(body.action)}:${
        String(body.attempt_id || body.task_id || "")
      }:${String(body.check_id || "")}`
      : body.kind === "archive"
      ? `archive:${task.id}:${task.version}`
      : [
          "delete_draft_task",
          "add_dependency",
          "remove_dependency",
          "record_integration",
        ].includes(String(body.kind))
      ? `${String(body.kind)}:${task.id}:${task.version}:${
        String(body.depends_on_task_id || "")
      }`
      : "";
    const prior = key ? commandIdentities.current.get(key) : undefined;
    const stable = reuseOperationIdentity(prior, body);
    const { id, request } = stable;
    if (key) {
      commandIdentities.current.set(key, {
        body: JSON.stringify(request),
        id,
      });
    }
    if (key) {
      localStorage.setItem(
        operationStorageKey,
        JSON.stringify([...commandIdentities.current]),
      );
    }
    try {
      await command({ ...request, operation_id: id } as never);
      if (key) commandIdentities.current.delete(key);
      if (key) {
        localStorage.setItem(
          operationStorageKey,
          JSON.stringify([...commandIdentities.current]),
        );
      }
      setError("");
      onChanged();
      return true;
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      return false;
    }
  };
  const queueMutation = async (body: Record<string, unknown>) => {
    if (queueGuard.current) return;
    queueGuard.current = true;
    queueRequestPending.current = true;
    setQueueBusy(true);
    let applied = false;
    try {
      applied = await apply(body);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
    queueRequestPending.current = false;
    if (!applied || currentTaskVersion.current !== task.version) {
      queueGuard.current = false;
      setQueueBusy(false);
    }
    if (applied && body.kind === "delete_draft_task") {
      localStorage.removeItem(operationStorageKey);
      onClose();
    }
  };
  const addDependency = () =>
    queueMutation({
      kind: "add_dependency",
      task_id: task.id,
      depends_on_task_id: dependency,
      expected_version: task.version,
    });
  const sendAnswer = async () => {
    if (!questionSession) return;
    const sent = await apply({
      kind: "guidance",
      task_id: task.id,
      role_generation_id: questionSession.role_generation_id,
      expected_version: task.version,
      body: answer,
    });
    if (sent) setAnswer("");
  };
  const checks = state.checks.filter((value) =>
    value.attempt_id === attempt?.id
  );
  const selectedChecks = (state.trip_task_verification || []).filter((value) =>
    value.attempt_id === attempt?.id
  );
  const checkApprovalPending = !singleAgent &&
    selectedChecks.some((selection) =>
      selection.authorization.action_state === "actionable" &&
      !selection.authorization.authorized
    );
  const showInputReport = needsAnswer &&
    (!checkApprovalPending || !!question ||
      currentInput?.state === "available");
  const historicalSuites = state.check_suites.filter((value) =>
    value.project_id === task.project_id
  );
  const tripChecks = state.trip_checks || [];
  const tripExplorer = state.trip_explorer || [];
  const tripLanes = state.trip_lanes || [];
  const continuationActions = state.continuation_actions || [];
  const taskDecisions = state.decisions.filter((decision) =>
    decision.subject.task_id === task.id
  );
  const workflowDecision = taskDecisions.find((decision) =>
    !decision.reason_code.startsWith("restart.")
  );
  const waitingBlocker = !terminal
    ? taskDecisions.find((decision) => decision.primary_blocker?.message)
      ?.primary_blocker
    : undefined;
  const waitingReason = waitingBlocker?.message;
  const retryOffered = !!workflowDecision?.control_policy.allowed_controls
    .includes("retry");
  const resumeOffered = !!singleAgentRun?.resume &&
    singleAgentRun.available_actions.includes("resume_run");
  const selectedCheckCount = singleAgentRun?.selected_check_ids.length ??
    task.workflow?.check_ids.length ?? 0;
  let singleAgentVerification: string;
  if (!singleAgentRun) {
    const selection = task.lifecycle === "ready" ? "queued task" : "draft";
    singleAgentVerification = selectedCheckCount
      ? `${selectedCheckCount} ${
        selectedCheckCount === 1 ? "check is" : "checks are"
      } selected for this ${selection}. Not run yet; no run has started.`
      : `No checks are selected for this ${selection}. No run has started.`;
  } else if (!selectedCheckCount) {
    singleAgentVerification = terminal
      ? "No checks were selected for this run."
      : "No checks are selected for this run. Your review of the result is the only verification.";
  } else {
    const passed = singleAgentRun.checks.filter((check) =>
      check.latest_run_passed === true
    ).length;
    const recorded = singleAgentRun.checks.filter((check) =>
      check.latest_run_passed !== null
    ).length;
    singleAgentVerification =
      `${passed} of ${selectedCheckCount} selected checks passed on the captured result.`;
    if (terminal) {
      if (recorded < selectedCheckCount) {
        singleAgentVerification +=
          " Some recorded check outcomes are unavailable.";
      }
      singleAgentVerification += task.lifecycle === "cancelled"
        ? " This run was cancelled; no further checks will run."
        : singleAgentRun.result?.state === "accepted"
        ? " This result was accepted."
        : task.archived
        ? " This task is archived."
        : " This run is complete.";
    } else {
      singleAgentVerification +=
        " Every selected check must pass before you can accept it.";
    }
  }
  // A failed step with a retry prerequisite is the service's own finding that
  // an instruction file still differs, so its review path is offered too.
  const guidanceConflict = !singleAgent && (
    task.progress?.reason_code === "workflow.attempt_readiness_stale" ||
    state.recovery.some((record) =>
      record.attempt_id === attempt?.id &&
      record.state === "attention_required" && (
        typeof record.retry_prerequisite === "string" ||
        (typeof record.detail === "object" && record.detail !== null &&
          "cause" in record.detail && typeof record.detail.cause === "string" &&
          record.detail.cause.includes(
            "worktree policy file changed after verified materialization:",
          ))
      )
    )
  );
  // Follows whichever explanation the panel would otherwise show.
  const setupProblem = !terminal && !singleAgent
    ? projectSetupProblem(
      project,
      task.progress ? task.progress.reason_code : waitingBlocker?.code,
    )
    : undefined;
  const restoreDecision = state.decisions.find((decision) =>
    decision.subject.recovery_id === "database-restore-hold"
  );
  const affectedByRestore = restoreDecision?.prerequisites.some((item) =>
    typeof item.evidence === "object" && item.evidence !== null &&
    "task_id" in item.evidence && item.evidence.task_id === task.id
  );
  const transitionProposals = state.controls.filter((value) =>
    value.attempt_id === attempt?.id && value.kind === "transition_proposal"
  );
  const managerGeneration = task.role_settings.filter((value) =>
    value.role === "manager"
  )
    .sort((left, right) => right.revision - left.revision)
    .find((value) => value.effective_generation_id)?.effective_generation_id;
  const proposals = transitionProposals.filter((value) =>
    !terminal && value.state === "proposed" &&
    !!managerGeneration && value.role_generation_id === managerGeneration &&
    value.payload.role_generation_id === managerGeneration &&
    value.payload.expected_task_version === task.version &&
    value.payload.source_phase === attempt?.phase &&
    (value.payload.plan_hash ?? null) === (attempt?.plan_hash ?? null) &&
    (value.payload.candidate_hash ?? null) === (attempt?.candidate_hash ?? null)
  );
  const historicalProposals = transitionProposals.filter((value) =>
    !proposals.includes(value)
  );
  const passedChecks =
    selectedChecks.filter((selection) =>
      selection.latest_run?.freshness_state === "current" &&
      selection.latest_run.status === "finished" &&
      selection.latest_run.exit_code === 0
    ).length;
  const guidance = state.guidance.filter((item) =>
    item.attempt_id === attempt?.id
  );
  const onTabKey = (event: ReactKeyboardEvent<HTMLButtonElement>) => {
    const index = TABS.findIndex((item) => item.id === tab);
    const next = event.key === "ArrowRight"
      ? (index + 1) % TABS.length
      : event.key === "ArrowLeft"
      ? (index - 1 + TABS.length) % TABS.length
      : event.key === "Home"
      ? 0
      : event.key === "End"
      ? TABS.length - 1
      : -1;
    if (next < 0) return;
    event.preventDefault();
    chooseTab(TABS[next].id);
    const target = document.getElementById(`task-tab-${TABS[next].id}`);
    target?.focus();
    // Focusing alone can leave a tab at the end of the horizontally scrolling
    // strip partly outside it on narrow windows.
    target?.scrollIntoView({ block: "nearest", inline: "nearest" });
  };

  const overview = (
    <div className="task-overview">
      <div className="task-overview-main">
        <section className="panel" aria-labelledby="task-summary-title">
          <h3 id="task-summary-title">Summary</h3>
          <ObservedUsageDetails usage={task.observed_usage} label="Task" />
          {task.description.trim()
            ? (
              <MarkdownContent
                className="task-description"
                text={task.description}
              />
            )
            : <p className="empty">No description.</p>}
          {task.acceptance_criteria.length > 0 && (
            <>
              <h4>Done when</h4>
              <ul className="acceptance-list">
                {task.acceptance_criteria.map((item) => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
            </>
          )}
        </section>
        {affectedByRestore && (
          <p className="warning">
            This task is on hold after a database restore. See Needs your
            attention for what to do.
          </p>
        )}
        {proposals.map((proposal) => (
          <section className="panel" key={String(proposal.id)}>
            <h3>The manager proposed moving to another step</h3>
            <TechnicalDetails>
              <pre>{JSON.stringify(proposal.payload, null, 2)}</pre>
            </TechnicalDetails>
            {!terminal && (
              <button
                onClick={() =>
                  apply({
                    kind: "apply_transition",
                    task_id: task.id,
                    proposal_id: proposal.id,
                    expected_version: task.version,
                  })}
              >
                Apply proposed step
              </button>
            )}
          </section>
        ))}
      </div>
      <div className="task-overview-side">
        {(task.progress || waitingReason) && (
          <section
            className="panel next-step"
            aria-labelledby="task-waiting-title"
          >
            <h3 id="task-waiting-title">Why it is waiting</h3>
            {/* A Single agent stage reason is already the header's status. */}
            {!failedProbe && !(singleAgent &&
              (task.progress?.waiting_reason || waitingReason) ===
                status.detail) &&
              (
                <p>
                  {setupProblem || task.progress?.waiting_reason ||
                    waitingReason}
                </p>
              )}
            {setupProblem && (
              <>
                <button
                  className="primary compact"
                  onClick={() => onOpenSetup(task.project_id)}
                >
                  Open project setup
                </button>
                <TechnicalDetails>
                  <p>
                    {task.progress?.waiting_reason || waitingReason}{" "}
                    · project readiness{" "}
                    {project?.trip?.readiness ?? "not reported"}
                  </p>
                </TechnicalDetails>
              </>
            )}
            {task.progress && <ProgressFacts progress={task.progress} />}
          </section>
        )}
        {showInputReport && (
          <section
            className="panel next-step"
            aria-labelledby="task-question-title"
            data-attention-target={questionSession
              ? `session:${questionSession.id}`
              : undefined}
            tabIndex={-1}
          >
            <CurrentInputReport
              content={content}
              error={contentError}
              titleId="task-question-title"
              onRefresh={() => {
                reloadContent();
                onChanged();
              }}
              onOpenReports={() => chooseTab("activity")}
            />
            {attempt?.plan && (
              <div className="question-plan">
                <button
                  type="button"
                  className="plan-open"
                  onClick={() => {
                    const view = currentPlanView(task);
                    if (view) openPlan(view);
                  }}
                >
                  Open the task's current plan
                </button>
                <small className="hint">
                  LLMRelay cannot tell which plan this report refers to, so the
                  task's current plan is offered only as context. Any plan or
                  section the agent means is named in its own words above.
                </small>
              </div>
            )}
            {questionSession
              ? (
                <>
                  <label>
                    Your reply to the manager
                    <textarea
                      value={answer}
                      onChange={(event) => setAnswer(event.target.value)}
                      placeholder="The manager receives this when it next pauses between steps."
                    />
                  </label>
                  <button
                    className="primary compact"
                    disabled={!answer.trim()}
                    onClick={() => void sendAnswer()}
                  >
                    Send reply
                  </button>
                  <p className="hint">
                    {askedBy && currentInput?.role !== "manager" &&
                      `Replies go to the manager, not directly to ${askedBy.toLowerCase()} that asked. `}
                    Sending a reply does not approve anything or continue the
                    work by itself. If the task is still waiting after the
                    manager has your reply, choose Retry in Controls.
                    {!retryOffered &&
                      " Retry is not available right now; Controls shows what the task is waiting on."}
                  </p>
                </>
              )
              : question
              ? (
                <p className="hint">
                  The manager that receives replies is no longer running, so a
                  reply cannot be sent now. Refresh to see the current state, or
                  use an available action in Controls below once you have
                  resolved what was asked.
                </p>
              )
              : singleAgent
              ? (
                <p className="hint">
                  To answer the agent, type your answer as the message for
                  Resume agent in the Single agent run panel below.{" "}
                  {resumeOffered
                    ? "Resume agent is available now."
                    : "Resume agent is not offered for this run right now. Refresh to check its current state."}
                </p>
              )
              : (
                <p className="hint">
                  This wait is not a question the manager can take a reply to.
                  Resolve what is described above, then use an available action
                  in Controls below.
                </p>
              )}
            {guidance.length > 0 && (
              <>
                <h4>Your replies</h4>
                <ul className="guidance-history">
                  {guidance.slice(0, 3).map((item) => (
                    <li key={String(item.id)}>
                      <span>{String(item.body)}</span>
                      <em>{guidanceState(String(item.state))}</em>
                    </li>
                  ))}
                </ul>
              </>
            )}
          </section>
        )}
        {planRequestProblem && (
          <p className="warning" role="status">
            {planRequestProblem}{" "}
            <button
              type="button"
              onClick={() => {
                setPlanRequestProblem("");
                onChanged();
              }}
            >
              Refresh
            </button>
          </p>
        )}
        <ReviewPanel
          task={task}
          verification={selectedChecks}
          actions={continuationActions}
          onChanged={onChanged}
          onOpenPlan={openPlan}
        />
        <WorkflowControls
          task={task}
          project={project}
          execution={state.workflow_execution?.find((item) => item.project_id === task.project_id)}
          controls={state.controls}
          sessions={state.active_sessions}
          switches={state.switches}
          actions={continuationActions}
          decision={workflowDecision}
          onOpenSetup={onOpenSetup}
          onOpenRecoveryRecord={onOpenRecoveryRecord}
          onChanged={onChanged}
        />
        <RecoveryPanel
          task={task}
          records={state.recovery.filter((value) =>
            value.attempt_id === attempt?.id &&
            value.state === "attention_required"
          )}
          sessions={state.active_sessions}
          actions={continuationActions}
          selectedRecordId={selectedRecoveryId}
          onChanged={onChanged}
        />
        {guidanceConflict &&
          (
            <GuidanceRecovery
              key={attempt?.id}
              task={task}
              onChanged={onChanged}
            />
          )}
        {singleAgent
          ? (
            <section className="panel verification-summary">
              <h3>Verification</h3>
              <p>
                {singleAgentVerification}
              </p>
            </section>
          )
          : (
            <section className="panel verification-summary">
              <h3>Verification</h3>
              <p>
                {selectedChecks.length
                  ? `${passedChecks} of ${selectedChecks.length} selected checks passed on the current result.`
                  : attempt
                  ? "No verification checks are selected yet."
                  : "Checks are selected when the plan is written."}
              </p>
              <button
                className="link-button"
                onClick={() => chooseTab("checks")}
              >
                Open Checks
              </button>
            </section>
          )}
      </div>
    </div>
  );

  const changes = (
    <div className="task-tab-stack">
      <WorkSummary task={task} />
      <section className="panel">
        <h3>Depends on</h3>
        <p className="hint">
          {dependenciesEditable
            ? "Add or remove dependencies before this task starts. Removing one lets this task proceed without waiting for those changes."
            : "Dependencies are fixed once a task starts. Restore an archived draft before editing its dependencies."}
        </p>
        {task.dependencies.map((item, index) => {
          const other = state.tasks.find((value) => value.id === item.task_id);
          return (
            <div className="review-row" key={index}>
              <strong>{other?.title || String(item.task_id)}</strong>
              <span>
                {item.verified_at
                  ? "Its accepted changes are in this task's starting commit."
                  : "Waiting for its accepted changes to be integrated."}
              </span>
              {dependenciesEditable && !item.verified_at && (
                <div className="compact-fields">
                  <label>
                    Integrated commit or branch
                    <input
                      aria-label={`Integration ref for ${String(item.task_id)}`}
                      value={integrationRef}
                      disabled={queueBusy}
                      onChange={(event) =>
                        setIntegrationRef(event.target.value)}
                      placeholder="Exact integrated git ref"
                    />
                  </label>
                  <button
                    disabled={queueBusy || !integrationRef}
                    onClick={() =>
                      queueMutation({
                        kind: "record_integration",
                        task_id: task.id,
                        depends_on_task_id: item.task_id,
                        git_ref: integrationRef,
                        expected_version: task.version,
                      })}
                  >
                    Record
                  </button>
                </div>
              )}
              {dependenciesEditable && (
                <button
                  disabled={queueBusy}
                  aria-label={`Remove dependency on ${
                    other?.title || String(item.task_id)
                  }`}
                  onClick={() =>
                    queueMutation({
                      kind: "remove_dependency",
                      task_id: task.id,
                      depends_on_task_id: item.task_id,
                      expected_version: task.version,
                    })}
                >
                  Remove dependency
                </button>
              )}
              <TechnicalDetails>
                <p>
                  {String(item.task_id)}
                  {item.verified_at ? ` · ${String(item.integration_ref)}` : ""}
                </p>
              </TechnicalDetails>
            </div>
          );
        })}
        {!task.dependencies.length && (
          <p className="empty">This task does not depend on another task.</p>
        )}
        {dependenciesEditable && (
          <div className="compact-fields">
            <label>
              Add a task this depends on
              <select
                aria-label="Dependency task"
                value={dependency}
                disabled={queueBusy}
                onChange={(event) => setDependency(event.target.value)}
              >
                <option value="">Choose a task in this project</option>
                {state.tasks.filter((value) =>
                  value.id !== task.id &&
                  value.project_id === task.project_id &&
                  !value.archived && value.lifecycle !== "cancelled" &&
                  !task.dependencies.some((item) => item.task_id === value.id)
                ).map((value) => (
                  <option value={value.id} key={value.id}>
                    {value.title}
                  </option>
                ))}
              </select>
            </label>
            <button disabled={queueBusy || !dependency} onClick={addDependency}>
              Add
            </button>
          </div>
        )}
      </section>
    </div>
  );

  const checksTab = singleAgent
    ? (
      <div className="task-tab-stack">
        <section className="panel">
          <h3>Selected checks</h3>
          <p className="hint">
            This Single agent run's checks are listed with the run on Overview,
            where you can review and authorize each exact command. TRIP review
            rounds do not apply to Single agent work.
          </p>
          <button className="link-button" onClick={() => chooseTab("overview")}>
            Open Overview
          </button>
        </section>
      </div>
    )
    : (
      <div className="task-tab-stack">
        <section className="panel">
          <h3>Verification checks</h3>
          <p className="hint">
            The manager selects which of the project's checks apply. Each check
            runs only after you approve it, and only on the exact result it was
            approved for.
          </p>
          {selectedChecks.map((selection) => {
            const check = tripChecks.find((item) =>
              item.id === selection.check_id
            );
            const authorized = selection.authorization.authorized;
            const actionable =
              selection.authorization.action_state !== "inactive";
            const run = selection.latest_run;
            const runLabel = !run
              ? "Not run yet"
              : run.freshness_state !== "current"
              ? "Result is out of date"
              : run.status === "finished"
              ? run.exit_code === 0 ? "Passed" : "Failed"
              : run.status.replaceAll("_", " ");
            return (
              <div
                className="verification-row"
                key={`${selection.check_id}:${selection.selected_revision}`}
              >
                <div>
                  <strong>
                    {check?.original_text || check?.check_key ||
                      "Selected check"}
                  </strong>
                  <small>
                    {check?.category === "focused"
                      ? "Focused check"
                      : check?.category === "broad"
                      ? "Full check"
                      : check?.category || "Check"}
                    {selection.required ? " · required" : ""}
                  </small>
                </div>
                <span
                  className={`badge ${
                    runLabel === "Passed"
                      ? "supported"
                      : runLabel === "Failed"
                      ? "danger"
                      : "waiting"
                  }`}
                >
                  {runLabel}
                </span>
                <TechnicalDetails>
                  <p>
                    {check?.check_key} · selection revision{" "}
                    {selection.selected_revision} · relevant inputs{" "}
                    {check?.relevant_inputs.join(", ") || "none declared"}{" "}
                    · coverage {run?.acceptance_coverage.length || 0}/
                    {check?.acceptance_rows.length || 0} acceptance rows
                    {run ? ` · ${run.status} · ${run.freshness_state}` : ""}
                  </p>
                  {run && Boolean(run.evidence) && (
                    <pre>{JSON.stringify(run.evidence, null, 2)}</pre>
                  )}
                </TechnicalDetails>
                {!terminal && (
                  <>
                    <ServiceCheckPermissionActions
                      selection={selection}
                      onDecision={(decision, lifetime) =>
                        apply({
                          kind: "trip",
                          action: "authorize_check",
                          attempt_id: selection.attempt_id,
                          check_id: selection.check_id,
                          selected_revision: selection.selected_revision,
                          exact_command_hash: selection.exact_command_hash,
                          scope_hash: selection.scope_hash,
                          decision,
                          lifetime,
                        })}
                      onRevoke={(rule_id, expected_revision) =>
                        apply({
                          kind: "trip",
                          action: "revoke_check_permission_rule",
                          rule_id,
                          expected_revision,
                        })}
                    />
                    <div className="button-row">
                      <button
                        disabled={!attempt ||
                          selection.attempt_id !== attempt.id ||
                          !authorized || !actionable}
                        onClick={() => {
                          const body = {
                            kind: "check_run",
                            attempt_id: attempt?.id,
                            check_id: selection.check_id,
                          };
                          const key =
                            `check_run:${body.attempt_id}:${body.check_id}`;
                          const stable = reuseOperationIdentity(
                            commandIdentities.current.get(key),
                            body,
                          );
                          const { id, request } = stable;
                          commandIdentities.current.set(key, {
                            body: JSON.stringify(request),
                            id,
                          });
                          localStorage.setItem(
                            operationStorageKey,
                            JSON.stringify([...commandIdentities.current]),
                          );
                          void operation(
                            { ...request, operation_id: id } as never,
                          ).then(() => {
                            commandIdentities.current.delete(key);
                            localStorage.setItem(
                              operationStorageKey,
                              JSON.stringify([...commandIdentities.current]),
                            );
                            onChanged();
                          }).catch((cause) => {
                            if (
                              !(cause instanceof ApiError && cause.ambiguous)
                            ) {
                              commandIdentities.current.delete(key);
                              localStorage.setItem(
                                operationStorageKey,
                                JSON.stringify([...commandIdentities.current]),
                              );
                            }
                            setError(
                              cause instanceof ApiError && cause.ambiguous
                                ? `${cause.message} Refresh and reconcile before retrying.`
                                : cause instanceof Error
                                ? cause.message
                                : String(cause),
                            );
                            onChanged();
                          });
                        }}
                      >
                        {selection.authorization.action_state ===
                            "current_receipt"
                          ? "Run the approved check again"
                          : "Run the approved check"}
                      </button>
                    </div>
                  </>
                )}
              </div>
            );
          })}
          {!selectedChecks.length && (
            <p className="empty">
              No verification checks are selected. The task cannot be verified
              as complete until the checks that apply have passed.
            </p>
          )}
        </section>
        {checks.length > 0 && (
          <section className="panel">
            <h3>Check runs</h3>
            {checks.map((check) => (
              <article className="review-row" key={String(check.id)}>
                <strong>
                  {tripChecks.find((item) =>
                    item.id === check.check_id
                  )
                    ?.original_text ||
                    String(check.suite_name || check.executable)}
                </strong>
                <span>
                  {check.status === "finished"
                    ? check.exit_code === 0
                      ? "Passed"
                      : `Failed (exit ${String(check.exit_code)})`
                    : String(check.status).replaceAll("_", " ")}
                </span>
                {Boolean(check.evidence) && (
                  <TechnicalDetails>
                    <pre>{JSON.stringify(check.evidence, null, 2)}</pre>
                  </TechnicalDetails>
                )}
              </article>
            ))}
          </section>
        )}
        {historicalSuites.length > 0 && (
          <details className="panel">
            <summary>Older check results</summary>
            {historicalSuites.map((suite) => {
              const runs = checks.filter((check) =>
                check.suite_name === suite.name || check.suite_id === suite.id
              );
              return (
                <article className="review-row" key={String(suite.id)}>
                  <strong>{String(suite.name)}</strong>
                  {runs.length
                    ? runs.map((run) => (
                      <div key={String(run.id)}>
                        <span>{String(run.status)}</span>
                        {Boolean(run.evidence) && (
                          <pre>{JSON.stringify(run.evidence, null, 2)}</pre>
                        )}
                      </div>
                    ))
                    : <span>No recorded result</span>}
                </article>
              );
            })}
          </details>
        )}
        <section className="panel">
          <h3>TRIP reviews</h3>
          {task.review_budgets.filter((budget) =>
            budget.attempt_id === attempt?.id
          ).map((budget) => (
            <article className="review-row" key={budget.id}>
              <strong>
                {budget.kind === "plan"
                  ? "Plan reviews"
                  : budget.kind === "code"
                  ? "Code reviews"
                  : "Final verifications"}
              </strong>
              <span>
                {budget.spent} used · {budget.remaining} left
              </span>
              {!terminal && budget.remaining === 0 && (
                <small className="hint">
                  No reviews of this kind are left. The dashboard cannot add
                  more.
                </small>
              )}
            </article>
          ))}
          {task.reviews.map((review) => (
            <article className="review-row" key={review.id}>
              <strong>
                {review.kind === "plan"
                  ? "Plan review"
                  : review.kind === "code"
                  ? "Code review"
                  : "Final verification"}
              </strong>
              <span>
                {review.verdict
                  ? outcomeLabel(review.verdict)
                  : review.delivery_state === "delivered"
                  ? "In progress"
                  : review.delivery_state.replaceAll("_", " ")}
              </span>
              {review.feedback && <MarkdownContent text={review.feedback} />}
            </article>
          ))}
          {!task.reviews.length && (
            <p className="empty">No reviews have been requested yet.</p>
          )}
        </section>
      </div>
    );

  const activity = (
    <div className="task-tab-stack">
      <section className="panel">
        <h3>Current agent output</h3>
        <SessionTree
          title="Agents for this task"
          empty="No agent has run for this task yet."
          sessions={taskSessions}
          access={access}
          permissionRequests={state.permission_requests}
          taskTitle={() => undefined}
          setupProjectId={() => task.project_id}
          onOpenSetup={onOpenSetup}
        />
      </section>
      <section className="panel">
        <h3>Reports</h3>
        <ReportList
          content={content}
          error={contentError}
          started={!!attempt}
        />
      </section>
      {historicalProposals.length > 0 && (
        <details className="panel">
          <summary>Earlier transition proposals</summary>
          <p>These proposals no longer apply to the current task state.</p>
          {historicalProposals.map((proposal) => (
            <article className="review-row" key={proposal.id}>
              <strong>
                {String(proposal.payload.source_phase)} →{" "}
                {String(proposal.payload.phase)}
              </strong>
              <TechnicalDetails>
                <pre>{JSON.stringify(proposal.payload, null, 2)}</pre>
              </TechnicalDetails>
            </article>
          ))}
        </details>
      )}
      <details className="panel">
        <summary>Task history</summary>
        <ActivityTimeline
          tasks={state.tasks}
          events={state.history.filter((event) =>
            event.entity_id === task.id || event.entity_id === attempt?.id ||
            taskSessions.some((session) => session.id === event.entity_id)
          )}
        />
      </details>
      {task.recipe_provenance && (
        <section className="panel">
          <h3>Created from a TRIP recipe</h3>
          <p>
            {task.recipe_provenance.recipe_name} · recipe revision{" "}
            {task.recipe_provenance.recipe_revision}
          </p>
          {task.recipe_provenance.schedule_id && (
            <p>
              Created by a schedule for{" "}
              {new Date(String(task.recipe_provenance.scheduled_for_utc))
                .toLocaleString()}.
            </p>
          )}
          <p className="hint">
            If the recipe's settings are out of date, save a current profile set
            and recipe, archive this draft, then create a fresh draft. Restore
            keeps the original settings.
          </p>
          <TechnicalDetails>
            <p>
              Exact recipe revision {task.recipe_provenance.recipe_revision_id}
              {" "}
              · profile revision {task.recipe_provenance.profile_revision_id}
              {task.recipe_provenance.schedule_id
                ? ` · schedule ${task.recipe_provenance.schedule_id} at ${task.recipe_provenance.scheduled_for_utc}`
                : ""}
            </p>
          </TechnicalDetails>
        </section>
      )}
      {!task.archived && (
        <section className="panel">
          <h3>Archive</h3>
          <p className="hint">
            {task.can_archive
              ? "Archiving hides this task. You can restore it from History later."
              : "Only completed tasks and drafts that never started can be archived."}
          </p>
          <button
            disabled={!task.can_archive}
            onClick={() =>
              apply({
                kind: "archive",
                task_id: task.id,
                expected_version: task.version,
              })}
          >
            Archive
          </button>
        </section>
      )}
      {Object.keys(task.legacy || {}).length > 0 && (
        <details className="panel">
          <summary>Preserved legacy record</summary>
          <p>
            <strong>Source:</strong> {String(task.legacy.source || "")}
          </p>
          <p>
            <strong>Hash:</strong>{" "}
            <code>{String(task.legacy.source_hash || "")}</code>
          </p>
          <h4>Validation runs</h4>
          <pre>{JSON.stringify(task.legacy.validation_runs || [], null, 2)}</pre>
          <h4>Validation bugs</h4>
          <pre>{JSON.stringify(task.legacy.validation_bugs || [], null, 2)}</pre>
          <h4>Sections and unknown fields</h4>
          <pre>{JSON.stringify({ frontmatter: task.legacy.frontmatter, unknown_frontmatter: task.legacy.unknown_frontmatter, sections: task.legacy.sections }, null, 2)}</pre>
          <h4>Original source</h4>
          <pre>{String(task.legacy.source_text || "")}</pre>
        </details>
      )}
    </div>
  );

  const roles = (
    <div className="task-tab-stack">
      <RoleSettings
        task={task}
        project={project}
        sessions={state.active_sessions}
        switches={state.switches}
        lanes={tripLanes}
        productionRestrictions={state.production_role_restrictions}
        capabilities={state.capabilities}
        runtimeAdmissions={(state.trip_setups || []).filter((setup) =>
          setup.project_id === task.project_id
        )
          .flatMap((setup) => setup.runtime_admissions)}
        actions={continuationActions}
        execution={state.workflow_execution?.find((item) =>
          item.project_id === task.project_id
        )}
        tasks={state.tasks}
        onChanged={onChanged}
      />
      {!singleAgent && (
        <details className="panel">
          <summary>Explorer and implementation lanes</summary>
          {tripExplorer.filter((item) => item.attempt_id === attempt?.id).map((
            decision,
          ) => (
            <article className="review-row" key={decision.id}>
              <strong>
                Explorer · {decision.stage === "planning"
                  ? "planning"
                  : decision.stage === "rescue"
                  ? "extra call"
                  : "final check"}
              </strong>
              <span>
                {decision.activated
                  ? decision.outcome ? "Findings recorded" : "Findings pending"
                  : "Not used"}
              </span>
              <small>
                Explorer findings inform the manager; they never approve a plan,
                change, review or result.
              </small>
              <TechnicalDetails>
                <pre>
                {JSON.stringify(
                  { trigger: decision.trigger, limits: decision.limits, census: decision.census },
                  null,
                  2,
                )}
                </pre>
              </TechnicalDetails>
            </article>
          ))}
          {tripLanes.filter((item) => item.attempt_id === attempt?.id).map((
            lane,
          ) => (
            <article className="review-row" key={lane.id}>
              <strong>Lane {lane.lane_key} · {lane.state}</strong>
              <span>Owns {lane.owned_paths.join(", ")}</span>
              <TechnicalDetails>
                <p>
                  Shared: {lane.shared_paths.join(", ") || "none"} · protected:
                  {" "}
                  {lane.protected_paths.join(", ") || "none"} · dependencies:
                  {" "}
                  {lane.dependencies.join(", ") || "none"}{" "}
                  · effective generation{" "}
                  {lane.effective_generation_id || "pending"}
                </p>
              </TechnicalDetails>
            </article>
          ))}
          {!tripLanes.some((item) => item.attempt_id === attempt?.id) && (
            <p className="hint">
              The implementer works in a single lane unless the plan splits the
              work.
            </p>
          )}
        </details>
      )}
    </div>
  );

  return (
    <div className="task-dialog-backdrop">
      <section
        ref={dialog}
        inert={planView !== undefined}
        className="task-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="task-dialog-title"
        aria-describedby="task-dialog-status"
        data-attention-target={`task:${task.id}`}
        tabIndex={-1}
        onKeyDown={trapTab}
      >
        <header className="task-dialog-header">
          <div className="task-dialog-topline">
            <span className="eyebrow">{project?.display_name || "Task"}</span>
            <button
              ref={closeButton}
              className="icon-button task-dialog-close"
              aria-label="Close task details"
              onClick={onClose}
            >
              <span aria-hidden="true">×</span>
            </button>
          </div>
          <div className="task-dialog-title-row">
            <h2 id="task-dialog-title">{task.title}</h2>
            <StatusBadge
              status={failedProbe
                ? { ...status, label: "Profile probe failed" }
                : status}
            />
          </div>
          <p className="task-dialog-status" id="task-dialog-status">
            {failedProbe
              ? "This probe did not establish the evidence needed to admit the profile. Review the recovery steps in the Single agent run panel."
              : status.detail}
          </p>
          {nextAction && onTaskAction && (
            <div className="task-dialog-next">
              <button
                className="primary"
                onClick={() => onTaskAction(nextAction)}
              >
                {nextAction.action.label}
              </button>
              {nextAction.item_ids.length > 1 && (
                <details className="task-dialog-more">
                  <summary>
                    {nextAction.item_ids.length} items need you for this task
                  </summary>
                  <ul>
                    {nextAction.item_ids.slice(1).map((itemId) => {
                      const item = state.attention.find((candidate) =>
                        candidate.id === itemId
                      );
                      if (!item) return null;
                      return (
                        <li key={itemId}>
                          <span>{item.title}</span>
                          <button
                            type="button"
                            onClick={() => onTaskAction(nextAction, itemId)}
                          >
                            {item.action?.label || "Open"}
                            <span className="visually-hidden">
                              : {item.title}
                            </span>
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                </details>
              )}
            </div>
          )}
          {headerPermission && onOpenPermission && (
            <div className="task-dialog-permission" role="status">
              <span>
                <strong>Waiting for your approval:</strong>{" "}
                {roleLabel(headerPermission.role)} wants to use{" "}
                {headerPermission.tool_name}
                {waitingPermissions.length > 1 &&
                  ` · ${waitingPermissions.length} requests waiting`}
              </span>
              <button
                className="task-permission-action"
                onClick={() =>
                  onOpenPermission(headerPermission)}
              >
                Review approval request
              </button>
            </div>
          )}
          <PhaseProgress task={task} />
          <div className="task-tabs" role="tablist" aria-label="Task sections">
            {TABS.map((item) => (
              <button
                key={item.id}
                id={`task-tab-${item.id}`}
                role="tab"
                aria-selected={tab === item.id}
                aria-controls="task-tab-panel"
                tabIndex={tab === item.id ? 0 : -1}
                className={tab === item.id ? "active" : ""}
                onClick={() => chooseTab(item.id)}
                onKeyDown={onTabKey}
              >
                {item.label}
              </button>
            ))}
          </div>
        </header>
        <div
          className="task-dialog-body"
          id="task-tab-panel"
          role="tabpanel"
          aria-labelledby={`task-tab-${tab}`}
        >
          {tab === "overview" && overview}
          {tab === "changes" && changes}
          {tab === "checks" && checksTab}
          {tab === "roles" && roles}
          {tab === "activity" && activity}
          {queueBusy && (
            <p role="status">
              Updating this task.{" "}
              <button onClick={onChanged}>Refresh task</button>
            </p>
          )}
          {deleteVersion === task.version && task.draft_deletion?.allowed && (
            <section className="panel" aria-label="Confirm draft deletion">
              <h3>Delete “{task.title}”?</h3>
              <p>
                This permanently removes the unused draft and its settings. It
                cannot be restored.
              </p>
              <button
                disabled={queueBusy}
                onClick={() => setDeleteVersion(null)}
              >
                Keep draft
              </button>
              <button
                className="danger"
                disabled={queueBusy}
                onClick={() =>
                  queueMutation({
                    kind: "delete_draft_task",
                    task_id: task.id,
                    expected_version: task.version,
                  })}
              >
                Confirm delete draft
              </button>
            </section>
          )}
          {error && <ErrorNotice error={error} />}
          <TechnicalDetails>
            <dl className="technical-list">
              <div>
                <dt>Task</dt>
                <dd>{task.id} · version {task.version}</dd>
              </div>
              <div>
                <dt>State</dt>
                <dd>
                  {task.lifecycle} · attention {task.attention}
                  {attempt
                    ? ` · phase ${attempt.phase} · ${attempt.status}`
                    : ""}
                </dd>
              </div>
              {attempt && (
                <div>
                  <dt>Attempt</dt>
                  <dd>{attempt.id}</dd>
                </div>
              )}
            </dl>
            {taskDecisions.map((decision, index) => (
              <div
                className="decision-explanation"
                key={`${decision.reason_code}:${index}`}
              >
                <strong>
                  {decision.disposition.replaceAll("_", " ")} ·{" "}
                  {decision.reason_code}
                </strong>
                <small>
                  Owner {decision.ownership.owner} ·{" "}
                  {decision.ownership.state.replaceAll("_", " ")}
                </small>
                {decision.prerequisites.map((item, itemIndex) => (
                  <small key={`${item.code}:${itemIndex}`}>
                    {item.state}: {item.message || item.code}
                  </small>
                ))}
                {decision.next_action && (
                  <small>
                    Next action {decision.next_action.operation}
                    {decision.next_action.enabled
                      ? " (available after current revalidation)"
                      : " (unavailable)"}
                  </small>
                )}
              </div>
            ))}
          </TechnicalDetails>
        </div>
        <footer className="task-dialog-footer">
          {task.lifecycle === "backlog" && task.draft_deletion && (
            <div>
              <button
                className="danger"
                disabled={queueBusy || !task.draft_deletion.allowed}
                aria-describedby={task.draft_deletion.reason
                  ? "draft-delete-reason"
                  : undefined}
                onClick={() => setDeleteVersion(task.version)}
              >
                Delete draft
              </button>
              {task.draft_deletion.reason && (
                <p id="draft-delete-reason" className="hint">
                  {task.draft_deletion.reason}
                </p>
              )}
              {dependentTasks.length > 0 && (
                <p className="hint">
                  Used by:{" "}
                  {dependentTasks.map((other) => other.title).join(", ")}
                </p>
              )}
            </div>
          )}
          {liveSession && tab !== "activity" && (
            <button onClick={() => chooseTab("activity")}>
              View agent output
            </button>
          )}
          <span className="hint">No automatic approvals</span>
        </footer>
      </section>
      {planView && (
        <PlanViewer
          view={planView}
          task={task}
          actions={continuationActions}
          returnFocus={planOpener}
          fallbackFocus={closeButton}
          onReopen={setPlanView}
          onClose={() => setPlanView(undefined)}
        />
      )}
    </div>
  );
}
