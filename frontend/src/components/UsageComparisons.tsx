import { useEffect, useRef, useState } from "react";
import { usageComparisons } from "../api";
import type {
  Project,
  UsageComparisonFilters,
  UsageComparisonGroup,
  UsageGroupBy,
} from "../types";
import { roleLabel, ROLES } from "../types";
import { ErrorNotice } from "./ErrorNotice";
import { ObservedUsageDetails } from "./SessionTree";

interface Props {
  projects: Project[];
  initialProjectId?: string;
}

export function UsageComparisons({ projects, initialProjectId }: Props) {
  const [filters, setFilters] = useState<UsageComparisonFilters>({
    group_by: "project",
    project_id: initialProjectId,
  });
  const [groups, setGroups] = useState<UsageComparisonGroup[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [applied, setApplied] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [stale, setStale] = useState(false);
  const [unavailable, setUnavailable] = useState(false);
  const epoch = useRef(0);
  const flight = useRef<AbortController | null>(null);
  useEffect(() => () => {
    epoch.current++;
    flight.current?.abort();
  }, []);
  const reset = () => {
    epoch.current++;
    flight.current?.abort();
    flight.current = null;
    setGroups([]);
    setCursor(null);
    setApplied(false);
    setBusy(false);
    setError("");
    setStale(false);
    setUnavailable(false);
  };
  const change = (next: UsageComparisonFilters) => {
    reset();
    setFilters(next);
  };
  const apply = async (more = false) => {
    if (flight.current) return;
    if (!more) reset();
    const controller = new AbortController();
    flight.current = controller;
    const current = ++epoch.current;
    setBusy(true);
    setError("");
    setStale(false);
    setUnavailable(false);
    try {
      const page = await usageComparisons(
        filters,
        more ? cursor ?? undefined : undefined,
        controller.signal,
      );
      if (current !== epoch.current) return;
      setGroups((old) => {
        const combined = more ? [...old, ...page.groups] : page.groups;
        const seen = new Set<string>();
        return combined.filter((group) => {
          if (seen.has(group.key)) return false;
          seen.add(group.key);
          return true;
        });
      });
      setCursor(page.next_cursor);
      setApplied(true);
    } catch (cause) {
      if (current === epoch.current) {
        const message = cause instanceof Error ? cause.message : String(cause);
        setError(message);
        setStale(
          /^(Results changed|This page is invalid|Invalid usage page)/.test(
            message,
          ),
        );
        setUnavailable(
          message.startsWith("Usage comparisons are unavailable:"),
        );
        setGroups([]);
        setApplied(false);
        setCursor(null);
      }
    } finally {
      if (current === epoch.current) {
        flight.current = null;
        setBusy(false);
      }
    }
  };
  const groupLabel = (group: UsageComparisonGroup) => {
    if (filters.group_by === "workflow") {
      return group.key === "trip"
        ? "TRIP"
        : group.key === "single_agent"
        ? "Single agent"
        : "Unknown workflow";
    }
    if (filters.group_by === "role") {
      const role = ROLES.find((role) => role === group.key);
      return role
        ? roleLabel(role)
        : group.key === "single_agent"
        ? "Single agent"
        : group.label;
    }
    return group.label;
  };
  return (
    <section aria-label="Observed usage comparisons">
      <p>
        Partial Codex response observations for sessions in visible projects,
        including archived tasks and old attempts. These are not billing totals,
        account usage or cost estimates. Claude usage and unobserved responses
        are unavailable.
      </p>
      <p>
        Sessions started (UTC) selects a cohort by session start date. Values
        include lifetime observations for those sessions, including responses
        observed later. Cached input and reasoning are already included in input
        and output.
      </p>
      <form
        className="history-filters"
        onSubmit={(event) => {
          event.preventDefault();
          void apply();
        }}
      >
        <label>
          Compare by<select
            value={filters.group_by}
            onChange={(event) => {
              const value = event.target.value;
              const groupBy: UsageGroupBy =
                value === "workflow" || value === "role" ? value : "project";
              change({ ...filters, group_by: groupBy });
            }}
          >
            <option value="project">Project</option>
            <option value="workflow">Workflow</option>
            <option value="role">Role</option>
          </select>
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
          Workflow<select
            value={filters.workflow ?? ""}
            onChange={(event) =>
              change({ ...filters, workflow: event.target.value || undefined })}
          >
            <option value="">All workflows</option>
            <option value="trip">TRIP</option>
            <option value="single_agent">Single agent</option>
            <option value="unknown">Unknown workflow</option>
          </select>
        </label>
        <label>
          Role<select
            value={filters.role ?? ""}
            onChange={(event) =>
              change({ ...filters, role: event.target.value || undefined })}
          >
            <option value="">All roles</option>
            {ROLES.map((role) => (
              <option key={role} value={role}>{roleLabel(role)}</option>
            ))}
            <option value="single_agent">Single agent</option>
          </select>
        </label>
        <fieldset>
          <legend>Sessions started (UTC)</legend>
          <label>
            From<input
              type="date"
              value={filters.start_date ?? ""}
              onChange={(event) =>
                change({
                  ...filters,
                  start_date: event.target.value || undefined,
                })}
            />
          </label>
          <label>
            Through<input
              type="date"
              value={filters.end_date ?? ""}
              onChange={(event) =>
                change({
                  ...filters,
                  end_date: event.target.value || undefined,
                })}
            />
          </label>
        </fieldset>
        <button disabled={busy}>{busy ? "Loading…" : "Apply"}</button>
      </form>
      {stale && (
        <p role="status">
          Saved usage changed. Choose Refresh comparisons to compare a fresh set
          of observations.
        </p>
      )}
      {unavailable && (
        <p role="status">
          Saved usage records cannot support this comparison. Values are
          unavailable. Share Technical details when reporting the problem.
        </p>
      )}
      {error && <ErrorNotice error={error} />}
      {(applied || error) && (
        <button
          disabled={busy}
          onClick={() => void apply()}
        >
          Refresh comparisons
        </button>
      )}
      {applied && !groups.length && (
        <p className="empty">No sessions match these filters.</p>
      )}
      {!!groups.length && (
        <div className="usage-comparisons">
          <table>
            <caption>
              Observed usage (partial); groups may share conflicting
              observations and must not be added together.
            </caption>
            <thead>
              <tr>
                <th>Group</th>
                <th>Sessions</th>
                <th>Coverage</th>
                <th>Observed tokens</th>
              </tr>
            </thead>
            <tbody>
              {groups.map((group) => (
                <tr key={group.key}>
                  <th scope="row">{groupLabel(group)}</th>
                  <td>
                    {group.sessions.toLocaleString()}
                    <small>
                      Codex: {group.codex_sessions} · Claude:{" "}
                      {group.claude_sessions} · Other/unknown:{" "}
                      {group.unknown_provider_sessions}
                    </small>
                  </td>
                  <td>
                    {group.sessions_with_observations} with observations;{" "}
                    {group.sessions_without_observations} without<small>
                      {group.usage.status === "invalid"
                        ? "Invalid observations"
                        : group.usage.status === "observed"
                        ? "Partial observations"
                        : "Unavailable"}
                    </small>
                  </td>
                  <td>
                    {group.usage.status === "observed" && group.usage.counters
                      ? group.usage.counters.total_tokens.toLocaleString()
                      : "Unavailable"}
                    {group.usage.reason && <p>{group.usage.reason}</p>}
                    <details>
                      <summary>Observation details</summary>
                      <ObservedUsageDetails
                        usage={group.usage}
                        label={groupLabel(group)}
                      />
                    </details>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {cursor && (
        <button disabled={busy} onClick={() => void apply(true)}>
          {busy ? "Loading…" : "Load more"}
        </button>
      )}
    </section>
  );
}
