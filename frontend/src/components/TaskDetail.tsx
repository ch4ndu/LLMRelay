import { useRef, useState } from "react";
import { ApiError, command, operation, reuseOperationIdentity } from "../api";
import type { AppState, Task } from "../types";
import { WorkflowControls } from "./WorkflowControls";
import { ReviewPanel } from "./ReviewPanel";
import { RoleSettings } from "./RoleSettings";
import { RecoveryPanel } from "./RecoveryPanel";
import { WorkSummary } from "./WorkSummary";
import { ServiceCheckPermissionActions } from "./ApprovalInbox";

export function TaskDetail(
  {
    task,
    state,
    selectedRecoveryId,
    onClose,
    onChanged,
    onOpenSetup = () => {},
  }: {
    task: Task;
    state: AppState;
    selectedRecoveryId?: string;
    onClose: () => void;
    onChanged: () => void;
    onOpenSetup?: (projectId: string) => void;
  },
) {
  const [dependency, setDependency] = useState("");
  const [integrationRef, setIntegrationRef] = useState("");
  const [error, setError] = useState("");
  const operationStorageKey = `llmrelay.task.operations.${task.id}`;
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
  const attempt = task.active_attempt;
  const project = state.projects.find((item) => item.id === task.project_id);
  const terminal = task.archived ||
    task.lifecycle === "done" || task.lifecycle === "cancelled";
  const apply = async (body: Record<string, unknown>) => {
    const key = body.kind === "trip"
      ? `${String(body.action)}:${
        String(body.attempt_id || body.task_id || "")
      }:${String(body.check_id || "")}`
      : body.kind === "archive" ? `archive:${task.id}:${task.version}` : "";
    const prior = key ? commandIdentities.current.get(key) : undefined;
    const stable = reuseOperationIdentity(prior, body);
    const { id, request } = stable;
    if (key) {
      commandIdentities.current.set(key, {
        body: JSON.stringify(request),
        id,
      });
    }
    if (key) {
      localStorage.setItem(
        operationStorageKey,
        JSON.stringify([...commandIdentities.current]),
      );
    }
    try {
      await command({ ...request, operation_id: id } as never);
      if (key) commandIdentities.current.delete(key);
      if (key) {
        localStorage.setItem(
          operationStorageKey,
          JSON.stringify([...commandIdentities.current]),
        );
      }
      setError("");
      onChanged();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  const addDependency = () =>
    apply({
      kind: "add_dependency",
      task_id: task.id,
      depends_on_task_id: dependency,
      expected_version: task.version,
    });
  const checks = state.checks.filter((value) =>
    value.attempt_id === attempt?.id
  );
  const selectedChecks = (state.trip_task_verification || []).filter((value) =>
    value.attempt_id === attempt?.id
  );
  const historicalSuites = state.check_suites.filter((value) =>
    value.project_id === task.project_id
  );
  const tripChecks = state.trip_checks || [];
  const tripExplorer = state.trip_explorer || [];
  const tripLanes = state.trip_lanes || [];
  const continuationActions = state.continuation_actions || [];
  const taskDecisions = state.decisions.filter((decision) =>
    decision.subject.task_id === task.id
  );
  const workflowDecision = taskDecisions.find((decision) =>
    !decision.reason_code.startsWith("restart.")
  );
  const restoreDecision = state.decisions.find((decision) =>
    decision.subject.recovery_id === "database-restore-hold"
  );
  const affectedByRestore = restoreDecision?.prerequisites.some((item) =>
    typeof item.evidence === "object" && item.evidence !== null &&
    "task_id" in item.evidence && item.evidence.task_id === task.id
  );
  const proposals = state.controls.filter((value) =>
    value.attempt_id === attempt?.id && value.kind === "transition_proposal"
  );

  return (
    <aside
      className="detail"
      data-attention-target={`task:${task.id}`}
      tabIndex={-1}
    >
      <header>
        <div>
          <span className="eyebrow">{task.id}</span>
          <h2>{task.title}</h2>
          <p>{task.description}</p>
        </div>
        <button
          className="icon"
          aria-label="Close task details"
          onClick={onClose}
        >
          ×
        </button>
      </header>
      <div className="detail-scroll">
        {task.recipe_provenance && <section className="panel">
          <h3>Recipe provenance</h3>
          <p>{task.recipe_provenance.recipe_name} · recipe revision {task.recipe_provenance.recipe_revision}</p>
          <small>Exact recipe revision {task.recipe_provenance.recipe_revision_id} · profile revision {task.recipe_provenance.profile_revision_id}</small>
          {task.recipe_provenance.schedule_id && <p>Created by schedule {task.recipe_provenance.schedule_id} at {task.recipe_provenance.scheduled_for_utc}.</p>}
          <p className="hint">If these pins are stale, save a current profile set and recipe, archive this draft, then create a fresh draft. Restore keeps the original pins.</p>
        </section>}
        {!task.archived && (
          <section className="panel">
            <h3>Archive task</h3>
            <p className="hint">
              {task.can_archive
                ? "This task can be restored from History later."
                : "Only completed tasks and backlog drafts with no attempts or Ready history can be archived."}
            </p>
            <button
              disabled={!task.can_archive}
              onClick={() => apply({
                kind: "archive",
                task_id: task.id,
                expected_version: task.version,
              })}
            >Archive</button>
          </section>
        )}
        {affectedByRestore && (
          <p className="warning">
            This task is held by the instance restore fence. See the single
            restore hold explanation in the attention inbox.
          </p>
        )}
        {taskDecisions.map((decision, index) => (
          <section
            className="panel decision-explanation"
            key={`${decision.reason_code}:${index}`}
          >
            <h3>
              {decision.disposition.replaceAll("_", " ")} ·{" "}
              {decision.reason_code.replaceAll("_", " ")}
            </h3>
            <p>
              {decision.primary_blocker?.message ||
                "Current recorded prerequisites are shown below."}
            </p>
            <small>
              Owner: {decision.ownership.owner} ·{" "}
              {decision.ownership.state.replaceAll("_", " ")}
            </small>
            {decision.prerequisites.map((item, itemIndex) => (
              <small key={`${item.code}:${itemIndex}`}>
                {item.state}: {item.message || item.code}
              </small>
            ))}
            {decision.next_action && (
              <small>
                Next action:{" "}
                {decision.next_action.operation.replaceAll("_", " ")}
                {decision.next_action.enabled
                  ? " (available after current revalidation)"
                  : " (unavailable)"}
              </small>
            )}
          </section>
        ))}
        <ReviewPanel
          task={task}
          verification={selectedChecks}
          actions={continuationActions}
          onChanged={onChanged}
        />
        <WorkflowControls
          task={task}
          project={project}
          controls={state.controls}
          sessions={state.active_sessions}
          switches={state.switches}
          actions={continuationActions}
          decision={workflowDecision}
          onOpenSetup={onOpenSetup}
          onChanged={onChanged}
        />
        {proposals.map((proposal) => (
          <section className="panel" key={String(proposal.id)}>
            <h3>Manager transition proposal</h3>
            <pre>{JSON.stringify(proposal.payload, null, 2)}</pre>
            {!terminal && (
              <button
                onClick={() =>
                  apply({
                    kind: "apply_transition",
                    task_id: task.id,
                    proposal_id: proposal.id,
                    expected_version: task.version,
                  })}
              >
                Apply proposed transition
              </button>
            )}
          </section>
        ))}
        <WorkSummary task={task} checks={checks} />
        <section className="panel">
          <h3>TRIP Verification</h3>
          <p className="hint">
            Selection revision{" "}
            {attempt?.selected_checks_revision || 0}. Commands are inherited
            from the activated project, selected by the task manager, and bound
            by the backend to the exact candidate, working directory, and
            authorization hashes.
          </p>
          {selectedChecks.map((selection) => {
            const check = tripChecks.find((item) =>
              item.id === selection.check_id
            );
            const authorized = selection.authorization.authorized;
            const actionable =
              selection.authorization.action_state !== "inactive";
            return (
              <div
                className="verification-row"
                key={`${selection.check_id}:${selection.selected_revision}`}
              >
                <div>
                  <strong>{check?.check_key || selection.check_id}</strong>
                  <small>{check?.category} · {check?.original_text}</small>
                </div>
                <span
                  className={`badge ${
                    selection.latest_run?.freshness_state === "current"
                      ? "supported"
                      : "waiting"
                  }`}
                >
                  {selection.latest_run
                    ? `${selection.latest_run.status} · ${selection.latest_run.freshness_state}`
                    : "not run"}
                </span>
                <small>
                  Relevant inputs:{" "}
                  {check?.relevant_inputs.join(", ") || "none declared"}
                </small>
                <small>
                  Coverage: {selection.latest_run?.acceptance_coverage.length ||
                    0}/{check?.acceptance_rows.length || 0} acceptance rows
                </small>
                {!terminal && (
                  <>
                    <ServiceCheckPermissionActions
                      selection={selection}
                      onDecision={(decision, lifetime) =>
                        apply({
                          kind: "trip",
                          action: "authorize_check",
                          attempt_id: selection.attempt_id,
                          check_id: selection.check_id,
                          selected_revision: selection.selected_revision,
                          exact_command_hash: selection.exact_command_hash,
                          scope_hash: selection.scope_hash,
                          decision,
                          lifetime,
                        })}
                      onRevoke={(rule_id, expected_revision) =>
                        apply({
                          kind: "trip",
                          action: "revoke_check_permission_rule",
                          rule_id,
                          expected_revision,
                        })}
                    />
                    <div className="button-row">
                      <button
                        disabled={!attempt ||
                          selection.attempt_id !== attempt.id ||
                          !authorized || !actionable}
                        onClick={() => {
                          const body = {
                            kind: "check_run",
                            attempt_id: attempt?.id,
                            check_id: selection.check_id,
                          };
                          const key =
                            `check_run:${body.attempt_id}:${body.check_id}`;
                          const stable = reuseOperationIdentity(
                            commandIdentities.current.get(key),
                            body,
                          );
                          const { id, request } = stable;
                          commandIdentities.current.set(key, {
                            body: JSON.stringify(request),
                            id,
                          });
                          localStorage.setItem(
                            operationStorageKey,
                            JSON.stringify([...commandIdentities.current]),
                          );
                          void operation(
                            { ...request, operation_id: id } as never,
                          ).then(() => {
                            commandIdentities.current.delete(key);
                            localStorage.setItem(
                              operationStorageKey,
                              JSON.stringify([...commandIdentities.current]),
                            );
                            onChanged();
                          }).catch((cause) => {
                            if (
                              !(cause instanceof ApiError && cause.ambiguous)
                            ) {
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
                          });
                        }}
                      >
                        {selection.authorization.action_state ===
                            "current_receipt"
                          ? "Rerun approved check"
                          : "Run approved check"}
                      </button>
                    </div>
                  </>
                )}
              </div>
            );
          })}
          {!selectedChecks.length && (
            <p className="empty">
              No task verification matrix is selected. Verified completion
              remains blocked when applicable evidence is missing.
            </p>
          )}
          {!!historicalSuites.length && (
            <>
              <h4>Historical check results</h4>
              {historicalSuites.map((suite) => {
                const runs = checks.filter((check) =>
                  check.suite_name === suite.name || check.suite_id === suite.id
                );
                return (
                  <article className="review-row" key={String(suite.id)}>
                    <strong>{String(suite.name)}</strong>
                    {runs.length
                      ? runs.map((run) => (
                        <div key={String(run.id)}>
                          <span>{String(run.status)}</span>
                          {Boolean(run.evidence) && (
                            <pre>{JSON.stringify(run.evidence, null, 2)}</pre>
                          )}
                        </div>
                      ))
                      : <span>No recorded result</span>}
                  </article>
                );
              })}
            </>
          )}
        </section>
        <section className="panel">
          <h3>Explorer and implementation lanes</h3>
          {tripExplorer.filter((item) => item.attempt_id === attempt?.id).map((
            decision,
          ) => (
            <article className="review-row" key={decision.id}>
              <strong>Explorer · {decision.stage}</strong>
              <span>
                {decision.activated
                  ? decision.outcome
                    ? "evidence recorded"
                    : "activated, evidence pending"
                  : "not invoked"} · {decision.trigger}
              </span>
              <small>
                Budget and census: {JSON.stringify({
                  limits: decision.limits,
                  census: decision.census,
                })}
              </small>
              <small>
                Explorer evidence is non-authoritative and never supplies plan,
                implementation, review, or human approval.
              </small>
            </article>
          ))}
          {tripLanes.filter((item) => item.attempt_id === attempt?.id).map((
            lane,
          ) => (
            <article className="review-row" key={lane.id}>
              <strong>Lane {lane.lane_key} · {lane.state}</strong>
              <span>Owned: {lane.owned_paths.join(", ")}</span>
              <small>
                Shared: {lane.shared_paths.join(", ") || "none"} · protected:
                {" "}
                {lane.protected_paths.join(", ") || "none"} · dependencies:{" "}
                {lane.dependencies.join(", ") || "none"} · effective generation
                {" "}
                {lane.effective_generation_id?.slice(0, 8) || "pending"}
              </small>
            </article>
          ))}
          {!tripLanes.some((item) => item.attempt_id === attempt?.id) && (
            <p className="hint">
              No explicit implementation lanes are admitted. Reviewed parallel
              lanes appear after manager admission with frozen source and seam
              bindings.
            </p>
          )}
        </section>
        <RoleSettings
          task={task}
          project={project}
          sessions={state.active_sessions}
          switches={state.switches}
          lanes={tripLanes}
          productionRestrictions={state.production_role_restrictions}
          capabilities={state.capabilities}
          actions={continuationActions}
          onChanged={onChanged}
        />
        <RecoveryPanel
          task={task}
          records={state.recovery.filter((value) =>
            value.attempt_id === attempt?.id &&
            value.state === "attention_required"
          )}
          selectedRecordId={selectedRecoveryId}
          onChanged={onChanged}
        />
        <section className="panel">
          <h3>Dependencies</h3>
          {task.dependencies.map((item, index) => (
            <div className="review-row" key={index}>
              <strong>{String(item.task_id)}</strong>
              <span>
                {item.verified_at
                  ? `Integrated at ${String(item.integration_ref)}`
                  : "Waiting for integration"}
              </span>
              {!terminal && !item.verified_at && (
                <div className="compact-fields">
                  <input
                    aria-label={`Integration ref for ${String(item.task_id)}`}
                    value={integrationRef}
                    onChange={(event) =>
                      setIntegrationRef(event.target.value)}
                    placeholder="Exact integrated git ref"
                  />
                  <button
                    disabled={!integrationRef}
                    onClick={() =>
                      apply({
                        kind: "record_integration",
                        task_id: task.id,
                        depends_on_task_id: item.task_id,
                        git_ref: integrationRef,
                        expected_version: task.version,
                      })}
                  >
                    Record
                  </button>
                </div>
              )}
            </div>
          ))}
          {!terminal && (
            <div className="compact-fields">
              <select
                aria-label="Dependency task"
                value={dependency}
                onChange={(event) => setDependency(event.target.value)}
              >
                <option value="">Choose same-project task</option>
                {state.tasks.filter((value) =>
                  value.id !== task.id && value.project_id === task.project_id
                ).map((value) => (
                  <option value={value.id} key={value.id}>
                    {value.id} · {value.title}
                  </option>
                ))}
              </select>
              <button disabled={!dependency} onClick={addDependency}>
                Add
              </button>
            </div>
          )}
        </section>
        <section className="panel">
          <h3>Review history and budgets</h3>
          {task.review_budgets.filter((budget) =>
            budget.attempt_id === attempt?.id
          ).map((budget) => (
            <article className="review-row" key={budget.id}>
              <strong>{budget.kind} review</strong>
              <span>{budget.spent} spent · {budget.remaining} remaining</span>
              {!terminal && budget.remaining === 0 && (
                <small className="hint">
                  Review allowance is exhausted. The dashboard cannot extend it.
                </small>
              )}
            </article>
          ))}
          {task.reviews.map((review) => (
            <article className="review-row" key={review.id}>
              <strong>{review.kind} · {review.delivery_state}</strong>
              <span>{review.verdict || "Pending"}</span>
              {review.feedback && <p>{review.feedback}</p>}
            </article>
          ))}
        </section>
        {Object.keys(task.legacy || {}).length > 0 && (
          <details className="panel">
            <summary>Preserved legacy record</summary>
            <p>
              <strong>Source:</strong> {String(task.legacy.source || "")}
            </p>
            <p>
              <strong>Hash:</strong>{" "}
              <code>{String(task.legacy.source_hash || "")}</code>
            </p>
            <h4>Validation runs</h4>
            <pre>{JSON.stringify(task.legacy.validation_runs || [], null, 2)}</pre>
            <h4>Validation bugs</h4>
            <pre>{JSON.stringify(task.legacy.validation_bugs || [], null, 2)}</pre>
            <h4>Sections and unknown fields</h4>
            <pre>{JSON.stringify({ frontmatter: task.legacy.frontmatter, unknown_frontmatter: task.legacy.unknown_frontmatter, sections: task.legacy.sections }, null, 2)}</pre>
            <h4>Original source</h4>
            <pre>{String(task.legacy.source_text || "")}</pre>
          </details>
        )}
        {error && <p className="error" role="alert">{error}</p>}
      </div>
    </aside>
  );
}
