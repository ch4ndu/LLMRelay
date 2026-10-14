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

