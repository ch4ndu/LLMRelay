import { terminalGuidance } from "../cmuxRouting";
import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import {
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
  useEffect,
  useRef,
  useState,
} from "react";
import { command, getRestartPreview, operation, operationId } from "../api";
import {
  cmuxNewestSurface,
  cmuxOutcomeWithDurableSurface,
  cmuxRouteLabel,
  cmuxSurfacePresentation,
  cmuxViewOutcomeFromSurface,
  recordedOutputText,
} from "../cmuxRouting";
import type {
  AppState,
  AttentionItem,
  AttentionTarget,
  CmuxKeyboardControlAction,
  CmuxKeyboardControlOutcome,
  CmuxSessionSurface,
  CmuxViewOutcome,
  RestartPreview,
  RestartResumeResult,
  Task,
} from "../types";
import { ActivityTimeline } from "./ActivityTimeline";
import { ApprovalInbox } from "./ApprovalInbox";
import { AttentionInbox } from "./AttentionInbox";
import { ResourceStatus } from "./ResourceStatus";
import { SessionTree } from "./SessionTree";

const savedSideWidth = () => {
  const value = Number(localStorage.getItem("agenticjira.workspace.sideWidth"));
  return Number.isFinite(value) ? Math.min(520, Math.max(260, value)) : 330;
};

export function Workspace(
  {
    state,
    onSelect,
    onChanged,
    onOpenSetup = () => {},
    onNavigateAttention = () => undefined,
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
  const [sideWidth, setSideWidth] = useState(savedSideWidth);
  const [restoreBusy, setRestoreBusy] = useState(false);
  const [autoResumeError, setAutoResumeError] = useState("");
  const [preview, setPreview] = useState<RestartPreview>();
  const [previewBusy, setPreviewBusy] = useState(false);
  const [previewError, setPreviewError] = useState("");
  const [resumeResult, setResumeResult] = useState<RestartResumeResult>();
  const [resumeBusy, setResumeBusy] = useState(false);
  const [resumeError, setResumeError] = useState("");
  const resumeOperation = useRef<string | undefined>(undefined);
  const [routes, setRoutes] = useState<Record<string, CmuxViewOutcome>>({});
  const [discardingRoutes, setDiscardingRoutes] = useState<
    Record<string, boolean>
  >({});
  const discardOperations = useRef<
    Record<
      string,
      { surfaceRouteId: string; sessionId: string; operationId: string }
    >
  >({});
  const durableSurfaces = useRef<Record<string, CmuxSessionSurface>>({});
  for (const session of state.active_sessions) {
    if (!session.cmux_surface) continue;
    const latest = cmuxNewestSurface(
      durableSurfaces.current[session.id],
      session.cmux_surface,
    );
    if (latest) durableSurfaces.current[session.id] = latest;
  }
  const durableSurfaceForSession = (sessionId: string) =>
    durableSurfaces.current[sessionId] ||
    state.active_sessions.find((session) => session.id === sessionId)
      ?.cmux_surface;
  const commitRoute = (sessionId: string, outcome: CmuxViewOutcome) => {
    const committed = cmuxOutcomeWithDurableSurface(
      outcome,
      durableSurfaceForSession(sessionId),
    ) || outcome;
    setRoutes((current) => ({ ...current, [sessionId]: committed }));
    return committed;
  };

  useEffect(() => {
    const keys = Array.from(
      { length: localStorage.length },
      (_, index) => localStorage.key(index),
    );
    for (const key of keys) {
      if (typeof key === "string" && key.startsWith("agenticjira.terminal.")) {
        localStorage.removeItem(key);
      }
    }
  }, []);
  useEffect(() => {
    localStorage.setItem("agenticjira.workspace.sideWidth", String(sideWidth));
  }, [sideWidth]);

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
    return { action, candidate, task };
  });
  const adjustSide = (value: number) =>
    setSideWidth(Math.min(520, Math.max(260, value)));
  const startResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    const startX = event.clientX;
    const startWidth = sideWidth;
    const move = (next: PointerEvent) =>
      adjustSide(startWidth + startX - next.clientX);
    const stop = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
      window.removeEventListener("pointercancel", stop);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
    window.addEventListener("pointercancel", stop);
  };

  const openCmuxSession = async (sessionId: string) => {
    const key = sessionId;
    setRoutes((current) => ({
      ...current,
      [key]: {
        state: "pending",
        message: "Reserving the exact persistent cmux presentation…",
        retry_available: false,
      },
    }));
    try {
      const result = commitRoute(
        key,
        cmuxViewOutcomeFromSurface(
          await onViewCmuxSession(sessionId),
        ),
      );
      onChanged();
      return result;
    } catch (cause) {
      const result: CmuxViewOutcome = {
        state: "failed",
        message: cause instanceof Error ? cause.message : String(cause),
        retry_available: true,
      };
      return commitRoute(key, result);
    }
  };

  const routeSurface = (sessionId: string) =>
    cmuxOutcomeWithDurableSurface(
      routes[sessionId],
      durableSurfaceForSession(sessionId),
    )?.surface || durableSurfaceForSession(sessionId);

  const setKeyboardControl = async (
    sessionId: string,
    surface: CmuxSessionSurface,
    action: CmuxKeyboardControlAction,
  ) => {
    const currentSurface = cmuxNewestSurface(
      durableSurfaceForSession(sessionId),
      surface,
    ) || surface;
    const presentation = cmuxSurfacePresentation(currentSurface);
    if (
      action === "acquire"
        ? !presentation.takeAvailable
        : !presentation.releaseAvailable
    ) return;
    setRoutes((current) => ({
      ...current,
      [sessionId]: {
        state: "pending",
        message: action === "acquire"
          ? "Keyboard control is pending on the existing authenticated cmux attachment…"
          : "Keyboard-control release is pending on the existing authenticated cmux attachment…",
        retry_available: false,
        surface: currentSurface,
      },
    }));
    try {
      const result = await onSetCmuxKeyboardControl(
        sessionId,
        currentSurface,
        action,
      );
      const outcome = commitRoute(
        sessionId,
        cmuxViewOutcomeFromSurface({
          state: result.state === "retired" ? "failed" : result.state,
          message: result.message,
          retry_available: result.state !== "retired",
          surface: result.surface,
        }),
      );
      onChanged();
      return outcome;
    } catch (cause) {
      const outcome = commitRoute(
        sessionId,
        cmuxViewOutcomeFromSurface({
          state: "failed",
          message: cause instanceof Error ? cause.message : String(cause),
          retry_available: true,
          surface: currentSurface,
        }),
      );
      return outcome;
    }
  };

  const takeKeyboardControl = async (sessionId: string) => {
    const viewed = await openCmuxSession(sessionId);
    const surface = viewed.surface || routeSurface(sessionId);
    if (!surface || !cmuxSurfacePresentation(surface).takeAvailable) return;
    await setKeyboardControl(sessionId, surface, "acquire");
  };

  const releaseKeyboardControl = async (sessionId: string) => {
    const surface = routeSurface(sessionId);
    if (!surface) return;
    await setKeyboardControl(sessionId, surface, "release");
  };

  const discardUnknownRoute = async (
    key: string,
    sessionId: string,
    route: CmuxViewOutcome,
  ) => {
    const currentRoute = cmuxOutcomeWithDurableSurface(
      route,
      durableSurfaceForSession(sessionId),
    ) || route;
    if (
      !currentRoute.surface ||
      !cmuxSurfacePresentation(currentRoute.surface).discardAvailable
    ) return;
    const prior = discardOperations.current[key];
    const retry = prior?.surfaceRouteId === currentRoute.surface.id &&
        prior.sessionId === sessionId
      ? prior
      : {
        surfaceRouteId: currentRoute.surface.id,
        sessionId,
        operationId: operationId(),
      };
    discardOperations.current[key] = retry;
    setDiscardingRoutes((current) => ({ ...current, [key]: true }));
    try {
      const result = await onDiscardCmuxSurface(
        sessionId,
        retry.surfaceRouteId,
        retry.operationId,
      );
      delete discardOperations.current[key];
      commitRoute(key, cmuxViewOutcomeFromSurface(result));
      onChanged();
    } catch (cause) {
      commitRoute(key, {
        ...currentRoute,
        message: cause instanceof Error ? cause.message : String(cause),
      });
    } finally {
      setDiscardingRoutes((current) => ({ ...current, [key]: false }));
    }
  };

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

  return (
    <div
      className="workspace"
      style={{ "--workspace-side-width": `${sideWidth}px` } as CSSProperties}
    >
      <div className="workspace-main">
        <div className="workspace-toolbar">
          <div>
            <span className="eyebrow">Live workspace</span>
            <h1>Roles and session access</h1>
            <p className="muted">
              View live output in cmux, and take keyboard control only when you
              explicitly need to answer a native prompt. Session lifecycle and
              workflow approvals remain in LLMRelay.
            </p>
            <details className="workspace-instructions">
              <summary>How session access works</summary>
              <p className="muted">
                View output reserves or focuses one mode-neutral task surface
                and never changes an existing lease. Take and Release are
                separate revision-bound human actions applied by that same
                authenticated attachment. Ctrl-] detaches and releases control,
                if held, without stopping the service-owned provider. Reopening
                a presentation never starts the host, resumes a provider, or
                bypasses dashboard permissions and workflow approvals.
              </p>
            </details>
          </div>
          <fieldset className="restore-controls">
            <legend>Restart restoration</legend>
            <label>
              <input
                type="checkbox"
                checked={state.instance_settings.auto_resume_eligible}
                disabled={restoreBusy}
                onChange={(event) => void setAutoResume(event.target.checked)}
              />
              Auto-resume eligible work
            </label>
            <small>
              Open each restart candidate below for its exact current recovery
              action. Automatic restoration still rechecks the same frozen
              identity and current authority before any resume.
            </small>
            <button
              disabled={previewBusy}
              onClick={() => void requestPreview()}
            >
              {previewBusy ? "Checking restart preview…" : "Preview restart"}
            </button>
            <button
              disabled={resumeBusy || state.restart_candidates.length === 0}
              onClick={() => void resumeEligible()}
            >
              {resumeBusy ? "Submitting resume…" : "Resume eligible"}
            </button>
            {resumeError && <ErrorNotice error={resumeError} />}
            {previewError && (
              <ErrorNotice error={previewError} />
            )}
            {autoResumeError && <ErrorNotice error={autoResumeError} />}
          </fieldset>
        </div>
        <SessionTree
          sessions={state.active_sessions}
          routes={routes}
          onView={(sessionId) => void openCmuxSession(sessionId)}
          onTake={(sessionId) => void takeKeyboardControl(sessionId)}
          onRelease={(sessionId) => void releaseKeyboardControl(sessionId)}
          setupProjectId={(session) =>
            state.trip_setups?.find((setup) =>
              setup.setup_operation_id === session.setup_operation_id
            )?.project_id ||
            state.tasks.find((task) => task.id === session.task_id)?.project_id}
          onOpenSetup={onOpenSetup}
        />
        {Object.entries(routes).map(([key, localRoute]) => {
          const sessionId = key;
          const route = cmuxOutcomeWithDurableSurface(
            localRoute,
            durableSurfaceForSession(sessionId),
          ) || localRoute;
          const output = recordedOutputText(route);
          const presentation = cmuxSurfacePresentation(
            route.surface,
            route.state === "pending",
          );
          const retryAvailable = route.surface
            ? presentation.retryAvailable
            : route.retry_available;
          return (
            <section
              className={`panel cmux-route ${route.state}`}
              aria-live="polite"
              key={key}
            >
              <header>
                <h3>Task terminal</h3>
                <span>{cmuxRouteLabel(route.state)}</span>
              </header>
              <p>{terminalGuidance(route)}</p>
              <TechnicalDetails>
        <p>{route.message}</p>
        {route.surface && <p>
          route revision {route.surface.binding_revision} · surface {route.surface.surface_state} · attachment {route.surface.attachment_state} · desired {route.surface.desired_input_state} · actual {route.surface.actual_input_state} · control revision {route.surface.applied_revision}/{route.surface.control_revision}
        </p>}
        {presentation.diagnostic && <p>{presentation.diagnostic}</p>}
      </TechnicalDetails>
      {output && <pre className="recorded-output">{output}</pre>}
      {route.surface && presentation.discardAvailable && (
                <button
                  disabled={discardingRoutes[key]}
                  onClick={() =>
                    void discardUnknownRoute(key, sessionId, route)}
                >
                  {discardingRoutes[key]
                    ? "Discarding unknown reservation…"
                    : "Discard unknown reservation"}
                </button>
              )}
              {retryAvailable && (
                <button
                  onClick={() => void openCmuxSession(sessionId)}
                >
                  {route.surface ? presentation.viewLabel : "View output again"}
                </button>
              )}
            </section>
          );
        })}
        {state.restart_candidates.length > 0 && (
          <section
            className="panel restart-candidates"
            aria-label="Restart candidates"
          >
            <header>
              <h3>Restart candidates</h3>
              <span>{state.restart_candidates.length}</span>
            </header>
            <p className="muted">
              Resume restores the exact native session. Continue releases a
              restart hold only after all prior processes are confirmed stopped,
              allowing the normal workflow to proceed.
            </p>
            {restartCandidateRows.slice(0, 20).map(({
              action,
              candidate,
              task,
            }) => (
              <article className="restart-candidate" key={candidate.session_id}>
                <span>
                  <strong>{candidate.session_id}</strong> · {candidate.state} ·
                  {" "}
                  {candidate.reason}
                </span>
                <small>
                  {action?.reason ||
                    "No current continuation action is projected for this candidate; refresh state before taking action."}
                </small>
                {task && (
                  <button onClick={() => onSelect(task)}>
                    Open task recovery
                  </button>
                )}
              </article>
            ))}
          </section>
        )}
        {preview && (
          <section
            className="panel restart-preview"
            aria-label="Restart preview"
          >
            <h3>Restart preview</h3>
            <p>{preview.snapshot.notice}</p>
            <small>
              Observed {preview.snapshot.captured_at} · process inventory{" "}
              {preview.snapshot.process_inventory} · boot identity{" "}
              {preview.snapshot.boot_identity}. This preview grants no resume
              authority.
            </small>
            {preview.sessions.map((session, index) => (
              <article
                className="restart-candidate"
                key={`${session.decision.subject.session_id || index}`}
              >
                <strong>
                  {session.decision.subject.session_id || "Unknown session"} ·
                  {" "}
                  {session.classification.replaceAll("_", " ")}
                </strong>
                <small>
                  {session.decision.primary_blocker?.message ||
                    session.decision.reason_code}
                </small>
                <small>
                  {session.can_resume_now
                    ? "Admission may be available after current revalidation"
                    : session.could_resume_after_confirmed_shutdown
                    ? "Needs verified quiescence before eligibility"
                    : "No current resume route"}
                </small>
              </article>
            ))}
          </section>
        )}
        {resumeResult && (
          <section
            className="panel restart-resume-result"
            aria-label="Restart resume result"
          >
            <h3>Restart resume · {resumeResult.state}</h3>
            <p>
              Queued {resumeResult.queued_ids.length}:{" "}
              {resumeResult.queued_ids.join(", ") || "none"}. Omitted{" "}
              {resumeResult.omitted_count}:{" "}
              {resumeResult.omitted_ids.join(", ") || "none"}.
            </p>
            {resumeResult.outcomes.map((outcome, index) => (
              <small key={`${outcome.session_id}:${index}`}>
                {outcome.session_id}: {outcome.state}
                {outcome.reason ? ` · ${outcome.reason}` : ""}
                {outcome.next_due_at
                  ? ` · retry after ${outcome.next_due_at}`
                  : ""}
              </small>
            ))}
          </section>
        )}
        <ActivityTimeline events={state.history} />
      </div>
      <div
        className="workspace-divider"
        role="separator"
        aria-label="Resize workspace details"
        aria-orientation="vertical"
        aria-valuemin={260}
        aria-valuemax={520}
        aria-valuenow={sideWidth}
        tabIndex={0}
        onPointerDown={startResize}
        onKeyDown={(event) => {
          if (event.key === "ArrowLeft") adjustSide(sideWidth + 16);
          else if (event.key === "ArrowRight") adjustSide(sideWidth - 16);
          else if (event.key === "Home") adjustSide(260);
          else if (event.key === "End") adjustSide(520);
          else return;
          event.preventDefault();
        }}
      />
      <aside className="workspace-side">
        <ApprovalInbox
          state={state}
          onSelect={(task) => onSelect(task)}
          onChanged={onChanged}
        />
        <AttentionInbox
          state={state}
          onNavigate={onNavigateAttention}
          onChanged={onChanged}
        />
        <ResourceStatus resources={state.resources} />
      </aside>
    </div>
  );
}
