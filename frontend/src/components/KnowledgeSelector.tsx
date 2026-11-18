import { useEffect, useRef, useState } from "react";
import {
  ApiError,
  command,
  knowledgePreview,
  knowledgeRead,
  operationId,
  utf8Bytes,
} from "../api";
import type {
  Command,
  KnowledgeConfirmation,
  KnowledgeManifest,
  KnowledgeObservation,
  KnowledgePreview,
  KnowledgePreviewRequest,
  KnowledgeRevision,
  KnowledgeSearchHit,
  KnowledgeSelection,
  Task,
  TaskContent,
} from "../types";
import { withStorageLock } from "../storageCoordination";
import { ErrorNotice } from "./ErrorNotice";

interface KnowledgeSelectorProps {
  projectId: string;
  consumer?: Omit<KnowledgePreviewRequest, "selection">;
  initialSelection?: KnowledgeSelection[];
  invalidationKey?: string;
  disabled?: boolean;
  allowEmptyPreview?: boolean;
  onSelectionChange?: (selection: KnowledgeSelection[]) => void;
  onConfirm: (confirmation: KnowledgeConfirmation | undefined) => void;
}
interface SelectedDetail {
  hit: KnowledgeSearchHit;
  revision: KnowledgeRevision;
  observation: KnowledgeObservation;
}
export function KnowledgeSelector(
  {
    projectId,
    consumer,
    initialSelection = [],
    invalidationKey = "",
    disabled = false,
    allowEmptyPreview = false,
    onSelectionChange,
    onConfirm,
  }: KnowledgeSelectorProps,
) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<KnowledgeSearchHit[]>([]);
  const [nextEntryId, setNextEntryId] = useState<string | null>(null);
  const [searching, setSearching] = useState(false);
  const [selection, setSelection] = useState<KnowledgeSelection[]>(
    initialSelection,
  );
  const [details, setDetails] = useState<Record<string, SelectedDetail>>({});
  const [preview, setPreview] = useState<
    { identity: string; value: KnowledgePreview }
  >();
  const [confirmed, setConfirmed] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const epoch = useRef(0);
  const identity = JSON.stringify({
    consumer,
    selection,
    invalidationKey,
    projectId,
  });
  const liveIdentity = useRef(identity);
  liveIdentity.current = identity;
  const confirmCallback = useRef(onConfirm);
  confirmCallback.current = onConfirm;
  useEffect(() => {
    setPreview(undefined);
    setConfirmed(false);
    confirmCallback.current(undefined);
  }, [identity]);
  const previousProject = useRef(projectId);
  useEffect(() => {
    if (previousProject.current === projectId) return;
    previousProject.current = projectId;
    epoch.current++;
    setHits([]);
    setNextEntryId(null);
    setSearching(false);
    setSelection([]);
    setDetails({});
    setPreview(undefined);
    setConfirmed(false);
    setBusy(false);
    setError("");
    confirmCallback.current(undefined);
    onSelectionChange?.([]);
  }, [projectId]);
  useEffect(() => () => {
    epoch.current++;
  }, []);
  async function search(after: string | null = null): Promise<void> {
    const current = ++epoch.current;
    setSearching(true);
    setError("");
    try {
      const result = await knowledgeRead(projectId, {
        kind: "search",
        search: {
          query,
          include_drafts: false,
          include_archived: false,
          after_entry_id: after,
          limit: 50,
        },
      });
      if (current !== epoch.current) return;
      setHits((old) => after ? [...old, ...result] : result);
      setNextEntryId(result.length === 50 ? result[result.length - 1].entry.id : null);
    } catch (cause) {
      if (current === epoch.current) setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (current === epoch.current) setSearching(false);
    }
  }
  function change(next: KnowledgeSelection[]): void {
    setSelection(next);
    setPreview(undefined);
    setConfirmed(false);
    onConfirm(undefined);
    onSelectionChange?.(next);
  }
  async function add(hit: KnowledgeSearchHit): Promise<void> {
    if (
      selection.length >= 16 ||
      selection.some((item) => item.entry_id === hit.entry.id)
    ) return;
    const current = ++epoch.current;
    setBusy(true);
    setError("");
    const before = liveIdentity.current;
    try {
      const [revision, observation] = await Promise.all([
        knowledgeRead(projectId, {
          kind: "revision",
          entry_id: hit.entry.id,
          revision_id: hit.revision_id,
        }),
        knowledgeRead(projectId, {
          kind: "freshness",
          entry_id: hit.entry.id,
          revision_id: hit.revision_id,
        }),
      ]);
      if (current !== epoch.current || before !== liveIdentity.current) return;
      setDetails((old) => ({
        ...old,
        [revision.id]: { hit, revision, observation },
      }));
      change([...selection, {
        entry_id: hit.entry.id,
        revision_id: revision.id,
        expected_entry_version: hit.entry.version,
        observation_fingerprint: observation.fingerprint,
        acknowledged_warning: false,
      }]);
    } catch (cause) {
      if (current === epoch.current && before === liveIdentity.current) {
        setError(cause instanceof Error ? cause.message : String(cause));
      }
    } finally {
      if (current === epoch.current) setBusy(false);
    }
  }
  async function previewSelection(): Promise<void> {
    if (!consumer) return;
    const before = identity;
    const current = ++epoch.current;
    setBusy(true);
    setError("");
    onConfirm(undefined);
    setConfirmed(false);
    try {
      const result = await knowledgePreview({ ...consumer, selection });
      if (current === epoch.current && liveIdentity.current === before) {
        setPreview({ identity: before, value: result });
      }
    } catch (cause) {
      if (current === epoch.current && liveIdentity.current === before) {
        setPreview(undefined);
        setError(cause instanceof Error ? cause.message : String(cause));
      }
    } finally {
      if (current === epoch.current) setBusy(false);
    }
  }
  const valid = preview?.identity === identity ? preview.value : undefined;
  return (
    <section className="knowledge-selector" aria-label="Knowledge selection">
      <h3>Project knowledge for this operation</h3>
      <p>
        Select exact approved revisions, then preview and confirm. Empty
        selection uses the existing path without an attachment. Selections are
        reference material and grant no authority.
      </p>
      <label>
        Find approved knowledge<input
          value={query}
          disabled={disabled || busy}
          onChange={(event) => {
            epoch.current++;
            setQuery(event.target.value);
            setHits([]);
            setNextEntryId(null);
            setSearching(false);
            setError("");
          }}
        />
      </label>
      <button
        type="button"
        disabled={disabled || busy || searching || utf8Bytes(query) > 512}
        onClick={() =>
          void search()}
      >
        Find approved knowledge
      </button>
      <ul>
        {hits.map((hit) => (
          <li key={hit.entry.id}>
            <button
              type="button"
              disabled={disabled || busy || searching || selection.length >= 16 ||
                selection.some((item) => item.entry_id === hit.entry.id)}
              onClick={() =>
                void add(hit)}
            >
              {hit.title} · revision {hit.revision_id}
            </button>
          </li>
        ))}
      </ul>
      {nextEntryId !== null && (
        <button type="button" disabled={disabled || busy || searching} onClick={() => void search(nextEntryId)}>
          Load more approved knowledge
        </button>
      )}
      <ol>
        {selection.map((item, index) => {
          const detail = details[item.revision_id];
          const observation = detail?.observation;
          const warning = observation &&
            ["needs_revalidation", "source_unavailable"].includes(
              observation.state,
            );
          return (
            <li key={item.revision_id}>
              <strong>{detail?.revision.title || item.entry_id}</strong>
              <p>
                Exact revision {item.revision_id} · entry version{" "}
                {item.expected_entry_version} · Approved selection
              </p>
              {detail && (
                <>
                  <p>
                    Freshness: {observation?.state.replaceAll("_", " ")}{" "}
                    · checked {observation?.checked_at}
                  </p>
                  {observation?.sources_omitted !== undefined && (
                    <p>Freshness details omitted for {observation.sources_omitted} sources.</p>
                  )}
                  <pre>{detail.revision.statement}</pre>
                  <details>
                    <summary>Exact provenance</summary>
                    <pre>{JSON.stringify(detail.revision.sources, null, 2)}</pre>
                  </details>
                </>
              )}
              {!detail && (
                <p>
                  Retained exact selector. Remove and find it again to review
                  current provenance and warnings before confirming.
                </p>
              )}
              {observation?.restore_operation_id
                ? (
                  <p role="alert">
                    Restored evidence requires a new explicit Revalidate in
                    Knowledge. A prior acknowledgement cannot clear this hold.
                  </p>
                )
                : warning && (
                  <label>
                    <input
                      type="checkbox"
                      disabled={disabled || busy}
                      checked={item.acknowledged_warning}
                      onChange={(event) =>
                        change(selection.map((selected, position) =>
                          position === index
                            ? {
                              ...selected,
                              acknowledged_warning: event.target.checked,
                            }
                            : selected
                        ))}
                    />{observation.sources_omitted !== undefined
                      ? "I acknowledge the current freshness warning."
                      : `I acknowledge this current warning: ${observation.sources.map((source) => source.reason).join("; ")}`}
                  </label>
                )}
              <button
                type="button"
                disabled={disabled || busy}
                onClick={() =>
                  change(selection.filter((_, position) =>
                    position !== index
                  ))}
              >
                Remove revision {item.revision_id}
              </button>
              {index > 0 && (
                <button
                  type="button"
                  disabled={disabled || busy}
                  onClick={() =>
                    change([
                      ...selection.slice(0, index - 1),
                      item,
                      selection[index - 1],
                      ...selection.slice(index + 1),
                    ])}
                >
                  Move earlier
                </button>
              )}
            </li>
          );
        })}
      </ol>
      <p>
        {selection.length}{" "}
        / 16 selected. Whole entries that exceed the available bytes are
        omitted.
      </p>
      {selection.length > 0 && (
        <button
          type="button"
          disabled={disabled || busy}
          onClick={() => change([])}
        >
          Clear knowledge selection
        </button>
      )}
      {(selection.length > 0 || allowEmptyPreview) && (
        <button
          type="button"
          disabled={disabled || busy || searching || !consumer ||
            selection.some((item) =>
              details[item.revision_id]?.observation.restore_operation_id
            )}
          onClick={() => void previewSelection()}
        >
          Preview knowledge attachment
        </button>
      )}
      {!consumer && selection.length > 0 && (
        <p>
          Save the task draft or prepare the original conversation operation to
          preview this selection.
        </p>
      )}
      {valid && (
        <section aria-label="Knowledge attachment preview">
          <h4>Exact attachment preview</h4>
          <pre>{valid.block || "No knowledge entries selected."}</pre>
          <p>
            {valid.included_count} included · {valid.omitted_count} omitted ·
            {" "}
            {valid.block_serialized_bytes} / {valid.block_limit} bytes
          </p>
          <ul>
            {valid.items.map((item) => (
              <li key={item.selection.revision_id}>
                Revision {item.selection.revision_id}:{" "}
                {item.disposition === "included"
                  ? "Included in full"
                  : "Omitted in full: byte limit"}
                {item.warning ? ` · ${item.warning}` : ""}
              </li>
            ))}
          </ul>
          <label>
            <input
              type="checkbox"
              disabled={disabled || busy}
              checked={confirmed}
              onChange={(event) => {
                setConfirmed(event.target.checked);
                onConfirm(
                  event.target.checked && consumer
                    ? {
                      request: { ...consumer, selection },
                      preview_fingerprint: valid.fingerprint,
                    }
                    : undefined,
                );
              }}
            />I confirm these exact included revisions and whole-entry omissions
            for this operation.
          </label>
        </section>
      )}
      {error && <ErrorNotice error={error} />}
    </section>
  );
}

interface TaskKnowledgePanelProps {
  task: Task;
  content?: TaskContent;
  initialSelection?: KnowledgeSelection[];
  onChanged: () => void | Promise<void>;
  onConfirmed?: () => void;
}
export function TaskKnowledgePanel(
  { task, content, initialSelection, onChanged, onConfirmed }:
    TaskKnowledgePanelProps,
) {
  const [confirmation, setConfirmation] = useState<KnowledgeConfirmation>();
  const [pending, setPending] = useState<Command>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [checked, setChecked] = useState(false);
  const [savedVersion, setSavedVersion] = useState<number>();
  const [terminalResult, setTerminalResult] = useState("");
  const key = `llmrelay.task.knowledge.${encodeURIComponent(task.project_id)}.${
    encodeURIComponent(task.id)
  }`;
  // A new scope gets a distinct lifetime even when an older request is still pending.
  const lifetime = useRef({ key, active: true });
  if (lifetime.current.key !== key) lifetime.current = { key, active: true };
  useEffect(() => {
    const scope = lifetime.current;
    scope.active = true;
    setPending(undefined);
    setConfirmation(undefined);
    setBusy(false);
    setError("");
    setChecked(false);
    setSavedVersion(undefined);
    setTerminalResult("");
    try {
      const raw = localStorage.getItem(key);
      if (raw) {
        const value: unknown = JSON.parse(raw);
        if (
          !value || typeof value !== "object" || !("operation_id" in value) ||
          !("kind" in value) || typeof value.operation_id !== "string" ||
          !["set_task_knowledge", "prepare_next_run_knowledge"].includes(
            String(value.kind),
          )
        ) {
          throw new Error(
            "The saved task knowledge request cannot be read. Keep browser storage and report its details.",
          );
        }
        setPending(value as Command);
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
    return () => {
      scope.active = false;
    };
  }, [key]);
  const preparation = task.knowledge?.preparation;
  const run = task.workflow?.run;
  const draft = !task.active_attempt &&
    ["backlog", "ready"].includes(task.lifecycle) && !task.archived;
  const terminalReview = [...task.reviews].reverse().find((review) =>
    ["code", "final"].includes(review.kind) &&
    review.delivery_state === "finished" &&
    ["request_changes", "needs_rework"].includes(review.verdict || "")
  );
  const terminalReports =
    content?.records.filter((record) =>
      record.review_kind === terminalReview?.kind &&
      record.outcome === terminalReview?.verdict
    ) || [];
  const next = preparation
    ? {
      parent: preparation.parent_attempt_id,
      continuation: preparation.continuation_kind,
      binding: preparation.parent_result_binding_id,
      review: preparation.parent_review_binding_id,
    }
    : task.active_attempt &&
        (task.lifecycle === "awaiting_review" ||
          !!run?.available_actions.includes("request_rework")) &&
        !task.archived
    ? {
      parent: task.active_attempt.id,
      continuation: run ? "single_agent_rework" : "trip_rework",
      binding: run?.result?.id || task.active_attempt.accepted_snapshot_id ||
        "",
      review: null,
    }
    : task.active_attempt?.phase === "needs_input" && terminalReview &&
        terminalResult &&
        terminalReports.some((record) => record.id === terminalResult)
    ? {
      parent: task.active_attempt.id,
      continuation: "terminal_replan",
      binding: terminalResult,
      review: terminalReview.id,
    }
    : undefined;
  const consumer: Omit<KnowledgePreviewRequest, "selection"> | undefined =
    task.knowledge && !task.archived && (draft || next?.binding)
      ? {
        project_id: task.project_id,
        user_text: task.knowledge.preview_user_text,
        expected_consumer_version: task.version,
        consumer: {
          kind: "task",
          task_id: task.id,
          parent_attempt_id: draft ? null : next!.parent,
          continuation: draft ? null : next!.continuation,
          parent_binding_id: draft ? null : next!.binding,
        },
      }
      : undefined;
  async function dispatch(request: Command, replay = false): Promise<void> {
    if (busy || (replay ? !checked || pending !== request : !!pending)) return;
    const scope = lifetime.current;
    const current = () => scope.active && lifetime.current === scope;
    const body = JSON.stringify(request);
    let journaled = false;
    let accepted = false;
    const removeExact = () => withStorageLock(key, () => {
      if (localStorage.getItem(key) === body) localStorage.removeItem(key);
    });
    const errorText = (value: unknown) => value instanceof Error ? value.message : String(value);
    setBusy(true);
    setError("");
    setChecked(false);
    try {
      await withStorageLock(key, () => {
        const existing = localStorage.getItem(key);
        if (replay ? existing !== body : existing !== null) {
          throw new Error("The task knowledge journal changed. Refresh the task and check the saved request.");
        }
        if (!replay) localStorage.setItem(key, body);
      });
      journaled = true;
      if (current()) setPending(request);
      const result = await command(request);
      accepted = true;
      await removeExact();
      journaled = false;
      if (!current()) return;
      setPending(undefined);
      setConfirmation(undefined);
      if (typeof result.result.version === "number") {
        setSavedVersion(result.result.version);
      }
      onConfirmed?.();
      await onChanged();
    } catch (cause) {
      let message = errorText(cause);
      if (journaled && !accepted && !replay &&
        !(cause instanceof ApiError && cause.ambiguous)) {
        try {
          await removeExact();
          journaled = false;
          if (current()) setPending(undefined);
        } catch (cleanupError) {
          message = errorText(cleanupError);
        }
      }
      if (current()) setError(
        journaled
          ? `${message} Refresh and check the outcome before retrying. The exact operation is retained.`
          : message,
      );
    } finally {
      if (current()) setBusy(false);
    }
  }
  async function save(continueRework: boolean): Promise<void> {
    if (
      !confirmation || !consumer ||
      JSON.stringify({
          ...consumer,
          selection: confirmation.request.selection,
        }) !== JSON.stringify(confirmation.request)
    ) {
      setError("The input or task version changed. Preview and confirm again.");
      return;
    }
    const id = operationId();
    if (draft) {
      await dispatch({
        kind: "set_task_knowledge",
        operation_id: id,
        request: confirmation.request,
        preview_fingerprint: confirmation.preview_fingerprint,
      });
    } else if (next) {
      await dispatch({
        kind: "prepare_next_run_knowledge",
        operation_id: id,
        request: {
          operation_id: id,
          project_id: task.project_id,
          task_id: task.id,
          parent_attempt_id: next.parent,
          continuation_kind: next.continuation,
          expected_task_version: task.version,
          expected_preparation_version: preparation?.version ?? null,
          parent_result_binding_id: next.binding,
          parent_review_binding_id: next.review,
          selection: confirmation.request.selection,
          preview_fingerprint: confirmation.preview_fingerprint,
          action: continueRework ? "save_and_continue_rework" : "save_only",
        },
      });
    }
  }
  return (
    <section className="panel" aria-label="Task knowledge">
      <h3>Task knowledge</h3>
      {task.knowledge?.admission_hold && (
        <ErrorNotice error={task.knowledge.admission_hold} />
      )}
      {preparation?.hold_reason && (
        <>
          <ErrorNotice error={preparation.hold_reason} />
          <p>
            Choose valid knowledge, preview and confirm, then Save and continue
            rework. Save only leaves the parent and child lineage unchanged.
          </p>
        </>
      )}
      {!draft && !preparation && terminalReview &&
        task.active_attempt?.phase === "needs_input" && (
        <label>
          Exact terminal review result<select
            value={terminalResult}
            onChange={(event) => {
              setTerminalResult(event.target.value);
              setConfirmation(undefined);
            }}
          >
            <option value="">Choose the recorded result for this review</option>
            {terminalReports.map((record) => (
              <option key={record.id} value={record.id}>
                {record.id} · {record.created_at} ·{" "}
                {record.summary.slice(0, 120)}
              </option>
            ))}
          </select>
          <small>
            Review{" "}
            {terminalReview.id}; the host checks the exact parent, review and
            result association before any child is created.
          </small>
        </label>
      )}
      {consumer && (
        <>
          <KnowledgeSelector
            key={`${task.project_id}:${task.id}:${
              draft ? "draft" : next?.parent
            }:${next?.continuation || ""}`}
            projectId={task.project_id}
            consumer={consumer}
            initialSelection={initialSelection ||
              preparation?.confirmation?.request.selection ||
              task.knowledge?.selection?.request.selection || []}
            invalidationKey={`${task.version}:${preparation?.version || 0}`}
            disabled={busy || !!pending}
            allowEmptyPreview
            onConfirm={setConfirmation}
          />
          <button
            type="button"
            disabled={busy || !!pending || !confirmation}
            onClick={() => void save(false)}
          >
            {draft
              ? "Save confirmed task knowledge"
              : "Save next-run knowledge only"}
          </button>
          {!draft && (
            <button
              type="button"
              disabled={busy || !!pending || !confirmation}
              onClick={() => void save(true)}
            >
              Save and continue rework
            </button>
          )}
          {savedVersion !== undefined && (
            <p role="status">
              Knowledge saved. Review the task's current version and choose Make
              Ready explicitly when ready.
            </p>
          )}
        </>
      )}
      {pending && (
        <section className="warning">
          <p>
            This request may already be committed. Refresh the task and check
            its selection, preparation or child before retrying.
          </p>
          <details>
            <summary>Exact retained task knowledge operation</summary>
            <pre>{JSON.stringify(pending, null, 2)}</pre>
          </details>
          <button
            type="button"
            disabled={busy}
            onClick={() => void onChanged()}
          >
            Refresh task outcome
          </button>
          <label>
            <input
              type="checkbox"
              checked={checked}
              onChange={(event) => setChecked(event.target.checked)}
            />I checked the task outcome.
          </label>
          <button
            type="button"
            disabled={busy || !checked}
            onClick={() => void dispatch(pending, true)}
          >
            Retry exact task knowledge operation
          </button>
        </section>
      )}
      {error && <ErrorNotice error={error} />}
      {(task.knowledge?.manifests || []).map((manifest) => (
        <TaskKnowledgeManifest
          key={manifest.id}
          projectId={task.project_id}
          manifestId={manifest.id}
        />
      ))}
    </section>
  );
}
function TaskKnowledgeManifest(
  { projectId, manifestId }: { projectId: string; manifestId: string },
) {
  const [manifest, setManifest] = useState<KnowledgeManifest>();
  const [error, setError] = useState("");
  useEffect(() => {
    let abandoned = false;
    void knowledgeRead(projectId, { kind: "manifest", manifest_id: manifestId })
      .then((value) => {
        if (!abandoned) setManifest(value);
      }).catch((cause: unknown) => {
        if (!abandoned) setError(cause instanceof Error ? cause.message : String(cause));
      });
    return () => {
      abandoned = true;
    };
  }, [projectId, manifestId]);
  return (
    <details>
      <summary>Frozen and available in context · {manifestId}</summary>
      <p>
        Delivery unverified. Availability does not establish that an agent read
        or understood the attachment.
      </p>
      {manifest && (
        <>
          <pre>{manifest.preview.block}</pre>
          <ul>
            {manifest.preview.items.map((item) => (
              <li key={item.selection.revision_id}>
                Revision {item.selection.revision_id} ·{" "}
                {item.disposition.replaceAll("_", " ")}
              </li>
            ))}
          </ul>
          <pre>{JSON.stringify(manifest.sources, null, 2)}</pre>
        </>
      )}
      {error && <ErrorNotice error={error} />}
    </details>
  );
}
