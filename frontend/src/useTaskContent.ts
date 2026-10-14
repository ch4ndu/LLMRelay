import { useEffect, useState } from "react";
import { getTaskContent } from "./api";
import { type Task, type TaskContent } from "./types";

/**
 * Current-attempt plans and reports, refetched when the service reports a new
 * content revision. Only content for the current attempt and revision is kept
 * or shown: a superseded request can still resolve after a newer one, and its
 * older content or error must never replace or hide the current state.
 */
export function useTaskContent(task: Task, enabled: boolean) {
  const attemptId = task.active_attempt?.id;
  // Absent from older services, which then bind content by attempt alone.
  const revision = task.active_attempt?.content_revision ?? "";
  const current = (
    value?: { attempt_id: string | null; content_revision: string },
  ) =>
    !!value && value.attempt_id === attemptId &&
    (!revision || value.content_revision === revision);
  const [content, setContent] = useState<TaskContent>();
  const [error, setError] = useState<
    { attempt_id: string | null; content_revision: string; message: string }
  >();
  const [reloads, setReloads] = useState(0);
  useEffect(() => {
    if (!enabled || !attemptId) return;
    const controller = new AbortController();
    getTaskContent(task.id, controller.signal).then((value) => {
      if (
        controller.signal.aborted || value.attempt_id !== attemptId ||
        (revision && value.content_revision !== revision)
      ) return;
      setContent(value);
      setError(undefined);
    }).catch((cause) => {
      if (controller.signal.aborted) return;
      setError({
        attempt_id: attemptId,
        content_revision: revision,
        message: cause instanceof Error ? cause.message : String(cause),
      });
    });
    return () => controller.abort();
  }, [task.id, attemptId, revision, enabled, reloads]);
  return {
    content: current(content) ? content : undefined,
    error: current(error) ? error!.message : "",
    reload: () => setReloads((count) => count + 1),
  };
}
