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
  roleLabel,
  type Role,
  type Task,
  type TaskAction,
  type TaskContent,
  type TaskProgress,
} from "../types";
import { projectSetupProblem, WorkflowControls } from "./WorkflowControls";
import { ReviewPanel } from "./ReviewPanel";
import { RoleSettings } from "./RoleSettings";
import { RecoveryPanel } from "./RecoveryPanel";
import { WorkSummary } from "./WorkSummary";
import { ServiceCheckPermissionActions } from "./ApprovalInbox";
import { MarkdownContent } from "./MarkdownContent";
import { guidanceState } from "./AttentionInbox";
import { ActivityTimeline } from "./ActivityTimeline";
import { SessionTree, useSessionAccess } from "./SessionTree";
import { PHASE_STEPS, phaseStep, StatusBadge, taskStatus } from "./TaskBoard";

export type TaskTab = "overview" | "changes" | "checks" | "activity";
const TABS: Array<{ id: TaskTab; label: string }> = [
  { id: "overview", label: "Overview" },
  { id: "changes", label: "Changes" },
  { id: "checks", label: "Checks" },
  { id: "activity", label: "Activity" },
];

const focusableSelector =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), summary, [tabindex]:not([tabindex="-1"])';

/**
 * Modal focus handling: focus moves into the dialog, Tab stays inside it,
 * Escape closes it from any control unless something inside already handled
 * that key, and focus returns to what opened it.
 */
function useDialogFocus(
  dialog: RefObject<HTMLElement | null>,
  initial: RefObject<HTMLElement | null>,
  onClose: () => void,
) {
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    const opener = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : undefined;
    const target = initial.current ?? dialog.current;
    target?.focus({ preventScroll: true });
    if (document.activeElement !== target) {
      dialog.current?.focus({ preventScroll: true });
    }
    document.body.classList.add("dialog-open");
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      event.preventDefault();
      close.current();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.body.classList.remove("dialog-open");
      if (opener?.isConnected) opener.focus({ preventScroll: true });
    };
  }, [dialog, initial]);
  return (event: ReactKeyboardEvent<HTMLElement>) => {
    if (event.key !== "Tab" || !dialog.current) return;
    const focusable = [
      ...dialog.current.querySelectorAll<HTMLElement>(focusableSelector),
    ].filter((element) => element.getClientRects().length > 0 || element === document.activeElement);
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
  const current = (value?: { attempt_id: string | null; content_revision: string }) =>
    !!value && value.attempt_id === attemptId &&
    (!revision || value.content_revision === revision);
  const [content, setContent] = useState<TaskContent>();
  const [error, setError] = useState<
    { attempt_id: string | null; content_revision: string; message: string }
  >();
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
  }, [task.id, attemptId, revision, enabled]);
  return {
    content: current(content) ? content : undefined,
    error: current(error) ? error!.message : "",
  };
}

function PhaseProgress({ task }: { task: Task }) {
  const current = phaseStep(task);
  if (current < 0 && task.lifecycle !== "done") return null;
  return (
    <ol className="phase-progress" aria-label="Progress">
      {PHASE_STEPS.map((step, index) => {
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
  { content, error, started, onlyLatest = false }: {
    content?: TaskContent;
    error: string;
    /** Whether the task has an attempt; without one nothing is fetched. */
    started: boolean;
    onlyLatest?: boolean;
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
  const records = onlyLatest
    ? content.records.filter((record) =>
      record.outcome === "needs_input" || record.outcome === "blocked"
    ).slice(-1)
    : [...content.records].reverse();
  if (!records.length) {
    return (
      <p className="empty">
        {onlyLatest
          ? "The agent's question is not available yet."
          : "No reports yet. Agents' plans, reviews and results appear here."}
      </p>
    );
  }
  return (
    <div className="report-list">
      {content.truncated && !onlyLatest && (
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
              <time dateTime={progress.waiting_since}>{when(progress.waiting_since)}</time>
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
    tab: controlledTab,
    onTabChange,
    onClose,
    onChanged,
    onOpenSetup = () => {},
    onTaskAction,
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
    tab?: TaskTab;
    onTabChange?: (tab: TaskTab) => void;
    onClose: () => void;
    onChanged: () => void;
    onOpenSetup?: (projectId: string) => void;
    /** Opens `itemId`, or the action's primary item, exactly. */
    onTaskAction?: (action: TaskAction, itemId?: string) => void;
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
  // Opening a role's settings from an attention item expands Agent settings
  // before the focus moves there; the user can still collapse it afterwards.
  const [settingsOpen, setSettingsOpen] = useState(!!settingsFocus);
  const [openedFor, setOpenedFor] = useState(settingsFocus);
  if (settingsFocus !== openedFor) {
    setOpenedFor(settingsFocus);
    if (settingsFocus) setSettingsOpen(true);
  }
  const nextAction = state.task_actions?.find((action) =>
    action.task_id === task.id
  );
  const tab = controlledTab ?? localTab;
  const chooseTab = (next: TaskTab) =>
    onTabChange ? onTabChange(next) : setLocalTab(next);
  const [dependency, setDependency] = useState("");
  const [integrationRef, setIntegrationRef] = useState("");
  const [answer, setAnswer] = useState("");
  const [error, setError] = useState("");
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
  const project = state.projects.find((item) => item.id === task.project_id);
  const terminal = task.archived ||
    task.lifecycle === "done" || task.lifecycle === "cancelled";
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
  // The reply form exists only for the host's Answer question item, and it
  // writes only to that item's exact session; a generic wait never opens it.
  const question = terminal ? undefined : state.attention.find((item) =>
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
  const needsAnswer = !!question || waitingForInput;
  const { content, error: contentError } = useTaskContent(
    task,
    tab === "activity" || needsAnswer,
  );
  const apply = async (body: Record<string, unknown>) => {
    const key = body.kind === "trip"
      ? `${String(body.action)}:${
        String(body.attempt_id || body.task_id || "")
      }:${String(body.check_id || "")}`
      : body.kind === "archive"
      ? `archive:${task.id}:${task.version}`
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
  const addDependency = () =>
    apply({
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
  // Follows whichever explanation the panel would otherwise show.
  const setupProblem = !terminal
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
  const proposals = state.controls.filter((value) =>
    value.attempt_id === attempt?.id && value.kind === "transition_proposal"
  );
  const passedChecks = selectedChecks.filter((selection) =>
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
    document.getElementById(`task-tab-${TABS[next].id}`)?.focus();
  };

  const overview = (
    <div className="task-overview">
      <div className="task-overview-main">
        <section className="panel" aria-labelledby="task-summary-title">
          <h3 id="task-summary-title">Summary</h3>
          {task.description.trim()
            ? <MarkdownContent className="task-description" text={task.description} />
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
          <section className="panel next-step" aria-labelledby="task-waiting-title">
            <h3 id="task-waiting-title">Why it is waiting</h3>
            <p>
              {setupProblem || task.progress?.waiting_reason || waitingReason}
            </p>
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
                    {task.progress?.waiting_reason || waitingReason} · project
                    readiness {project?.trip?.readiness ?? "not reported"}
                  </p>
                </TechnicalDetails>
              </>
            )}
            {task.progress && <ProgressFacts progress={task.progress} />}
          </section>
        )}
        {needsAnswer && (
          <section
            className="panel next-step"
            aria-labelledby="task-question-title"
            data-attention-target={questionSession
              ? `session:${questionSession.id}`
              : undefined}
            tabIndex={-1}
          >
            <h3 id="task-question-title">What the agent reported</h3>
            <ReportList
              content={content}
              error={contentError}
              started={!!attempt}
              onlyLatest
            />
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
                </>
              )
              : question
              ? (
                <p className="hint">
                  The manager that asked is no longer running, so a reply
                  cannot be sent now. Refresh to see the current state, or
                  choose Continue below once you have resolved what it asked
                  for.
                </p>
              )
              : (
                <p className="hint">
                  This wait is not a question the manager can take a reply
                  to. Resolve what the report describes, then choose Continue
                  below.
                </p>
              )}
            {guidance.length > 0 && (
              <ul className="guidance-history">
                {guidance.slice(0, 3).map((item) => (
                  <li key={String(item.id)}>
                    <span>{String(item.body)}</span>
                    <em>{guidanceState(String(item.state))}</em>
                  </li>
                ))}
              </ul>
            )}
          </section>
        )}
        <ReviewPanel
          task={task}
          verification={selectedChecks}
          actions={continuationActions}
          onChanged={onChanged}
        />
        <WorkflowControls
          task={task}
          project={project}
          controls={state.controls}
          sessions={state.active_sessions}
          switches={state.switches}
          actions={continuationActions}
          decision={workflowDecision}
          onOpenSetup={onOpenSetup}
          onChanged={onChanged}
        />
        <RecoveryPanel
          task={task}
          records={state.recovery.filter((value) =>
            value.attempt_id === attempt?.id &&
            value.state === "attention_required"
          )}
          sessions={state.active_sessions}
          selectedRecordId={selectedRecoveryId}
          onChanged={onChanged}
        />
        <section className="panel verification-summary">
          <h3>Verification</h3>
          <p>
            {selectedChecks.length
              ? `${passedChecks} of ${selectedChecks.length} selected checks passed on the current result.`
              : attempt
              ? "No verification checks are selected yet."
              : "Checks are selected when the plan is written."}
          </p>
          <button className="link-button" onClick={() => chooseTab("checks")}>
            Open Checks
          </button>
        </section>
      </div>
    </div>
  );

  const changes = (
    <div className="task-tab-stack">
      <WorkSummary task={task} />
      <section className="panel">
        <h3>Depends on</h3>
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
              {!terminal && !item.verified_at && (
                <div className="compact-fields">
                  <label>
                    Integrated commit or branch
                    <input
                      aria-label={`Integration ref for ${String(item.task_id)}`}
                      value={integrationRef}
                      onChange={(event) => setIntegrationRef(event.target.value)}
                      placeholder="Exact integrated git ref"
                    />
                  </label>
                  <button
                    disabled={!integrationRef}
                    onClick={() =>
                      apply({
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
        {!terminal && (
          <div className="compact-fields">
            <label>
              Add a task this depends on
              <select
                aria-label="Dependency task"
                value={dependency}
                onChange={(event) => setDependency(event.target.value)}
              >
                <option value="">Choose a task in this project</option>
                {state.tasks.filter((value) =>
                  value.id !== task.id && value.project_id === task.project_id
                ).map((value) => (
                  <option value={value.id} key={value.id}>
                    {value.title}
                  </option>
                ))}
              </select>
            </label>
            <button disabled={!dependency} onClick={addDependency}>
              Add
            </button>
          </div>
        )}
      </section>
    </div>
  );

  const checksTab = (
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
                <strong>{check?.original_text || check?.check_key || "Selected check"}</strong>
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
                  {check?.relevant_inputs.join(", ") || "none declared"} ·
                  coverage {run?.acceptance_coverage.length || 0}/
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
                {tripChecks.find((item) => item.id === check.check_id)
                  ?.original_text || String(check.suite_name || check.executable)}
              </strong>
              <span>
                {check.status === "finished"
                  ? check.exit_code === 0 ? "Passed" : `Failed (exit ${String(check.exit_code)})`
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
        <h3>Reviews</h3>
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
          taskTitle={() => undefined}
          setupProjectId={() => task.project_id}
          onOpenSetup={onOpenSetup}
        />
      </section>
      <section className="panel">
        <h3>Reports</h3>
        <ReportList content={content} error={contentError} started={!!attempt} />
      </section>
      <details
        className="panel"
        open={settingsOpen}
        onToggle={(event) => setSettingsOpen(event.currentTarget.open)}
      >
        <summary>Agent settings</summary>
        <RoleSettings
          task={task}
          project={project}
          sessions={state.active_sessions}
          switches={state.switches}
          lanes={tripLanes}
          productionRestrictions={state.production_role_restrictions}
          capabilities={state.capabilities}
          actions={continuationActions}
          onChanged={onChanged}
        />
      </details>
      <details className="panel">
        <summary>Explorer and implementation lanes</summary>
        {tripExplorer.filter((item) => item.attempt_id === attempt?.id).map((
          decision,
        ) => (
          <article className="review-row" key={decision.id}>
            <strong>
              Explorer ·{" "}
              {decision.stage === "planning"
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
                {lane.protected_paths.join(", ") || "none"} · dependencies:{" "}
                {lane.dependencies.join(", ") || "none"} · effective generation
                {" "}
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
          <h3>Created from a recipe</h3>
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
            If the recipe's settings are out of date, save a current profile
            set and recipe, archive this draft, then create a fresh draft.
            Restore keeps the original settings.
          </p>
          <TechnicalDetails>
            <p>
              Exact recipe revision {task.recipe_provenance.recipe_revision_id}
              {" "}· profile revision {task.recipe_provenance.profile_revision_id}
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

  return (
    <div className="task-dialog-backdrop">
      <section
        ref={dialog}
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
            <StatusBadge status={status} />
          </div>
          <p className="task-dialog-status" id="task-dialog-status">
            {status.detail}
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
                            <span className="visually-hidden">: {item.title}</span>
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                </details>
              )}
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
          {tab === "activity" && activity}
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
                  {attempt ? ` · phase ${attempt.phase} · ${attempt.status}` : ""}
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
          {liveSession && tab !== "activity" && (
            <button onClick={() => chooseTab("activity")}>
              View agent output
            </button>
          )}
          <span className="hint">No automatic approvals</span>
        </footer>
      </section>
    </div>
  );
}
