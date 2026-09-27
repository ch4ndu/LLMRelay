import { TechnicalDetails } from "./ErrorNotice";
import { useState } from "react";
import {
  cmuxOutcomeWithDurableSurface,
  cmuxRouteLabel,
  cmuxSurfacePresentation,
} from "../cmuxRouting";
import type { CmuxSessionSurface, CmuxViewOutcome, Session } from "../types";

export function SessionTree(
  { sessions, routes, onView, onTake, onRelease, setupProjectId, onOpenSetup }:
    {
      sessions: Session[];
      routes: Record<string, CmuxViewOutcome>;
      onView: (id: string) => void;
      onTake: (id: string) => void;
      onRelease: (id: string) => void;
      setupProjectId: (session: Session) => string | undefined;
      onOpenSetup: (projectId: string) => void;
    },
) {
  const [expandedReasons, setExpandedReasons] = useState<
    Record<string, boolean>
  >({});
  return (
    <section className="session-tree">
      <header>
        <h3>Session access</h3>
        <span>{sessions.length}</span>
      </header>
      {sessions.map((session) => {
        const persisted = session.cmux_surface;
        const route = cmuxOutcomeWithDurableSurface(
          routes[session.id],
          persisted,
        );
        const surface = route?.surface || persisted;
        const durablePresentation = cmuxSurfacePresentation(surface);
        const actionability = cmuxSurfacePresentation(
          surface,
          route?.state === "pending",
        );
        const state = route?.state || durablePresentation.state;
        const reason = (session.exit_reason || session.launch_error || "")
          .slice(0, 2048);
        const exitLabel = session.exit_code != null
          ? `Exit ${session.exit_code}`
          : session.exit_status
          ? `Exit ${session.exit_status}`
          : "Launch failed";
        const projectId = session.setup_operation_id
          ? setupProjectId(session)
          : undefined;
        const reasonExpanded = !!expandedReasons[session.id];
        const running = session.status === "running";
        const waitingForStartup = running && session.readiness === "unknown";
        // A finished session never routes to cmux again: View resolves to
        // recorded output before it examines any stale presentation row.
        // Keep that safe history path available even if the old row is
        // pending or unknown; live sessions remain fenced below.
        const canTake = running && actionability.takeAvailable;
        const canRelease = running && actionability.releaseAvailable;
        return (
          <article key={session.id}>
            <span
              className={`status ${
                waitingForStartup ? "paused" : session.status
              }`}
            />
            <div className="session-metadata">
              <strong>
                {session.setup_operation_id ? "Project setup · " : ""}
                {session.role.replaceAll("_", " ")} · {session.provider}
              </strong>
              <small>
                {session.task_id} · gen {session.generation} ·{" "}
                {waitingForStartup ? "Waiting for startup" : session.status}
              </small>
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
                    answer, choose Take keyboard control.
                    {session.provider === "codex" &&
                      " If Codex shows “Hooks need review,” review the listed hooks before deciding whether to trust them."}
                    {" "}
                    When finished, choose Release keyboard control so automatic
                    work can continue. If startup is still loading, wait for
                    this notice to clear.
                  </p>
                </div>
              )}
              <TechnicalDetails>
                Process: {session.status}; launch:{" "}
                {session.launch_state ?? "unknown"}; readiness:{" "}
                {session.readiness}; output capture: {session.capture_state}
                {session.workflow_version
                  ? `; workflow: ${session.workflow_version}`
                  : ""}
                {session.lane_id ? `; lane: ${session.lane_id}` : ""}
              </TechnicalDetails>
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
              <small>
                {route
                  ? `cmux ${cmuxRouteLabel(state)}`
                  : surface
                  ? `cmux ${surface.surface_state} · attachment ${surface.attachment_state}`
                  : "No current cmux presentation"}
              </small>
              {surface && (
                <small>
                  surface revision {surface.binding_revision} · control revision
                  {" "}
                  {surface.applied_revision}/{surface.control_revision} · actual
                  {" "}
                  {surface.actual_input_state}
                </small>
              )}
              {!running && (
                <small className="muted">
                  This non-running session exposes recorded output only;
                  keyboard control is unavailable.
                </small>
              )}
              {surface && actionability.diagnostic && (
                <TechnicalDetails>{actionability.diagnostic}</TechnicalDetails>
              )}
              {surface && actionability.guidance && (
                <small className="warning">
                  {actionability.guidance}
                </small>
              )}
              {session.input_control && running && (
                <small className="warning input-control-hold" role="status">
                  Keyboard control is active. Automatic progress is paused until
                  Ctrl-] detaches or the control pane closes.
                </small>
              )}
              {projectId && (
                <div className="session-setup-controls">
                  <strong>Project setup session</strong>
                  <span>
                    Stop or Replace remains an explicit human action in project
                    setup. Viewing this session does not change it.
                  </span>
                  <button onClick={() => onOpenSetup(projectId)}>
                    Open project setup controls
                  </button>
                </div>
              )}
            </div>
            <div className="session-actions">
              <button
                className={state === "pending" ? "selected" : ""}
                disabled={running && !actionability.viewAvailable}
                onClick={() => onView(session.id)}
              >
                {actionability.viewLabel}
              </button>
              {running && (
                <button
                  disabled={!canTake}
                  onClick={() => onTake(session.id)}
                >
                  Take keyboard control
                </button>
              )}
              {canRelease && (
                <button onClick={() => onRelease(session.id)}>
                  Release keyboard control
                </button>
              )}
            </div>
          </article>
        );
      })}
      {!sessions.length && <p className="empty">No attached role sessions.</p>}
    </section>
  );
}
