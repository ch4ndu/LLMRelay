import { useEffect, useRef, useState } from "react";
import { ApiError, historicalSource, historySearch } from "../api";
import type {
  HistoricalSource,
  HistorySearchFilters,
  HistorySearchHit,
  HistorySourceKind,
  Project,
  Task,
} from "../types";
import { ErrorNotice } from "./ErrorNotice";

const sourceKinds: { value: HistorySourceKind; label: string }[] = [
  { value: "task", label: "Task text" },
  { value: "comment", label: "Human comments" },
  { value: "report", label: "Reports and plans" },
  { value: "review", label: "Reviews" },
  { value: "explorer_decision", label: "Explorer decisions" },
  { value: "result_acceptance", label: "Result feedback" },
  { value: "rework", label: "Rework feedback" },
];
interface Props {
  projects: Project[];
  initialProjectId?: string;
  tasks: Task[];
  onOpen: (task: Task) => void;
}
export function HistorySearch(
  { projects, initialProjectId, tasks, onOpen }: Props,
) {
  const [filters, setFilters] = useState<HistorySearchFilters>({
    query: "",
    project_id: initialProjectId,
    exclude_archived: false,
  });
  const [results, setResults] = useState<HistorySearchHit[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [searched, setSearched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [stale, setStale] = useState(false);
  const [source, setSource] = useState<HistoricalSource | null>(null);
  const [sourceBusy, setSourceBusy] = useState(false);
  const [sourceError, setSourceError] = useState("");
  const [sourceUnavailable, setSourceUnavailable] = useState(false);
  const epoch = useRef(0);
  const sourceEpoch = useRef(0);
  const flight = useRef<AbortController | null>(null);
  const sourceFlight = useRef<AbortController | null>(null);
  useEffect(() => () => {
    epoch.current++;
    sourceEpoch.current++;
    flight.current?.abort();
    sourceFlight.current?.abort();
  }, []);
  const reset = () => {
    epoch.current++;
    sourceEpoch.current++;
    flight.current?.abort();
    sourceFlight.current?.abort();
    flight.current = null;
    sourceFlight.current = null;
    setResults([]);
    setCursor(null);
    setSearched(false);
    setBusy(false);
    setError("");
    setStale(false);
    setSource(null);
    setSourceBusy(false);
    setSourceError("");
    setSourceUnavailable(false);
  };
  const change = (next: HistorySearchFilters) => {
    reset();
    setFilters(next);
  };
  const search = async (more = false) => {
    if (flight.current) return;
    if (!more) reset();
    const controller = new AbortController();
    flight.current = controller;
    const current = ++epoch.current;
    setBusy(true);
    setError("");
    setStale(false);
    try {
      const page = await historySearch(
        { ...filters, query: filters.query.trim() },
        more ? cursor ?? undefined : undefined,
        controller.signal,
      );
      if (current !== epoch.current) return;
      setResults((old) => {
        const combined = more ? [...old, ...page.results] : page.results;
        const seen = new Set<string>();
        return combined.filter((hit) => {
          const key = JSON.stringify(hit.source);
          if (seen.has(key)) return false;
          seen.add(key);
          return true;
        });
      });
      setCursor(page.next_cursor);
      setSearched(true);
    } catch (cause) {
      if (current === epoch.current) {
        const message = cause instanceof Error ? cause.message : String(cause);
        setError(message);
        setStale(
          /^(Results changed|This page is invalid|Invalid search page)/.test(
            message,
          ),
        );
        setCursor(null);
      }
    } finally {
      if (current === epoch.current) {
        flight.current = null;
        setBusy(false);
      }
    }
  };
  const open = async (hit: HistorySearchHit) => {
    sourceFlight.current?.abort();
    const controller = new AbortController();
    sourceFlight.current = controller;
    const current = ++sourceEpoch.current;
    setSource(null);
    setSourceError("");
    setSourceBusy(true);
    setSourceUnavailable(false);
    try {
      const value = await historicalSource(hit.source, controller.signal);
      if (current === sourceEpoch.current) setSource(value);
    } catch (cause) {
      if (current === sourceEpoch.current) {
        setSourceError(cause instanceof Error ? cause.message : String(cause));
        setSourceUnavailable(cause instanceof ApiError && cause.status === 404);
      }
    } finally {
      if (current === sourceEpoch.current) {
        sourceFlight.current = null;
        setSourceBusy(false);
      }
    }
  };
  const sourceTask = source &&
    tasks.find((task) => task.id === source.hit.source.task_id);
  return (
    <section aria-label="Search history">
      <p>
        Search saved task text, human comments, reports, reviews and decisions
        across all attempts. Enter 1–256 UTF-8 bytes. ASCII letters ignore case;
        other letters match case exactly. %, _ and quotes are literal text.
      </p>
      <form
        className="history-filters"
        onSubmit={(event) => {
          event.preventDefault();
          void search();
        }}
      >
        <label>
          Search text<input
            value={filters.query}
            onChange={(event) =>
              change({ ...filters, query: event.target.value })}
          />
        </label>
        <label>
          Project<select
            value={filters.project_id ?? ""}
            onChange={(event) =>
              change({
                ...filters,
                project_id: event.target.value || undefined,
              })}
          >
            <option value="">All projects</option>
            {projects.map((project) => (
              <option key={project.id} value={project.id}>
                {project.display_name}
              </option>
            ))}
          </select>
        </label>
        <label>
          Source<select
            value={filters.kind ?? ""}
            onChange={(event) =>
              change({
                ...filters,
                kind: sourceKinds.find((kind) =>
                  kind.value === event.target.value
                )?.value,
              })}
          >
            <option value="">All sources</option>
            {sourceKinds.map((kind) => (
              <option key={kind.value} value={kind.value}>{kind.label}</option>
            ))}
          </select>
        </label>
        <label>
          <input
            type="checkbox"
            checked={filters.exclude_archived}
            onChange={(event) =>
              change({ ...filters, exclude_archived: event.target.checked })}
          />Exclude archived tasks
        </label>
        <button
          disabled={busy || !filters.query.trim() ||
            new TextEncoder().encode(filters.query.trim()).length > 256}
        >
          {busy ? "Searching…" : "Search"}
        </button>
      </form>
      {stale && (
        <p role="status">
          Saved history changed. Choose Refresh search before loading more
          results.
        </p>
      )}
      {error && <ErrorNotice error={error} />}
      {(searched || error) && (
        <button
          disabled={busy}
          onClick={() => void search()}
        >
          Refresh search
        </button>
      )}
      {searched && !results.length && (
        <p className="empty">No matching saved sources.</p>
      )}
      <div className="history-search-results">
        {results.map((hit) => (
          <article key={`${hit.source.kind}:${hit.source.id}`}>
            <button
              onClick={() =>
                void open(hit)}
            >
              <strong>{hit.task_title}</strong>
              <span>
                {hit.source.kind.replaceAll("_", " ")} · {hit.project_name}
                {hit.archived ? " · Archived" : ""}
              </span>
              <time>{hit.created_at}</time>
              <p>{hit.snippet}</p>
            </button>
          </article>
        ))}
      </div>
      {cursor && (
        <button disabled={busy} onClick={() => void search(true)}>
          {busy ? "Loading…" : "Load more"}
        </button>
      )}
      {sourceBusy && <p role="status">Loading the exact historical source…</p>}
      {sourceUnavailable && (
        <p role="status">
          This exact historical source is no longer available. Refresh search to
          see the saved sources that remain.
        </p>
      )}
      {sourceError && <ErrorNotice error={sourceError} />}
      {source && (
        <section className="historical-source" aria-label="Historical source">
          <h2>{source.hit.task_title}</h2>
          <p>
            Historical {source.hit.source.kind.replaceAll("_", " ")} ·{" "}
            {source.hit.created_at}
          </p>
          {source.hit.source.attempt_id && (
            <p>Attempt: {source.hit.source.attempt_id}</p>
          )}
          <p>
            This saved evidence does not grant approval or change current work.
          </p>
          <pre>{source.text}</pre>
          {source.truncated && (
            <p>
              Only a bounded excerpt is shown. This source was truncated for
              display.
            </p>
          )}
          {sourceTask
            ? (
              <button onClick={() => onOpen(sourceTask)}>Open task</button>
            )
            : (
              <p>
                The task is unavailable in the current task list. Refresh the
                dashboard.
              </p>
            )}
          <button
            onClick={() => {
              sourceEpoch.current++;
              sourceFlight.current?.abort();
              setSource(null);
            }}
          >
            Close source
          </button>
        </section>
      )}
    </section>
  );
}
