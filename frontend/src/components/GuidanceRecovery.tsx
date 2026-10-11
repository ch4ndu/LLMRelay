import { useRef, useState } from "react";
import {
  ApiError,
  command,
  getGuidanceReauthorization,
  operationId,
} from "../api";
import type {
  GuidanceDiffLine,
  GuidancePreviewFile,
  GuidanceReauthorizationPreview,
  Task,
} from "../types";
import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";

const diffPrefix = { context: "  ", removed: "- ", added: "+ " } as const;

function DiffLines({ lines }: { lines: GuidanceDiffLine[] }) {
  return (
    <pre className="guidance-diff">
      {lines.map((line, index) =>
        line.kind === "unchanged"
          ? (
            <span className="diff-unchanged" key={index}>
              {`… ${line.lines} unchanged line${line.lines === 1 ? "" : "s"} …\n`}
            </span>
          )
          : (
            <span className={`diff-${line.kind}`} key={index}>
              {diffPrefix[line.kind]}
              {line.text}
            </span>
          )
      )}
    </pre>
  );
}

// The comparison is labelled by its actual source: only `pinned` is the
// approved copy; a base-commit comparison or none is never presented as one.
function FileComparison({ file }: { file: GuidancePreviewFile }) {
  const baseline = file.baseline;
  const diff = baseline && baseline.source !== "unavailable" ? file.diff : null;
  return (
    <>
      <p className="hint">
        {!baseline
          ? "This service offers no comparison for this file."
          : baseline.source === "pinned"
          ? "Changes compared with the approved copy:"
          : baseline.source === "base_commit"
          ? "Changes compared with the version in the workspace's base commit. This is not the approved copy, which could not be compared:"
          : `No comparison is available${
            baseline.reason ? `: ${baseline.reason}` : "."
          }`}
      </p>
      {diff && <DiffLines lines={diff} />}
      {diff
        ? (
          <details>
            <summary>Complete new content</summary>
            <pre>{file.content}</pre>
          </details>
        )
        : (
          <>
            <p className="hint">The complete new content in the task's workspace:</p>
            <pre>{file.content}</pre>
          </>
        )}
      <TechnicalDetails>
        {baseline && (
          <p>
            Compared with {baseline.source} at {baseline.revision}
            {baseline.sha256 ? ` (${baseline.sha256})` : ""}
          </p>
        )}
        <p>Approved content: {file.previous_sha256}</p>
        <p>Content shown here: {file.sha256}</p>
      </TechnicalDetails>
    </>
  );
}

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
  const attempt = task.active_attempt;
  // A preview reviewed for another task version, attempt or plan is stale and
  // can never approve the content now in the worktree.
  const stale = !!preview && (
    preview.expected_version !== task.version ||
    preview.attempt_id !== attempt?.id ||
    (attempt?.plan_hash !== undefined && preview.plan_hash !== attempt.plan_hash)
  );
  const approve = async () => {
    if (
      !preview || busy || uncertain || stale || preview.files.length === 0
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
        This task changed a file that also supplies agent instructions. If the
        approved plan includes this documentation edit, review its content
        below and approve it for the remaining task steps. Otherwise restore
        the file yourself. LLMRelay never restores or discards the edit for
        you.
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
            <p className="hint">
              {preview.workspace
                ? `Read from the task's workspace at ${preview.workspace.path}, based on commit ${
                  preview.workspace.base_revision.slice(0, 12)
                }.`
                : "This service did not report which workspace these files were read from."}
            </p>
            {preview.files.map((file) => (
              <details key={file.path} open>
                <summary>Changed file: {file.path}</summary>
                <FileComparison file={file} />
              </details>
            ))}
            <p>
              This approves only the displayed documentation content.
              Independent review and every other hold still apply.
            </p>
            {stale && (
              <p>
                The task changed since this preview. Choose Review planned
                documentation update again before approving.
              </p>
            )}
            <button
              disabled={busy || uncertain || stale}
              onClick={() => void approve()}
            >
              Approve planned documentation update
            </button>
          </>
        ))}
    </section>
  );
}
