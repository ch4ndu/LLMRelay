import { useRef, useState } from "react";
import {
  ApiError,
  command,
  getGuidanceReauthorization,
  operationId,
} from "../api";
import type { GuidanceReauthorizationPreview, Task } from "../types";
import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";

export function GuidanceRecovery(
  { task, onChanged }: { task: Task; onChanged: () => void },
) {
  const [preview, setPreview] = useState<GuidanceReauthorizationPreview>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [uncertain, setUncertain] = useState(false);
  const identity = useRef<{ body: string; id: string } | undefined>(undefined);
  const load = async () => {
    setBusy(true);
    setError("");
    try {
      setPreview(await getGuidanceReauthorization(task.id));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };
  const approve = async () => {
    if (
      !preview || busy || uncertain ||
      preview.expected_version !== task.version ||
      preview.attempt_id !== task.active_attempt?.id ||
      preview.files.length === 0
    ) return;
    const request = {
      kind: "reauthorize_attempt_guidance",
      task_id: task.id,
      attempt_id: preview.attempt_id,
      expected_version: preview.expected_version,
      plan_hash: preview.plan_hash,
      config_revision_id: preview.config_revision_id,
      policy_hash: preview.policy_hash,
      files: preview.files.map(({ path, previous_sha256, sha256 }) => ({
        path,
        previous_sha256,
        sha256,
      })),
    };
    const body = JSON.stringify(request);
    const id = identity.current?.body === body
      ? identity.current.id
      : operationId();
    identity.current = { body, id };
    setBusy(true);
    setError("");
    try {
      await command({ ...request, operation_id: id });
      identity.current = undefined;
      setPreview(undefined);
      onChanged();
    } catch (cause) {
      const ambiguous = cause instanceof ApiError && cause.ambiguous;
      setUncertain(ambiguous);
      setError(
        `${cause instanceof Error ? cause.message : String(cause)}${
          ambiguous
            ? " Refresh and inspect the task audit before approving again; the outcome is unknown."
            : ""
        }`,
      );
      onChanged();
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className="panel recovery guidance-recovery">
      <h3>Review changed instruction files</h3>
      <p>
        This task changed a file that also supplies agent instructions. Preserve
        an approved documentation edit. Review its content below before
        approving it for the remaining task steps.
      </p>
      <button disabled={busy || uncertain} onClick={() => void load()}>
        Review planned documentation update
      </button>
      {error && <ErrorNotice error={error} />}
      {preview && (preview.files.length === 0
        ? (
          <p>
            No changed instruction files qualify under this approved plan. Other
            file changes or recovery holds must be resolved separately.
          </p>
        )
        : (
          <>
            {preview.files.map((file) => (
              <details key={file.path} open>
                <summary>{file.path}</summary>
                <pre>{file.content}</pre>
                <TechnicalDetails>
                  <p>Previous content: {file.previous_sha256}</p>
                  <p>Reviewed content: {file.sha256}</p>
                </TechnicalDetails>
              </details>
            ))}
            <p>
              This approves only the displayed documentation content.
              Independent review and every other hold still apply.
            </p>
            {preview.expected_version !== task.version && (
              <p>The task changed. Review a fresh preview before approving.</p>
            )}
            <button
              disabled={busy || uncertain ||
                preview.expected_version !== task.version ||
                preview.attempt_id !== task.active_attempt?.id}
              onClick={() => void approve()}
            >
              Approve planned documentation update
            </button>
          </>
        ))}
    </section>
  );
}
