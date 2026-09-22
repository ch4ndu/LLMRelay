import { useState } from "react";
import { command, operationId } from "../api";
import type { Task } from "../types";
export function RecoveryPanel(
  { task, records, onChanged }: {
    task: Task;
    records: Array<Record<string, unknown>>;
    onChanged: () => void;
  },
) {
  const [evidence, setEvidence] = useState("");
  const [error, setError] = useState("");
  const unresolved = records.filter((candidate) =>
    candidate.state === "attention_required"
  );
  if (!task.active_attempt || !unresolved.length) return null;
  const record = unresolved.find((candidate) => {
    const detail = candidate.detail as Record<string, unknown> | undefined;
    return typeof candidate.session_id === "string" ||
      typeof detail?.check_id === "string" ||
      ["workspace_reservation", "graceful_stop_deadline"].includes(
        String(detail?.kind || ""),
      );
  }) || unresolved[0];
  const recoveryKind = String(
    (record.detail as Record<string, unknown> | undefined)?.kind || "",
  );
  const materialization = recoveryKind.includes("materialization");
  const exactRecovery = ["workspace_reservation", "graceful_stop_deadline"]
    .includes(recoveryKind);
  const genericResolver = !exactRecovery &&
    (typeof record.session_id === "string" ||
      typeof (record.detail as Record<string, unknown> | undefined)?.check_id ===
        "string");
  const resolve = async (decision: string) => {
    try {
      await command({
        kind: "resolve_recovery",
        operation_id: operationId(),
        task_id: task.id,
        attempt_id: task.active_attempt!.id,
        session_id: record.session_id,
        expected_version: task.version,
        decision,
        evidence,
      });
      onChanged();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  return (
    <section className="panel recovery">
      <h3>Recovery decision</h3>
      <p>
        {exactRecovery
          ? "This record has an exact recovery command. Generic recovery decisions are intentionally unavailable because they cannot recheck its complete immutable binding."
          : genericResolver
          ? "Process ownership is unresolved. Human text annotates the decision; the service still verifies every recorded PID and start identity. After reconciliation, use the exact session Resume action in role settings."
          : "This historical control record has no exact process, check, workspace, or claim tuple. It was definitively rejected, so generic recovery would be guaranteed to fail; refresh and submit a corrected versioned control."}
      </p>
      {exactRecovery && (
        <p className="hint">
          Use the current recovery and continuation action above this panel.
        </p>
      )}
      {genericResolver && (
        <>
          <textarea
            aria-label="Recovery evidence"
            value={evidence}
            onChange={(e) => setEvidence(e.target.value)}
          />
          <div className="button-row">
            <button
              disabled={!evidence.trim()}
              onClick={() =>
                resolve(
                  materialization
                    ? "retry_materialization"
                    : "confirm_quiescent",
                )}
            >
              {materialization
                ? "Retry materialization"
                : "Verify quiescence and reconcile"}
            </button>
            <button
              className="danger"
              disabled={!evidence.trim()}
              onClick={() => resolve("cancel")}
            >
              Verify and cancel
            </button>
          </div>
        </>
      )}
      {!exactRecovery && !genericResolver && (
        <div className="button-row">
          <button onClick={onChanged}>
            Refresh and review corrected control
          </button>
        </div>
      )}
      {error && <p className="error" role="alert">{error}</p>}
    </section>
  );
}
