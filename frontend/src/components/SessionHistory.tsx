import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { type FormEvent, useEffect, useRef, useState } from "react";
import {
  ApiError,
  getHistoryExportText,
  listHistoryPaneViews,
  openHistoryPane,
  operation,
  operationId,
  ProtocolError,
  readHistoryPage,
} from "../api";
import { RecordedTextReader } from "../cmuxRouting";
import {
  type HistoryExportText,
  type HistoryPaneView,
  type HistoryPaneViewState,
  type HistorySelection,
  roleLabel,
  type Session,
  type TranscriptPage,
} from "../types";

const uuidPattern =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

const errorText = (cause: unknown) =>
  cause instanceof Error ? cause.message : String(cause);

const historySelectionKey = (selection: HistorySelection) =>
  `${selection.session_id}/${selection.transcript_epoch}`;

type TranscriptEpochPage = {
  epochs: string[];
  gaps: number;
  nextEpoch: string | null;
  nextSequence: number;
  hasMore: boolean;
};

const transcriptEpochPage = (
  value: unknown,
): TranscriptEpochPage | undefined => {
  if (
    typeof value !== "object" || value === null || !("frames" in value) ||
    !("next_sequence" in value) || !("has_more" in value)
  ) return undefined;
  const { frames, next_sequence: nextSequence, has_more: hasMore } = value;
  const rawNextEpoch = "next_epoch" in value ? value.next_epoch : null;
  if (
    !Array.isArray(frames) || typeof hasMore !== "boolean" ||
    typeof nextSequence !== "number" || !Number.isSafeInteger(nextSequence) ||
    (rawNextEpoch != null && typeof rawNextEpoch !== "string")
  ) return undefined;
  const list: unknown[] = frames;
  const epochs: string[] = [];
  let gaps = 0;
  for (const frame of list) {
    if (
      typeof frame !== "object" || frame === null || !("epoch" in frame) ||
      !("gap" in frame)
    ) return undefined;
    const { epoch, gap } = frame;
    if (typeof epoch !== "string" || typeof gap !== "boolean") return undefined;
    if (gap) gaps++;
    else epochs.push(epoch);
  }
  return {
    epochs,
    gaps,
    nextEpoch: typeof rawNextEpoch === "string" ? rawNextEpoch : null,
    nextSequence,
    hasMore,
  };
};

/**
 * Lists the session's invocations with saved output, oldest first. No
 * read-only record lists past epochs, so this scans the general transcript;
 * it only reports epochs and never changes which one is being read.
 */
const discoverInvocations = async (sessionId: string) => {
  const epochs: string[] = [];
  let gaps = 0;
  let cursor: { epoch: string | null; sequence: number } = {
    epoch: null,
    sequence: 0,
  };
  for (;;) {
    const page = transcriptEpochPage(
      await operation<unknown>({
        kind: "transcript",
        session_id: sessionId,
        after_epoch: cursor.epoch,
        after_sequence: cursor.sequence,
        limit_bytes: 512 * 1024,
      }),
    );
    if (!page) {
      throw new Error(
        "The service returned an unsupported response. Refresh after updating the service.",
      );
    }
    for (const epoch of page.epochs) {
      if (uuidPattern.test(epoch) && !epochs.includes(epoch)) epochs.push(epoch);
    }
    gaps += page.gaps;
    if (
      !page.hasMore ||
      (page.nextEpoch === cursor.epoch && page.nextSequence === cursor.sequence)
    ) break;
    cursor = { epoch: page.nextEpoch, sequence: page.nextSequence };
  }
  return { epochs, gaps };
};

/**
 * A page is shown only if it stays on the selected epoch and advances past
 * the cursor. Gap markers may carry the retention record's own epoch.
 */
const historyPageProblem = (
  page: TranscriptPage,
  epoch: string,
  after: number,
) => {
  if (
    (page.next_epoch != null && page.next_epoch !== epoch) ||
    page.frames.some((frame) => !frame.gap && frame.epoch !== epoch)
  ) {
    return "The service returned output from a different invocation than the one selected, so it was not shown.";
  }
  let previous = after;
  for (const frame of page.frames) {
    if (frame.gap) continue;
    if (frame.sequence <= previous) {
      return "The service returned saved output out of order, so it was not shown.";
    }
    previous = frame.sequence;
  }
  if (page.has_more && page.next_sequence <= after) {
    return "The service did not advance through the saved output, so loading stopped.";
  }
  return undefined;
};

const retentionSummary = ({ retention, high_water_sequence }: HistoryExportText) => {
  if (retention.first_retained_sequence === null) {
    return "No output is saved for this invocation.";
  }
  if (retention.complete) {
    return `All output for this invocation is saved, through frame ${high_water_sequence}.`;
  }
  const start = retention.first_retained_sequence > 1
    ? ` Saved output starts at frame ${retention.first_retained_sequence}.`
    : "";
  return `Some output for this invocation was not kept.${start} LLMRelay keeps at most 10 MiB of output per session, and removed output cannot be restored.`;
};

const MAX_SHOWN_MATCHES = 200;

type SearchResults = {
  query: string;
  through: number;
  total: number;
  lines: Array<{ number: number; text: string }>;
};

const searchSavedOutput = (
  copy: HistoryExportText,
  query: string,
): SearchResults => {
  const needle = query.toLocaleLowerCase();
  const lines: SearchResults["lines"] = [];
  let total = 0;
  copy.text.split("\n").forEach((text, index) => {
    if (!text.toLocaleLowerCase().includes(needle)) return;
    total++;
    if (lines.length < MAX_SHOWN_MATCHES) lines.push({ number: index + 1, text });
  });
  return { query, through: copy.high_water_sequence, total, lines };
};

// The object URL is revoked once the click has started the download, so the
// saved text does not stay addressable from the page.
const downloadSavedOutput = (copy: HistoryExportText) => {
  const url = URL.createObjectURL(
    new Blob([copy.text], { type: "text/plain;charset=utf-8" }),
  );
  const link = document.createElement("a");
  link.href = url;
  link.download = `llmrelay-output-${copy.session_id.slice(0, 8)}-${
    copy.transcript_epoch.slice(0, 8)
  }-frame-${copy.high_water_sequence}.txt`;
  document.body.append(link);
  try {
    link.click();
  } finally {
    link.remove();
    URL.revokeObjectURL(url);
  }
};

const historyPaneStateLabel: Record<HistoryPaneViewState, string> = {
  reserved: "Opening",
  opened: "Opened in cmux",
  unknown: "Not confirmed; check cmux",
  failed: "Not opened",
  closed: "Closed",
};

export type HistoryPaneRequest = { id: string; acknowledgedUnknownViewId?: string };

type SavedCopyAction = "search" | "download" | "show" | "refresh";

/**
 * Reads one exact session epoch. It is remounted for every selection, and
 * each response is applied only while the run that requested it is current,
 * so a closed reader or another epoch never receives a late page.
 */
function HistorySelectionReader(
  { selection, final, paneRequests }: {
    selection: HistorySelection;
    /** No later frame can be added to this epoch. */
    final: boolean;
    paneRequests: Map<string, HistoryPaneRequest>;
  },
) {
  const key = historySelectionKey(selection);
  const run = useRef(0);
  const paging = useRef<number | undefined>(undefined);
  const cursor = useRef(0);
  const decoder = useRef<RecordedTextReader | undefined>(undefined);
  const [text, setText] = useState("");
  const [through, setThrough] = useState(0);
  const [gaps, setGaps] = useState(0);
  const [hasMore, setHasMore] = useState<boolean>();
  const [loading, setLoading] = useState<"page" | "all">();
  const [pageError, setPageError] = useState("");
  const [saved, setSaved] = useState<HistoryExportText>();
  const [savedBusy, setSavedBusy] = useState<SavedCopyAction>();
  const [savedError, setSavedError] = useState("");
  const [showSaved, setShowSaved] = useState(false);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchResults>();
  const [views, setViews] = useState<HistoryPaneView[]>();
  const [viewsError, setViewsError] = useState("");
  const [paneBusy, setPaneBusy] = useState(false);
  const [paneMessage, setPaneMessage] = useState("");
  const [paneError, setPaneError] = useState("");

  const loadPages = async (all: boolean) => {
    const mine = run.current;
    if (paging.current === mine) return;
    paging.current = mine;
    setLoading(all ? "all" : "page");
    setPageError("");
    try {
      let more: boolean;
      do {
        const after = cursor.current;
        const page = await readHistoryPage(selection, after);
        if (mine !== run.current) return;
        const problem = historyPageProblem(
          page,
          selection.transcript_epoch,
          after,
        );
        if (problem) throw new Error(problem);
        // Cursor zero means no frame of this epoch is shown yet, so the page
        // replaces any earlier "nothing saved" marker instead of repeating it.
        const reader = after === 0
          ? new RecordedTextReader()
          : decoder.current ?? new RecordedTextReader();
        decoder.current = reader;
        more = page.has_more;
        const chunk = reader.push(page.frames) +
          (!more && final ? reader.finish() : "");
        const pageGaps = page.frames.filter((frame) => frame.gap).length;
        cursor.current = page.next_sequence;
        setText((current) => after === 0 ? chunk : current + chunk);
        setGaps((current) => after === 0 ? pageGaps : current + pageGaps);
        setThrough(page.next_sequence);
        setHasMore(more);
      } while (all && more);
    } catch (cause) {
      if (mine === run.current) setPageError(errorText(cause));
    } finally {
      if (paging.current === mine) paging.current = undefined;
      if (mine === run.current) setLoading(undefined);
    }
  };

  const withSavedCopy = async (
    action: SavedCopyAction,
    use: (copy: HistoryExportText) => void,
  ) => {
    if (savedBusy) return;
    const mine = run.current;
    setSavedBusy(action);
    setSavedError("");
    try {
      let copy = action === "refresh" ? undefined : saved;
      if (!copy) {
        copy = await getHistoryExportText(selection);
        if (mine !== run.current) return;
        if (historySelectionKey(copy) !== key) {
          throw new Error(
            "The service returned a saved copy of a different invocation, so it was not used.",
          );
        }
        setSaved(copy);
      }
      use(copy);
    } catch (cause) {
      if (mine === run.current) setSavedError(errorText(cause));
    } finally {
      if (mine === run.current) setSavedBusy(undefined);
    }
  };

  const search = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const needle = query.trim();
    if (!needle) return;
    void withSavedCopy(
      "search",
      (copy) => setResults(searchSavedOutput(copy, needle)),
    );
  };

  const refreshViews = async () => {
    const mine = run.current;
    setViewsError("");
    try {
      const listed = await listHistoryPaneViews(selection);
      if (mine !== run.current) return;
      if (listed.some((view) => historySelectionKey(view) !== key)) {
        throw new Error(
          "The service listed history panes for a different invocation, so they were not shown.",
        );
      }
      setViews(listed);
    } catch (cause) {
      if (mine === run.current) setViewsError(errorText(cause));
    }
  };

  // An uncertain request keeps its operation ID, so checking it replays the
  // same durable record rather than creating another pane.
  const requestPane = async (request: HistoryPaneRequest) => {
    if (paneBusy) return;
    const mine = run.current;
    paneRequests.set(key, request);
    setPaneBusy(true);
    setPaneMessage("");
    setPaneError("");
    try {
      const outcome = await openHistoryPane(
        selection,
        request.id,
        request.acknowledgedUnknownViewId,
      );
      if (
        outcome.view.operation_id !== request.id ||
        historySelectionKey(outcome.view) !== key
      ) {
        throw new Error(
          "The service returned a different history pane record. Refresh history panes and check cmux before opening another.",
        );
      }
      paneRequests.delete(key);
      if (mine !== run.current) return;
      setPaneMessage(outcome.message);
      setViews((current) => [
        outcome.view,
        ...(current ?? []).filter((view) => view.id !== outcome.view.id),
      ]);
    } catch (cause) {
      const refused = cause instanceof ProtocolError ||
        (cause instanceof ApiError && !cause.ambiguous);
      if (refused) paneRequests.delete(key);
      if (mine === run.current) setPaneError(errorText(cause));
    } finally {
      if (mine === run.current) {
        setPaneBusy(false);
        void refreshViews();
      }
    }
  };

  useEffect(() => {
    void loadPages(false);
    void refreshViews();
    return () => {
      run.current++;
    };
  }, []);

  const pending = paneBusy ? undefined : paneRequests.get(key);
  const latestView = views?.[0];
  const unknownView = views?.find((view) => view.state === "unknown");
  const pageStatus = showSaved && saved
    ? `Showing the complete saved copy, through frame ${saved.high_water_sequence}.`
    : loading === "all"
    ? "Loading all remaining saved output…"
    : loading
    ? "Loading saved output…"
    : hasMore === undefined
    ? ""
    : hasMore
    ? `Showing saved output through frame ${through}. More saved output is available.`
    : final
    ? `Showing all saved output. It ends at frame ${through}.`
    : `Showing all output saved so far, through frame ${through}. The session is still running, so more may follow.`;

  return (
    <>
      <pre
        className="recorded-output session-history-text"
        tabIndex={0}
        aria-label="Recorded output"
        aria-busy={!!loading}
      >
        {showSaved && saved ? saved.text : text}
      </pre>
      {pageStatus && <p aria-live="polite">{pageStatus}</p>}
      {gaps > 0 && !showSaved && (
        <p className="warning">
          {gaps === 1
            ? "This output has a marker where saved output is missing."
            : `This output has ${gaps} markers where saved output is missing.`}{" "}
          Missing output cannot be restored.
        </p>
      )}
      {pageError && <ErrorNotice error={pageError} />}
      {!showSaved && (
        <div className="button-row">
          {hasMore === undefined && pageError && (
            <button disabled={!!loading} onClick={() => void loadPages(false)}>
              Try loading saved output again
            </button>
          )}
          {hasMore && (
            <>
              <button disabled={!!loading} onClick={() => void loadPages(false)}>
                Load more output
              </button>
              <button disabled={!!loading} onClick={() => void loadPages(true)}>
                Load all remaining output
              </button>
            </>
          )}
          {hasMore === false && !final && (
            <button disabled={!!loading} onClick={() => void loadPages(true)}>
              Check for newer output
            </button>
          )}
        </div>
      )}
      <form
        className="session-history-search"
        role="search"
        aria-label="Search all saved output"
        onSubmit={search}
      >
        <label>
          Search all saved output
          <input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
        <button type="submit" disabled={!!savedBusy || !query.trim()}>
          {savedBusy === "search" ? "Searching…" : "Search"}
        </button>
      </form>
      <div className="button-row">
        <button
          disabled={!!savedBusy}
          onClick={() => void withSavedCopy("download", downloadSavedOutput)}
        >
          {savedBusy === "download"
            ? "Preparing download…"
            : "Download saved output (.txt)"}
        </button>
        {showSaved
          ? (
            <button onClick={() => setShowSaved(false)}>
              Show paged output
            </button>
          )
          : (
            <button
              disabled={!!savedBusy}
              onClick={() => void withSavedCopy("show", () => setShowSaved(true))}
            >
              {savedBusy === "show"
                ? "Loading complete copy…"
                : "Show complete saved copy"}
            </button>
          )}
        {saved && (
          <button
            disabled={!!savedBusy}
            onClick={() =>
              void withSavedCopy(
                "refresh",
                (copy) =>
                  setResults((current) =>
                    current && searchSavedOutput(copy, current.query)
                  ),
              )}
          >
            {savedBusy === "refresh"
              ? "Refreshing saved copy…"
              : "Refresh saved copy"}
          </button>
        )}
      </div>
      <small className="hint">
        Search, download and the complete copy always cover all output saved
        for this invocation, not only the part shown above.
      </small>
      {saved && <p>{retentionSummary(saved)}</p>}
      {savedError && <ErrorNotice error={savedError} />}
      {results && (
        <div
          className="session-history-results"
          role="region"
          aria-label="Search results"
        >
          <p aria-live="polite">
            {results.total === 0
              ? `No lines match “${results.query}” in the saved output through frame ${results.through}.`
              : `${results.total} matching ${
                results.total === 1 ? "line" : "lines"
              } for “${results.query}” in the saved output through frame ${results.through}${
                results.total > results.lines.length
                  ? `; showing the first ${results.lines.length}`
                  : ""
              }.`}
          </p>
          {results.lines.length > 0 && (
            <ul>
              {results.lines.map((line) => (
                <li key={line.number}>
                  <small>Line {line.number}</small> <code>{line.text}</code>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {saved && (
        <TechnicalDetails>
          <p>
            saved copy {saved.export_hash} · high-water frame{" "}
            {saved.high_water_sequence} · first retained frame{" "}
            {saved.retention.first_retained_sequence ?? "none"} · gap markers{" "}
            {saved.retention.gap_count}
          </p>
        </TechnicalDetails>
      )}
      <div className="session-history-panes">
        <h5>Read-only history in cmux</h5>
        <p>
          Opens this saved output in a separate cmux pane with a read-only
          pager. The pane cannot send input to the agent, and this reader keeps
          working if cmux is unavailable.
        </p>
        {pending
          ? (
            <div className="warning" role="status">
              <p>
                LLMRelay could not confirm whether your last request reached
                cmux. Check cmux before opening another history pane.
              </p>
              <button onClick={() => void requestPane(pending)}>
                Check previous history pane request
              </button>
            </div>
          )
          : latestView?.state === "reserved"
          ? (
            <p role="status">
              A history pane is still opening. Choose Refresh history panes to
              see its result before opening another.
            </p>
          )
          : unknownView
          ? (
            <div className="warning" role="status">
              <p>
                LLMRelay could not confirm whether an earlier history pane
                opened. Check cmux before opening another history pane.
              </p>
              <button
                disabled={paneBusy}
                onClick={() =>
                  void requestPane({
                    id: operationId(),
                    acknowledgedUnknownViewId: unknownView.id,
                  })}
              >
                {paneBusy ? "Opening history pane…" : "Open another history pane"}
              </button>
            </div>
          )
          : (
            <button
              disabled={paneBusy || (!views && !viewsError)}
              onClick={() => void requestPane({ id: operationId() })}
            >
              {paneBusy
                ? "Opening history pane…"
                : latestView?.state === "failed" && latestView.retry_allowed
                ? "Try opening the history pane again"
                : "Open read-only history in cmux"}
            </button>
          )}
        {paneMessage && <p aria-live="polite">{paneMessage}</p>}
        {paneError && <ErrorNotice error={paneError} />}
        {views && views.length > 0 && (
          <ul aria-label="History panes for this invocation">
            {views.map((view) => (
              <li key={view.id}>
                {historyPaneStateLabel[view.state]} · requested{" "}
                {new Date(view.created_at).toLocaleString()}
                {view.failure_reason && <small> {view.failure_reason}</small>}
                <TechnicalDetails>
                  <p>
                    history pane {view.id} · operation {view.operation_id} ·
                    state {view.state} · frame {view.high_water_sequence} ·
                    saved copy {view.export_hash ?? "none"} · service start{" "}
                    {view.service_boot_id} · cmux workspace{" "}
                    {view.workspace_id ?? "none"} · surface{" "}
                    {view.surface_id ?? "none"}
                    {view.retry_allowed ? " · retry allowed" : ""}
                  </p>
                </TechnicalDetails>
              </li>
            ))}
          </ul>
        )}
        {views?.length === 0 && (
          <small className="hint">
            No history panes have been opened for this invocation.
          </small>
        )}
        {viewsError && <ErrorNotice error={viewsError} />}
        <div className="button-row">
          <button onClick={() => void refreshViews()}>
            Refresh history panes
          </button>
        </div>
      </div>
    </>
  );
}

export function SessionHistory(
  { session, paneRequests, onClose }: {
    session: Session;
    paneRequests: Map<string, HistoryPaneRequest>;
    onClose: () => void;
  },
) {
  const latest = session.transcript_epoch;
  // The reader stays on the epoch it opened; a newer invocation is offered
  // explicitly and never followed silently.
  const [epoch, setEpoch] = useState(latest);
  useEffect(() => {
    if (!epoch && latest) setEpoch(latest);
  }, [epoch, latest]);
  const [invocations, setInvocations] = useState<
    { epochs: string[]; gaps: number }
  >();
  const [discovering, setDiscovering] = useState(false);
  const [discoverError, setDiscoverError] = useState("");
  const discovery = useRef(0);
  useEffect(() => () => {
    discovery.current++;
  }, []);

  const discover = async () => {
    const mine = ++discovery.current;
    setDiscovering(true);
    setDiscoverError("");
    try {
      const found = await discoverInvocations(session.id);
      if (mine === discovery.current) setInvocations(found);
    } catch (cause) {
      if (mine === discovery.current) setDiscoverError(errorText(cause));
    } finally {
      if (mine === discovery.current) setDiscovering(false);
    }
  };

  const listed = invocations && latest && !invocations.epochs.includes(latest)
    ? [...invocations.epochs, latest]
    : invocations?.epochs;
  const invocationName = (value: string, index: number) =>
    `Invocation ${index + 1}${value === latest ? " (latest)" : ""}`;
  const position = epoch && listed ? listed.indexOf(epoch) : -1;
  const selectedName = !epoch
    ? "No invocation yet"
    : listed && position >= 0
    ? `${invocationName(epoch, position)} of ${listed.length}`
    : epoch === latest
    ? "Latest invocation"
    : "Earlier invocation";

  return (
    <section
      className="session-output session-history"
      aria-label={`${roleLabel(session.role)} recorded output`}
    >
      <header>
        <h4>Recorded output</h4>
        <span>{selectedName}</span>
      </header>
      <p>
        Everything LLMRelay saved from this session's terminal, as plain text.
        Reading it never restarts the agent, sends it input or takes keyboard
        control, and it works whether or not cmux is available.
      </p>
      {session.capture_state === "failed" && (
        <p className="warning">
          Recording stopped early for this session, so later output was not
          saved.
        </p>
      )}
      {epoch && latest && epoch !== latest && (
        <div className="warning" role="status">
          <p>
            You are reading an earlier invocation of this session. A newer one
            exists.
          </p>
          <button onClick={() => setEpoch(latest)}>
            Read the latest invocation
          </button>
        </div>
      )}
      {epoch
        ? (
          <>
            <small>
              Generation {session.generation} · invocation {epoch.slice(0, 8)}
            </small>
            <TechnicalDetails>
              <p>
                session {session.id} · role generation{" "}
                {session.role_generation_id} · generation {session.generation}
                {" "}· transcript epoch {epoch}
                {latest && epoch !== latest ? ` · latest epoch ${latest}` : ""}
                {" "}· output capture {session.capture_state}
              </p>
            </TechnicalDetails>
            <HistorySelectionReader
              key={`${session.id}/${epoch}`}
              selection={{ session_id: session.id, transcript_epoch: epoch }}
              final={epoch !== latest ||
                ["exited", "launch_failed"].includes(session.status)}
              paneRequests={paneRequests}
            />
          </>
        )
        : <p>No saved output is identified for this session yet.</p>}
      <div className="session-history-invocations">
        {listed
          ? (
            <>
              <p>
                {listed.length === 1
                  ? "This session has one invocation with saved output."
                  : `This session has ${listed.length} invocations with saved output. Each one is read separately.`}
              </p>
              {!!invocations?.gaps && (
                <small className="hint">
                  Some older output was removed by the 10 MiB limit, so earlier
                  invocations may be missing from this list.
                </small>
              )}
              <div className="button-row">
                {listed.map((value, index) => (
                  <button
                    key={value}
                    aria-pressed={value === epoch}
                    onClick={() => setEpoch(value)}
                  >
                    {invocationName(value, index)}
                  </button>
                ))}
              </div>
            </>
          )
          : (
            <button disabled={discovering} onClick={() => void discover()}>
              {discovering
                ? "Looking for earlier invocations…"
                : "Find earlier invocations"}
            </button>
          )}
        {discoverError && <ErrorNotice error={discoverError} />}
      </div>
      <div className="button-row">
        <button onClick={onClose}>Close recorded output</button>
      </div>
    </section>
  );
}
