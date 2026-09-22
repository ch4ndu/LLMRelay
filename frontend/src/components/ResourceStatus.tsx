export function ResourceStatus(
  { resources }: { resources: Record<string, unknown> },
) {
  const processes = (resources.processes || []) as Array<
    Record<string, unknown>
  >;
  const capacity = (resources.capacity || {}) as Record<string, unknown>;
  const occupied = (capacity.occupied_by_provider || {}) as Record<
    string,
    unknown
  >;
  const managers = (capacity.occupied_managers_by_provider || {}) as Record<
    string,
    unknown
  >;
  return (
    <section className="panel resources">
      <header>
        <h3>Resources</h3>
        <span>{String(resources.observed_at || "Unavailable")}</span>
      </header>
      <div className="metrics">
        <div>
          <strong>{String(resources.active_sessions || 0)}</strong>
          <small>Sessions</small>
        </div>
        <div>
          <strong>{String(resources.running_checks || 0)}</strong>
          <small>Checks</small>
        </div>
        <div>
          <strong>{String(resources.queued_guidance || 0)}</strong>
          <small>Guidance</small>
        </div>
      </div>
      {Boolean(capacity.policy) && (
        <p className="hint">
          {String(capacity.policy)} · global{" "}
          {String(capacity.occupied_global ?? "–")}/
          {String(capacity.global_processes ?? "–")} · Codex{" "}
          {String(occupied.codex ?? "–")}/
          {String(capacity.active_invocations_per_provider ?? "–")} · Claude
          {" "}
          {String(occupied.claude ?? "–")}/
          {String(capacity.active_invocations_per_provider ?? "–")} · reserved
          {" "}
          {String(capacity.issued_reservations ?? "–")} · Codex managers{" "}
          {String(managers.codex ?? "–")}/
          {String(capacity.managers_per_provider ?? "–")} · Claude managers{" "}
          {String(managers.claude ?? "–")}/
          {String(capacity.managers_per_provider ?? "–")}
        </p>
      )}
      {processes.map((process) => (
        <div className="process" key={`${process.session_id}-${process.pid}`}>
          <span className={`status ${process.state}`} />
          <div>
            <strong>PID {String(process.pid)}</strong>
            <small>
              {String(process.state).replaceAll("_", " ")} · {process.rss_bytes
                ? `${(Number(process.rss_bytes) / 1048576).toFixed(1)} MiB`
                : "RSS unavailable"} · {String(process.cpu_percent ?? "–")}% CPU
            </small>
          </div>
        </div>
      ))}
    </section>
  );
}
