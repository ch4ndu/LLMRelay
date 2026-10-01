import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { useRef, useState } from "react";
import { command, operationId } from "../api";
import {
  type FailedStepDetail,
  roleLabel,
  type Session,
  type Task,
} from "../types";

const exactRecoveryKinds = [
  "workspace_reservation",
  "graceful_stop_deadline",
];
const restoreEvidenceKinds = [
  "database_restore_claim",
  "database_restore_freeze",
];
const failedStepKind = "coordinator_failure";

function recoveryDetail(record: Record<string, unknown>) {
  const detail = record.detail;
  return typeof detail === "object" && detail !== null && !Array.isArray(detail)
    ? detail as Record<string, unknown>
    : undefined;
}

const isFailedStep = (record: Record<string, unknown>) =>
  recoveryDetail(record)?.kind === failedStepKind;

function failedStepDetail(
  detail: Record<string, unknown> | undefined,
): FailedStepDetail | undefined {
  if (
    detail?.kind !== failedStepKind ||
    typeof detail.task_id !== "string" ||
    typeof detail.operation !== "string" ||
    (detail.effect_certainty !== "none" &&
      detail.effect_certainty !== "possible") ||
    typeof detail.cause !== "string" ||
    typeof detail.failure_key !== "string"
  ) return undefined;
  return {
    kind: failedStepKind,
    task_id: detail.task_id,
    operation: detail.operation,
    causal_identity: detail.causal_identity,
    effect_certainty: detail.effect_certainty,
    cause: detail.cause,
    failure_key: detail.failure_key,
  };
}

export function RecoveryPanel(
  { task, records, sessions = [], selectedRecordId, onChanged }: {
    task: Task;
    records: Array<Record<string, unknown>>;
    sessions?: Session[];
    /** An exact record opened from attention; no other record replaces it. */
    selectedRecordId?: string;
    onChanged: () => void;
  },
) {
  const unresolved = records.filter((candidate) =>
    candidate.state === "attention_required"
  );
  const attempt = task.active_attempt;
  const decisionFor = (record: Record<string, unknown>) =>
    attempt && (
      <RecoveryDecision
        // Evidence, errors and retry identity belong to one record, so a
        // different record starts from a clean form.
        key={typeof record.id === "string" ? record.id : ""}
        task={task}
        attemptId={attempt.id}
        record={record}
        session={sessions.find((session) => session.id === record.session_id)}
        onChanged={onChanged}
      />
    );
  if (selectedRecordId === undefined) {
    // Independent failed-step holds are never hidden behind the chosen record.
    const others = unresolved.filter((candidate) => !isFailedStep(candidate));
    const record = others.find((candidate) => {
      const detail = recoveryDetail(candidate);
      const kind = String(detail?.kind || "");
      return typeof candidate.session_id === "string" ||
        typeof detail?.check_id === "string" ||
        exactRecoveryKinds.includes(kind) ||
        restoreEvidenceKinds.includes(kind);
    }) || others[0];
    return (
      <>
        {record && decisionFor(record)}
        {unresolved.filter(isFailedStep).map(decisionFor)}
      </>
    );
  }
  const record = unresolved.find((candidate) =>
    candidate.id === selectedRecordId
  );
  if (record && attempt) return decisionFor(record);
  return (
    <section className="panel recovery" role="status">
      <h3>Recovery</h3>
      <p>
        The problem you opened was resolved or changed, so there is nothing to
        do for it here. Open a current item from Needs your attention, or close
        and reopen this task.
      </p>
    </section>
  );
}

function RecoveryDecision(
  { task, attemptId, record, session, onChanged }: {
    task: Task;
    attemptId: string;
    record: Record<string, unknown>;
    session?: Session;
    onChanged: () => void;
  },
) {
  const [evidence, setEvidence] = useState("");
  const [error, setError] = useState("");
  const retry = useRef<{ body: string; id: string } | undefined>(undefined);
  const detail = recoveryDetail(record);
  const recoveryKind = String(detail?.kind || "");
  const sessionId = typeof record.session_id === "string"
    ? record.session_id
    : undefined;
  const failedStep = recoveryKind === failedStepKind;
  const materialization = recoveryKind.includes("materialization");
  const exactRecovery = exactRecoveryKinds.includes(recoveryKind);
  const restoreEvidence = restoreEvidenceKinds.includes(recoveryKind);
  const genericResolver = !exactRecovery && !failedStep &&
    (sessionId !== undefined ||
      typeof detail?.check_id === "string" || restoreEvidence ||
      materialization);
  const marker = typeof record.id === "string"
    ? `recovery_record:${record.id}`
    : undefined;
  const resolve = async (decision: string) => {
    const recoveryId = typeof record.id === "string" ? record.id : "";
    if (!recoveryId) {
      setError(
        "The displayed recovery record has no ID. Refresh before resolving it.",
      );
      return;
    }
    setError("");
    const request = {
      task_id: task.id,
      attempt_id: attemptId,
      recovery_id: recoveryId,
      ...(failedStep
        ? { session_id: null }
        : sessionId === undefined
        ? {}
        : { session_id: sessionId }),
      expected_version: task.version,
      decision,
      evidence,
    };
    const body = JSON.stringify(request);
    const id = retry.current?.body === body ? retry.current.id : operationId();
    retry.current = { body, id };
    try {
      await command({
        kind: "resolve_recovery",
        operation_id: id,
        ...request,
      });
      retry.current = undefined;
      onChanged();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  if (failedStep) {
    const hold = failedStepDetail(detail);
    const exact = hold !== undefined && record.attempt_id === attemptId &&
      hold.task_id === task.id && record.session_id === null;
    const blockedBy = Array.isArray(record.retry_blocked_by)
      ? record.retry_blocked_by.map(String)
      : undefined;
    // A possible effect is retried only once the service lists no unsettled
    // outcome; a missing list counts as unsettled.
    const retryBlocked = hold?.effect_certainty !== "none" &&
      (blockedBy === undefined || blockedBy.length > 0);
    return (
      <section
        className="panel recovery"
        data-attention-target={marker}
        tabIndex={-1}
      >
        <h3>An automatic step failed</h3>
        <p>
          LLMRelay stopped automatic work on this task because one of its
          automatic steps failed. Other tasks are not held by this failure.{" "}
          {hold?.effect_certainty === "none"
            ? "This kind of step does not start agent actions."
            : "This step may have started an agent action before it failed."}
        </p>
        <p>
          {!exact
            ? "The details of this failed step do not match the task shown here. Refresh to see the current state."
            : retryBlocked
            ? "Retry failed step becomes available after LLMRelay records the outcome of the agent action this step may have started. Check the task's Activity tab in the meantime. To stop the task instead, choose Cancel in Controls."
            : "Check the task's Activity tab, describe what you found below, then choose Retry failed step. LLMRelay then evaluates the task again from its current state. It never repeats a step whose outcome is unknown, and other holds on this task stay in place. To stop the task instead, choose Cancel in Controls."}
        </p>
        <TechnicalDetails>
          <p>
            {hold
              ? `${hold.operation} · effect ${hold.effect_certainty} · `
              : "unreadable failed-step detail · "}
            recovery record {String(record.id || "unknown")} · attempt{" "}
            {String(record.attempt_id || "unknown")}
          </p>
          {hold && (
            <>
              <pre>{hold.cause}</pre>
              <p>
                causal identity {JSON.stringify(hold.causal_identity)} · failure
                key {hold.failure_key}
              </p>
            </>
          )}
          {blockedBy && blockedBy.length > 0 && (
            <p>Waiting for the outcome of: {blockedBy.join(" · ")}</p>
          )}
        </TechnicalDetails>
        {exact
          ? (
            <>
              <textarea
                aria-label="Recovery evidence"
                placeholder="What did you check? For example: the Activity tab shows the step never finished."
                value={evidence}
                onChange={(e) => setEvidence(e.target.value)}
              />
              <div className="button-row">
                <button
                  disabled={retryBlocked || !evidence.trim()}
                  onClick={() => resolve("retry_failed_step")}
                >
                  Retry failed step
                </button>
              </div>
            </>
          )
          : (
            <div className="button-row">
              <button onClick={onChanged}>
                Refresh to see the current state
              </button>
            </div>
          )}
        {error && <ErrorNotice error={error} />}
      </section>
    );
  }
  return (
    <section
      className="panel recovery"
      data-attention-target={marker}
      tabIndex={-1}
    >
      <h3>
        {session
          ? `Manual action needed: ${roleLabel(session.role)}`
          : "Manual action needed"}
      </h3>
      <p>
        {exactRecovery
          ? "Use the recovery action shown above for this task. It checks the affected session before allowing work to continue."
          : !genericResolver
          ? "This earlier request no longer has a recovery action. Refresh the task and use its current controls to submit a corrected request."
          : materialization
          ? "LLMRelay could not finish preparing the rework. Describe what you saw below, then choose Retry materialization to prepare the task files again."
          : restoreEvidence
          ? "This task was interrupted by a database restore. Describe what you know below, then choose Check recovery and continue. LLMRelay confirms the recorded state before continuing."
          : `LLMRelay stopped automatic work because it could not confirm that ${
            session ? `the ${roleLabel(session.role).toLowerCase()}` : "an agent"
          } has fully stopped. Check the agent's output or terminal, describe what you saw below, then choose Check recovery and continue. LLMRelay verifies the recorded processes itself before anything continues.`}
      </p>
      <TechnicalDetails>
      <p>
        {exactRecovery
          ? "This record has an exact recovery command. Generic recovery decisions are intentionally unavailable because they cannot recheck its complete immutable binding."
          : recoveryKind === "database_restore_claim"
          ? "This restore recorded a prelaunch claim reservation without a session or check. Your note only annotates the decision; the service confirms that the recorded prior state is eligible before reconciliation."
          : recoveryKind === "database_restore_freeze"
          ? "This restore recorded an interrupted local freeze with no external process identity. Your note only annotates the decision; the service confirms the operation-bound record and current recovery-required freeze before reconciliation."
          : materialization
          ? "Rework materialization needs a decision for this exact intent. Retry uses its recorded intent ID; cancellation verifies the whole parent and child lineage."
          : genericResolver
          ? "Process ownership is unresolved. Your note annotates the decision; the service still verifies every recorded PID and start identity. After reconciliation, use the exact session Resume action in the task's agent settings."
          : "This historical control record has no exact process, check, workspace, or claim tuple. It was definitively rejected, so generic recovery would be guaranteed to fail; refresh and submit a corrected versioned control."}
      </p>
      <p>Recovery record {String(record.id || "unknown")}{sessionId ? ` · session ${sessionId}` : ""}</p>
      </TechnicalDetails>
      {exactRecovery && (
        <p className="hint">
          Use the current recovery and continuation action above this panel.
        </p>
      )}
      {genericResolver && (
        <>
          <textarea
            aria-label="Recovery evidence"
            placeholder="What did you see? For example: the agent's terminal is closed."
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
                : "Check recovery and continue"}
            </button>
            {!restoreEvidence && (
              <button
                className="danger"
                disabled={!evidence.trim()}
                onClick={() => resolve("cancel")}
              >
                Verify and cancel
              </button>
            )}
          </div>
        </>
      )}
      {!exactRecovery && !genericResolver && (
        <div className="button-row">
          <button onClick={onChanged}>
            Refresh to see the current state
          </button>
        </div>
      )}
      {error && <ErrorNotice error={error} />}
    </section>
  );
}
