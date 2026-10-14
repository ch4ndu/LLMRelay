import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { type FormEvent, useEffect, useRef, useState } from "react";
import {
  ApiError,
  command,
  getHistoryExportText,
  listHistoryPaneViews,
  openHistoryPane,
  operation,
  operationId,
  ProtocolError,
  readHistoryPage,
} from "../api";
import {
  cmuxNewestSurface,
  cmuxOutcomeWithDurableSurface,
  cmuxRouteLabel,
  cmuxSurfaceOutcome,
  cmuxSurfacePresentation,
  cmuxViewOutcomeFromSurface,
  RecordedTextReader,
  recordedOutputText,
  terminalGuidance,
} from "../cmuxRouting";
import {
  type CmuxKeyboardControlAction,
  type CmuxKeyboardControlOutcome,
  type CmuxSessionSurface,
  type CmuxViewOutcome,
  type Command,
  type HistoryExportText,
  type HistoryPaneView,
  type HistoryPaneViewState,
  type HistorySelection,
  type NativePrompt,
  type NativePromptDismissal,
  type NativeTurnFailureKind,
  type ObservedUsage,
  type PermissionRequest,
  type ProviderFailureHold,
  type ProviderFailureHoldRelease,
  roleLabel,
  type Session,
  type TranscriptPage,
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

const releaseOutcomeMessage: Record<ProviderFailureHoldRelease, string> = {
  released:
    "Hold released. The agent was not restarted or sent anything. Refresh to check which continuation controls are available; nothing is chained to this release.",
  already_released:
    "This hold was already released, so nothing changed. Refresh to see the current state.",
  expired:
    "This hold's cooldown had already ended, so nothing changed. Automatic work that is already allowed may continue at a later step.",
  superseded:
    "This hold no longer applies because the agent accepted a newer turn, the role was replaced, or the task moved on. Nothing changed.",
};

const releaseOutcomes: ProviderFailureHoldRelease[] = [
  "released",
  "already_released",
  "expired",
  "superseded",
];

function FailedSessionStopNotice({ session, onChanged }: { session: Session; onChanged: () => void }) {
  const storageKey = `llmrelay.failure.stop.${session.id}`;
  const request = useRef<Command | undefined>((() => {
    try {
      const value: unknown = JSON.parse(localStorage.getItem(storageKey) || "null");
      if (value && typeof value === "object" && "kind" in value &&
        value.kind === "stop_failed_session" && "session_id" in value &&
        value.session_id === session.id && "operation_id" in value &&
        typeof value.operation_id === "string") {
        return { ...value, kind: "stop_failed_session", operation_id: value.operation_id };
      }
    } catch { return undefined; }
    return undefined;
  })());
  const inFlight = useRef(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [details, setDetails] = useState("");
  const [uncertain, setUncertain] = useState(!!request.current);
  const [finished, setFinished] = useState(false);
  const bindingKey = JSON.stringify(session.failed_session_stop);
  useEffect(() => {
    if (!request.current) setFinished(false);
  }, [bindingKey]);
  const stop = async () => {
    if (inFlight.current) return;
    const reconciling = !!request.current;
    const captured = request.current ?? (session.failed_session_stop
      ? { kind: "stop_failed_session", ...session.failed_session_stop, operation_id: operationId() }
      : undefined);
    if (!captured) return;
    request.current = captured;
    inFlight.current = true;
    setBusy(true);
    setMessage("Checking the stop request and the session's exit…");
    setDetails("");
    try {
      localStorage.setItem(storageKey, JSON.stringify(captured));
      const response = await command(captured);
      const state = response.result.state;
      if (response.result.operation_id !== captured.operation_id || typeof state !== "string" || !["stop_requested", "stop_already_requested", "stop_recovery_required"].includes(state)) {
        throw new Error("Unexpected stop receipt; refresh and check the same request.");
      }
      setMessage(state === "stop_recovery_required"
        ? "The task pause is reserved, but signal delivery is unconfirmed. Refresh to check the exit and use the task's recovery controls if it still needs attention."
        : "Stop requested. Checking the session's exit; task advancement stays paused until Continue or Run next.");
      setDetails(JSON.stringify(response.result.detail ?? null));
      setUncertain(false);
      setFinished(true);
      localStorage.removeItem(storageKey);
      request.current = undefined;
    } catch (cause) {
      const ambiguous = reconciling || !(cause instanceof ApiError) || cause.ambiguous;
      setMessage(ambiguous
        ? "The stop request's outcome is uncertain. Refresh, then check this same request before choosing another action."
        : "The stop request was refused because its session or task is no longer eligible. Refresh and use the current task or session controls.");
      setDetails(cause instanceof Error ? cause.message : String(cause));
      setUncertain(ambiguous);
      if (!ambiguous) {
        localStorage.removeItem(storageKey);
        request.current = undefined;
        setFinished(true);
      }
    } finally {
      inFlight.current = false;
      setBusy(false);
      onChanged();
    }
  };
  const pause = session.failure_stop_pause;
  if (!session.failed_session_stop && !pause && !request.current) return null;
  const currentMessage = pause?.state === "finished"
    ? "The failed session has stopped and the task is paused. Open task controls to check whether Continue or Run next is available. Any provider hold still applies."
    : session.status === "recovery_required"
    ? "The stop still needs a recovery check. Open this task's recovery controls; automatic advancement stays paused."
    : pause
    ? "Checking the failed session's exit and waiting for other active agents to finish. Task advancement stays paused until Continue or Run next."
    : message || (uncertain ? "An earlier stop request needs checking. Refresh, then check this same request; no new stop is sent automatically." : "");
  return (
    <div className="session-failure-stop" role="status">
      <p>Stop signals only this failed session and pauses automatic task advancement until Continue or Run next. Other active agents may finish their current work.</p>
      {currentMessage && <p>{currentMessage}</p>}
      {uncertain && currentMessage !== message && <p>{message}</p>}
      {(uncertain || (!finished && session.failed_session_stop)) && (
        <button disabled={busy} onClick={() => void stop()}>
          {busy ? "Checking stop and exit…" : uncertain ? "Check previous stop request" : "Stop failed session and pause task"}
        </button>
      )}
      <button disabled={busy} onClick={onChanged}>Refresh session state</button>
      {(details || request.current) && <TechnicalDetails>
        {request.current && <pre>{JSON.stringify(request.current, null, 2)}</pre>}
        {details && <pre>{details}</pre>}
      </TechnicalDetails>}
    </div>
  );
}

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

function ProviderFailureHoldNotice(
  { hold, session, onReleased, onOpenRoleSettings, onOpenControls, onOpenSetup }: {
    hold: ProviderFailureHold;
    session: Session;
    onReleased: () => void;
    onOpenRoleSettings?: (session: Session) => void;
    onOpenControls?: (session: Session, managerChange?: boolean) => void;
    onOpenSetup?: () => void;
  },
) {
  const storageKey = `llmrelay.failure.release.${hold.id}`;
  const request = useRef<Command | undefined>((() => {
    try {
      const value: unknown = JSON.parse(localStorage.getItem(storageKey) || "null");
      if (value && typeof value === "object" && "kind" in value &&
        value.kind === "release_provider_failure_hold" && "hold_id" in value &&
        value.hold_id === hold.id && "operation_id" in value && typeof value.operation_id === "string") {
        return { ...value, kind: "release_provider_failure_hold", operation_id: value.operation_id };
      }
    } catch { return undefined; }
    return undefined;
  })());
  const inFlight = useRef(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [uncertain, setUncertain] = useState(!!request.current);
  const [outcome, setOutcome] = useState<ProviderFailureHoldRelease>();
  const stopped = session.status === "exited" &&
    (session.process_group_quiescent === true || session.process_group_quiescent === 1);
  const release = async () => {
    if (inFlight.current || (!stopped && !request.current)) return;
    const reconciling = !!request.current;
    const captured = request.current ?? {
      kind: "release_provider_failure_hold",
      operation_id: operationId(),
      task_id: hold.task_id,
      attempt_id: hold.attempt_id,
      session_id: hold.session_id,
      hold_id: hold.id,
      expected_task_version: hold.task_version,
    };
    request.current = captured;
    inFlight.current = true;
    setBusy(true);
    setError("");
    try {
      localStorage.setItem(storageKey, JSON.stringify(captured));
      const response = await command(captured);
      const state = releaseOutcomes.find((value) =>
        value === response.result.state
      );
      if (!state || response.result.operation_id !== captured.operation_id) {
        throw new Error(
          "The service returned an unsupported provider hold result. Refresh the dashboard and check the hold before retrying.",
        );
      }
      localStorage.removeItem(storageKey);
      request.current = undefined;
      setUncertain(false);
      setOutcome(state);
      onReleased();
    } catch (cause) {
      const ambiguous = reconciling || !(cause instanceof ApiError) || cause.ambiguous;
      setUncertain(ambiguous);
      if (!ambiguous) {
        localStorage.removeItem(storageKey);
        request.current = undefined;
      }
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  return (
    <div className="session-provider-hold warning" role="status">
      <strong>{roleLabel(session.role)} is on hold after a provider error</strong>
      <p>{session.provider === "codex" ? "Codex" : "Claude"} · {session.launch.model || "Model not recorded"} · {stopped
        ? "Invocation stopped; provider hold remains."
        : session.status === "interrupt_requested"
        ? "Stopping; exit is not yet confirmed."
        : session.status === "running"
        ? "Invocation still open; a failure does not prove it is idle."
        : "Invocation settlement is not confirmed. Refresh or use the task's recovery controls."}</p>
      <p>
        The provider reported a failure: {hold.cause || "the provider did not say why"}.{" "}
        {hold.expires_at
          ? `LLMRelay is pausing automatic work for this role for a short cooldown of its own, until about ${
            new Date(hold.expires_at).toLocaleTimeString()
          }. This is not the provider's reset time.`
          : "This needs your action: LLMRelay will not start, resume or send input to this role while the hold stands."}
      </p>
      <p>{hold.remedy || "Open agent output to inspect the failure and choose the next step."}</p>
      <p>Existing checkout changes, accepted reports and checkpoints are retained. Stopping the invocation or releasing this hold does not discard them.</p>
      <p>
        Release provider hold removes only this restriction. It does not
        restart the agent or send it anything. Continue, Run next or exact
        resume is a separate action, available only when the service offers it.
      </p>
      {outcome
        ? <p>{releaseOutcomeMessage[outcome]}</p>
        : (
          <div className="button-row">
            <button disabled={busy || (!stopped && !uncertain)} onClick={() => void release()}>
              {busy ? "Releasing provider hold…" : uncertain ? "Check previous release request" : "Release provider hold"}
            </button>
          </div>
        )}
      {!stopped && !uncertain && <p>Release becomes available after LLMRelay confirms this invocation and its process group have stopped. {session.failed_session_stop
        ? "Use Stop failed session and pause task above."
        : "No current failed-session stop binding is available. Use the task's Pause or Cancel controls, or inspect its recovery controls."}</p>}
      {uncertain && <p role="status">The release outcome is uncertain. Refresh, then check the same request; refresh sends no release or resume.</p>}
      {session.role === "manager" && !session.setup_operation_id &&
        <p>Save and verify the requested model or provider in Manager settings, then use Open manager change controls to apply it under the existing replacement checks.</p>}
      {session.setup_operation_id || session.role === "explorer" || session.role === "single_agent"
        ? <p>A checkpoint model/provider switch is not available for this {session.setup_operation_id ? "setup invocation" : roleLabel(session.role)}. Use its existing pause, cancel or setup recovery controls.</p>
        : onOpenRoleSettings && <button disabled={busy} onClick={() => onOpenRoleSettings(session)}>Choose model or provider for {roleLabel(session.role)}</button>}
      {onOpenControls && <button onClick={() => onOpenControls(session)}>Open task controls</button>}
      {!onOpenControls && !session.setup_operation_id &&
        <p>Open this task from Workspace to reach its workflow controls and role settings.</p>}
      {session.role === "manager" && !session.setup_operation_id && onOpenControls &&
        <button onClick={() => onOpenControls(session, true)}>Open manager change controls</button>}
      {session.setup_operation_id && onOpenSetup && <button onClick={onOpenSetup}>Open project setup</button>}
      <button disabled={busy} onClick={onReleased}>Refresh provider recovery</button>
      {error && <ErrorNotice error={error} />}
      <TechnicalDetails>
        {request.current && <pre>{JSON.stringify(request.current, null, 2)}</pre>}
        <p>
          {hold.kind} · {hold.attribution} attribution · held since{" "}
          {new Date(hold.created_at).toLocaleString()}
          {hold.expires_at
            ? ` · app cooldown until ${new Date(hold.expires_at).toLocaleString()}`
            : " · no app cooldown; needs your action"}
        </p>
        <p>
          hold {hold.id} · {hold.source === "failure_hook"
            ? `failure hook ${hold.failure_hook_event_id}`
            : `history observation ${hold.history_observation_id}`}
          {hold.accepted_hook_event_id
            ? ` · accepted turn hook ${hold.accepted_hook_event_id}`
            : ""}
        </p>
        <p>task {hold.task_id} · attempt {hold.attempt_id} · session {hold.session_id} · role generation {hold.role_generation_id} · transcript epoch {hold.transcript_epoch}</p>
      </TechnicalDetails>
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

const uuidPattern =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

const errorText = (cause: unknown) =>
  cause instanceof Error ? cause.message : String(cause);

const historySelectionKey = (selection: HistorySelection) =>
  `${selection.session_id}/${selection.transcript_epoch}`;

type TranscriptEpochPage = {
  epochs: string[];
  gaps: number;
  nextEpoch: string | null;
  nextSequence: number;
  hasMore: boolean;
};

const transcriptEpochPage = (
  value: unknown,
): TranscriptEpochPage | undefined => {
  if (
    typeof value !== "object" || value === null || !("frames" in value) ||
    !("next_sequence" in value) || !("has_more" in value)
  ) return undefined;
  const { frames, next_sequence: nextSequence, has_more: hasMore } = value;
  const rawNextEpoch = "next_epoch" in value ? value.next_epoch : null;
  if (
    !Array.isArray(frames) || typeof hasMore !== "boolean" ||
    typeof nextSequence !== "number" || !Number.isSafeInteger(nextSequence) ||
    (rawNextEpoch != null && typeof rawNextEpoch !== "string")
  ) return undefined;
  const list: unknown[] = frames;
  const epochs: string[] = [];
  let gaps = 0;
  for (const frame of list) {
    if (
      typeof frame !== "object" || frame === null || !("epoch" in frame) ||
      !("gap" in frame)
    ) return undefined;
    const { epoch, gap } = frame;
    if (typeof epoch !== "string" || typeof gap !== "boolean") return undefined;
    if (gap) gaps++;
    else epochs.push(epoch);
  }
  return {
    epochs,
    gaps,
    nextEpoch: typeof rawNextEpoch === "string" ? rawNextEpoch : null,
    nextSequence,
    hasMore,
  };
};

/**
 * Lists the session's invocations with saved output, oldest first. No
 * read-only record lists past epochs, so this scans the general transcript;
 * it only reports epochs and never changes which one is being read.
 */
const discoverInvocations = async (sessionId: string) => {
  const epochs: string[] = [];
  let gaps = 0;
  let cursor: { epoch: string | null; sequence: number } = {
    epoch: null,
    sequence: 0,
  };
  for (;;) {
    const page = transcriptEpochPage(
      await operation<unknown>({
        kind: "transcript",
        session_id: sessionId,
        after_epoch: cursor.epoch,
        after_sequence: cursor.sequence,
        limit_bytes: 512 * 1024,
      }),
    );
    if (!page) {
      throw new Error(
        "The service returned an unsupported response. Refresh after updating the service.",
      );
    }
    for (const epoch of page.epochs) {
      if (uuidPattern.test(epoch) && !epochs.includes(epoch)) epochs.push(epoch);
    }
    gaps += page.gaps;
    if (
      !page.hasMore ||
      (page.nextEpoch === cursor.epoch && page.nextSequence === cursor.sequence)
    ) break;
    cursor = { epoch: page.nextEpoch, sequence: page.nextSequence };
  }
  return { epochs, gaps };
};

/**
 * A page is shown only if it stays on the selected epoch and advances past
 * the cursor. Gap markers may carry the retention record's own epoch.
 */
const historyPageProblem = (
  page: TranscriptPage,
  epoch: string,
  after: number,
) => {
  if (
    (page.next_epoch != null && page.next_epoch !== epoch) ||
    page.frames.some((frame) => !frame.gap && frame.epoch !== epoch)
  ) {
    return "The service returned output from a different invocation than the one selected, so it was not shown.";
  }
  let previous = after;
  for (const frame of page.frames) {
    if (frame.gap) continue;
    if (frame.sequence <= previous) {
      return "The service returned saved output out of order, so it was not shown.";
    }
    previous = frame.sequence;
  }
  if (page.has_more && page.next_sequence <= after) {
    return "The service did not advance through the saved output, so loading stopped.";
  }
  return undefined;
};

const retentionSummary = ({ retention, high_water_sequence }: HistoryExportText) => {
  if (retention.first_retained_sequence === null) {
    return "No output is saved for this invocation.";
  }
  if (retention.complete) {
    return `All output for this invocation is saved, through frame ${high_water_sequence}.`;
  }
  const start = retention.first_retained_sequence > 1
    ? ` Saved output starts at frame ${retention.first_retained_sequence}.`
    : "";
  return `Some output for this invocation was not kept.${start} LLMRelay keeps at most 10 MiB of output per session, and removed output cannot be restored.`;
};

const MAX_SHOWN_MATCHES = 200;

type SearchResults = {
  query: string;
  through: number;
  total: number;
  lines: Array<{ number: number; text: string }>;
};

const searchSavedOutput = (
  copy: HistoryExportText,
  query: string,
): SearchResults => {
  const needle = query.toLocaleLowerCase();
  const lines: SearchResults["lines"] = [];
  let total = 0;
  copy.text.split("\n").forEach((text, index) => {
    if (!text.toLocaleLowerCase().includes(needle)) return;
    total++;
    if (lines.length < MAX_SHOWN_MATCHES) lines.push({ number: index + 1, text });
  });
  return { query, through: copy.high_water_sequence, total, lines };
};

// The object URL is revoked once the click has started the download, so the
// saved text does not stay addressable from the page.
const downloadSavedOutput = (copy: HistoryExportText) => {
  const url = URL.createObjectURL(
    new Blob([copy.text], { type: "text/plain;charset=utf-8" }),
  );
  const link = document.createElement("a");
  link.href = url;
  link.download = `llmrelay-output-${copy.session_id.slice(0, 8)}-${
    copy.transcript_epoch.slice(0, 8)
  }-frame-${copy.high_water_sequence}.txt`;
  document.body.append(link);
  try {
    link.click();
  } finally {
    link.remove();
    URL.revokeObjectURL(url);
  }
};

const historyPaneStateLabel: Record<HistoryPaneViewState, string> = {
  reserved: "Opening",
  opened: "Opened in cmux",
  unknown: "Not confirmed; check cmux",
  failed: "Not opened",
  closed: "Closed",
};

type HistoryPaneRequest = { id: string; acknowledgedUnknownViewId?: string };

type SavedCopyAction = "search" | "download" | "show" | "refresh";

/**
 * Reads one exact session epoch. It is remounted for every selection, and
 * each response is applied only while the run that requested it is current,
 * so a closed reader or another epoch never receives a late page.
 */
function HistorySelectionReader(
  { selection, final, paneRequests }: {
    selection: HistorySelection;
    /** No later frame can be added to this epoch. */
    final: boolean;
    paneRequests: Map<string, HistoryPaneRequest>;
  },
) {
  const key = historySelectionKey(selection);
  const run = useRef(0);
  const paging = useRef<number | undefined>(undefined);
  const cursor = useRef(0);
  const decoder = useRef<RecordedTextReader | undefined>(undefined);
  const [text, setText] = useState("");
  const [through, setThrough] = useState(0);
  const [gaps, setGaps] = useState(0);
  const [hasMore, setHasMore] = useState<boolean>();
  const [loading, setLoading] = useState<"page" | "all">();
  const [pageError, setPageError] = useState("");
  const [saved, setSaved] = useState<HistoryExportText>();
  const [savedBusy, setSavedBusy] = useState<SavedCopyAction>();
  const [savedError, setSavedError] = useState("");
  const [showSaved, setShowSaved] = useState(false);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchResults>();
  const [views, setViews] = useState<HistoryPaneView[]>();
  const [viewsError, setViewsError] = useState("");
  const [paneBusy, setPaneBusy] = useState(false);
  const [paneMessage, setPaneMessage] = useState("");
  const [paneError, setPaneError] = useState("");

  const loadPages = async (all: boolean) => {
    const mine = run.current;
    if (paging.current === mine) return;
    paging.current = mine;
    setLoading(all ? "all" : "page");
    setPageError("");
    try {
      let more: boolean;
      do {
        const after = cursor.current;
        const page = await readHistoryPage(selection, after);
        if (mine !== run.current) return;
        const problem = historyPageProblem(
          page,
          selection.transcript_epoch,
          after,
        );
        if (problem) throw new Error(problem);
        // Cursor zero means no frame of this epoch is shown yet, so the page
        // replaces any earlier "nothing saved" marker instead of repeating it.
        const reader = after === 0
          ? new RecordedTextReader()
          : decoder.current ?? new RecordedTextReader();
        decoder.current = reader;
        more = page.has_more;
        const chunk = reader.push(page.frames) +
          (!more && final ? reader.finish() : "");
        const pageGaps = page.frames.filter((frame) => frame.gap).length;
        cursor.current = page.next_sequence;
        setText((current) => after === 0 ? chunk : current + chunk);
        setGaps((current) => after === 0 ? pageGaps : current + pageGaps);
        setThrough(page.next_sequence);
        setHasMore(more);
      } while (all && more);
    } catch (cause) {
      if (mine === run.current) setPageError(errorText(cause));
    } finally {
      if (paging.current === mine) paging.current = undefined;
      if (mine === run.current) setLoading(undefined);
    }
  };

  const withSavedCopy = async (
    action: SavedCopyAction,
    use: (copy: HistoryExportText) => void,
  ) => {
    if (savedBusy) return;
    const mine = run.current;
    setSavedBusy(action);
    setSavedError("");
    try {
      let copy = action === "refresh" ? undefined : saved;
      if (!copy) {
        copy = await getHistoryExportText(selection);
        if (mine !== run.current) return;
        if (historySelectionKey(copy) !== key) {
          throw new Error(
            "The service returned a saved copy of a different invocation, so it was not used.",
          );
        }
        setSaved(copy);
      }
      use(copy);
    } catch (cause) {
      if (mine === run.current) setSavedError(errorText(cause));
    } finally {
      if (mine === run.current) setSavedBusy(undefined);
    }
  };

  const search = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const needle = query.trim();
    if (!needle) return;
    void withSavedCopy(
      "search",
      (copy) => setResults(searchSavedOutput(copy, needle)),
    );
  };

  const refreshViews = async () => {
    const mine = run.current;
    setViewsError("");
    try {
      const listed = await listHistoryPaneViews(selection);
      if (mine !== run.current) return;
      if (listed.some((view) => historySelectionKey(view) !== key)) {
        throw new Error(
          "The service listed history panes for a different invocation, so they were not shown.",
        );
      }
      setViews(listed);
    } catch (cause) {
      if (mine === run.current) setViewsError(errorText(cause));
    }
  };

  // An uncertain request keeps its operation ID, so checking it replays the
  // same durable record rather than creating another pane.
  const requestPane = async (request: HistoryPaneRequest) => {
    if (paneBusy) return;
    const mine = run.current;
    paneRequests.set(key, request);
    setPaneBusy(true);
    setPaneMessage("");
    setPaneError("");
    try {
      const outcome = await openHistoryPane(
        selection,
        request.id,
        request.acknowledgedUnknownViewId,
      );
      if (
        outcome.view.operation_id !== request.id ||
        historySelectionKey(outcome.view) !== key
      ) {
        throw new Error(
          "The service returned a different history pane record. Refresh history panes and check cmux before opening another.",
        );
      }
      paneRequests.delete(key);
      if (mine !== run.current) return;
      setPaneMessage(outcome.message);
      setViews((current) => [
        outcome.view,
        ...(current ?? []).filter((view) => view.id !== outcome.view.id),
      ]);
    } catch (cause) {
      const refused = cause instanceof ProtocolError ||
        (cause instanceof ApiError && !cause.ambiguous);
      if (refused) paneRequests.delete(key);
      if (mine === run.current) setPaneError(errorText(cause));
    } finally {
      if (mine === run.current) {
        setPaneBusy(false);
        void refreshViews();
      }
    }
  };

  useEffect(() => {
    void loadPages(false);
    void refreshViews();
    return () => {
      run.current++;
    };
  }, []);

  const pending = paneBusy ? undefined : paneRequests.get(key);
  const latestView = views?.[0];
  const unknownView = views?.find((view) => view.state === "unknown");
  const pageStatus = showSaved && saved
    ? `Showing the complete saved copy, through frame ${saved.high_water_sequence}.`
    : loading === "all"
    ? "Loading all remaining saved output…"
    : loading
    ? "Loading saved output…"
    : hasMore === undefined
    ? ""
    : hasMore
    ? `Showing saved output through frame ${through}. More saved output is available.`
    : final
    ? `Showing all saved output. It ends at frame ${through}.`
    : `Showing all output saved so far, through frame ${through}. The session is still running, so more may follow.`;

  return (
    <>
      <pre
        className="recorded-output session-history-text"
        tabIndex={0}
        aria-label="Recorded output"
        aria-busy={!!loading}
      >
        {showSaved && saved ? saved.text : text}
      </pre>
      {pageStatus && <p aria-live="polite">{pageStatus}</p>}
      {gaps > 0 && !showSaved && (
        <p className="warning">
          {gaps === 1
            ? "This output has a marker where saved output is missing."
            : `This output has ${gaps} markers where saved output is missing.`}{" "}
          Missing output cannot be restored.
        </p>
      )}
      {pageError && <ErrorNotice error={pageError} />}
      {!showSaved && (
        <div className="button-row">
          {hasMore === undefined && pageError && (
            <button disabled={!!loading} onClick={() => void loadPages(false)}>
              Try loading saved output again
            </button>
          )}
          {hasMore && (
            <>
              <button disabled={!!loading} onClick={() => void loadPages(false)}>
                Load more output
              </button>
              <button disabled={!!loading} onClick={() => void loadPages(true)}>
                Load all remaining output
              </button>
            </>
          )}
          {hasMore === false && !final && (
            <button disabled={!!loading} onClick={() => void loadPages(true)}>
              Check for newer output
            </button>
          )}
        </div>
      )}
      <form
        className="session-history-search"
        role="search"
        aria-label="Search all saved output"
        onSubmit={search}
      >
        <label>
          Search all saved output
          <input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
        <button type="submit" disabled={!!savedBusy || !query.trim()}>
          {savedBusy === "search" ? "Searching…" : "Search"}
        </button>
      </form>
      <div className="button-row">
        <button
          disabled={!!savedBusy}
          onClick={() => void withSavedCopy("download", downloadSavedOutput)}
        >
          {savedBusy === "download"
            ? "Preparing download…"
            : "Download saved output (.txt)"}
        </button>
        {showSaved
          ? (
            <button onClick={() => setShowSaved(false)}>
              Show paged output
            </button>
          )
          : (
            <button
              disabled={!!savedBusy}
              onClick={() => void withSavedCopy("show", () => setShowSaved(true))}
            >
              {savedBusy === "show"
                ? "Loading complete copy…"
                : "Show complete saved copy"}
            </button>
          )}
        {saved && (
          <button
            disabled={!!savedBusy}
            onClick={() =>
              void withSavedCopy(
                "refresh",
                (copy) =>
                  setResults((current) =>
                    current && searchSavedOutput(copy, current.query)
                  ),
              )}
          >
            {savedBusy === "refresh"
              ? "Refreshing saved copy…"
              : "Refresh saved copy"}
          </button>
        )}
      </div>
      <small className="hint">
        Search, download and the complete copy always cover all output saved
        for this invocation, not only the part shown above.
      </small>
      {saved && <p>{retentionSummary(saved)}</p>}
      {savedError && <ErrorNotice error={savedError} />}
      {results && (
        <div
          className="session-history-results"
          role="region"
          aria-label="Search results"
        >
          <p aria-live="polite">
            {results.total === 0
              ? `No lines match “${results.query}” in the saved output through frame ${results.through}.`
              : `${results.total} matching ${
                results.total === 1 ? "line" : "lines"
              } for “${results.query}” in the saved output through frame ${results.through}${
                results.total > results.lines.length
                  ? `; showing the first ${results.lines.length}`
                  : ""
              }.`}
          </p>
          {results.lines.length > 0 && (
            <ul>
              {results.lines.map((line) => (
                <li key={line.number}>
                  <small>Line {line.number}</small> <code>{line.text}</code>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {saved && (
        <TechnicalDetails>
          <p>
            saved copy {saved.export_hash} · high-water frame{" "}
            {saved.high_water_sequence} · first retained frame{" "}
            {saved.retention.first_retained_sequence ?? "none"} · gap markers{" "}
            {saved.retention.gap_count}
          </p>
        </TechnicalDetails>
      )}
      <div className="session-history-panes">
        <h5>Read-only history in cmux</h5>
        <p>
          Opens this saved output in a separate cmux pane with a read-only
          pager. The pane cannot send input to the agent, and this reader keeps
          working if cmux is unavailable.
        </p>
        {pending
          ? (
            <div className="warning" role="status">
              <p>
                LLMRelay could not confirm whether your last request reached
                cmux. Check cmux before opening another history pane.
              </p>
              <button onClick={() => void requestPane(pending)}>
                Check previous history pane request
              </button>
            </div>
          )
          : latestView?.state === "reserved"
          ? (
            <p role="status">
              A history pane is still opening. Choose Refresh history panes to
              see its result before opening another.
            </p>
          )
          : unknownView
          ? (
            <div className="warning" role="status">
              <p>
                LLMRelay could not confirm whether an earlier history pane
                opened. Check cmux before opening another history pane.
              </p>
              <button
                disabled={paneBusy}
                onClick={() =>
                  void requestPane({
                    id: operationId(),
                    acknowledgedUnknownViewId: unknownView.id,
                  })}
              >
                {paneBusy ? "Opening history pane…" : "Open another history pane"}
              </button>
            </div>
          )
          : (
            <button
              disabled={paneBusy || (!views && !viewsError)}
              onClick={() => void requestPane({ id: operationId() })}
            >
              {paneBusy
                ? "Opening history pane…"
                : latestView?.state === "failed" && latestView.retry_allowed
                ? "Try opening the history pane again"
                : "Open read-only history in cmux"}
            </button>
          )}
        {paneMessage && <p aria-live="polite">{paneMessage}</p>}
        {paneError && <ErrorNotice error={paneError} />}
        {views && views.length > 0 && (
          <ul aria-label="History panes for this invocation">
            {views.map((view) => (
              <li key={view.id}>
                {historyPaneStateLabel[view.state]} · requested{" "}
                {new Date(view.created_at).toLocaleString()}
                {view.failure_reason && <small> {view.failure_reason}</small>}
                <TechnicalDetails>
                  <p>
                    history pane {view.id} · operation {view.operation_id} ·
                    state {view.state} · frame {view.high_water_sequence} ·
                    saved copy {view.export_hash ?? "none"} · service start{" "}
                    {view.service_boot_id} · cmux workspace{" "}
                    {view.workspace_id ?? "none"} · surface{" "}
                    {view.surface_id ?? "none"}
                    {view.retry_allowed ? " · retry allowed" : ""}
                  </p>
                </TechnicalDetails>
              </li>
            ))}
          </ul>
        )}
        {views?.length === 0 && (
          <small className="hint">
            No history panes have been opened for this invocation.
          </small>
        )}
        {viewsError && <ErrorNotice error={viewsError} />}
        <div className="button-row">
          <button onClick={() => void refreshViews()}>
            Refresh history panes
          </button>
        </div>
      </div>
    </>
  );
}

function SessionHistory(
  { session, paneRequests, onClose }: {
    session: Session;
    paneRequests: Map<string, HistoryPaneRequest>;
    onClose: () => void;
  },
) {
  const latest = session.transcript_epoch;
  // The reader stays on the epoch it opened; a newer invocation is offered
  // explicitly and never followed silently.
  const [epoch, setEpoch] = useState(latest);
  useEffect(() => {
    if (!epoch && latest) setEpoch(latest);
  }, [epoch, latest]);
  const [invocations, setInvocations] = useState<
    { epochs: string[]; gaps: number }
  >();
  const [discovering, setDiscovering] = useState(false);
  const [discoverError, setDiscoverError] = useState("");
  const discovery = useRef(0);
  useEffect(() => () => {
    discovery.current++;
  }, []);

  const discover = async () => {
    const mine = ++discovery.current;
    setDiscovering(true);
    setDiscoverError("");
    try {
      const found = await discoverInvocations(session.id);
      if (mine === discovery.current) setInvocations(found);
    } catch (cause) {
      if (mine === discovery.current) setDiscoverError(errorText(cause));
    } finally {
      if (mine === discovery.current) setDiscovering(false);
    }
  };

  const listed = invocations && latest && !invocations.epochs.includes(latest)
    ? [...invocations.epochs, latest]
    : invocations?.epochs;
  const invocationName = (value: string, index: number) =>
    `Invocation ${index + 1}${value === latest ? " (latest)" : ""}`;
  const position = epoch && listed ? listed.indexOf(epoch) : -1;
  const selectedName = !epoch
    ? "No invocation yet"
    : listed && position >= 0
    ? `${invocationName(epoch, position)} of ${listed.length}`
    : epoch === latest
    ? "Latest invocation"
    : "Earlier invocation";

  return (
    <section
      className="session-output session-history"
      aria-label={`${roleLabel(session.role)} recorded output`}
    >
      <header>
        <h4>Recorded output</h4>
        <span>{selectedName}</span>
      </header>
      <p>
        Everything LLMRelay saved from this session's terminal, as plain text.
        Reading it never restarts the agent, sends it input or takes keyboard
        control, and it works whether or not cmux is available.
      </p>
      {session.capture_state === "failed" && (
        <p className="warning">
          Recording stopped early for this session, so later output was not
          saved.
        </p>
      )}
      {epoch && latest && epoch !== latest && (
        <div className="warning" role="status">
          <p>
            You are reading an earlier invocation of this session. A newer one
            exists.
          </p>
          <button onClick={() => setEpoch(latest)}>
            Read the latest invocation
          </button>
        </div>
      )}
      {epoch
        ? (
          <>
            <small>
              Generation {session.generation} · invocation {epoch.slice(0, 8)}
            </small>
            <TechnicalDetails>
              <p>
                session {session.id} · role generation{" "}
                {session.role_generation_id} · generation {session.generation}
                {" "}· transcript epoch {epoch}
                {latest && epoch !== latest ? ` · latest epoch ${latest}` : ""}
                {" "}· output capture {session.capture_state}
              </p>
            </TechnicalDetails>
            <HistorySelectionReader
              key={`${session.id}/${epoch}`}
              selection={{ session_id: session.id, transcript_epoch: epoch }}
              final={epoch !== latest ||
                ["exited", "launch_failed"].includes(session.status)}
              paneRequests={paneRequests}
            />
          </>
        )
        : <p>No saved output is identified for this session yet.</p>}
      <div className="session-history-invocations">
        {listed
          ? (
            <>
              <p>
                {listed.length === 1
                  ? "This session has one invocation with saved output."
                  : `This session has ${listed.length} invocations with saved output. Each one is read separately.`}
              </p>
              {!!invocations?.gaps && (
                <small className="hint">
                  Some older output was removed by the 10 MiB limit, so earlier
                  invocations may be missing from this list.
                </small>
              )}
              <div className="button-row">
                {listed.map((value, index) => (
                  <button
                    key={value}
                    aria-pressed={value === epoch}
                    onClick={() => setEpoch(value)}
                  >
                    {invocationName(value, index)}
                  </button>
                ))}
              </div>
            </>
          )
          : (
            <button disabled={discovering} onClick={() => void discover()}>
              {discovering
                ? "Looking for earlier invocations…"
                : "Find earlier invocations"}
            </button>
          )}
        {discoverError && <ErrorNotice error={discoverError} />}
      </div>
      <div className="button-row">
        <button onClick={onClose}>Close recorded output</button>
      </div>
    </section>
  );
}

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
              {session.provider_failure_holds?.map((hold) => (
                <ProviderFailureHoldNotice
                  key={hold.id}
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
              {session.provider === "codex" && session.status === "exited" &&
                (session.exit_success === false || session.exit_success === 0) &&
                !failure && !session.provider_failure_holds?.length && (
                <p>No attributable provider failure is recorded for this stopped invocation. Inspect recorded output and the task's recovery controls. An error observed only after exit cannot be classified by the current history reader.</p>
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
