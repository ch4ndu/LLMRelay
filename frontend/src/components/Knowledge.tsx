import { useEffect, useRef, useState } from "react";
import {
  ApiError,
  historySearch,
  knowledgeCommand,
  knowledgeCommandIsCurrent,
  knowledgeRead,
  operationId,
  recoveryJournalChanged,
  utf8Bytes,
} from "../api";
import type {
  KnowledgeCommand,
  KnowledgeDraftBody,
  KnowledgeEntry,
  KnowledgeLifecycleEvent,
  KnowledgeManifest,
  KnowledgeObservation,
  KnowledgeRevision,
  KnowledgeSearchHit,
  KnowledgeSourceSelector,
} from "../types";
import { withStorageLock } from "../storageCoordination";
import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";

const errorText = (cause: unknown): string =>
  cause instanceof Error ? cause.message : String(cause);
const knowledgeInFlight = new Set<string>();
export function knowledgeSourceLabel(source: KnowledgeSourceSelector): string {
  switch (source.kind) {
    case "historical":
      return `Historical ${source.source.kind} ${source.source.id} · task ${source.source.task_id} · attempt ${
        source.source.attempt_id || "none"
      }`;
    case "conversation":
      return `Conversation ${source.source.conversation_id} · exact messages ${
        source.source.messages.map((message) => message.message_id).join(", ")
      }`;
    case "finding":
      return `${source.parent.kind} ${
        source.parent.kind === "explorer"
          ? source.parent.decision_id
          : source.parent.role_result_id
      } · attempt ${source.parent.attempt_id} · ${source.field}${
        source.index === null ? "" : `[${source.index}]`
      }`;
    case "repository":
      return `Committed file ${source.commit}:${source.path}`;
    case "user_assertion":
      return "User assertion · no external verification";
  }
}

function useKnowledgeMutation(
  projectId: string,
  onSaved: (entryId?: string, revisionId?: string, revalidated?: boolean) => void,
) {
  const key = `llmrelay.knowledge.pending.${encodeURIComponent(projectId)}`;
  const [pending, setPending] = useState<KnowledgeCommand>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [checked, setChecked] = useState<string>();
  const [journalUnreadable, setJournalUnreadable] = useState(false);
  // A new scope gets a distinct lifetime even when an older request is still pending.
  const lifetime = useRef({ key, active: true });
  if (lifetime.current.key !== key) lifetime.current = { key, active: true };
  useEffect(() => {
    const scope = lifetime.current;
    scope.active = true;
    setPending(undefined);
    setBusy(false);
    setError("");
    setChecked(undefined);
    const sync = () => {
      try {
        const raw = localStorage.getItem(key);
        const saved: unknown = raw === null ? undefined : JSON.parse(raw);
        if (raw !== null && (
          !knowledgeCommandIsCurrent(saved) ||
          (saved.kind === "mutate" ? saved.request.project_id : saved.project_id) !== projectId ||
          JSON.stringify(saved) !== raw
        )) throw new Error("Invalid saved knowledge request");
        setPending(knowledgeCommandIsCurrent(saved) ? saved : undefined);
        setJournalUnreadable(false);
        setChecked((reviewed) => reviewed === raw ? reviewed : undefined);
      } catch {
        setPending(undefined);
        setChecked(undefined);
        setJournalUnreadable(true);
      }
    };
    const onLocalWrite = (event: Event) => {
      if (event instanceof window.CustomEvent && event.detail === key) sync();
    };
    const onStorage = (event: StorageEvent) => {
      if (event.key === key || event.key === null) sync();
    };
    sync();
    window.addEventListener(recoveryJournalChanged, onLocalWrite);
    window.addEventListener("storage", onStorage);
    return () => {
      scope.active = false;
      window.removeEventListener(recoveryJournalChanged, onLocalWrite);
      window.removeEventListener("storage", onStorage);
    };
  }, [key, projectId]);
  async function dispatch(
    request: KnowledgeCommand,
    replay = false,
  ): Promise<void> {
    const body = JSON.stringify(request);
    if (busy || journalUnreadable || knowledgeInFlight.has(key) ||
      (replay ? checked !== body || pending !== request : !!pending)) return;
    const scope = lifetime.current;
    const current = () => scope.active && lifetime.current === scope;
    let journaled = false;
    let accepted = false;
    const removeExact = () => withStorageLock(key, () => {
      if (localStorage.getItem(key) !== body) return false;
      localStorage.removeItem(key);
      return true;
    });
    const notify = () => window.dispatchEvent(
      new window.CustomEvent(recoveryJournalChanged, { detail: key }),
    );
    knowledgeInFlight.add(key);
    setBusy(true);
    setError("");
    setChecked(undefined);
    try {
      await withStorageLock(key, () => {
        if (!current()) return;
        const existing = localStorage.getItem(key);
        if (replay ? existing !== body : existing !== null) {
          throw new Error("The project knowledge journal changed. Refresh Knowledge and check the saved request.");
        }
        if (!replay) localStorage.setItem(key, body);
        journaled = true;
      });
      if (!journaled) return;
      if (!current()) {
        if (!replay) await removeExact();
        return;
      }
      notify();
      const result = await knowledgeCommand(request);
      accepted = true;
      const removed = await removeExact();
      journaled = false;
      if (!current() || !removed) return;
      if (request.kind === "mutate" && "id" in result) {
        onSaved(
          result.id,
          result.latest_draft_id || result.approved_revision_id || undefined,
        );
      } else if (request.kind === "revalidate") {
        onSaved(request.entry_id, request.revision_id, true);
      }
    } catch (cause) {
      let message = errorText(cause);
      if (journaled && !accepted && !replay &&
        !(cause instanceof ApiError && cause.ambiguous)) {
        try {
          await removeExact();
          journaled = false;
        } catch (cleanupError) {
          message = errorText(cleanupError);
        }
      }
      if (current()) setError(
        journaled
          ? `${message} Refresh and check the outcome before retrying. The original operation identity is retained.`
          : message,
      );
    } finally {
      knowledgeInFlight.delete(key);
      notify();
      if (current()) setBusy(false);
    }
  }
  const notice = (
    <>
      {pending && (
        <section className="warning" aria-label="Pending knowledge operation">
          <p>
            This operation may already be committed. Refresh Knowledge and
            inspect the exact revision before retrying. Retrying uses the
            original ID and body.
          </p>
          <details>
            <summary>Original operation</summary>
            <pre>{JSON.stringify(pending, null, 2)}</pre>
          </details>
          <label>
            <input
              type="checkbox"
              checked={checked === JSON.stringify(pending)}
              disabled={busy || knowledgeInFlight.has(key)}
              onChange={(event) =>
                setChecked(event.target.checked ? JSON.stringify(pending) : undefined)}
            />I refreshed and checked this operation's outcome.
          </label>
          <button
            type="button"
            disabled={busy || knowledgeInFlight.has(key) || checked !== JSON.stringify(pending)}
            onClick={() =>
              void dispatch(pending, true)}
          >
            Check / retry exact saved operation
          </button>
        </section>
      )}
      {journalUnreadable && <ErrorNotice error="The saved knowledge request cannot be read. Refresh Knowledge and keep the saved request available for troubleshooting." />}
      {error && <ErrorNotice error={error} />}
    </>
  );
  return { dispatch, blocked: busy || !!pending || journalUnreadable, busy, notice };
}

interface DraftEditorProps {
  projectId: string;
  body?: KnowledgeDraftBody;
  entry?: KnowledgeEntry;
  onSaved?: (entryId?: string, revisionId?: string) => void;
  onClose: () => void;
}
export function KnowledgeDraftEditor(
  { projectId, body, entry, onSaved, onClose }: DraftEditorProps,
) {
  const [draft, setDraft] = useState<KnowledgeDraftBody>(
    body ||
      {
        title: "",
        statement: "",
        category: "note",
        tags: [],
        sources: [{ kind: "user_assertion" }],
      },
  );
  const [tagText, setTagText] = useState(draft.tags.join(", "));
  const [commit, setCommit] = useState("");
  const [path, setPath] = useState("");
  const [sourceQuery, setSourceQuery] = useState("");
  const [sourceHits, setSourceHits] = useState<
    import("../types").HistorySearchHit[]
  >([]);
  const [sourceError, setSourceError] = useState("");
  const sourceEpoch = useRef(0);
  useEffect(() => () => {
    sourceEpoch.current++;
  }, []);
  async function findSources(): Promise<void> {
    const current = ++sourceEpoch.current;
    setSourceError("");
    try {
      const result = await historySearch({
        query: sourceQuery,
        project_id: projectId,
        exclude_archived: false,
      });
      if (current === sourceEpoch.current) {
        setSourceHits(
          result.results.filter((hit) => hit.project_id === projectId),
        );
      }
    } catch (cause) {
      if (current === sourceEpoch.current) setSourceError(errorText(cause));
    }
  }
  const mutation = useKnowledgeMutation(projectId, (entryId, revisionId) => {
    onSaved?.(entryId, revisionId);
    onClose();
  });
  const valid = draft.title.trim() && utf8Bytes(draft.title) <= 256 &&
    draft.statement.trim() && utf8Bytes(draft.statement) <= 8192 &&
    draft.category.trim() && utf8Bytes(draft.category) <= 64 &&
    draft.tags.length <= 16 &&
    draft.tags.every((tag) => tag.trim() && utf8Bytes(tag) <= 64) &&
    draft.sources.length > 0 && draft.sources.length <= 16 &&
    ![draft.title, draft.statement, draft.category, ...draft.tags].some((
      value,
    ) => value.includes("\0")) && utf8Bytes(JSON.stringify({
        kind: "mutate",
        request: {
          operation_id: "00000000-0000-0000-0000-000000000000",
          project_id: projectId,
          mutation: {
            kind: "draft",
            entry_id: entry?.id || null,
            expected_version: entry?.version ?? null,
            body: draft,
          },
        },
      })) <= 32768;
  return (
    <section
      className="panel knowledge-editor"
      aria-label="Knowledge draft editor"
    >
      <h3>
        {entry ? "Edit as a new draft revision" : "Save as knowledge draft"}
      </h3>
      <p>
        Write your statement from the exact source. Source previews may be
        incomplete. Saving creates a draft; approval is a separate explicit
        action. An existing approval remains until explicit replacement.
      </p>
      <fieldset disabled={mutation.blocked}>
        <label>
          Knowledge title<input
            value={draft.title}
            onChange={(event) =>
              setDraft({ ...draft, title: event.target.value })}
          />
        </label>
        <label>
          Knowledge statement<textarea
            rows={6}
            value={draft.statement}
            onChange={(event) =>
              setDraft({ ...draft, statement: event.target.value })}
          />
        </label>
        <small>{utf8Bytes(draft.statement)} / 8192 UTF-8 bytes</small>
        <label>
          Category<input
            value={draft.category}
            onChange={(event) =>
              setDraft({ ...draft, category: event.target.value })}
          />
        </label>
        <label>
          Tags (comma separated)<input
            value={tagText}
            onChange={(event) => {
              setTagText(event.target.value);
              setDraft({
                ...draft,
                tags: event.target.value.split(",").map((tag) => tag.trim())
                  .filter(Boolean),
              });
            }}
          />
        </label>
        <ol>
          {draft.sources.map((source, index) => (
            <li key={index}>
              <p>{knowledgeSourceLabel(source)}</p>
              <details>
                <summary>Exact source selector</summary>
                <pre>{JSON.stringify(source, null, 2)}</pre>
              </details>
              <button
                type="button"
                onClick={() =>
                  setDraft({
                    ...draft,
                    sources: draft.sources.filter((_, position) =>
                      position !== index
                    ),
                  })}
              >
                Remove source {index + 1}
              </button>
            </li>
          ))}
        </ol>
        <button
          type="button"
          disabled={draft.sources.some((source) =>
            source.kind === "user_assertion"
          ) || draft.sources.length >= 16}
          onClick={() =>
            setDraft({
              ...draft,
              sources: [...draft.sources, { kind: "user_assertion" }],
            })}
        >
          Add user assertion origin
        </button>
        <details>
          <summary>Add exact historical source</summary>
          <label>
            Find source text<input
              value={sourceQuery}
              onChange={(event) => {
                sourceEpoch.current++;
                setSourceQuery(event.target.value);
                setSourceHits([]);
              }}
            />
          </label>
          <button
            type="button"
            disabled={!sourceQuery.trim() ||
              utf8Bytes(sourceQuery.trim()) > 256}
            onClick={() => void findSources()}
          >
            Find historical sources
          </button>
          <ul>
            {sourceHits.map((hit) => (
              <li key={JSON.stringify(hit.source)}>
                <p>
                  {hit.task_title} · {hit.source.kind} · attempt{" "}
                  {hit.source.attempt_id || "none"} · record {hit.source.id}
                </p>
                <p>Search excerpt only: {hit.snippet}</p>
                <button
                  type="button"
                  disabled={draft.sources.length >= 16 ||
                    draft.sources.some((source) =>
                      source.kind === "historical" &&
                      JSON.stringify(source.source) ===
                        JSON.stringify(hit.source)
                    )}
                  onClick={() =>
                    setDraft({
                      ...draft,
                      sources: [...draft.sources, {
                        kind: "historical",
                        source: hit.source,
                      }],
                    })}
                >
                  Add exact historical source {hit.source.id}
                </button>
              </li>
            ))}
          </ul>
          {sourceError && <ErrorNotice error={sourceError} />}
          <p>
            The host resolves complete supported source content; a search
            excerpt is never the source authority.
          </p>
        </details>
        <details>
          <summary>Add committed repository source</summary>
          <label>
            Exact commit<input
              value={commit}
              onChange={(event) => setCommit(event.target.value)}
            />
          </label>
          <label>
            Relative file path<input
              value={path}
              onChange={(event) => setPath(event.target.value)}
            />
          </label>
          <button
            type="button"
            disabled={!/^(?:[a-fA-F0-9]{40}|[a-fA-F0-9]{64})$/.test(commit) ||
              !path || utf8Bytes(path) > 512 || path.startsWith("/") ||
              path.includes("\\") || /[\x00-\x1f\x7f]/.test(path) ||
              path.split("/").some((part) =>
                ["", ".", "..", ".git"].includes(part)
              ) || draft.sources.length >= 16}
            onClick={() => {
              setDraft({
                ...draft,
                sources: [...draft.sources, {
                  kind: "repository",
                  commit,
                  path,
                }],
              });
              setCommit("");
              setPath("");
            }}
          >
            Add committed file
          </button>
        </details>
        <button
          type="button"
          disabled={!valid}
          onClick={() =>
            void mutation.dispatch({
              kind: "mutate",
              request: {
                operation_id: operationId(),
                project_id: projectId,
                mutation: {
                  kind: "draft",
                  entry_id: entry?.id || null,
                  expected_version: entry?.version ?? null,
                  body: draft,
                },
              },
            })}
        >
          Save knowledge draft
        </button>
      </fieldset>
      {mutation.notice}
      <button type="button" disabled={mutation.busy} onClick={onClose}>
        Close draft editor
      </button>
      <p>
        After saving, open Knowledge and include drafts to review and approve
        the exact revision.
      </p>
    </section>
  );
}

export function SaveKnowledgeSource(
  { projectId, source, title = "", statement = "" }: {
    projectId: string;
    source: KnowledgeSourceSelector;
    title?: string;
    statement?: string;
  },
) {
  const [open, setOpen] = useState(false);
  const [saved, setSaved] = useState(false);
  return (
    <div className="knowledge-source-save">
      <button
        type="button"
        onClick={() => {
          setOpen(true);
          setSaved(false);
        }}
      >
        Save as knowledge
      </button>
      {saved && (
        <p role="status">
          Knowledge draft saved. Open Knowledge and include drafts to review it;
          it has no approval.
        </p>
      )}
      {open && (
        <KnowledgeDraftEditor
          key={JSON.stringify(source)}
          projectId={projectId}
          body={{
            title,
            statement,
            category: "note",
            tags: [],
            sources: [source],
          }}
          onSaved={() => setSaved(true)}
          onClose={() => setOpen(false)}
        />
      )}
    </div>
  );
}

export function KnowledgeManifestDetail(
  { projectId, manifestId, delivery = "Delivery unverified" }: {
    projectId: string;
    manifestId: string;
    delivery?: string;
  },
) {
  const [manifest, setManifest] = useState<KnowledgeManifest>();
  const [error, setError] = useState("");
  useEffect(() => {
    let abandoned = false;
    setManifest(undefined);
    setError("");
    void knowledgeRead(projectId, { kind: "manifest", manifest_id: manifestId })
      .then((value) => {
        if (!abandoned) setManifest(value);
      }).catch((cause: unknown) => {
        if (!abandoned) setError(errorText(cause));
      });
    return () => {
      abandoned = true;
    };
  }, [projectId, manifestId]);
  return (
    <details className="knowledge-manifest">
      <summary>Knowledge attachment · {manifestId}</summary>
      <p>
        Frozen and available in context.{" "}
        {delivery}. This does not establish that an agent read or understood it.
      </p>
      {error && <ErrorNotice error={error} />}
      {manifest && (
        <>
          <pre>{manifest.preview.block}</pre>
          <ul>
            {manifest.preview.items.map((item) => (
              <li key={item.selection.revision_id}>
                Revision {item.selection.revision_id} ·{" "}
                {item.disposition.replaceAll("_", " ")} ·{" "}
                {item.freshness.replaceAll("_", " ")}
              </li>
            ))}
          </ul>
          <details>
            <summary>Frozen provenance and binding</summary>
            <pre>{JSON.stringify({ binding: manifest.binding, sources: manifest.sources }, null, 2)}</pre>
          </details>
        </>
      )}
    </details>
  );
}

export function Knowledge({ projectId }: { projectId: string }) {
  const [query, setQuery] = useState("");
  const [drafts, setDrafts] = useState(false);
  const [archived, setArchived] = useState(false);
  const [hits, setHits] = useState<KnowledgeSearchHit[]>([]);
  const [more, setMore] = useState<string | null>(null);
  const [detail, setDetail] = useState<
    {
      entry: KnowledgeEntry;
      revision: KnowledgeRevision;
      observation: KnowledgeObservation;
      history: KnowledgeLifecycleEvent[];
    }
  >();
  const [source, setSource] = useState<{ label: string; text: string }>();
  const [editor, setEditor] = useState<"new" | "edit">();
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [showHistory, setShowHistory] = useState(false);
  const [historyMore, setHistoryMore] = useState(false);
  const [approved, setApproved] = useState(false);
  const [archiveConfirmed, setArchiveConfirmed] = useState(false);
  const epoch = useRef(0);
  useEffect(() => () => {
    epoch.current++;
  }, []);
  async function search(
    after: string | null = null,
    refreshEpoch?: number,
  ): Promise<void> {
    const current = refreshEpoch ?? ++epoch.current;
    setBusy(true);
    setError("");
    try {
      const result = await knowledgeRead(projectId, {
        kind: "search",
        search: {
          query,
          include_drafts: drafts,
          include_archived: archived,
          after_entry_id: after,
          limit: 50,
        },
      });
      if (current === epoch.current) {
        setHits((old) => after ? [...old, ...result] : result);
        setMore(
          result.length === 50 ? result[result.length - 1].entry.id : null,
        );
      }
    } catch (cause) {
      if (current === epoch.current) setError(errorText(cause));
    } finally {
      if (refreshEpoch === undefined && current === epoch.current) setBusy(false);
    }
  }
  useEffect(() => {
    setHits([]);
    setDetail(undefined);
    setSource(undefined);
    setEditor(undefined);
    void search();
  }, [projectId, drafts, archived]);
  async function open(
    entryId: string,
    revisionId: string,
    refreshEpoch?: number,
    restoreOnFailure = true,
  ): Promise<void> {
    const current = refreshEpoch ?? ++epoch.current;
    setBusy(true);
    setError("");
    setDetail(undefined);
    setSource(undefined);
    setApproved(false);
    setArchiveConfirmed(false);
    setEditor(undefined);
    try {
      const [entry, revision, observation, history] = await Promise.all([
        knowledgeRead(projectId, { kind: "entry", entry_id: entryId }),
        knowledgeRead(projectId, {
          kind: "revision",
          entry_id: entryId,
          revision_id: revisionId,
        }),
        knowledgeRead(projectId, {
          kind: "freshness",
          entry_id: entryId,
          revision_id: revisionId,
        }),
        knowledgeRead(projectId, {
          kind: "history",
          entry_id: entryId,
          after_version: 0,
        }),
      ]);
      if (current === epoch.current) {
        setDetail({ entry, revision, observation, history });
        setHistoryMore(history.length === 50);
      }
    } catch (cause) {
      if (current === epoch.current) {
        if (restoreOnFailure && refreshEpoch !== undefined && detail?.entry.id === entryId &&
          detail.revision.id === revisionId) {
          setDetail(detail);
        }
        setError(errorText(cause));
      }
    } finally {
      if (refreshEpoch === undefined && current === epoch.current) setBusy(false);
    }
  }
  async function refresh(
    entryId = detail?.entry.id,
    revisionId = detail?.revision.id,
    restoreOnFailure = true,
  ): Promise<void> {
    // Both reads share a lifetime; neither may invalidate the other or end busy early.
    const current = ++epoch.current;
    try {
      await Promise.all([
        search(null, current),
        entryId && revisionId ? open(entryId, revisionId, current, restoreOnFailure) : undefined,
      ]);
    } finally {
      if (current === epoch.current) setBusy(false);
    }
  }
  const visibleEpoch = epoch.current;
  const mutation = useKnowledgeMutation(projectId, (entryId, revisionId, revalidated) => {
    if (epoch.current !== visibleEpoch) return;
    if (detail) {
      const viewedRevisionRevalidated = revalidated && detail.revision.id === revisionId;
      void refresh(detail.entry.id, detail.revision.id, !viewedRevisionRevalidated);
    } else {
      void refresh(entryId, revisionId);
    }
  });
  async function openSource(index: number): Promise<void> {
    if (!detail) return;
    const current = ++epoch.current;
    setSource(undefined);
    setError("");
    try {
      const value = await knowledgeRead(projectId, {
        kind: "source",
        entry_id: detail.entry.id,
        revision_id: detail.revision.id,
        index,
      });
      if (current === epoch.current) {
        setSource({
          label: knowledgeSourceLabel(value.source.selector),
          text: value.text,
        });
      }
    } catch (cause) {
      if (current === epoch.current) {
        setError(
          `The exact source is unavailable. Its original identity is retained; no current source was substituted. ${
            errorText(cause)
          }`,
        );
      }
    }
  }
  return (
    <section className="knowledge-view">
      <header className="page-heading">
        <div>
          <h1>Knowledge</h1>
          <p>
            Knowledge belongs to the selected project. Approval, freshness and
            user assertion origin are separate facts.
          </p>
        </div>
      </header>
      <label>
        Search project knowledge<input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
        />
      </label>
      <button
        type="button"
        disabled={busy || utf8Bytes(query) > 512}
        onClick={() => void search()}
      >
        Search knowledge
      </button>
      <label>
        <input
          type="checkbox"
          checked={drafts}
          disabled={busy}
          onChange={(event) => setDrafts(event.target.checked)}
        />Include drafts
      </label>
      <label>
        <input
          type="checkbox"
          checked={archived}
          disabled={busy}
          onChange={(event) => setArchived(event.target.checked)}
        />Include archived
      </label>
      <p>Default results show current approved revisions.</p>
      <button
        type="button"
        disabled={busy}
        onClick={() => void refresh()}
      >
        Refresh Knowledge
      </button>
      <button
        type="button"
        disabled={mutation.blocked}
        onClick={() => setEditor("new")}
      >
        New knowledge draft
      </button>
      {error && <ErrorNotice error={error} />}
      {mutation.notice}
      <ul>
        {hits.map((hit) => (
          <li key={hit.entry.id}>
            <button
              disabled={busy}
              onClick={() => void open(hit.entry.id, hit.revision_id)}
            >
              {hit.title} · {hit.revision_id}
              {hit.entry.archived_at ? " · Archived" : ""}
            </button>
          </li>
        ))}
      </ul>
      {more && (
        <button disabled={busy} onClick={() => void search(more)}>
          Load more knowledge
        </button>
      )}
      {detail && (
        <article className="panel" aria-label="Knowledge revision">
          <h2>{detail.revision.title}</h2>
          <p>
            Exact revision {detail.revision.id} · entry {detail.entry.id}{" "}
            · version {detail.entry.version}
          </p>
          <p>
            Approval: {detail.entry.approved_revision_id === detail.revision.id
              ? "Current approved revision"
              : detail.entry.latest_draft_id === detail.revision.id
              ? "Draft — no approval"
              : "Historical revision"}
            {detail.entry.archived_at ? " · Entry archived" : ""}
          </p>
          {detail.history.filter((event) =>
            event.kind === "approved" &&
            event.revision_id === detail.revision.id
          ).map((event) => (
            <p key={event.entry_version}>
              Explicit approval by {event.actor} at {event.created_at}
              {event.replaced_revision_id
                ? `; replaced revision ${event.replaced_revision_id}`
                : ""}.
            </p>
          ))}
          <p>
            Freshness: {detail.observation.state.replaceAll("_", " ")}{" "}
            · scoped check {detail.observation.checked_at}
          </p>
          <p>
            Origin:{" "}
            {detail.revision.sources.some((item) =>
                item.selector.kind === "user_assertion"
              )
              ? "Includes a user assertion; no external verification is claimed for that assertion"
              : "Source based statement"} · author {detail.revision.author}
          </p>
          <pre>{detail.revision.statement}</pre>
          <p>{detail.revision.category} · {detail.revision.tags.join(", ")}</p>
          {detail.observation.sources.map((item, index) => (
            <p key={index}>{item.state.replaceAll("_", " ")}: {item.reason}</p>
          ))}
          {detail.observation.sources_omitted !== undefined && (
            <p>Freshness details omitted for {detail.observation.sources_omitted} sources.</p>
          )}
          <TechnicalDetails>
            <p>Observation fingerprint <code>{detail.observation.fingerprint}</code></p>
          </TechnicalDetails>
          {detail.observation.restore_operation_id && (
            <p role="alert">
              Restored source observations require new explicit revalidation,
              even if an old warning was acknowledged.
            </p>
          )}
          <h3>Provenance</h3>
          <ol>
            {detail.revision.sources.map((item, index) => (
              <li key={item.identity_hash}>
                <p>{knowledgeSourceLabel(item.selector)}</p>
                <small>Original content hash {item.content_hash}</small>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() =>
                    void openSource(index)}
                >
                  Open exact source {index + 1}
                </button>
              </li>
            ))}
          </ol>
          {source && (
            <section aria-label="Exact knowledge source">
              <h4>{source.label}</h4>
              <p>
                Historical source identity; this is not navigation to a current
                replacement.
              </p>
              <pre>{source.text}</pre>
            </section>
          )}
          {!detail.entry.archived_at && (
            <>
              <button
                type="button"
                disabled={mutation.blocked}
                onClick={() => setEditor("edit")}
              >
                Edit as draft
              </button>
              <button
                type="button"
                disabled={mutation.blocked}
                onClick={() =>
                  void mutation.dispatch({
                    kind: "revalidate",
                    project_id: projectId,
                    operation_id: operationId(),
                    entry_id: detail.entry.id,
                    revision_id: detail.revision.id,
                    expected_version: detail.entry.version,
                    expected_observation: detail.observation.fingerprint,
                  })}
              >
                Revalidate exact revision
              </button>
              {detail.entry.latest_draft_id === detail.revision.id && (
                <section>
                  <label>
                    <input
                      type="checkbox"
                      checked={approved}
                      onChange={(event) => setApproved(event.target.checked)}
                    />Approve revision {detail.revision.id}
                    {detail.entry.approved_revision_id
                      ? ` and replace approved revision ${detail.entry.approved_revision_id}`
                      : " as this entry's first approved revision"}.
                  </label>
                  <button
                    type="button"
                    disabled={mutation.blocked || !approved}
                    onClick={() =>
                      void mutation.dispatch({
                        kind: "mutate",
                        request: {
                          project_id: projectId,
                          operation_id: operationId(),
                          mutation: {
                            kind: "approve",
                            entry_id: detail.entry.id,
                            expected_version: detail.entry.version,
                            revision_id: detail.revision.id,
                            replaces_revision_id:
                              detail.entry.approved_revision_id,
                          },
                        },
                      })}
                  >
                    Approve named revision
                  </button>
                </section>
              )}
              <label>
                <input
                  type="checkbox"
                  checked={archiveConfirmed}
                  onChange={(event) =>
                    setArchiveConfirmed(event.target.checked)}
                />Archive this entry for future selection and retain all
                historical revisions and attachments.
              </label>
              <button
                type="button"
                disabled={mutation.blocked || !archiveConfirmed}
                onClick={() =>
                  void mutation.dispatch({
                    kind: "mutate",
                    request: {
                      project_id: projectId,
                      operation_id: operationId(),
                      mutation: {
                        kind: "archive",
                        entry_id: detail.entry.id,
                        expected_version: detail.entry.version,
                      },
                    },
                  })}
              >
                Archive knowledge entry
              </button>
            </>
          )}
          {detail.entry.latest_draft_id &&
            detail.entry.latest_draft_id !== detail.revision.id && (
            <button
              disabled={busy}
              onClick={() =>
                void open(detail.entry.id, detail.entry.latest_draft_id!)}
            >
              Open latest draft revision
            </button>
          )}
          {detail.entry.approved_revision_id &&
            detail.entry.approved_revision_id !== detail.revision.id && (
            <button
              disabled={busy}
              onClick={() =>
                void open(detail.entry.id, detail.entry.approved_revision_id!)}
            >
              Open current approved revision
            </button>
          )}
          {showHistory && historyMore && detail.history.length > 0 && (
            <button
              type="button"
              disabled={busy}
              onClick={() => {
                const before = ++epoch.current;
                const entryId = detail.entry.id;
                const after =
                  detail.history[detail.history.length - 1].entry_version;
                setBusy(true);
                void knowledgeRead(projectId, {
                  kind: "history",
                  entry_id: entryId,
                  after_version: after,
                }).then((events) => {
                  if (before === epoch.current) {
                    setDetail((current) =>
                      current?.entry.id === entryId
                        ? {
                          ...current,
                          history: [...current.history, ...events],
                        }
                        : current
                    );
                    setHistoryMore(events.length === 50);
                  }
                }).catch((cause: unknown) => {
                  if (before === epoch.current) setError(errorText(cause));
                }).finally(() => {
                  if (before === epoch.current) setBusy(false);
                });
              }}
            >
              Load more revision history
            </button>
          )}
          <label>
            <input
              type="checkbox"
              checked={showHistory}
              onChange={(event) => setShowHistory(event.target.checked)}
            />Show revision and lifecycle history
          </label>
          {showHistory && (
            <ul>
              {detail.history.map((item) => (
                <li key={item.entry_version}>
                  {item.kind} · version {item.entry_version} · {item.actor} ·
                  {" "}
                  {item.created_at}
                  {item.revision_id && (
                    <button
                      disabled={busy}
                      onClick={() =>
                        void open(detail.entry.id, item.revision_id!)}
                    >
                      Open exact revision {item.revision_id}
                    </button>
                  )}
                  {item.replaced_revision_id && (
                    <span>Replaced {item.replaced_revision_id}</span>
                  )}
                </li>
              ))}
            </ul>
          )}
        </article>
      )}
      {editor && (
        <KnowledgeDraftEditor
          key={`${editor}:${detail?.revision.id || "new"}`}
          projectId={projectId}
          entry={editor === "edit" ? detail?.entry : undefined}
          body={editor === "edit" && detail
            ? {
              title: detail.revision.title,
              statement: detail.revision.statement,
              category: detail.revision.category,
              tags: detail.revision.tags,
              sources: detail.revision.sources.map((source) => source.selector),
            }
            : undefined}
          onSaved={(entryId, revisionId) => {
            if (entryId && revisionId) void refresh(entryId, revisionId);
          }}
          onClose={() => setEditor(undefined)}
        />
      )}
    </section>
  );
}
