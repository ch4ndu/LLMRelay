import { TechnicalDetails } from "./ErrorNotice";
import { type HistoryPaneRequest, SessionHistory } from "./SessionHistory";
import {
  FailedSessionStopNotice,
  pendingReleasesWithoutHold,
  ProviderFailureHoldNotice,
} from "./SessionFailureNotices";
import { useEffect, useRef, useState } from "react";
import {
  ApiError,
  command,
  operationId,
} from "../api";
import {
  cmuxNewestSurface,
  cmuxOutcomeWithDurableSurface,
  cmuxRouteLabel,
  cmuxSurfaceOutcome,
  cmuxSurfacePresentation,
  cmuxViewOutcomeFromSurface,
  recordedOutputText,
  terminalGuidance,
} from "../cmuxRouting";
import {
  type CmuxKeyboardControlAction,
  type CmuxKeyboardControlOutcome,
  type CmuxSessionSurface,
  type CmuxViewOutcome,
  type NativePrompt,
  type NativePromptDismissal,
  type NativeTurnFailureKind,
  type ObservedUsage,
  type PermissionRequest,
  type ProviderFailureHold,
  roleLabel,
  type Session,
} from "../types";

export interface SessionAccessHandlers {
  onView: (sessionId: string) => Promise<CmuxViewOutcome>;
  onSetKeyboardControl: (
    sessionId: string,
    surface: CmuxSessionSurface,
    action: CmuxKeyboardControlAction,
  ) => Promise<CmuxKeyboardControlOutcome>;
  onDiscard: (
    sessionId: string,
    surfaceRouteId: string,
    operationId: string,
  ) => Promise<CmuxViewOutcome>;
  onChanged: () => void;
}

export interface SessionAccess {
  routes: Record<string, CmuxViewOutcome>;
  discarding: Record<string, boolean>;
  routeFor: (sessionId: string) => CmuxViewOutcome | undefined;
  view: (sessionId: string) => Promise<CmuxViewOutcome>;
  take: (sessionId: string) => Promise<void>;
  release: (sessionId: string) => Promise<void>;
  discard: (sessionId: string) => Promise<void>;
  /** Hides the output locally. The session and any keyboard control continue. */
  close: (sessionId: string) => void;
  refresh: () => void;
}

/**
 * Output and keyboard-control state for sessions shown in one view. Every
 * presentation derives from the newest durable surface; a local operation
 * result is only a transient overlay until the next refresh.
 */
export function useSessionAccess(
  sessions: Session[],
  handlers: SessionAccessHandlers,
): SessionAccess {
  const [routes, setRoutes] = useState<Record<string, CmuxViewOutcome>>({});
  const [discarding, setDiscarding] = useState<Record<string, boolean>>({});
  const discardOperations = useRef<
    Record<
      string,
      { surfaceRouteId: string; sessionId: string; operationId: string }
    >
  >({});
  const durableSurfaces = useRef<Record<string, CmuxSessionSurface>>({});
  for (const session of sessions) {
    if (!session.cmux_surface) continue;
    const latest = cmuxNewestSurface(
      durableSurfaces.current[session.id],
      session.cmux_surface,
    );
    if (latest) durableSurfaces.current[session.id] = latest;
  }
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
  const durableSurfaceForSession = (sessionId: string) =>
    durableSurfaces.current[sessionId] ||
    sessions.find((session) => session.id === sessionId)?.cmux_surface;
  const routeFor = (sessionId: string) => {
    const local = routes[sessionId];
    return local &&
      (cmuxOutcomeWithDurableSurface(local, durableSurfaceForSession(sessionId)) ||
        local);
  };
  const commitRoute = (sessionId: string, outcome: CmuxViewOutcome) => {
    const committed = cmuxOutcomeWithDurableSurface(
      outcome,
      durableSurfaceForSession(sessionId),
    ) || outcome;
    setRoutes((current) => ({ ...current, [sessionId]: committed }));
    return committed;
  };
  const routeSurface = (sessionId: string) =>
    cmuxOutcomeWithDurableSurface(
      routes[sessionId],
      durableSurfaceForSession(sessionId),
    )?.surface || durableSurfaceForSession(sessionId);

  const view = async (sessionId: string) => {
    setRoutes((current) => ({
      ...current,
      [sessionId]: {
        state: "pending",
        message: "Reserving the exact persistent cmux presentation…",
        retry_available: false,
      },
    }));
    try {
      const result = commitRoute(
        sessionId,
        cmuxViewOutcomeFromSurface(await handlers.onView(sessionId)),
      );
      handlers.onChanged();
      return result;
    } catch (cause) {
      return commitRoute(sessionId, {
        state: "failed",
        message: cause instanceof Error ? cause.message : String(cause),
        retry_available: true,
      });
    }
  };

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
      const result = await handlers.onSetKeyboardControl(
        sessionId,
        currentSurface,
        action,
      );
      commitRoute(
        sessionId,
        cmuxViewOutcomeFromSurface({
          state: result.state === "retired" ? "failed" : result.state,
          message: result.message,
          retry_available: result.state !== "retired",
          surface: result.surface,
        }),
      );
      handlers.onChanged();
    } catch (cause) {
      commitRoute(
        sessionId,
        cmuxViewOutcomeFromSurface({
          state: "failed",
          message: cause instanceof Error ? cause.message : String(cause),
          retry_available: true,
          surface: currentSurface,
        }),
      );
    }
  };

  const take = async (sessionId: string) => {
    const viewed = await view(sessionId);
    const surface = viewed.surface || routeSurface(sessionId);
    if (!surface || !cmuxSurfacePresentation(surface).takeAvailable) return;
    await setKeyboardControl(sessionId, surface, "acquire");
  };

  const release = async (sessionId: string) => {
    const surface = routeSurface(sessionId);
    if (!surface) return;
    await setKeyboardControl(sessionId, surface, "release");
  };

  const discard = async (sessionId: string) => {
    const localRoute = routes[sessionId];
    const durable = durableSurfaceForSession(sessionId);
    // A stored unknown reservation is discardable without first opening the
    // output, which is unavailable while that reservation stands.
    const currentRoute = localRoute
      ? cmuxOutcomeWithDurableSurface(localRoute, durable) || localRoute
      : durable && cmuxSurfaceOutcome(durable);
    if (
      !currentRoute ||
      !currentRoute.surface ||
      !cmuxSurfacePresentation(currentRoute.surface).discardAvailable
    ) return;
    const prior = discardOperations.current[sessionId];
    const retry = prior?.surfaceRouteId === currentRoute.surface.id &&
        prior.sessionId === sessionId
      ? prior
      : {
        surfaceRouteId: currentRoute.surface.id,
        sessionId,
        operationId: operationId(),
      };
    discardOperations.current[sessionId] = retry;
    setDiscarding((current) => ({ ...current, [sessionId]: true }));
    try {
      const result = await handlers.onDiscard(
        sessionId,
        retry.surfaceRouteId,
        retry.operationId,
      );
      delete discardOperations.current[sessionId];
      commitRoute(sessionId, cmuxViewOutcomeFromSurface(result));
      handlers.onChanged();
    } catch (cause) {
      commitRoute(sessionId, {
        ...currentRoute,
        message: cause instanceof Error ? cause.message : String(cause),
      });
    } finally {
      setDiscarding((current) => ({ ...current, [sessionId]: false }));
    }
  };

  const close = (sessionId: string) =>
    setRoutes((current) => {
      const { [sessionId]: _closed, ...rest } = current;
      return rest;
    });

  return {
    routes,
    discarding,
    routeFor,
    view,
    take,
    release,
    discard,
    close,
    refresh: handlers.onChanged,
  };
}

export const nativeTurnFailureLabel: Record<NativeTurnFailureKind, string> = {
  authentication_failed: "sign-in failed",
  oauth_org_not_allowed: "this sign-in's organization is not allowed",
  account_on_hold: "the account is on hold",
  verification_required: "the account needs verification",
  billing_error: "a billing problem",
  rate_limit: "a rate or usage limit was reached",
  overloaded: "the model is overloaded or at capacity",
  usage_limit_exceeded: "the usage allowance or plan access does not permit this request",
  invalid_request: "the provider rejected the request",
  model_not_found: "the model is not available",
  server_error: "a provider server error",
  max_output_tokens: "the output limit was reached",
  cloud_credential_error: "a cloud credential problem",
  unknown: "an unrecognized provider error",
};

export const nativePromptLabel: Record<NativePrompt["kind"], string> = {
  permission_prompt: "asking for permission in its own terminal",
  elicitation_dialog: "showing a question in its own terminal",
  elicitation_url_dialog: "asking you to open a link in its own terminal",
  agent_needs_input: "waiting for your input in its own terminal",
};

function NativePromptNotice({ prompt, onChanged }: { prompt: NativePrompt; onChanged: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [uncertain, setUncertain] = useState(false);
  const request = useRef<({ operation_id: string } & NativePromptDismissal) | undefined>(undefined);
  const bindingKey = JSON.stringify(prompt.dismissal);
  useEffect(() => {
    if (!request.current) setError("");
  }, [bindingKey]);
  const dismiss = async () => {
    if (busy) return;
    const captured = request.current ?? (prompt.dismissal
      ? { ...prompt.dismissal, operation_id: operationId() }
      : undefined);
    if (!captured) return;
    request.current = captured;
    setBusy(true);
    setError("");
    try {
      const response = await command({ kind: "dismiss_native_prompt", ...captured });
      if (response.result.state !== "native_prompt_dismissed") {
        throw new Error("Unexpected dismissal receipt; refresh and check the same request.");
      }
      request.current = undefined;
      setUncertain(false);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      const ambiguous = !(cause instanceof ApiError) || cause.ambiguous;
      setUncertain(ambiguous);
      if (!ambiguous) request.current = undefined;
    } finally {
      setBusy(false);
      onChanged();
    }
  };
  return (
    <div className="session-native-prompt warning" role="status">
      <strong>{prompt.dismissed ? "Notice dismissed; native status unknown" : `The agent is ${nativePromptLabel[prompt.kind]}`}</strong>
      <p>Choose View output, then Take control to inspect or answer the prompt in the agent's terminal, and Release control when you are done.</p>
      {!prompt.dismissed && <><p>LLMRelay cannot answer this prompt.</p><p>Hides this notice in LLMRelay. It does not answer the prompt or confirm it was resolved.</p></>}
      {(!prompt.dismissed || uncertain) && prompt.dismissal && (
        <button disabled={busy || (!!error && !uncertain)} onClick={() => void dismiss()}>
          {busy ? "Dismissing notice…" : uncertain ? "Check previous dismissal" : "Dismiss notice"}
        </button>
      )}
      {error && <><p>{uncertain ? "The dismissal outcome is uncertain. Refresh and check the same request." : "This notice changed before it could be dismissed. Refresh to see the current notice."}</p><button disabled={busy} onClick={onChanged}>Refresh notice</button><TechnicalDetails><pre>{error}</pre></TechnicalDetails></>}
    </div>
  );
}

const reportSupersededByTurn = (session: Session) => {
  const report = session.latest_invocation_report;
  const acceptedAt = session.native_turn?.accepted_at;
  return !!report && !!acceptedAt &&
    Date.parse(acceptedAt) > Date.parse(report.created_at);
};

// A report only describes the session until a newer accepted turn moves past it.
function currentReportLabel(session: Session): string | undefined {
  const report = session.latest_invocation_report;
  if (!report) {
    return session.reported_in_latest_invocation === true ||
        session.reported_in_latest_invocation === 1
      ? "Report submitted"
      : undefined;
  }
  if (reportSupersededByTurn(session)) return undefined;
  if (report.outcome !== "candidate_ready") return "Report submitted";
  return report.consumed_at
    ? "Implementation submitted — awaiting verification"
    : "Implementation submitted — awaiting processing";
}

/** Plain state of one agent session, never a raw process enum. */
export function sessionStateLabel(
  session: Session,
  awaitingApproval = false,
): string {
  if (session.status === "running") {
    if (session.readiness === "unknown") return "Waiting for startup";
    if (awaitingApproval) return "Waiting for your approval";
    if (session.native_turn?.failure) return "Turn stopped with an error";
    if (session.native_prompt) return session.native_prompt.dismissed
      ? "Notice dismissed; native status unknown"
      : "Waiting in its own terminal";
    if (session.input_control) return "You have keyboard control";
    return currentReportLabel(session) ?? "Running";
  }
  switch (session.status) {
    case "launch_reserved":
      return "Starting";
    case "interrupt_requested":
      return "Stopping";
    case "recovery_required":
      return "Needs a recovery check";
    case "launch_failed":
      return "Could not start";
    case "exited":
      return currentReportLabel(session) ? "Finished its turn" : "Stopped";
    default:
      return session.status.replaceAll("_", " ");
  }
}

function SessionOutput(
  { session, route, access }: {
    session: Session;
    route: CmuxViewOutcome;
    access: SessionAccess;
  },
) {
  const output = recordedOutputText(route);
  const presentation = cmuxSurfacePresentation(
    route.surface,
    route.state === "pending",
  );
  // A stopped session is read through its recorded output, never a new view.
  const retryAvailable = session.status === "running" &&
    (route.surface ? presentation.retryAvailable : route.retry_available);
  return (
    <section
      className={`session-output cmux-route ${route.state}`}
      aria-label={`${roleLabel(session.role)} output`}
    >
      <header>
        <h4>Agent output</h4>
        <span>{cmuxRouteLabel(route.state)}</span>
      </header>
      <p aria-live="polite">{terminalGuidance(route)}</p>
      <TechnicalDetails>
        <p>{route.message}</p>
        {route.surface && (
          <p>
            route revision {route.surface.binding_revision} · surface{" "}
            {route.surface.surface_state} · attachment{" "}
            {route.surface.attachment_state} · desired{" "}
            {route.surface.desired_input_state} · actual{" "}
            {route.surface.actual_input_state} · control revision{" "}
            {route.surface.applied_revision}/{route.surface.control_revision}
          </p>
        )}
        {presentation.diagnostic && <p>{presentation.diagnostic}</p>}
      </TechnicalDetails>
      {output && (
        <pre className="recorded-output" tabIndex={0} aria-label="Recorded output">
          {output}
        </pre>
      )}
      {route.recorded_output?.has_more && (
        <small className="hint">
          This is only the first part of the saved output. Choose{" "}
          {recordedOutputAction(session)} to read all of it.
        </small>
      )}
      <div className="button-row">
        {route.surface && presentation.discardAvailable && (
          <button
            disabled={access.discarding[session.id]}
            onClick={() => void access.discard(session.id)}
          >
            {access.discarding[session.id]
              ? "Discarding unknown reservation…"
              : "Discard unknown reservation"}
          </button>
        )}
        {retryAvailable && (
          <button onClick={() => void access.view(session.id)}>
            {route.surface ? presentation.viewLabel : "View output again"}
          </button>
        )}
        <button onClick={() => access.close(session.id)}>Close output</button>
      </div>
      <small className="hint">
        {session.status === "running"
          ? "Closing output only hides it here; the agent keeps working."
          : "Closing output only hides the recorded output here."}
      </small>
    </section>
  );
}

const recordedOutputAction = (session: Session) =>
  session.status === "running" ? "Read recorded output" : "View recorded output";

export function ObservedUsageDetails({ usage, label }: { usage?: ObservedUsage | null; label: string }) {
  const counters = usage?.status === "observed" ? usage.counters : null;
  const count = (value: number | null) => value === null ? "Unavailable" : value.toLocaleString();
  return (
    <section aria-label={label}>
      <h4>{label} — observed usage (partial)</h4>
      {counters
        ? <>
          <p>Provider-reported observed total: {count(counters.total_tokens)} tokens</p>
          <p>
            Input: {count(counters.input_tokens)} · Cached input (included in input): {count(counters.cached_input_tokens)} · Cache write: {count(counters.cache_write_input_tokens)}
          </p>
          <p>
            Output: {count(counters.output_tokens)} · Reasoning (included in output): {count(counters.reasoning_output_tokens)}
          </p>
        </>
        : <p>Usage unavailable. {usage?.reason ?? "No response usage has been observed."}</p>}
      {usage && <p>
        {count(usage.observed_responses)} observed responses · Source: {usage.source}
        {usage.last_observed_at && ` · Last observation: ${new Date(usage.last_observed_at).toLocaleString()}`}
      </p>}
    </section>
  );
}

export function SessionTree(
  {
    sessions,
    access,
    permissionRequests = [],
    taskTitle = () => undefined,
    setupProjectId,
    onOpenSetup,
    onOpenRoleSettings,
    onOpenControls,
    title = "Agent sessions",
    empty = "No agent sessions yet.",
  }: {
    sessions: Session[];
    access: SessionAccess;
    permissionRequests?: PermissionRequest[];
    taskTitle?: (session: Session) => string | undefined;
    setupProjectId: (session: Session) => string | undefined;
    onOpenSetup: (projectId: string) => void;
    onOpenRoleSettings?: (session: Session) => void;
    onOpenControls?: (session: Session, managerChange?: boolean) => void;
    title?: string;
    empty?: string;
  },
) {
  const [expandedReasons, setExpandedReasons] = useState<
    Record<string, boolean>
  >({});
  const [historyShown, setHistoryShown] = useState<Record<string, boolean>>(
    {},
  );
  // Outlives a closed reader, so an uncertain history pane request is checked
  // under its original operation ID when the reader is reopened.
  const [paneRequests] = useState(() => new Map<string, HistoryPaneRequest>());
  const setHistory = (sessionId: string, shown: boolean) =>
    setHistoryShown((current) => ({ ...current, [sessionId]: shown }));
  return (
    <section className="session-tree" aria-label={title}>
      <header>
        <h3>{title}</h3>
        <span>{sessions.length}</span>
      </header>
      <details className="session-help">
        <summary>How output and control work</summary>
        <p>
          View output opens the agent's live activity. Take control lets you
          type an answer when the agent asks a question; Release control lets
          automation continue. Closing the output does not stop the agent, and
          approvals always stay in LLMRelay.
        </p>
        <p>
          View recorded output and Read recorded output show everything
          LLMRelay saved from the session as plain text, even when its terminal
          is unavailable. Reading saved output never restarts the agent, sends
          it input or takes keyboard control.
        </p>
      </details>
      {sessions.map((session) => {
        const persisted = session.cmux_surface;
        const route = access.routeFor(session.id);
        const surface = route?.surface || persisted;
        const actionability = cmuxSurfacePresentation(
          surface,
          route?.state === "pending",
        );
        const reason = (session.exit_reason || session.launch_error || "")
          .slice(0, 2048);
        const exitLabel = session.exit_code != null
          ? `Exit code ${session.exit_code}`
          : session.exit_status
          ? `Exit ${session.exit_status}`
          : "Could not start";
        const projectId = session.setup_operation_id
          ? setupProjectId(session)
          : undefined;
        const reasonExpanded = !!expandedReasons[session.id];
        const running = session.status === "running";
        const waitingForStartup = running && session.readiness === "unknown";
        // A finished session never routes to cmux again: it offers only its
        // recorded output, whatever stale presentation row remains.
        const canTake = running && actionability.takeAvailable;
        const canRelease = running && actionability.releaseAvailable;
        const awaitingApproval = permissionRequests.some((request) =>
          request.actionable && request.session_id === session.id
        );
        const failure = session.native_turn?.failure;
        const unclassified = !failure && !session.provider_failure_holds?.length && !session.failure_stop_pause;
        const exitedWithoutResult = unclassified && session.status === "exited" &&
          session.generation_status === "exited" &&
          (session.has_newer_generation === false || session.has_newer_generation === 0) &&
          !session.latest_invocation_report &&
          !session.reported_in_latest_invocation &&
          (session.exit_success === false || session.exit_success === 0 ||
            (!!session.native_turn?.accepted_hook_event_id && !!session.native_turn.accepted_at && session.readiness === "busy"));
        const recoveryRows: { id: string; hold?: ProviderFailureHold }[] = [
          ...(session.provider_failure_holds ?? []).map((hold) => ({ id: hold.id, hold })),
          ...pendingReleasesWithoutHold(session).map((request) => ({ id: request.hold_id })),
        ];
        const unclassifiedHelp = <>
          <p>{exitedWithoutResult
            ? "This invocation stopped without a recorded result. Inspect its recorded output to decide what to do next; no provider failure reason is recorded."
            : `If the agent seems stuck or its output needs attention, choose ${running ? "View output" : "View recorded output"} to inspect it. Opening output does not stop the agent.`}</p>
          {running && <p>If the live terminal is unavailable, choose Read recorded output to inspect saved activity.</p>}
          {session.setup_operation_id
            ? projectId && <p>Use Open project setup controls below to stop or replace this setup session.</p>
            : onOpenControls && <>
              <p>{session.role === "single_agent"
                ? "Open task controls to check the Pause or Cancel actions available for this Single agent run."
                : exitedWithoutResult
                ? "Open task controls to check the current recovery and continuation options."
                : "To interrupt current work, open task controls and choose Pause now. Cancel is also available where the task allows it."}</p>
              <button onClick={() => onOpenControls(session)}>Open task controls</button>
            </>}
        </>;
        const acceptedAt = session.native_turn?.accepted_at;
        const state = sessionStateLabel(session, awaitingApproval);
        const owner = session.setup_operation_id
          ? "Project setup"
          : taskTitle(session);
        return (
          <article key={session.id} data-attention-target={`session:${session.id}`} tabIndex={-1}>
            <span
              aria-hidden="true"
              className={`status ${waitingForStartup ? "paused" : session.status}`}
            />
            <div className="session-metadata">
              <strong>
                {roleLabel(session.role)}
                <span className="session-state">{state}</span>
              </strong>
              <small>
                {owner ? `${owner} · ` : ""}
                {session.provider === "claude" ? "Claude" : "Codex"}
                {running && " · Session open"}
                {running && acceptedAt &&
                  ` · Latest turn accepted by the agent ${
                    new Date(acceptedAt).toLocaleString()
                  }`}
              </small>
              <small>
                {session.observed_usage?.session.status === "observed" && session.observed_usage.session.counters
                  ? `Observed usage (partial): ${session.observed_usage.session.counters.total_tokens.toLocaleString()} tokens`
                  : "Usage unavailable"}
              </small>
              {awaitingApproval && running && (
                <small className="warning">
                  Waiting for your approval. Open Approvals to review the
                  request.
                </small>
              )}
              {running && !awaitingApproval && currentReportLabel(session) &&
                !failure && (
                <small>
                  It submitted its report in this run and the session is still
                  open. LLMRelay processes the report and verifies the work
                  separately; this does not mean the task is complete.
                </small>
              )}
              {reportSupersededByTurn(session) && session.latest_invocation_report && (
                <small>
                  Its report from{" "}
                  {new Date(session.latest_invocation_report.created_at)
                    .toLocaleString()} is history: the agent accepted a newer
                  turn after it.
                </small>
              )}
              {failure && (
                <div className="session-turn-failure" role="status">
                  <strong>
                    Its latest turn stopped: {nativeTurnFailureLabel[failure.kind]}
                  </strong>
                  <p>
                    {running ? "The session is still open. " : ""}Choose View
                    output to see the agent's terminal and decide how to
                    continue. LLMRelay does not retry or switch models in response to this notice.
                  </p>
                  <TechnicalDetails>
                    <p>{failure.provider_error}</p>
                    <p>
                      {failure.kind} · observed{" "}
                      {new Date(failure.observed_at).toLocaleString()} · hook
                      event {failure.hook_event_id}
                    </p>
                    {failure.details && <pre>{failure.details}</pre>}
                  </TechnicalDetails>
                </div>
              )}
              <FailedSessionStopNotice session={session} onChanged={access.refresh} />
              {recoveryRows.map(({ id, hold }) => (
                <ProviderFailureHoldNotice
                  key={JSON.stringify([session.task_id, session.attempt_id, id])}
                  holdId={id}
                  hold={hold}
                  session={session}
                  onReleased={access.refresh}
                  onOpenRoleSettings={onOpenRoleSettings}
                  onOpenControls={onOpenControls}
                  onOpenSetup={projectId ? () => onOpenSetup(projectId) : undefined}
                />
              ))}
              {session.failure_stop_pause && !session.provider_failure_holds?.length && (
                <p>Existing checkout changes, accepted reports and checkpoints are retained. {session.failure_stop_pause.state === "finished"
                  ? "The failure stop is settled. Open task controls to see whether Continue, Run next or exact resume is available."
                  : "The task remains paused while its stop is checked; continuation is not yet available."}
                  {onOpenControls && <button onClick={() => onOpenControls(session)}>Open task controls</button>}
                </p>
              )}
              {exitedWithoutResult && <div className="session-unclassified-exit" role="status">{unclassifiedHelp}</div>}
              {unclassified && (running || session.status === "interrupt_requested") && (
                <details className="session-unclassified-help">
                  <summary>Having trouble?</summary>
                  {unclassifiedHelp}
                </details>
              )}
              {running && session.native_prompt &&
                (!awaitingApproval || session.native_prompt.kind !== "permission_prompt") && (
                <NativePromptNotice prompt={session.native_prompt} onChanged={access.refresh} />
              )}
              {waitingForStartup && (
                <div className="session-startup-notice warning" role="status">
                  <strong>Waiting for the coding tool to become ready</strong>
                  <p>
                    The process has started, but LLMRelay has not confirmed that
                    this session is ready to work. It may be waiting for a
                    startup prompt.
                  </p>
                  <p>
                    Choose View output to inspect the session. If it needs an
                    answer, choose Take control.
                    {session.provider === "codex" &&
                      " If Codex shows “Hooks need review,” review the listed hooks before deciding whether to trust them."}
                    {" "}
                    When finished, choose Release control so automatic work can
                    continue. If startup is still loading, wait for this notice
                    to clear.
                  </p>
                </div>
              )}
              {(session.exit_reason || session.launch_error ||
                session.exit_status ||
                session.exit_code != null) && (
                <div className="session-exit">
                  <small>{exitLabel}</small>
                  {reason && (
                    <button
                      className="session-disclosure"
                      aria-expanded={reasonExpanded}
                      aria-controls={`session-exit-${session.id}`}
                      onClick={() =>
                        setExpandedReasons((current) => ({
                          ...current,
                          [session.id]: !current[session.id],
                        }))}
                    >
                      {reasonExpanded
                        ? "Hide exit details"
                        : "Show exit details"}
                    </button>
                  )}
                  {reasonExpanded && (
                    <div
                      className="session-exit-detail"
                      id={`session-exit-${session.id}`}
                      role="region"
                      aria-label={`${exitLabel} details`}
                    >
                      {reason}
                    </div>
                  )}
                </div>
              )}
              {!running && (
                <small className="muted">
                  This session is not running, so only its recorded output is
                  available.
                </small>
              )}
              {running && surface && actionability.guidance && (
                <small className="warning">{actionability.guidance}</small>
              )}
              {!route && surface && actionability.discardAvailable && (
                <div className="session-reservation-notice" role="status">
                  <small>
                    {running
                      ? "An earlier terminal for this session was never confirmed."
                      : "An earlier terminal for this session was never confirmed. Its recorded output is not affected."}{" "}
                    Discard unknown reservation only clears that terminal
                    record; it does not change the agent or its output.
                  </small>
                  <button
                    disabled={access.discarding[session.id]}
                    onClick={() => void access.discard(session.id)}
                  >
                    {access.discarding[session.id]
                      ? "Discarding unknown reservation…"
                      : "Discard unknown reservation"}
                  </button>
                </div>
              )}
              {session.input_control && running && (
                <small className="warning input-control-hold" role="status">
                  You have keyboard control. Automatic progress waits until you
                  choose Release control or press Ctrl-] in the terminal.
                </small>
              )}
              <TechnicalDetails>
                <ObservedUsageDetails usage={session.observed_usage?.current_turn} label="Current accepted turn" />
                <ObservedUsageDetails usage={session.observed_usage?.session} label="Session" />
                <p>
                  Session {session.id} · generation {session.generation} ·
                  process {session.status}; launch{" "}
                  {session.launch_state ?? "unknown"}; readiness{" "}
                  {session.readiness}; output capture {session.capture_state}
                  {session.workflow_version
                    ? `; workflow ${session.workflow_version}`
                    : ""}
                  {session.lane_id ? `; lane ${session.lane_id}` : ""}
                  {session.native_turn?.accepted_hook_event_id
                    ? `; accepted turn hook ${session.native_turn.accepted_hook_event_id}`
                    : ""}
                  {session.native_prompt
                    ? `; native prompt ${session.native_prompt.kind} hook ${session.native_prompt.hook_event_id}`
                    : ""}
                </p>
                {surface && (
                  <p>
                    cmux {route ? cmuxRouteLabel(route.state) : surface.surface_state}{" "}
                    · attachment {surface.attachment_state} · surface revision{" "}
                    {surface.binding_revision} · control revision{" "}
                    {surface.applied_revision}/{surface.control_revision} ·
                    actual {surface.actual_input_state}
                  </p>
                )}
                {surface && actionability.diagnostic && (
                  <p>{actionability.diagnostic}</p>
                )}
              </TechnicalDetails>
              {projectId && (
                <div className="session-setup-controls">
                  <strong>Project setup session</strong>
                  <span>
                    Stop or replace this session from Project setup. Viewing it
                    does not change it.
                  </span>
                  <button onClick={() => onOpenSetup(projectId)}>
                    Open project setup controls
                  </button>
                </div>
              )}
            </div>
            <div className="session-actions">
              {running && (
                <button
                  className={route?.state === "pending" ? "selected" : ""}
                  disabled={!actionability.viewAvailable}
                  onClick={() => void access.view(session.id)}
                >
                  {actionability.viewLabel}
                </button>
              )}
              {running && (
                <button disabled={!canTake} onClick={() => void access.take(session.id)}>
                  Take control
                </button>
              )}
              {canRelease && (
                <button onClick={() => void access.release(session.id)}>
                  Release control
                </button>
              )}
              <button
                className={historyShown[session.id] ? "selected" : ""}
                onClick={() => setHistory(session.id, true)}
              >
                {recordedOutputAction(session)}
              </button>
            </div>
            {route && (
              <div className="session-output-slot">
                <SessionOutput session={session} route={route} access={access} />
              </div>
            )}
            {historyShown[session.id] && (
              <div className="session-output-slot">
                <SessionHistory
                  session={session}
                  paneRequests={paneRequests}
                  onClose={() => setHistory(session.id, false)}
                />
              </div>
            )}
          </article>
        );
      })}
      {!sessions.length && <p className="empty">{empty}</p>}
    </section>
  );
}
