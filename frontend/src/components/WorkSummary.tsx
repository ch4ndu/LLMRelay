import { TechnicalDetails } from "./ErrorNotice";
import type { Task } from "../types";

/** The frozen file changes of the task's current or accepted result. */
export function WorkSummary({ task }: { task: Task }) {
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
  const total = Number(snapshot?.manifest?.total_entries ?? entries.length);
  return (
    <section className="panel summary" aria-labelledby="changes-title">
      <header>
        <h3 id="changes-title">Changed files</h3>
        {entries.length > 0 && <span className="count">{total}</span>}
      </header>
      {entries.length
        ? (
          <>
            <ul className="file-changes">
              {entries.map((entry, index) => (
                <li key={`${String(entry.path)}-${index}`}>
                  <code>{String(entry.path)}</code>
                  <span className={entry.deleted ? "badge danger" : "badge"}>
                    {entry.deleted ? "Deleted" : "Changed"}
                  </span>
                </li>
              ))}
            </ul>
            {total > entries.length && (
              <p className="hint">
                Showing the first {entries.length} of {total} files.
              </p>
            )}
          </>
        )
        : (
          <p className="empty">
            {attempt
              ? "No changes are frozen for review yet. They appear here after the implementer finishes."
              : "Work has not started yet."}
          </p>
        )}
      <TechnicalDetails>
        <dl>
          <div>
            <dt>Starting commit</dt>
            <dd>{attempt?.base_revision || "Not started"}</dd>
          </div>
          <div>
            <dt>Plan revision</dt>
            <dd>{attempt?.plan_hash || "Pending"}</dd>
          </div>
          <div>
            <dt>Result revision</dt>
            <dd>{attempt?.candidate_hash || "Pending"}</dd>
          </div>
          <div>
            <dt>Lineage</dt>
            <dd>
              {attempt?.parent_attempt_id
                ? `Rework of ${attempt.parent_attempt_id}`
                : "Original attempt"}
            </dd>
          </div>
        </dl>
        {entries.length > 0 && (
          <ul>
            {entries.map((entry, index) => (
              <li key={`mode-${String(entry.path)}-${index}`}>
                <code>{String(entry.path)}</code> ·{" "}
                {entry.deleted ? "deleted" : String(entry.kind)} · mode{" "}
                {Number(entry.mode).toString(8)}
              </li>
            ))}
          </ul>
        )}
      </TechnicalDetails>
    </section>
  );
}
