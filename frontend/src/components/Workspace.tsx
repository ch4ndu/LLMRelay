import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { useRef, useState } from "react";
import { command, getRestartPreview, operation, operationId } from "../api";
import {
  type AppState,
  type AttentionItem,
  type AttentionTarget,
  type CmuxKeyboardControlAction,
  type CmuxKeyboardControlOutcome,
  type CmuxSessionSurface,
  type CmuxViewOutcome,
  type ContinuationActionKind,
  type RestartPreview,
  type RestartResumeResult,
  roleLabel,
  type Task,
  type TaskAction,
} from "../types";
import { ActivityTimeline } from "./ActivityTimeline";
import { ApprovalInbox, pendingApprovalCount } from "./ApprovalInbox";
import { AttentionInbox } from "./AttentionInbox";
import { ResourceStatus } from "./ResourceStatus";
import { SessionTree, useSessionAccess } from "./SessionTree";
import { TaskSections } from "./TaskBoard";

const settledRestartStates = ["resumed", "released_fresh_dispatch", "cancelled"];

/** The restart steps you can take, named by what the service projects. */
const restartStepLabels: Partial<Record<ContinuationActionKind, string>> = {
  exact_resume: "Can resume the same conversation",
  continue_fresh_dispatch: "Can continue with a new session",
  recover_ownership: "Needs a recovery check before it can continue",
};

export function Workspace(
  {
    state,
    onSelect,
    onChanged,
    onOpenSetup = () => {},
    onNavigateAttention = () => undefined,
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
    state: AppState;
    onSelect: (task: Task) => void;
    onChanged: () => void;
    onOpenSetup?: (projectId: string) => void;
    onNavigateAttention?: (
      item: AttentionItem,
      target: AttentionTarget,
    ) => string | undefined;
    onTaskAction?: (action: TaskAction) => void;
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
  const [restoreBusy, setRestoreBusy] = useState(false);
  const [autoResumeError, setAutoResumeError] = useState("");
  const [preview, setPreview] = useState<RestartPreview>();
  const [previewBusy, setPreviewBusy] = useState(false);
  const [previewError, setPreviewError] = useState("");
  const [resumeResult, setResumeResult] = useState<RestartResumeResult>();
  const [resumeBusy, setResumeBusy] = useState(false);
  const [resumeError, setResumeError] = useState("");
  const resumeOperation = useRef<string | undefined>(undefined);
  const access = useSessionAccess(state.active_sessions, {
    onView: onViewCmuxSession,
    onSetKeyboardControl: onSetCmuxKeyboardControl,
    onDiscard: onDiscardCmuxSurface,
    onChanged,
  });
  const taskById = (taskId: string) =>
    state.tasks.find((task) => task.id === taskId);
  const liveSessions = state.active_sessions.filter((session) =>
    !["exited", "launch_failed"].includes(session.status)
  );
  const pastSessions = state.active_sessions.filter((session) =>
    ["exited", "launch_failed"].includes(session.status)
  );

  const restartCandidateRows = state.restart_candidates.map((candidate) => {
    const action = state.continuation_actions.find((candidateAction) =>
      candidateAction.binding.session_id === candidate.session_id &&
      candidateAction.binding.attempt_id === candidate.attempt_id &&
      candidateAction.binding.task_id === candidate.task_id
    );
    const task = state.tasks.find((candidateTask) =>
      candidateTask.id === candidate.task_id &&
      candidateTask.active_attempt?.id === candidate.attempt_id
    );
    const session = state.active_sessions.find((item) =>
      item.id === candidate.session_id
    );
    return { action, candidate, task, session };
  });
  // Urgent only when the row still belongs to an unfinished task's current
  // attempt and the service offers an enabled step you can take for exactly
  // that session; waits, reconciliation and terminal rows are history.
  const currentRestarts = restartCandidateRows.filter(({ action, candidate, task }) =>
    !settledRestartStates.includes(candidate.state) && !!task && !task.archived &&
    !["done", "cancelled"].includes(task.lifecycle) &&
    !!action?.enabled && action.kind in restartStepLabels
  );
  const earlierRestarts = restartCandidateRows.filter((row) =>
    !currentRestarts.includes(row)
  );
  const bulkResumable = currentRestarts.some(({ action }) =>
    action?.kind === "exact_resume" && action.operation === "restart_resume"
  );

  const setAutoResume = async (enabled: boolean) => {
    setRestoreBusy(true);
    setAutoResumeError("");
    try {
      await command({
        kind: "set_auto_resume",
        operation_id: operationId(),
        expected_version: state.instance_settings.version,
        enabled,
      });
      onChanged();
    } catch (cause) {
      setAutoResumeError(
        cause instanceof Error ? cause.message : String(cause),
      );
      await onChanged();
    } finally {
      setRestoreBusy(false);
    }
  };
  const requestPreview = async () => {
    setPreviewBusy(true);
    setPreviewError("");
    setPreview(undefined);
    try {
      setPreview(await getRestartPreview());
    } catch (cause) {
      setPreviewError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setPreviewBusy(false);
    }
  };
  const resumeEligible = async () => {
    const id = resumeOperation.current || operationId();
    resumeOperation.current = id;
    setResumeBusy(true);
    setResumeError("");
    try {
      const result = await operation<RestartResumeResult>({
        kind: "restart_resume",
        operation_id: id,
      });
      if (
        !Array.isArray(result.queued_ids) ||
        !Array.isArray(result.omitted_ids) ||
        !Array.isArray(result.outcomes) ||
        typeof result.omitted_count !== "number"
      ) {
        throw new Error(
          "Restart resume returned an unsupported result; refresh before retrying the same operation ID.",
        );
      }
      setResumeResult(result);
      resumeOperation.current = undefined;
      onChanged();
    } catch (cause) {
      setResumeError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setResumeBusy(false);
    }
  };
  const sessionLabel = (sessionId: string) => {
    const session = state.active_sessions.find((item) => item.id === sessionId);
    const task = session && taskById(session.task_id);
    return session
      ? `${roleLabel(session.role)}${task ? ` · ${task.title}` : ""}`
      : "Earlier session";
  };
  const restartRow = (
    { action, candidate, task, session }: typeof restartCandidateRows[number],
    current: boolean,
  ) => (
    <article className="restart-candidate" key={candidate.session_id}>
      <div>
        <strong>
          {session ? roleLabel(session.role) : "Agent"} ·{" "}
          {task?.title || taskById(candidate.task_id)?.title || "Earlier task"}
        </strong>
        {current && action && (
          <span className="restart-step">{restartStepLabels[action.kind]}</span>
        )}
        <small>
          {current
            ? action?.reason
            : "No step is offered for this session now. It is kept here for reference."}
        </small>
        <TechnicalDetails>
          <p>
            Session {candidate.session_id} · {candidate.state} ·{" "}
            {candidate.reason}
          </p>
        </TechnicalDetails>
      </div>
      {task && (
        <button onClick={() => onSelect(task)}>
          Open task<span className="visually-hidden">: {task.title}</span>
        </button>
      )}
    </article>
  );
  const approvalCount = pendingApprovalCount(state);

  return (
    <div className="workspace">
      <header className="page-heading workspace-heading">
        <div>
          <span className="eyebrow">Workspace</span>
          <h1>What's happening</h1>
          <p>
            What is waiting for you comes first, then active work and the
            agents running it.
          </p>
        </div>
        <nav className="waiting-summary" aria-label="Waiting for you">
          <a href="#workspace-attention" className={state.attention.length ? "has-items" : ""}>
            Needs your attention
            <span className="count">{state.attention.length}</span>
          </a>
          <a href="#workspace-approvals" className={approvalCount ? "has-items" : ""}>
            Approvals<span className="count">{approvalCount}</span>
          </a>
          <a href="#workspace-tasks">
            Active tasks
            <span className="count">
              {state.tasks.filter((task) =>
                !task.archived && !["done", "cancelled"].includes(task.lifecycle)
              ).length}
            </span>
          </a>
        </nav>
      </header>
      <div className="workspace-inboxes">
        <AttentionInbox
          state={state}
          onNavigate={onNavigateAttention}
          onChanged={onChanged}
        />
        <ApprovalInbox
          state={state}
          onSelect={(task) => onSelect(task)}
          onChanged={onChanged}
        />
      </div>
      {currentRestarts.length > 0 && (
        <section
          className="panel restart-candidates"
          aria-label="Restart recovery"
        >
          <header>
            <h2>Waiting after restart</h2>
            <span className="count">{currentRestarts.length}</span>
          </header>
          <p className="hint">
            These sessions were running when LLMRelay stopped. Each shows the
            next step LLMRelay offers for it; open its task to take that step.
          </p>
          {currentRestarts.slice(0, 20).map((row) => restartRow(row, true))}
        </section>
      )}
      <TaskSections
        tasks={state.tasks}
        projects={state.projects}
        sessions={state.active_sessions}
        taskActions={state.task_actions}
        onOpen={onSelect}
        onTaskAction={onTaskAction}
      />
      <section className="panel workspace-sessions" aria-labelledby="workspace-sessions-title">
        <header>
          <h2 id="workspace-sessions-title">Agents</h2>
        </header>
        <SessionTree
          title="Running now"
          empty="No agent is running right now."
          sessions={liveSessions}
          access={access}
          taskTitle={(session) => taskById(session.task_id)?.title}
          setupProjectId={(session) =>
            state.trip_setups?.find((setup) =>
              setup.setup_operation_id === session.setup_operation_id
            )?.project_id ||
            taskById(session.task_id)?.project_id}
          onOpenSetup={onOpenSetup}
        />
        {pastSessions.length > 0 && (
          <details className="past-sessions">
            <summary>Earlier sessions ({pastSessions.length})</summary>
            <SessionTree
              title="Earlier sessions"
              sessions={pastSessions}
              access={access}
              taskTitle={(session) => taskById(session.task_id)?.title}
              setupProjectId={(session) =>
                state.trip_setups?.find((setup) =>
                  setup.setup_operation_id === session.setup_operation_id
                )?.project_id ||
                taskById(session.task_id)?.project_id}
              onOpenSetup={onOpenSetup}
            />
          </details>
        )}
      </section>
      <details className="panel restart-tools">
        <summary>Restart and recovery tools</summary>
        <fieldset className="restore-controls">
          <legend>After LLMRelay restarts</legend>
          <label>
            <input
              type="checkbox"
              checked={state.instance_settings.auto_resume_eligible}
              disabled={restoreBusy}
              onChange={(event) => void setAutoResume(event.target.checked)}
            />
            Resume eligible work automatically
          </label>
          <small>
            Automatic resume still rechecks each session before continuing it.
            Open a task above to handle its session yourself.
          </small>
          <div className="button-row">
            <button
              disabled={previewBusy}
              onClick={() => void requestPreview()}
            >
              {previewBusy ? "Checking…" : "Preview restart"}
            </button>
            <button
              disabled={resumeBusy || !bulkResumable}
              onClick={() => void resumeEligible()}
            >
              {resumeBusy ? "Submitting resume…" : "Resume eligible sessions"}
            </button>
          </div>
          {resumeError && <ErrorNotice error={resumeError} />}
          {previewError && <ErrorNotice error={previewError} />}
          {autoResumeError && <ErrorNotice error={autoResumeError} />}
        </fieldset>
        {earlierRestarts.length > 0 && (
          <details className="restart-history">
            <summary>Earlier restart sessions ({earlierRestarts.length})</summary>
            {earlierRestarts.slice(0, 20).map((row) => restartRow(row, false))}
          </details>
        )}
        {preview && (
          <section className="restart-preview" aria-label="Restart preview">
            <h3>Restart preview</h3>
            <p>{preview.snapshot.notice}</p>
            <small>This preview does not resume anything.</small>
            {preview.sessions.map((session, index) => (
              <article
                className="restart-candidate"
                key={`${session.decision.subject.session_id || index}`}
              >
                <strong>
                  {session.decision.subject.session_id
                    ? sessionLabel(session.decision.subject.session_id)
                    : "Unknown session"}
                </strong>
                <small>
                  {session.can_resume_now
                    ? "Can resume after LLMRelay rechecks it"
                    : session.could_resume_after_confirmed_shutdown
                    ? "Can resume once its earlier process is confirmed stopped"
                    : "Cannot be resumed"}
                  {session.decision.primary_blocker?.message
                    ? ` · ${session.decision.primary_blocker.message}`
                    : ""}
                </small>
                <TechnicalDetails>
                  <p>
                    {session.classification.replaceAll("_", " ")} ·{" "}
                    {session.decision.reason_code} · observed{" "}
                    {preview.snapshot.captured_at} · process inventory{" "}
                    {preview.snapshot.process_inventory} · boot identity{" "}
                    {preview.snapshot.boot_identity}
                  </p>
                </TechnicalDetails>
              </article>
            ))}
          </section>
        )}
        {resumeResult && (
          <section
            className="restart-resume-result"
            aria-label="Restart resume result"
          >
            <h3>Resume result</h3>
            <p>
              {resumeResult.queued_ids.length} session(s) queued to resume;{" "}
              {resumeResult.omitted_count} not resumed.
            </p>
            {resumeResult.outcomes.map((outcome, index) => (
              <small key={`${outcome.session_id}:${index}`}>
                {sessionLabel(outcome.session_id)}:{" "}
                {outcome.state.replaceAll("_", " ")}
                {outcome.reason ? ` · ${outcome.reason}` : ""}
                {outcome.next_due_at
                  ? ` · tries again after ${
                    new Date(outcome.next_due_at).toLocaleString()
                  }`
                  : ""}
              </small>
            ))}
          </section>
        )}
      </details>
      <details className="panel workspace-activity">
        <summary>Recent activity and resources</summary>
        <ActivityTimeline events={state.history} tasks={state.tasks} />
        <ResourceStatus resources={state.resources} />
      </details>
    </div>
  );
}
