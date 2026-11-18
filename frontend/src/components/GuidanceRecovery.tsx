import { useEffect, useRef, useState } from "react";
import {
  ApiError,
  command,
  getGuidanceReauthorization,
  operationId,
  recoveryJournalChanged,
} from "../api";
import { guidanceApprovalIsFor } from "../apiValidation";
import { withStorageLock } from "../storageCoordination";
import type {
  Command,
  GuidanceDiffLine,
  GuidancePreviewFile,
  GuidanceReauthorizationPreview,
  Task,
} from "../types";
import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";

const diffPrefix = { context: "  ", removed: "- ", added: "+ " } as const;

const approvalsInFlight = new Set<string>();
type ApprovalJournal =
  | { request: Command; saved: string; error?: never }
  | { request?: never; saved?: never; error?: string };

function readApproval(key: string, taskId: string): ApprovalJournal {
  try {
    const saved = localStorage.getItem(key);
    if (saved === null) return {};
    const request: unknown = JSON.parse(saved);
    if (guidanceApprovalIsFor(request, taskId) && JSON.stringify(request) === saved) {
      return { request, saved };
    }
  } catch {
    // Unreadable storage cannot establish whether an earlier approval is pending.
  }
  return { error: "The saved instruction approval could not be checked in browser storage. Reload the dashboard and check the task activity before approving." };
}

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
  { task, onChanged, allowFreshApproval = true }: {
    task: Task;
    onChanged: () => Promise<void> | void;
    allowFreshApproval?: boolean;
  },
) {
  // Keep one unresolved approval per task, even after its active attempt changes.
  const storageKey = `llmrelay.guidance.approval.${encodeURIComponent(task.project_id)}:${encodeURIComponent(task.id)}`;
  return <GuidanceRecoveryPanel key={storageKey} task={task} storageKey={storageKey} allowFreshApproval={allowFreshApproval} onChanged={onChanged} />;
}

function GuidanceRecoveryPanel(
  { task, storageKey, allowFreshApproval, onChanged }: {
    task: Task;
    storageKey: string;
    allowFreshApproval: boolean;
    onChanged: () => Promise<void> | void;
  },
) {
  const [preview, setPreview] = useState<GuidanceReauthorizationPreview>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [journal, setJournal] = useState(() => readApproval(storageKey, task.id));
  const [checked, setChecked] = useState<string>();
  const previewInFlight = useRef(false);
  const approvalContext = useRef(new AbortController());
  const lifetime = useRef(new AbortController());
  const attempt = task.active_attempt;
  useEffect(() => {
    const scope = new AbortController();
    lifetime.current = scope;
    return () => scope.abort();
  }, []);
  useEffect(() => {
    const context = new AbortController();
    approvalContext.current = context;
    if (!allowFreshApproval) setPreview(undefined);
    return () => context.abort();
  }, [task.version, attempt?.id, attempt?.plan_hash, allowFreshApproval]);
  useEffect(() => {
    const sync = () => {
      const latest = readApproval(storageKey, task.id);
      setJournal(latest);
      setChecked((reviewed) => reviewed === latest.saved ? reviewed : undefined);
    };
    const onStorage = (event: StorageEvent) => {
      if (event.key === storageKey || event.key === null) {
        sync();
      }
    };
    const onLocalWrite = (event: Event) => {
      if (event instanceof window.CustomEvent && event.detail === storageKey) sync();
    };
    window.addEventListener("storage", onStorage);
    window.addEventListener(recoveryJournalChanged, onLocalWrite);
    sync();
    return () => {
      window.removeEventListener("storage", onStorage);
      window.removeEventListener(recoveryJournalChanged, onLocalWrite);
    };
  }, [storageKey, task.id]);
  const notifyJournal = () => window.dispatchEvent(
    new window.CustomEvent(recoveryJournalChanged, { detail: storageKey }),
  );

  const load = async () => {
    if (!allowFreshApproval || previewInFlight.current || approvalsInFlight.has(storageKey)) return;
    const signal = approvalContext.current.signal;
    const mounted = lifetime.current.signal;
    const current = readApproval(storageKey, task.id);
    setJournal(current);
    if (current.request || current.error) return;
    previewInFlight.current = true;
    setBusy(true);
    setError("");
    setPreview(undefined);
    try {
      const next = await getGuidanceReauthorization(task.id);
      if (!signal.aborted) setPreview(next);
    } catch (cause) {
      if (!signal.aborted) setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      previewInFlight.current = false;
      if (!mounted.aborted) setBusy(false);
    }
  };
  // A preview reviewed for another task version, attempt or plan is stale and
  // can never approve the content now in the worktree.
  const stale = !!preview && (
    preview.expected_version !== task.version ||
    preview.attempt_id !== attempt?.id ||
    (attempt?.plan_hash !== undefined && preview.plan_hash !== attempt.plan_hash)
  );
  const approve = async (replay: boolean) => {
    if (busy || previewInFlight.current || approvalsInFlight.has(storageKey)) return;
    const current = readApproval(storageKey, task.id);
    setJournal(current);
    if (current.error) return;
    let { request, saved } = current;
    const signal = approvalContext.current.signal;
    const mounted = lifetime.current.signal;
    if (replay) {
      if (!request || checked !== saved) return;
    } else {
      if (!allowFreshApproval || request || !preview || stale || preview.files.length === 0) return;
      request = {
        kind: "reauthorize_attempt_guidance",
        operation_id: operationId(),
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
      if (!guidanceApprovalIsFor(request, task.id)) {
        setError("The instruction approval could not be bound to this review. Review the planned documentation update again.");
        return;
      }
      const approval = request;
      saved = JSON.stringify(approval);
      const serialized = saved;
      approvalsInFlight.add(storageKey);
      setBusy(true);
      setError("");
      try {
        const published = await withStorageLock(storageKey, () => {
          const latest = readApproval(storageKey, task.id);
          if (latest.request || latest.error) return latest;
          localStorage.setItem(storageKey, serialized);
          return { request: approval, saved: serialized };
        }, signal);
        if (published && !signal.aborted) setJournal(published);
        if (published?.saved !== serialized) return;
        if (signal.aborted) {
          await withStorageLock(storageKey, () => {
            if (localStorage.getItem(storageKey) === serialized) localStorage.removeItem(storageKey);
          });
          return;
        }
      } catch {
        if (!signal.aborted) setError("Browser storage could not save this instruction approval for recovery. The approval was not sent.");
        return;
      } finally {
        approvalsInFlight.delete(storageKey);
        if (signal.aborted) notifyJournal();
        if (!mounted.aborted) setBusy(false);
      }
    }
    approvalsInFlight.add(storageKey);
    notifyJournal();
    setBusy(true);
    setError("");
    setChecked(undefined);
    let clearSaved = false;
    let confirmed = false;
    let replaced = false;
    let failure = "";
    try {
      await command(request);
      confirmed = true;
      clearSaved = true;
    } catch (cause) {
      clearSaved = !replay && !(cause instanceof ApiError && cause.ambiguous);
      failure = cause instanceof Error ? cause.message : String(cause);
    }
    try {
      if (clearSaved) {
        await withStorageLock(storageKey, () => {
          if (localStorage.getItem(storageKey) === saved) {
            localStorage.removeItem(storageKey);
          } else {
            replaced = true;
          }
        });
      }
    } catch {
      failure = "Browser storage could not clear the saved instruction approval. Check the task activity and the saved approval before continuing.";
    } finally {
      approvalsInFlight.delete(storageKey);
      notifyJournal();
      if (!mounted.aborted) setBusy(false);
    }
    if (signal.aborted || replaced) return;
    if (confirmed || !clearSaved) setPreview(undefined);
    setError(failure);
    try {
      await onChanged();
    } catch {
      if (!signal.aborted) setError("The task could not be refreshed. Reload the dashboard and check the task activity.");
    }
  };
  if (!allowFreshApproval && !journal.request && !journal.error && !error) return null;
  return (
    <section className="panel recovery guidance-recovery">
      <h3>{journal.request ? "Reconcile saved instruction approval" : "Review changed instruction files"}</h3>
      {allowFreshApproval && !journal.request && <p>
        This task changed a file that also supplies agent instructions. If the
        approved plan includes this documentation edit, review its content
        below and approve it for the remaining task steps. Otherwise restore
        the file yourself. LLMRelay never restores or discards the edit for
        you.
      </p>}
      {journal.request
        ? (
          <div>
            <p role="status">
              {busy || approvalsInFlight.has(storageKey) ? "Checking the saved instruction approval…" :
                "The outcome of the saved instruction approval is uncertain. Refresh and inspect the task activity before checking the original request."}
            </p>
            <label>
              <input
                type="checkbox"
                checked={checked === journal.saved}
                disabled={busy || approvalsInFlight.has(storageKey)}
                onChange={(event) => setChecked(event.target.checked ? journal.saved : undefined)}
              />
              I checked the task activity for this approval.
            </label>
            <button disabled={busy || approvalsInFlight.has(storageKey) || checked !== journal.saved} onClick={() => void approve(true)}>
              Check saved instruction approval
            </button>
          </div>
        )
        : allowFreshApproval && (
          <button disabled={busy || !!journal.error} onClick={() => void load()}>
            Review planned documentation update
          </button>
        )}
      {journal.error && <ErrorNotice error={journal.error} />}
      {error && <ErrorNotice error={error} />}
      {allowFreshApproval && preview && !journal.request && !journal.error && (preview.files.length === 0
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
              disabled={busy || stale}
              onClick={() => void approve(false)}
            >
              Approve planned documentation update
            </button>
          </>
        ))}
    </section>
  );
}
