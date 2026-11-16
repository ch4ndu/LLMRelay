import { useEffect, useRef, useState } from "react";
import type { ReactElement } from "react";
import { ApiError, comparisonEvidence, historicalSource, usageComparisons, workflowComparisons } from "../api";
import type {
  Project,
  Task,
  WorkflowComparisonQuery,
  WorkflowComparisonPage,
  WorkflowComparisonGroup,
  ComparisonDimension,
  ComparisonDurationSummary,
  ComparisonEvidenceFact,
  ComparisonEvidenceMetric,
  ComparisonEvidencePage,
  ComparisonEvidenceRow,
  HistoricalSource,
  HistoricalSourceKey,
  ObservedUsage,
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

interface WorkflowComparisonProps extends Props {
  tasks: Task[];
  onOpen: (task: Task) => void;
}

function SessionUsageComparisons({ projects, initialProjectId }: Props) {
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
        Sessions created (UTC) selects a cohort by saved session creation date,
        rather than execution start. Values
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
          <legend>Sessions created (UTC)</legend>
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

export function UsageComparisons(props: WorkflowComparisonProps): ReactElement {
  const [mode, setMode] = useState<"session" | "task" | "run">("session");
  return <section aria-label="Workflow and provider comparisons">
    <label>Counting unit<select value={mode} onChange={(event) => {
      const value = event.target.value;
      if (value === "session" || value === "task" || value === "run") setMode(value);
    }}>
      <option value="session">Session usage</option>
      <option value="task">Tasks</option>
      <option value="run">Runs</option>
    </select></label>
    {mode === "session" ? <SessionUsageComparisons {...props} /> :
      <WorkflowComparisons key={mode} {...props} cohort={mode} />}
  </section>;
}

const dimensions: { value: ComparisonDimension; label: string }[] = [
  { value: "project", label: "Project" }, { value: "workflow", label: "Workflow" },
  { value: "preset", label: "Preset" }, { value: "provider", label: "Provider" },
  { value: "model", label: "Exact model" }, { value: "effort", label: "Effort" },
  { value: "profile_revision", label: "Profile revision" }, { value: "adapter", label: "Adapter ID / hash" },
];
const evidenceMetrics: { value: ComparisonEvidenceMetric; label: string }[] = [
  { value: "all", label: "All facts" }, { value: "outcome", label: "Outcomes and runs" },
  { value: "execution", label: "Executions" }, { value: "usage", label: "Usage observations" },
  { value: "human_change", label: "Human changes" }, { value: "provider_verdict", label: "Provider verdicts" },
  { value: "repair", label: "Reviewer repairs" }, { value: "recovery", label: "Infrastructure recoveries" },
  { value: "check", label: "Selected checks" }, { value: "timing", label: "Timing and boundaries" },
];
const exactFilters: { key: "model" | "effort" | "profile_revision" | "adapter_id" | "adapter_hash"; label: string }[] = [
  { key: "model", label: "Exact model" }, { key: "effort", label: "Effort" },
  { key: "profile_revision", label: "Profile revision" }, { key: "adapter_id", label: "Adapter ID" },
  { key: "adapter_hash", label: "Adapter hash" },
];
const words = (value: string) => value.replaceAll("_", " ");
const millis = (value: number | null) => value === null ? "Unavailable" : `${value.toLocaleString()} ms`;
const messageOf = (cause: unknown) => cause instanceof Error ? cause.message : String(cause);
const selectionKey = (query: WorkflowComparisonQuery) => JSON.stringify([
  query.cohort, query.group_by,
  ...(["project_id", "workflow", "preset", "provider", "model", "effort", "profile_revision", "adapter_id", "adapter_hash", "start_date", "end_date"] as const)
    .map((key) => query[key]?.trim() || null),
]);
const usageStatus = (usage: ObservedUsage) => usage.status === "invalid" ? "Invalid observations" : usage.status === "unavailable" ? "Unavailable" : usage.partial ? "Partial observations" : "Observed";
const needsApply = (cause: unknown) => /Apply again|Results changed|page is invalid|Invalid .*page/i.test(messageOf(cause));

function Duration({ label, value }: { label: string; value: ComparisonDurationSummary }) {
  return <div><strong>{label}</strong><p>Sum: {millis(value.sum_millis)} · Mean: {millis(value.mean_millis)}</p>
    <small>{value.complete_samples} known samples · {value.unavailable_samples} unavailable · {value.ongoing_samples} ongoing</small></div>;
}

function GroupSummary({ group, inspect }: { group: WorkflowComparisonGroup; inspect: (metric: ComparisonEvidenceMetric) => void }) {
  const c = group.counts;
  return <div className="comparison-summary">
    <div><h3>Outcomes</h3><p>{c.units} selected units · {c.accepted} human accepted · {c.cancelled} cancelled · {c.failed} failed · {c.incomplete} incomplete</p>
      <p>Provider completion and reviewer approval do not establish human acceptance.</p>
      <button onClick={() => inspect("outcome")}>Inspect outcomes</button></div>
    <div><h3>Changes, repairs and recovery</h3>
      <p>Recorded human changes: {c.human_changes} · classification unavailable: {c.human_classification_unavailable}</p>
      <p>Recorded provider verdicts: {c.provider_verdicts} · negative: {c.provider_negative_verdicts} · classification unavailable: {c.provider_classification_unavailable}</p>
      <p>Known repairs authorized: {c.repairs_authorized} · known repairs started: {c.repairs_started} · classification unavailable: {c.repair_classification_unavailable}</p>
      <p>Recorded infrastructure recoveries: {c.infrastructure_recoveries}. Missing classification remains unavailable; recorded zero does not establish zero unrecorded events. These counts can overlap; they are not disjoint outcomes or completed repairs.</p>
      <button onClick={() => inspect("human_change")}>Inspect human changes</button>{" "}
      <button onClick={() => inspect("provider_verdict")}>Inspect provider verdicts</button>{" "}
      <button onClick={() => inspect("repair")}>Inspect repairs</button>{" "}
      <button onClick={() => inspect("recovery")}>Inspect recoveries</button></div>
    <div><h3>Selected checks across contributing runs</h3>
      <p>{c.selected_checks} selected checks · {c.unknown_check_matrices} unknown matrices</p>
      <p>{c.checks_passed} passed · {c.checks_failed} failed · {c.checks_running} running · {c.checks_not_run} not run · {c.checks_stale} stale · {c.checks_unavailable} unavailable</p>
      <p>A known empty matrix has zero selected checks; an unknown matrix is unavailable. Accepted receipts stay pinned to their historical selection.</p>
      <button onClick={() => inspect("check")}>Inspect checks</button></div>
    <div><h3>Time and coverage</h3>
      <Duration label="Task elapsed to human acceptance" value={group.elapsed_to_acceptance} />
      <Duration label="Complete run observed execution intervals (sum of invocations)" value={group.observed_execution_intervals} />
      <Duration label="Known partial execution intervals" value={group.known_partial_execution_intervals} />
      <p>Epoch coverage: {c.complete_execution_epochs} complete · {c.missing_execution_epochs} missing · {c.ongoing_execution_epochs} ongoing · {c.never_executed_epochs} never executed</p>
      <Duration label="Recorded recovery waits (partial wait coverage)" value={group.recorded_recovery_waits} />
      <Duration label="Check execution elapsed" value={group.check_execution} />
      <button onClick={() => inspect("timing")}>Inspect timing</button></div>
    <div><h3>Matching execution usage</h3>
      <p>{c.matching_sessions} matching sessions · {c.sessions_with_usage} with observations · {c.sessions_without_usage} without observations</p>
      <p>Usage coverage: {usageStatus(group.usage)}</p><ObservedUsageDetails usage={group.usage} label="Matching executions" />
      <button onClick={() => inspect("execution")}>Inspect executions</button>{" "}
      <button onClick={() => inspect("usage")}>Inspect usage</button></div>
  </div>;
}

type FactField = readonly [string, string | number | boolean | null];
function FactFields({ fields }: { fields: FactField[] }) {
  return <dl className="comparison-facts">{fields.map(([label, value]) =>
    <div key={label}><dt>{label}</dt><dd>{value === null ? "Unavailable" : typeof value === "boolean" ? value ? "Yes" : "No" : value}</dd></div>)}</dl>;
}

function EvidenceFact({ fact }: { fact: ComparisonEvidenceFact }) {
  switch (fact.kind) {
    case "outcome": return <><h4>Recorded outcome: {words(fact.outcome)}</h4><FactFields fields={[
      ["Task lifecycle", fact.lifecycle], ["Run status", fact.run_status], ["Archived task", fact.archived], ["Human acceptance at", fact.human_acceptance_at],
    ]} /></>;
    case "run": return <><h4>Historical run</h4><FactFields fields={[
      ["Run status", fact.status], ["Workflow", fact.workflow], ["Preset", fact.preset], ["Run created at (UTC)", fact.created_at],
      ["Human acceptance at", fact.human_acceptance_at], ["Candidate hash", fact.candidate_hash], ["Selection revision", fact.selection_revision],
    ]} /></>;
    case "execution": return <><h4>Recorded execution</h4><FactFields fields={[
      ["Role generation", fact.role_generation_id], ["Session", fact.session_id], ["Transcript epoch", fact.transcript_epoch], ["Matches execution filters", fact.matching],
      ["Provider", fact.identity.provider], ["Model", fact.identity.model], ["Effort", fact.identity.effort],
      ["Profile revision", fact.identity.profile_revision], ["Profile hash", fact.identity.profile_hash], ["Settings revision", fact.identity.settings_revision],
      ["Adapter ID", fact.identity.adapter_id], ["Adapter hash", fact.identity.adapter_hash], ["Executable version", fact.identity.executable_version],
      ["Compatibility revision", fact.identity.compatibility_revision],
    ]} /><p>Usage coverage: {usageStatus(fact.usage)}</p><ObservedUsageDetails usage={fact.usage} label="Execution" /></>;
    case "usage_observation": return <><h4>Response usage observation</h4><FactFields fields={[
      ["Provider", fact.provider], ["Native thread", fact.native_thread_id], ["Native turn", fact.native_turn_id], ["Response", fact.response_id],
      ["Session", fact.session_id], ["Transcript epoch", fact.transcript_epoch], ["Role generation", fact.role_generation_id],
      ["Accepted hook event", fact.accepted_hook_event_id], ["Invalid observation", fact.invalid], ["Received at", fact.received_at],
      ["Owner task", fact.owner_task_id], ["Owner attempt", fact.owner_attempt_id], ["Source version", fact.source_version], ["Contract revision", fact.contract_revision],
      ["Input tokens", fact.counters.input_tokens], ["Cached input tokens (subset)", fact.counters.cached_input_tokens],
      ["Cache write input tokens", fact.counters.cache_write_input_tokens], ["Output tokens", fact.counters.output_tokens], ["Reasoning tokens (subset)", fact.counters.reasoning_output_tokens], ["Total tokens", fact.counters.total_tokens],
    ]} /></>;
    case "classification_unavailable": return <><h4>{words(fact.metric)} classification unavailable</h4><p>{fact.reason}</p></>;
    case "human_change": return <><h4>Human request for changes</h4><FactFields fields={[["Decision", fact.decision_id], ["Child attempt", fact.child_attempt_id]]} /></>;
    case "provider_verdict": return <><h4>Provider verdict: {words(fact.verdict)}</h4><FactFields fields={[["Decision", fact.decision_id], ["Result", fact.result_id]]} /></>;
    case "repair": return <><h4>Reviewer repair authorization</h4><FactFields fields={[
      ["Authorization", fact.authorization_id], ["Originating decision", fact.originating_decision_id], ["Started session", fact.started_session_id], ["Classification reason", fact.classification_reason],
    ]} /><p>{fact.started_session_id ? "Repair execution started; completion is not implied." : "No linked repair start is established by this authorization."}</p></>;
    case "recovery": return <><h4>Infrastructure recovery</h4><FactFields fields={[
      ["Recovery", fact.recovery_id], ["Session", fact.session_id], ["State", fact.state], ["Reason", fact.reason], ["Created at", fact.created_at],
      ["Resolved at", fact.resolved_at], ["Recorded wait", millis(fact.elapsed_millis)], ["Ongoing", fact.ongoing],
    ]} /></>;
    case "check": return <><h4>Check: {words(fact.state)}</h4><FactFields fields={[
      ["Check", fact.check_id], ["Selection revision", fact.selection_revision], ["Receipt", fact.receipt_id], ["Selected in matrix", fact.selected],
      ["Candidate hash", fact.candidate_hash], ["Inputs hash", fact.inputs_hash], ["Check elapsed", millis(fact.elapsed_millis)], ["Reason", fact.reason],
      ["Recorded status", fact.recorded_status], ["Freshness", fact.freshness_state], ["Launch state", fact.launch_state], ["Exit code", fact.exit_code],
    ]} /></>;
    case "unknown_check_matrix": return <><h4>Selected check matrix unavailable</h4><p>{fact.reason}</p></>;
    case "timing": {
      const t = fact.invocation;
      return <><h4>Invocation timing: {words(t.state)}</h4><FactFields fields={[
        ["Evidence basis", t.evidence_basis], ["Session", t.session_id], ["Role generation", t.role_generation_id], ["Transcript epoch", t.transcript_epoch],
        ["Process identity hash", t.process_identity_hash], ["Start boundary", t.start_boundary_id], ["End boundary", t.end_boundary_id],
        ["Observed start", t.observed_start], ["Observed end", t.observed_end], ["Observed interval", millis(t.elapsed_millis)], ["Unavailable reason", t.unavailable_reason],
      ]} /></>;
    }
    case "boundary": return <><h4>Execution boundary observation</h4><FactFields fields={[
      ["Boundary", fact.boundary_id], ["Session", fact.session_id], ["Transcript epoch", fact.transcript_epoch], ["Process identity hash", fact.process_identity_hash],
      ["Boundary kind", fact.boundary_kind], ["Observed at", fact.observed_at],
    ]} /></>;
    case "boundary_conflict": return <><h4>Conflicting boundary fact</h4><FactFields fields={[["Boundary", fact.boundary_id], ["Fact hash", fact.fact_hash], ["Observed at", fact.observed_at]]} /></>;
    case "task_timing": return <><h4>Task elapsed timing: {words(fact.state)}</h4><FactFields fields={[
      ["Elapsed to human acceptance", millis(fact.elapsed_millis)], ["First start coverage complete", fact.first_start_coverage_complete], ["Reason", fact.reason],
    ]} /></>;
    default: {
      const unhandled: never = fact;
      return unhandled;
    }
  }
}

function WorkflowComparisons({ projects, initialProjectId, tasks, onOpen, cohort }: WorkflowComparisonProps & { cohort: "task" | "run" }) {
  const [filters, setFilters] = useState<WorkflowComparisonQuery>({ cohort, group_by: "project", project_id: initialProjectId });
  const [result, setResult] = useState<WorkflowComparisonPage | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [stale, setStale] = useState(false);
  const [group, setGroup] = useState<string | null>(null);
  const [metric, setMetric] = useState<ComparisonEvidenceMetric>("all");
  const [evidence, setEvidence] = useState<ComparisonEvidencePage | null>(null);
  const [evidenceBusy, setEvidenceBusy] = useState(false);
  const [evidenceError, setEvidenceError] = useState("");
  const [source, setSource] = useState<HistoricalSource | null>(null);
  const [sourceBusy, setSourceBusy] = useState(false);
  const [sourceError, setSourceError] = useState("");
  const [sourceMissing, setSourceMissing] = useState(false);
  const epoch = useRef(0);
  const evidenceEpoch = useRef(0);
  const sourceEpoch = useRef(0);
  const flight = useRef<AbortController | null>(null);
  const evidenceFlight = useRef<AbortController | null>(null);
  const sourceFlight = useRef<AbortController | null>(null);
  const evidencePanel = useRef<HTMLElement | null>(null);
  useEffect(() => {
    if (group !== null) evidencePanel.current?.focus();
  }, [group]);
  useEffect(() => () => {
    epoch.current++;
    evidenceEpoch.current++;
    sourceEpoch.current++;
    flight.current?.abort();
    evidenceFlight.current?.abort();
    sourceFlight.current?.abort();
  }, []);
  const clearSource = () => {
    sourceEpoch.current++;
    sourceFlight.current?.abort();
    sourceFlight.current = null;
    setSource(null);
    setSourceBusy(false);
    setSourceError("");
    setSourceMissing(false);
  };
  const clearEvidence = () => {
    evidenceEpoch.current++;
    evidenceFlight.current?.abort();
    evidenceFlight.current = null;
    setEvidence(null);
    setEvidenceBusy(false);
    setEvidenceError("");
    setGroup(null);
    clearSource();
  };
  const invalidate = (message: string) => {
    epoch.current++;
    flight.current?.abort();
    flight.current = null;
    setBusy(false);
    clearEvidence();
    setStale(true);
    setError(message);
  };
  const apply = async (more = false) => {
    if (more && (!result?.next_cursor || stale || flight.current)) return;
    flight.current?.abort();
    const current = ++epoch.current;
    const controller = new AbortController();
    flight.current = controller;
    if (!more) {
      clearEvidence();
      setResult(null);
      setStale(false);
    }
    setBusy(true);
    setError("");
    try {
      const page = await workflowComparisons(more && result ? { ...result.selection, cursor: result.next_cursor } : filters, controller.signal);
      if (current !== epoch.current) return;
      if (more && result && (page.snapshot !== result.snapshot || selectionKey(page.selection) !== selectionKey(result.selection))) {
        invalidate("Saved comparisons changed. Apply again to read one consistent snapshot.");
        return;
      }
      setResult(more && result ? { ...page, groups: [...result.groups, ...page.groups], displayed_group_units: result.displayed_group_units + page.displayed_group_units } : page);
    } catch (cause) {
      if (current !== epoch.current) return;
      if (needsApply(cause)) invalidate(messageOf(cause));
      else setError(messageOf(cause));
    } finally {
      if (current === epoch.current) { flight.current = null; setBusy(false); }
    }
  };
  const inspect = async (key: string, selectedMetric: ComparisonEvidenceMetric, more = false) => {
    if (!result || stale || (more && (!evidence?.next_cursor || evidenceFlight.current))) return;
    evidenceFlight.current?.abort();
    clearSource();
    const current = ++evidenceEpoch.current;
    const controller = new AbortController();
    evidenceFlight.current = controller;
    setGroup(key);
    setMetric(selectedMetric);
    setEvidenceBusy(true);
    setEvidenceError("");
    if (!more) setEvidence(null);
    try {
      const page = await comparisonEvidence({
        selection: JSON.stringify(result.selection), snapshot: result.snapshot, group: key, metric: selectedMetric,
        cursor: more ? evidence?.next_cursor : undefined,
      }, controller.signal);
      if (current !== evidenceEpoch.current) return;
      if (page.snapshot !== result.snapshot) {
        invalidate("Saved evidence changed. Apply again to read one consistent snapshot.");
        return;
      }
      setEvidence(more && evidence ? { ...page, rows: [...evidence.rows, ...page.rows] } : page);
    } catch (cause) {
      if (current !== evidenceEpoch.current) return;
      if (needsApply(cause)) invalidate(messageOf(cause));
      else setEvidenceError(messageOf(cause));
    } finally {
      if (current === evidenceEpoch.current) { evidenceFlight.current = null; setEvidenceBusy(false); }
    }
  };
  const openSource = async (key: HistoricalSourceKey) => {
    clearSource();
    const current = ++sourceEpoch.current;
    const controller = new AbortController();
    sourceFlight.current = controller;
    setSourceBusy(true);
    try {
      const value = await historicalSource(key, controller.signal);
      if (current === sourceEpoch.current) setSource(value);
    } catch (cause) {
      if (current === sourceEpoch.current) {
        setSourceError(messageOf(cause));
        setSourceMissing(cause instanceof ApiError && cause.status === 404);
      }
    } finally {
      if (current === sourceEpoch.current) { sourceFlight.current = null; setSourceBusy(false); }
    }
  };
  const dirty = !!result && selectionKey(filters) !== selectionKey(result.selection);
  const attributionLabel = (g: WorkflowComparisonGroup) => {
    if (g.attribution.state !== "known") return `${words(g.attribution.state)} attribution`;
    if (result?.selection.group_by === "project") return projects.find((p) => p.id === g.attribution.values[0])?.display_name ?? g.attribution.values.join(", ");
    return g.attribution.values.join(", ");
  };
  const attributionDetail = (g: WorkflowComparisonGroup) => {
    switch (g.attribution.state) {
      case "unavailable": return "No recorded values";
      case "mixed":
      case "incomplete": return "Inspect evidence for recorded identities";
      case "known": return `Recorded values: ${g.attribution.values.join(", ")}`;
    }
  };
  const evidenceGroup = result?.groups.find((candidate) => candidate.key === group);
  return <section aria-label="Task and run comparisons" className="workflow-comparisons">
    <p>{cohort === "task" ? "Tasks created (UTC)" : "Runs created (UTC)"}. From and Through are inclusive UTC days. Run creation is not execution start.</p>
    <p>Observational comparisons across unlike tasks; no causal ranking. Archived tasks are included. Execution filters select a matching recorded tuple within a contributing run, while outcomes and attribution retain the whole selected task or run.</p>
    <form className="history-filters" onSubmit={(event) => { event.preventDefault(); void apply(); }}>
      <label>Compare by<select value={filters.group_by} onChange={(event) => {
        const dimension = dimensions.find((d) => d.value === event.target.value);
        if (dimension) setFilters({ ...filters, group_by: dimension.value });
      }}>{dimensions.map((d) => <option key={d.value} value={d.value}>{d.label}</option>)}</select></label>
      <label>Project<select value={filters.project_id ?? ""} onChange={(event) => setFilters({ ...filters, project_id: event.target.value || undefined })}>
        <option value="">All projects</option>{projects.map((p) => <option key={p.id} value={p.id}>{p.display_name}</option>)}
      </select></label>
      <label>Workflow<select value={filters.workflow ?? ""} onChange={(event) => setFilters({ ...filters, workflow: event.target.value || undefined })}>
        <option value="">All workflows</option><option value="trip">TRIP</option><option value="single_agent">Single agent</option>
      </select></label>
      <label>Preset<select value={filters.preset ?? ""} onChange={(event) => setFilters({ ...filters, preset: event.target.value || undefined })}>
        <option value="">All presets</option><option value="single_agent">Single agent</option><option value="repository_research">Repository research</option><option value="build_and_review">Build and review</option>
      </select></label>
      <label>Provider<select value={filters.provider ?? ""} onChange={(event) => setFilters({ ...filters, provider: event.target.value || undefined })}>
        <option value="">All providers</option><option value="codex">Codex</option><option value="claude">Claude</option>
      </select></label>
      {exactFilters.map(({ key, label }) => <label key={key}>{label}<input maxLength={256} value={filters[key] ?? ""} onChange={(event) => setFilters({ ...filters, [key]: event.target.value || undefined })} /></label>)}
      <fieldset><legend>{cohort === "task" ? "Tasks" : "Runs"} created (UTC)</legend>
        <label>From<input type="date" value={filters.start_date ?? ""} onChange={(event) => setFilters({ ...filters, start_date: event.target.value || undefined })} /></label>
        <label>Through<input type="date" value={filters.end_date ?? ""} onChange={(event) => setFilters({ ...filters, end_date: event.target.value || undefined })} /></label>
      </fieldset>
      <button disabled={busy}>{busy ? "Loading comparisons…" : stale ? "Apply again" : "Apply"}</button>
    </form>
    {dirty && <p role="status">Controls have changed. Displayed results keep the applied selection until you choose Apply.</p>}
    {busy && <p role="status">Loading comparisons…</p>}
    {stale && <p role="status">Saved facts changed. Evidence and pagination are disabled. Any displayed aggregates remain from the prior snapshot. Choose Apply again.</p>}
    {error && <ErrorNotice error={error} />}
    {result && <>
      <p>Applied {result.cohort} cohort · grouped by {words(result.selection.group_by)} · {result.total_selected_units} selected units · {result.displayed_group_units} units in loaded groups</p>
      <FactFields fields={[["Applied project", result.selection.project_id ?? "All projects"], ["Applied workflow", result.selection.workflow ?? "All workflows"],
        ["Applied preset", result.selection.preset ?? "All presets"], ["Applied provider", result.selection.provider ?? "All providers"],
        ...exactFilters.map(({ key, label }): FactField => [`Applied ${label.toLowerCase()}`, result.selection[key] ?? "All values"]),
        ["Applied From (UTC)", result.selection.start_date ?? "Unbounded"], ["Applied Through (UTC)", result.selection.end_date ?? "Unbounded"]]} />
      <details><summary>Counting, timing and coverage scope</summary><p>{result.date_basis}</p><p>{result.outcome_scope}</p><p>{result.usage_scope}</p><p>{result.timing_scope}</p><p>{result.wait_scope}</p>
        <p>Observed boundaries include unmeasured launch and reconciliation latency. Summed invocation intervals may overlap and are not task elapsed time or CPU time. Recorded recovery waits do not measure all queue or human waiting.</p></details>
      {!result.groups.length && <p className="empty">No {cohort === "task" ? "tasks" : "runs"} match the applied filters.</p>}
      {result.groups.map((g) => <article key={g.key} className="comparison-group">
        <h2>{attributionLabel(g)}</h2><p>Attribution: {words(g.attribution.state)} · {attributionDetail(g)}</p>
        <button disabled={stale || busy} onClick={() => void inspect(g.key, "all")}>Inspect all evidence</button>
        <p>{g.counts.units} units · {g.counts.accepted} human accepted · {g.counts.cancelled} cancelled · {g.counts.failed} failed · {g.counts.incomplete} incomplete</p>
        <details><summary>Metrics, samples and coverage</summary><fieldset disabled={stale || busy}><legend>Metrics for {attributionLabel(g)}</legend><GroupSummary group={g} inspect={(m) => void inspect(g.key, m)} /></fieldset></details>
      </article>)}
      {result.next_cursor && !stale && <button disabled={busy} onClick={() => void apply(true)}>Load more groups</button>}
    </>}
    {group !== null && !stale && <section ref={evidencePanel} tabIndex={-1} className="comparison-evidence" aria-label="Comparison evidence" aria-busy={evidenceBusy}>
      <h2>Historical comparison evidence</h2><p>Group: {evidenceGroup ? attributionLabel(evidenceGroup) : "Unavailable"}. Exact old attempts remain here even when another attempt is active.</p>
      <label>Evidence metric<select value={metric} onChange={(event) => {
        const selected = evidenceMetrics.find((m) => m.value === event.target.value);
        if (selected) void inspect(group, selected.value);
      }}>{evidenceMetrics.map((m) => <option key={m.value} value={m.value}>{m.label}</option>)}</select></label>
      <button onClick={clearEvidence}>Close evidence</button>
      {evidenceBusy && <p role="status">Loading historical evidence…</p>}
      {evidenceError && <><ErrorNotice error={evidenceError} /><button onClick={() => void inspect(group, metric)}>Retry evidence</button></>}
      {evidence && <><p>{evidence.rows.length} of {evidence.total_rows} facts loaded</p>
        {!evidence.rows.length && <p>No recorded facts for this metric. Missing evidence does not establish zero or an absent outcome.</p>}
        {evidence.rows.map((row: ComparisonEvidenceRow) => {
          const task = tasks.find((t) => t.id === row.task_id);
          return <article key={row.key} className="comparison-fact"><FactFields fields={[["Unit", row.unit_id], ["Task", row.task_id], ["Exact attempt", row.attempt_id]]} />
            {row.recipe_attribution ? <><h4>Recorded recipe and schedule origin</h4><FactFields fields={[
              ["Recipe title", row.recipe_attribution.recipe_title], ["Recipe revision", row.recipe_attribution.recipe_revision_id],
              ["Schedule", row.recipe_attribution.schedule_id], ["Scheduled for (UTC)", row.recipe_attribution.scheduled_for_utc],
              ["Occurrence", row.recipe_attribution.occurrence_id], ["Execution authorization", row.recipe_attribution.authorization_id],
              ["Claimed root attempt", row.recipe_attribution.root_attempt_id], ["Occurrence outcome", row.recipe_attribution.occurrence_outcome],
              ["Occurrence reason", row.recipe_attribution.occurrence_reason],
            ]} /><p>Origin records do not establish execution, usage or success. Occurrences without a task remain in schedule history.</p></> :
              <p>Recipe and schedule attribution unavailable for this historical fact.</p>}
            <EvidenceFact fact={row.fact} />
            {row.truncated && <p>Truncated: this fact contains a bounded excerpt; some text is omitted.</p>}
            {row.source && <><FactFields fields={[["Source kind", words(row.source.kind)], ["Source ID", row.source.id], ["Source task", row.source.task_id], ["Source attempt", row.source.attempt_id]]} />
              <button onClick={() => { if (row.source) void openSource(row.source); }}>View historical source</button></>}
            {task ? <><p>Current task project: {projects.find((project) => project.id === task.project_id)?.display_name ?? "Unavailable project name"} · {task.project_id}</p><button onClick={() => onOpen(task)}>Open task (current context){task.archived ? " · Archived" : ""}</button></> : <p>Current task context unavailable. Historical facts remain shown.</p>}
          </article>;
        })}
        {evidence.next_cursor && <button disabled={evidenceBusy} onClick={() => void inspect(group, metric, true)}>Load more evidence</button>}
      </>}
      {sourceBusy && <p role="status">Loading historical source…</p>}
      {sourceMissing && <p>This source is no longer available. Frozen comparison facts and outcomes remain unchanged.</p>}
      {sourceError && <ErrorNotice error={sourceError} />}
      {source && <section className="historical-source" aria-label="Historical comparison source">
        <h3>{source.hit.task_title}</h3><FactFields fields={[["Project", source.hit.project_name], ["Project ID", source.hit.project_id], ["Source kind", words(source.hit.source.kind)],
          ["Source ID", source.hit.source.id], ["Task", source.hit.source.task_id], ["Exact attempt", source.hit.source.attempt_id], ["Archived", source.hit.archived], ["Created at", source.hit.created_at]]} />
        <p>Source content is read separately; it does not replace the applied snapshot facts.</p><pre>{source.text}</pre>
        {source.truncated && <p>Truncated historical source excerpt.</p>}<button onClick={clearSource}>Close source</button>
      </section>}
    </section>}
  </section>;
}
