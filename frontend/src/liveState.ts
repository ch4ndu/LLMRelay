import { ApiError, getState, waitForState } from "./api";
import type { AppState, StateCursor, StateWaitResult } from "./types";

/** Mutable like `transportTimeouts`; each controller validates a copy at start. */
export const liveTiming = {
  waitMs: 25_000,
  requestMs: 35_000,
  watchdogMs: 30_000,
  reconnectMinMs: 1_000,
  reconnectMaxMs: 30_000,
};
export type LiveTiming = typeof liveTiming;

export interface LiveTransport {
  read(signal: AbortSignal): Promise<AppState>;
  wait(
    cursor: StateCursor,
    timing: LiveTiming,
    signal: AbortSignal,
  ): Promise<StateWaitResult>;
}
export interface LiveScheduler {
  setTimeout(callback: () => void, delayMs: number): number;
  clearTimeout(handle: number): void;
}
export interface LiveEnvironment {
  transport: LiveTransport;
  scheduler: LiveScheduler;
}
/** The dashboard's transport and clock; the DOM harness replaces them. */
export const liveEnvironment: LiveEnvironment = {
  transport: { read: getState, wait: waitForState },
  scheduler: {
    setTimeout: (callback, delayMs) => window.setTimeout(callback, delayMs),
    clearTimeout: (handle) => window.clearTimeout(handle),
  },
};

export interface LiveStatus {
  online: boolean;
  error: string;
  /** An authoritative read is in flight or queued; parked waits never set it. */
  refreshing: boolean;
}
export interface LiveStateListener {
  onState(state: AppState): void;
  onStatus(status: LiveStatus): void;
}
export interface LiveStateController {
  /** Settles after an authoritative read that started after this call. */
  refresh(): Promise<void>;
  latest(): AppState | undefined;
  dispose(): void;
}

/** Orders canonical decimal revisions exactly, beyond Number precision. */
export const compareRevisions = (left: string, right: string) =>
  left.length - right.length || (left < right ? -1 : left > right ? 1 : 0);

const isWithin = (value: number, min: number, max: number) =>
  Number.isInteger(value) && value >= min && value <= max;

function validatedTiming(timing: LiveTiming): LiveTiming {
  if (!isWithin(timing.waitMs, 1_000, 25_000)) {
    throw new RangeError("liveTiming.waitMs must be 1000 to 25000 ms.");
  }
  if (!isWithin(timing.requestMs, timing.waitMs + 1, 300_000)) {
    throw new RangeError(
      "liveTiming.requestMs must exceed waitMs and be at most 300000 ms.",
    );
  }
  if (!isWithin(timing.watchdogMs, 1_000, 300_000)) {
    throw new RangeError("liveTiming.watchdogMs must be 1000 to 300000 ms.");
  }
  if (
    !isWithin(timing.reconnectMaxMs, 1_000, 30_000) ||
    !isWithin(timing.reconnectMinMs, 1_000, timing.reconnectMaxMs)
  ) {
    throw new RangeError(
      "liveTiming reconnect backoff must satisfy 1000 <= min <= max <= 30000 ms.",
    );
  }
  return { ...timing };
}

type SnapshotSource = "read" | "state_changed" | "reset";

/**
 * Keeps the newest committed snapshot: one cancellable long-poll loop, an
 * independent watchdog read, and coalesced authoritative reads. A snapshot is
 * presentation only; it never grants mutation authority.
 */
export function startLiveState(
  listener: LiveStateListener,
  { transport, scheduler }: LiveEnvironment = liveEnvironment,
): LiveStateController {
  const timing = validatedTiming(liveTiming);
  let disposed = false;
  let accepted: AppState | undefined;
  const retiredIncarnations = new Set<string>();
  // Advanced by every reset and by dispose; a response to a request issued
  // under an earlier generation is discarded however late it arrives.
  let generation = 0;
  let activeWait: AbortController | undefined;
  const activeReads = new Set<AbortController>();
  let activeRead: Promise<boolean> | undefined;
  let queuedRead:
    | { promise: Promise<boolean>; resolve: (reached: boolean) => void }
    | undefined;
  const sleepers = new Map<number, () => void>();
  let watchdog: number | undefined;
  let status: LiveStatus = { online: false, error: "", refreshing: false };

  const setStatus = (change: Partial<LiveStatus>) => {
    const next = { ...status, ...change };
    if (
      disposed ||
      (next.online === status.online && next.error === status.error &&
        next.refreshing === status.refreshing)
    ) return;
    status = next;
    listener.onStatus(next);
  };
  const failureMessage = (cause: unknown) =>
    cause instanceof Error ? cause.message : String(cause);
  const sleep = (delayMs: number) =>
    new Promise<void>((resolve) => {
      const handle = scheduler.setTimeout(() => {
        sleepers.delete(handle);
        resolve();
      }, delayMs);
      sleepers.set(handle, resolve);
    });

  const accept = (snapshot: AppState, source: SnapshotSource) => {
    if (retiredIncarnations.has(snapshot.incarnation)) return;
    const previous = accepted;
    if (previous) {
      if (
        source === "reset" || snapshot.incarnation !== previous.incarnation
      ) {
        if (snapshot.incarnation !== previous.incarnation) {
          retiredIncarnations.add(previous.incarnation);
        }
        generation += 1;
        activeWait?.abort();
      } else {
        const order = compareRevisions(snapshot.revision, previous.revision);
        // A read at the same revision still refreshes volatile process and
        // lease observations; a repeated wake carries nothing new.
        if (order < 0 || (order === 0 && source !== "read")) return;
      }
    }
    accepted = snapshot;
    listener.onState(snapshot);
  };

  const runRead = async (): Promise<boolean> => {
    const readGeneration = generation;
    const request = new AbortController();
    activeReads.add(request);
    setStatus({ refreshing: true });
    try {
      const snapshot = await transport.read(request.signal);
      if (disposed) return false;
      if (readGeneration === generation) accept(snapshot, "read");
      setStatus({ online: true, error: "" });
      return true;
    } catch (cause) {
      if (!disposed && !request.signal.aborted) {
        setStatus({ online: false, error: failureMessage(cause) });
      }
      return false;
    } finally {
      activeReads.delete(request);
    }
  };
  const startRead = (): Promise<boolean> => {
    const read = runRead().finally(() => {
      activeRead = undefined;
      const next = queuedRead;
      queuedRead = undefined;
      if (next && !disposed) {
        void startRead().then(next.resolve);
      } else {
        next?.resolve(false);
        setStatus({ refreshing: false });
      }
    });
    activeRead = read;
    return read;
  };
  // A read already in flight may predate the caller's mutation, so later
  // callers share one read queued behind it instead of reusing it.
  const requestRead = (): Promise<boolean> => {
    if (disposed) return Promise.resolve(false);
    if (!activeRead) return startRead();
    if (!queuedRead) {
      let resolve: (reached: boolean) => void = () => {};
      const promise = new Promise<boolean>((settle) => {
        resolve = settle;
      });
      queuedRead = { promise, resolve };
    }
    return queuedRead.promise;
  };

  const connect = async () => {
    let backoffMs = timing.reconnectMinMs;
    const backOff = async () => {
      await sleep(backoffMs);
      backoffMs = Math.min(backoffMs * 2, timing.reconnectMaxMs);
    };
    let reconnect = true;
    while (!disposed) {
      const current = accepted;
      if (reconnect || !current) {
        reconnect = !(await requestRead()) || !accepted;
        if (reconnect) await backOff();
        continue;
      }
      const waitGeneration = generation;
      const request = new AbortController();
      activeWait = request;
      try {
        const result = await transport.wait(
          { incarnation: current.incarnation, revision: current.revision },
          timing,
          request.signal,
        );
        if (disposed || waitGeneration !== generation) continue;
        if (result.outcome !== "unchanged") {
          accept(result.state, result.outcome);
        }
        setStatus({ online: true, error: "" });
        backoffMs = timing.reconnectMinMs;
      } catch (cause) {
        if (disposed || request.signal.aborted) continue;
        // 503 is the service's bounded waiter capacity or shutdown notice:
        // back off without reporting the still-current state as offline.
        if (!(cause instanceof ApiError && cause.status === 503)) {
          setStatus({ online: false, error: failureMessage(cause) });
          reconnect = true;
        }
        await backOff();
      } finally {
        if (activeWait === request) activeWait = undefined;
      }
    }
  };

  const scheduleWatchdog = () => {
    watchdog = scheduler.setTimeout(() => {
      watchdog = undefined;
      void requestRead().then(() => {
        if (!disposed) scheduleWatchdog();
      });
    }, timing.watchdogMs);
  };

  scheduleWatchdog();
  void connect();
  return {
    refresh: () => requestRead().then(() => undefined),
    latest: () => accepted,
    dispose() {
      if (disposed) return;
      disposed = true;
      generation += 1;
      if (watchdog !== undefined) scheduler.clearTimeout(watchdog);
      activeWait?.abort();
      for (const request of activeReads) request.abort();
      for (const [handle, wake] of sleepers) {
        scheduler.clearTimeout(handle);
        wake();
      }
      sleepers.clear();
      queuedRead?.resolve(false);
      queuedRead = undefined;
    },
  };
}
