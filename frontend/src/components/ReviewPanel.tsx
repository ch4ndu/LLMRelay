import { useRef, useState } from "react";
import {
  ApiError,
  command,
  operationIntent,
  reuseOperationIdentity,
} from "../api";
import type { ContinuationAction, Task, TripTaskVerification } from "../types";

export function ReviewPanel(
  { task, verification = [], actions = [], onChanged }: {
    task: Task;
    verification?: TripTaskVerification[];
    actions?: ContinuationAction[];
    onChanged: () => void;
  },
) {
  const [feedback, setFeedback] = useState("");
  const [carryApproval, setCarryApproval] = useState(false);
  const [additionalJustification, setAdditionalJustification] = useState("");
  const [error, setError] = useState("");
  const attempt = task.active_attempt;
  const operationStorageKey = `llmrelay.review.operations.${task.id}`;
  const commandIdentities = useRef(
    new Map<string, { body: string; id: string }>(
      (() => {
        try {
          const value = JSON.parse(
            localStorage.getItem(operationStorageKey) || "[]",
          );
          return Array.isArray(value) ? value : [];
        } catch {
          return [];
        }
      })(),
    ),
  );
  const invoke = async (body: Record<string, unknown>) => {
    const key = operationIntent(body);
    const stable = reuseOperationIdentity(
      commandIdentities.current.get(key),
      body,
    );
    const { id, request } = stable;
    commandIdentities.current.set(key, { body: JSON.stringify(request), id });
    localStorage.setItem(
      operationStorageKey,
      JSON.stringify([...commandIdentities.current]),
    );
    try {
      await command({ ...request, operation_id: id } as never);
      commandIdentities.current.delete(key);
      localStorage.setItem(
        operationStorageKey,
        JSON.stringify([...commandIdentities.current]),
      );
      setError("");
      onChanged();
      return true;
    } catch (cause) {
      if (!(cause instanceof ApiError && cause.ambiguous)) {
        commandIdentities.current.delete(key);
        localStorage.setItem(
          operationStorageKey,
          JSON.stringify([...commandIdentities.current]),
        );
      }
      setError(
        cause instanceof ApiError && cause.ambiguous
          ? `${cause.message} Refresh and reconcile before retrying.`
          : cause instanceof Error
          ? cause.message
          : String(cause),
      );
      onChanged();
      return false;
    }
  };
  const relevantActions = actions.filter((action) =>
    action.binding.task_id === task.id ||
    action.binding.attempt_id === attempt?.id
  );
  const implementationAuthorization = relevantActions.find((action) =>
    action.kind === "authorize_implementation"
  );
  const migration = relevantActions.find((action) =>
    action.kind === "migrate_attempt"
  );
  const additionalExplorer = relevantActions.find((action) =>
    action.kind === "authorize_additional_explorer"
  );

  const apply = async (decision: "accept" | "request_changes") => {
    if (!attempt) return;
    try {
      const applied = attempt.phase === "awaiting_plan_approval"
        ? await invoke({
          kind: "approve_plan",
          task_id: task.id,
          attempt_id: attempt.id,
          expected_version: task.version,
          plan_hash: attempt.plan_hash,
        })
        : await invoke({
          kind: "human_review",
          task_id: task.id,
          attempt_id: attempt.id,
          expected_version: task.version,
          decision,
          feedback,
          carry_plan_approval: decision === "request_changes" && carryApproval,
        });
      if (applied) setFeedback("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  if (
    !attempt ||
    ![
        "awaiting_plan_approval",
        "awaiting_human_review",
        "awaiting_implementation_authorization",
      ].includes(attempt.phase) && !migration && !additionalExplorer
  ) return null;
  const plan = attempt.phase === "awaiting_plan_approval";
  const implementation =
    attempt.phase === "awaiting_implementation_authorization";
  const bindingString = (
    action: ContinuationAction | undefined,
    key: string,
  ) => {
    const value = action?.binding[key];
    return typeof value === "string" ? value : undefined;
  };
  return (
    <section className="panel review-panel">
      <span className="eyebrow">Your decision</span>
      <h3>
        {implementation
          ? "Authorize implementation of this exact plan"
          : plan
          ? "Approve the reviewed plan"
          : "Accept the reviewed result"}
      </h3>
      <p>
        {implementation
          ? "Plan approval and implementation authorization are separate human decisions. This authorizes only the current reviewed plan."
          : plan
          ? `Plan ${
            attempt.plan_hash?.slice(0, 12)
          } passed independent review. Read the exact persisted plan before approval.`
          : `Candidate ${
            attempt.candidate_hash?.slice(0, 12)
          } passed configured checks and final review.`}
      </p>
      {!plan && !implementation && (
        <p className="hint">
          Verification evidence: {verification.filter((item) =>
            item.latest_run?.freshness_state === "current" &&
            item.latest_run.exit_code === 0
          ).length}/{verification.length}{" "}
          selected checks current. Manager conformance revision{" "}
          {attempt.manager_conformance_revision}; human acceptance remains a
          separate decision.
        </p>
      )}
      {(plan || implementation) && (
        <pre className="plan-content">{attempt.plan || "The persisted plan text is unavailable; approval is disabled."}</pre>
      )}
      {!plan && (
        <>
          <textarea
            aria-label="Review or rework feedback"
            placeholder="Required context for requested rework"
            value={feedback}
            onChange={(event) => setFeedback(event.target.value)}
          />
          <label className="toggle">
            <input
              type="checkbox"
              checked={carryApproval}
              onChange={(event) => setCarryApproval(event.target.checked)}
            />Carry plan approval only if scope and role configuration still
            match
          </label>
        </>
      )}
      <div className="button-row">
        {!plan && !implementation && (
          <button
            disabled={!feedback.trim()}
            onClick={() => apply("request_changes")}
          >
            Request rework
          </button>
        )}
        {implementation
          ? (
            <button
              className="primary"
              disabled={!implementationAuthorization?.enabled ||
                !bindingString(implementationAuthorization, "plan_hash")}
              onClick={() =>
                void invoke({
                  kind: "trip",
                  action: "authorize_implementation",
                  task_id: task.id,
                  attempt_id: attempt.id,
                  expected_task_version: task.version,
                  plan_hash: bindingString(
                    implementationAuthorization,
                    "plan_hash",
                  ),
                })}
            >
              Authorize implementation
            </button>
          )
          : (
            <button
              className="primary"
              disabled={plan && !attempt.plan}
              onClick={() => apply("accept")}
            >
              {plan ? "Approve plan" : "Accept result"}
            </button>
          )}
      </div>
      {migration && (
        <section className="review-row">
          <strong>Migrate this active attempt to the current workflow</strong>
          <span>{migration.reason}</span>
          <button
            disabled={!migration.enabled ||
              !bindingString(migration, "plan_hash") ||
              !bindingString(migration, "config_revision_id")}
            onClick={() =>
              void invoke({
                kind: "trip",
                action: "migrate_attempt",
                task_id: task.id,
                attempt_id: attempt.id,
                expected_task_version: task.version,
                reviewed_plan_hash: bindingString(migration, "plan_hash"),
                config_revision_id: bindingString(
                  migration,
                  "config_revision_id",
                ),
              })}
          >
            Migrate current attempt
          </button>
        </section>
      )}
      {additionalExplorer && (
        <section className="review-row">
          <strong>Authorize one additional Explorer call</strong>
          <span>{additionalExplorer.reason}</span>
          <textarea
            aria-label="Additional Explorer justification"
            maxLength={400}
            value={additionalJustification}
            onChange={(event) => setAdditionalJustification(event.target.value)}
            placeholder="Bounded justification for one rescue Explorer call"
          />
          <button
            disabled={!additionalExplorer.enabled ||
              !additionalJustification.trim()}
            onClick={() =>
              void invoke({
                kind: "trip",
                action: "authorize_additional_explorer",
                task_id: task.id,
                attempt_id: attempt.id,
                expected_task_version: task.version,
                stage: "rescue",
                justification: additionalJustification.trim(),
              })}
          >
            Authorize one rescue Explorer
          </button>
        </section>
      )}
      {error && <p className="error" role="alert">{error}</p>}
    </section>
  );
}
