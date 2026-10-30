/**
 * Exclusive Web Locks for localStorage that several open dashboard windows
 * share. Only pages running this code take these locks; reload older tabs
 * after upgrading. A section's lock is released as soon as it returns, so
 * network work can never hold it.
 */
export class StorageLockUnavailableError extends Error {
  constructor(options?: ErrorOptions) {
    super("This browser could not coordinate saving with other open windows.", options);
    this.name = "StorageLockUnavailableError";
  }
}

export const storageLockName = (storageKey: string): string =>
  `llmrelay.storage-lock:${storageKey}`;

type SectionOutcome<T> = { value: T } | { error: unknown };

/**
 * Runs the synchronous `section` while holding the lock for `storageKey`.
 * A request that `signal` cancels before the section runs resolves undefined.
 */
export async function withStorageLock<T>(
  storageKey: string,
  section: () => T,
  signal?: AbortSignal,
): Promise<T | undefined> {
  const locks: LockManager | undefined = globalThis.navigator?.locks;
  if (!locks) throw new StorageLockUnavailableError();
  const runSection = (): SectionOutcome<T> | undefined => {
    if (signal?.aborted) return undefined;
    try {
      return { value: section() };
    } catch (error) {
      return { error };
    }
  };
  let outcome: SectionOutcome<T> | undefined;
  try {
    outcome = await locks.request(
      storageLockName(storageKey),
      { mode: "exclusive", signal },
      runSection,
    );
  } catch (cause) {
    if (signal?.aborted) return undefined;
    throw new StorageLockUnavailableError({ cause });
  }
  if (!outcome) return undefined;
  if ("error" in outcome) throw outcome.error;
  if (outcome.value instanceof Promise) {
    throw new TypeError("Storage lock sections must finish synchronously.");
  }
  return outcome.value;
}
