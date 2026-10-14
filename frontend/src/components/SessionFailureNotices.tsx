import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { useEffect, useRef, useState } from "react";
import {
  ApiError,
  command,
  operationId,
} from "../api";
import {
  type Command,
  type ProviderFailureHold,
  type ProviderFailureHoldRelease,
  roleLabel,
  type Session,
} from "../types";

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

type ProviderHoldReleaseCommand = Command & {
  kind: "release_provider_failure_hold";
  task_id: string;
  attempt_id: string;
  session_id: string;
  hold_id: string;
  expected_task_version: number;
};

const releaseStoragePrefix = "llmrelay.failure.release.";

function pendingReleaseMatches(
  value: unknown,
  key: string,
  session: Session,
): value is ProviderHoldReleaseCommand {
  return !!value && typeof value === "object" &&
    Object.keys(value).length === 7 &&
    "kind" in value && value.kind === "release_provider_failure_hold" &&
    "operation_id" in value && typeof value.operation_id === "string" && !!value.operation_id.trim() &&
    "hold_id" in value && typeof value.hold_id === "string" && !!value.hold_id.trim() &&
    key === `${releaseStoragePrefix}${value.hold_id}` &&
    "task_id" in value && typeof value.task_id === "string" && !!value.task_id.trim() && value.task_id === session.task_id &&
    "attempt_id" in value && typeof value.attempt_id === "string" && !!value.attempt_id.trim() && value.attempt_id === session.attempt_id &&
    "session_id" in value && typeof value.session_id === "string" && !!value.session_id.trim() && value.session_id === session.id &&
    "expected_task_version" in value && typeof value.expected_task_version === "number" && Number.isSafeInteger(value.expected_task_version);
}

function readPendingRelease(key: string, session: Session): ProviderHoldReleaseCommand | undefined {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(key) || "null");
    return pendingReleaseMatches(value, key, session) ? value : undefined;
  } catch {
    return undefined;
  }
}

export function pendingReleasesWithoutHold(session: Session): ProviderHoldReleaseCommand[] {
  const requests: ProviderHoldReleaseCommand[] = [];
  try {
    const activeIds = new Set(session.provider_failure_holds?.map((hold) => hold.id));
    for (let index = 0; index < localStorage.length; index += 1) {
      const key = localStorage.key(index);
      if (!key?.startsWith(releaseStoragePrefix) || activeIds.has(key.slice(releaseStoragePrefix.length))) continue;
      const request = readPendingRelease(key, session);
      if (request) requests.push(request);
    }
  } catch {
    return requests;
  }
  return requests;
}

export function FailedSessionStopNotice({ session, onChanged }: { session: Session; onChanged: () => void }) {
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

export function ProviderFailureHoldNotice(
  { holdId, hold, session, onReleased, onOpenRoleSettings, onOpenControls, onOpenSetup }: {
    holdId: string;
    hold?: ProviderFailureHold;
    session: Session;
    onReleased: () => void;
    onOpenRoleSettings?: (session: Session) => void;
    onOpenControls?: (session: Session, managerChange?: boolean) => void;
    onOpenSetup?: () => void;
  },
) {
  const storageKey = `${releaseStoragePrefix}${holdId}`;
  const request = useRef(readPendingRelease(storageKey, session));
  const inFlight = useRef(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [uncertain, setUncertain] = useState(!!request.current);
  const [outcome, setOutcome] = useState<ProviderFailureHoldRelease | "refused">();
  useEffect(() => {
    if (!request.current) {
      setOutcome(undefined);
      setError("");
    }
  }, [hold?.task_version]);
  const stopped = session.status === "exited" &&
    (session.process_group_quiescent === true || session.process_group_quiescent === 1);
  const release = async () => {
    if (inFlight.current || outcome || (!request.current && (!hold || !stopped))) return;
    const captured: ProviderHoldReleaseCommand | undefined = request.current ?? (hold ? {
      kind: "release_provider_failure_hold",
      operation_id: operationId(),
      task_id: hold.task_id,
      attempt_id: hold.attempt_id,
      session_id: hold.session_id,
      hold_id: hold.id,
      expected_task_version: hold.task_version,
    } : undefined);
    if (!captured) return;
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
      // Release rejection is receipt-first; stop commands can fail after commit.
      const refused = cause instanceof ApiError && cause.status === 409 &&
        cause.operationId === captured.operation_id && !cause.ambiguous;
      setUncertain(!refused);
      if (refused) {
        localStorage.removeItem(storageKey);
        request.current = undefined;
        setOutcome("refused");
      }
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  const releaseNotice = <>
    {outcome
      ? <p>{outcome === "refused"
        ? "The service refused this release request. Refresh to see the current hold and recovery options."
        : releaseOutcomeMessage[outcome]}</p>
      : <div className="button-row">
        <button disabled={busy || (!stopped && !uncertain)} onClick={() => void release()}>
          {busy ? "Releasing provider hold…" : uncertain ? "Check previous release request" : "Release provider hold"}
        </button>
      </div>}
    {uncertain && <p role="status">The release outcome is uncertain. Refresh, then check the same request; refresh sends no release or resume.</p>}
    {error && <ErrorNotice error={error} />}
    {request.current && <TechnicalDetails><pre>{JSON.stringify(request.current, null, 2)}</pre></TechnicalDetails>}
  </>;
  if (!hold) return (
    <div className="session-provider-release-request" role="status">
      <strong>Previous provider hold release request</strong>
      <p>The current session state no longer shows this hold. This saved request is history; it does not pause or resume the task.</p>
      {releaseNotice}
      <button disabled={busy} onClick={onReleased}>Refresh provider recovery</button>
    </div>
  );
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
      {releaseNotice}
      {!stopped && !uncertain && <p>Release becomes available after LLMRelay confirms this invocation and its process group have stopped. {session.failed_session_stop
        ? "Use Stop failed session and pause task above."
        : "No current failed-session stop binding is available. Use the task's Pause or Cancel controls, or inspect its recovery controls."}</p>}
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
      <TechnicalDetails>
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

