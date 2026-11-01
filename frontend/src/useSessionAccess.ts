import { useEffect, useRef, useState } from "react";
import { operationId } from "./api";
import {
  cmuxNewestSurface,
  cmuxOutcomeWithDurableSurface,
  cmuxSurfaceOutcome,
  cmuxSurfacePresentation,
  cmuxViewOutcomeFromSurface,
} from "./cmuxRouting";
import type {
  CmuxKeyboardControlAction,
  CmuxKeyboardControlOutcome,
  CmuxSessionSurface,
  CmuxViewOutcome,
  Session,
} from "./types";

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
    try {
      const keys = Array.from(
        { length: localStorage.length },
        (_, index) => localStorage.key(index),
      );
      for (const key of keys) {
        if (typeof key === "string" && key.startsWith("agenticjira.terminal.")) {
          localStorage.removeItem(key);
        }
      }
    } catch { /* Legacy terminal preferences do not control current session access. */ }
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
  // Close and each explicit View or Take start a new local presentation of a
  // session. A result from an earlier one still completes its native work and
  // refresh, but must not reopen closed output or replace a newer view.
  const presentations = useRef<Record<string, number>>({});
  const currentPresentation = (sessionId: string) =>
    presentations.current[sessionId] ?? 0;
  const nextPresentation = (sessionId: string) => {
    const next = currentPresentation(sessionId) + 1;
    presentations.current[sessionId] = next;
    return next;
  };
  const publishRoute = (
    sessionId: string,
    presentation: number,
    outcome: CmuxViewOutcome,
  ) =>
    setRoutes((current) =>
      currentPresentation(sessionId) === presentation
        ? { ...current, [sessionId]: outcome }
        : current
    );
  const commitRoute = (
    sessionId: string,
    presentation: number,
    outcome: CmuxViewOutcome,
  ) => {
    const committed = cmuxOutcomeWithDurableSurface(
      outcome,
      durableSurfaceForSession(sessionId),
    ) || outcome;
    publishRoute(sessionId, presentation, committed);
    return committed;
  };
  const routeSurface = (sessionId: string) =>
    cmuxOutcomeWithDurableSurface(
      routes[sessionId],
      durableSurfaceForSession(sessionId),
    )?.surface || durableSurfaceForSession(sessionId);

  // The outcome is returned even when its presentation is no longer current,
  // so Take can still acquire the surface it viewed.
  const presentView = async (sessionId: string, presentation: number) => {
    publishRoute(sessionId, presentation, {
      state: "pending",
      message: "Reserving the exact persistent cmux presentation…",
      retry_available: false,
    });
    try {
      const result = commitRoute(
        sessionId,
        presentation,
        cmuxViewOutcomeFromSurface(await handlers.onView(sessionId)),
      );
      handlers.onChanged();
      return result;
    } catch (cause) {
      return commitRoute(sessionId, presentation, {
        state: "failed",
        message: cause instanceof Error ? cause.message : String(cause),
        retry_available: true,
      });
    }
  };
  const view = (sessionId: string) =>
    presentView(sessionId, nextPresentation(sessionId));

  const setKeyboardControl = async (
    sessionId: string,
    surface: CmuxSessionSurface,
    action: CmuxKeyboardControlAction,
    presentation: number,
  ) => {
    const currentSurface = cmuxNewestSurface(
      durableSurfaceForSession(sessionId),
      surface,
    ) || surface;
    const actions = cmuxSurfacePresentation(currentSurface);
    if (
      action === "acquire" ? !actions.takeAvailable : !actions.releaseAvailable
    ) return;
    publishRoute(sessionId, presentation, {
      state: "pending",
      message: action === "acquire"
        ? "Keyboard control is pending on the existing authenticated cmux attachment…"
        : "Keyboard-control release is pending on the existing authenticated cmux attachment…",
      retry_available: false,
      surface: currentSurface,
    });
    try {
      const result = await handlers.onSetKeyboardControl(
        sessionId,
        currentSurface,
        action,
      );
      commitRoute(
        sessionId,
        presentation,
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
        presentation,
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
    const presentation = nextPresentation(sessionId);
    const viewed = await presentView(sessionId, presentation);
    const surface = viewed.surface || routeSurface(sessionId);
    if (!surface || !cmuxSurfacePresentation(surface).takeAvailable) return;
    await setKeyboardControl(sessionId, surface, "acquire", presentation);
  };

  const release = async (sessionId: string) => {
    const presentation = currentPresentation(sessionId);
    const surface = routeSurface(sessionId);
    if (!surface) return;
    await setKeyboardControl(sessionId, surface, "release", presentation);
  };

  const discard = async (sessionId: string) => {
    const presentation = currentPresentation(sessionId);
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
      commitRoute(sessionId, presentation, cmuxViewOutcomeFromSurface(result));
      handlers.onChanged();
    } catch (cause) {
      commitRoute(sessionId, presentation, {
        ...currentRoute,
        message: cause instanceof Error ? cause.message : String(cause),
      });
    } finally {
      setDiscarding((current) => ({ ...current, [sessionId]: false }));
    }
  };

  const close = (sessionId: string) => {
    nextPresentation(sessionId);
    setRoutes((current) => {
      const { [sessionId]: _closed, ...rest } = current;
      return rest;
    });
  };

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

