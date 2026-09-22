import type { Task } from "../types";

export function WorkSummary(
  { task, checks }: { task: Task; checks: Array<Record<string, unknown>> },
) {
  const attempt = task.active_attempt;
  const snapshot =
    task.snapshots.find((value) =>
      value.id === attempt?.accepted_snapshot_id
    ) ||
    [...task.snapshots].reverse().find((value) =>
      value.attempt_id === attempt?.id && value.kind === "candidate"
    );
  const snapshotEntries = snapshot?.manifest?.entries;
  const entries = Array.isArray(snapshotEntries)
    ? snapshotEntries.slice(0, 100) as Array<Record<string, unknown>>
    : [];
  return (
    <section className="panel summary">
      <header>
        <h3>Work summary</h3>
        <span>{attempt?.base_revision?.slice(0, 8) || "Not started"}</span>
      </header>
      <dl>
        <div>
          <dt>Plan revision</dt>
          <dd>{attempt?.plan_hash?.slice(0, 12) || "Pending"}</dd>
        </div>
        <div>
          <dt>Candidate revision</dt>
          <dd>{attempt?.candidate_hash?.slice(0, 12) || "Pending"}</dd>
        </div>
        <div>
          <dt>Checks</dt>
          <dd>
            {checks.length
              ? `${
                checks.filter((value) =>
                  value.status === "finished" && value.exit_code === 0
                ).length
              }/${checks.length} passed`
              : "Pending"}
          </dd>
        </div>
        <div>
          <dt>Lineage</dt>
          <dd>
            {attempt?.parent_attempt_id
              ? `Rework of ${attempt.parent_attempt_id.slice(0, 8)}`
              : "Original attempt"}
          </dd>
        </div>
      </dl>
      <h4>Acceptance criteria</h4>
      <ul>
        {task.acceptance_criteria.map((item) => <li key={item}>{item}</li>)}
      </ul>
      <h4>Frozen change summary</h4>
      {entries.length
        ? (
          <ul>
            {entries.map((entry, index) => (
              <li key={`${String(entry.path)}-${index}`}>
                <code>{String(entry.path)}</code> ·{" "}
                {entry.deleted ? "deleted" : String(entry.kind)} · mode{" "}
                {Number(entry.mode).toString(8)}
              </li>
            ))}
          </ul>
        )
        : <p className="empty">No frozen candidate file manifest yet.</p>}
      <h4>Check evidence</h4>
      {checks.map((check) => (
        <article className="review-row" key={String(check.id)}>
          <strong>{String(check.suite_name || check.executable)}</strong>
          <span>
            {String(check.status)}
            {check.exit_code !== null && check.exit_code !== undefined
              ? ` · exit ${String(check.exit_code)}`
              : ""}
          </span>
          {Boolean(check.evidence) && (
            <pre>{JSON.stringify(check.evidence, null, 2)}</pre>
          )}
        </article>
      ))}
    </section>
  );
}
