import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { MarkdownContent } from "./MarkdownContent";
import { useRef, useState } from "react";
import {
  ApiError,
  command,
  operationIntent,
  reuseOperationIdentity,
} from "../api";
import type {
  AcceptanceEvidence,
  ContinuationAction,
  SingleAgentCheck,
  SingleAgentStage,
  Task,
  TripTaskVerification,
} from "../types";
import {
  UncertainRequestNotice,
  useWorkflowCommands,
} from "./WorkflowControls";

export interface PlanView {
  task_id: string;
  attempt_id: string;
  plan_hash: string | null;
  content_revision: string | null;
  text: string;
}

export function ReviewPanel(
  { task, verification = [], actions = [], onChanged, onOpenPlan }: {
    task: Task;
    verification?: TripTaskVerification[];
    actions?: ContinuationAction[];
    onChanged: () => void;
    /** Opens the complete plan read-only; it never decides anything. */
    onOpenPlan?: (plan: PlanView) => void;
  },
) {
  const [feedback, setFeedback] = useState("");
  const [carryApproval, setCarryApproval] = useState(false);
  const [additionalJustification, setAdditionalJustification] = useState("");
  const [error, setError] = useState("");
  const [pending, setPending] = useState(false);
  const [submittedDecision, setSubmittedDecision] = useState<string | null>(null);
  const inFlight = useRef(false);
  const attempt = task.active_attempt;
  const terminal = ["done", "cancelled"].includes(task.lifecycle) ||
    ["done", "cancelled"].includes(attempt?.status ?? "");
  const reviewStage = JSON.stringify([task.id, attempt?.id, attempt?.phase]);
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
    if (terminal || inFlight.current || submittedDecision === reviewStage) {
      return false;
    }
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
    inFlight.current = true;
    setPending(true);
    try {
      await command({ ...request, operation_id: id } as never);
      commandIdentities.current.delete(key);
      localStorage.setItem(
        operationStorageKey,
        JSON.stringify([...commandIdentities.current]),
      );
      setError("");
      if (body.kind === "human_review" || body.kind === "approve_plan") {
        setSubmittedDecision(reviewStage);
      }
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
    } finally {
      inFlight.current = false;
      setPending(false);
    }
  };
  if (task.workflow?.family === "single_agent") {
    return <SingleAgentRunPanel task={task} onChanged={onChanged} />;
  }
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
    terminal || submittedDecision === reviewStage || !attempt ||
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
    <section
      className="panel review-panel"
      data-attention-target={`attempt:${attempt.id}`}
      tabIndex={-1}
    >
      <span className="eyebrow">Your decision</span>
      <h3>
        {implementation
          ? "Allow implementation of this plan"
          : plan
          ? "Approve the reviewed plan"
          : "Accept the reviewed result"}
      </h3>
      <p>
        {implementation
          ? "Approving the plan and allowing implementation are separate decisions. This allows implementation of this exact plan only."
          : plan
          ? "The plan passed its independent review. Read it before approving."
          : "The result passed its selected checks and final review."}
      </p>
      {!plan && !implementation && (
        <p className="hint">
          {verification.filter((item) =>
            item.latest_run?.freshness_state === "current" &&
            item.latest_run.exit_code === 0
          ).length} of {verification.length}{" "}
          selected checks passed on this result. Accepting it is your decision.
        </p>
      )}
      {(plan || implementation) && (
        attempt.plan
          ? (
            <>
              {onOpenPlan && (
                <div className="plan-preview-actions">
                  <button
                    type="button"
                    className="plan-open"
                    onClick={() =>
                      attempt.plan && onOpenPlan({
                        task_id: task.id,
                        attempt_id: attempt.id,
                        plan_hash: attempt.plan_hash ?? null,
                        content_revision: attempt.content_revision ?? null,
                        text: attempt.plan,
                      })}
                  >
                    Open plan
                  </button>
                </div>
              )}
              <MarkdownContent className="plan-content" text={attempt.plan} />
            </>
          )
          : (
            <p className="warning">
              The plan text is not available, so it cannot be approved. Refresh
              the dashboard; if it stays missing, check Technical details.
            </p>
          )
      )}
      <TechnicalDetails>
        <p>
          {plan || implementation
            ? `Plan ${attempt.plan_hash || "unavailable"}`
            : `Candidate ${attempt.candidate_hash || "unavailable"} · manager conformance revision ${attempt.manager_conformance_revision ?? "none"}`}
        </p>
      </TechnicalDetails>
      {!plan && (
        <>
          <textarea
            aria-label="Review or rework feedback"
            disabled={pending}
            placeholder="Describe what should change (required to request rework)"
            value={feedback}
            onChange={(event) => setFeedback(event.target.value)}
          />
          <label className="toggle">
            <input
              type="checkbox"
              disabled={pending}
              checked={carryApproval}
              onChange={(event) => setCarryApproval(event.target.checked)}
            />Keep the approved plan for the rework (only when the task scope
            and agent settings are unchanged)
          </label>
        </>
      )}
      <div className="button-row">
        {!plan && !implementation && (
          <button
            disabled={pending || !feedback.trim()}
            onClick={() => apply("request_changes")}
          >
            Request rework
          </button>
        )}
        {implementation
          ? (
            <button
              className="primary"
              disabled={pending || !implementationAuthorization?.enabled ||
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
              Allow implementation
            </button>
          )
          : (
            <button
              className="primary"
              disabled={pending || (plan && !attempt.plan)}
              onClick={() => apply("accept")}
            >
              {plan ? "Approve plan" : "Accept result"}
            </button>
          )}
      </div>
      {migration && (
        <section className="review-row">
          <strong>Move this task to the current workflow</strong>
          <span>{migration.reason}</span>
          <button
            disabled={pending || !migration.enabled ||
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
            Move to current workflow
          </button>
        </section>
      )}
      {additionalExplorer && (
        <section className="review-row">
          <strong>Approve one more Explorer call</strong>
          <span>{additionalExplorer.reason}</span>
          <textarea
            aria-label="Additional Explorer justification"
            disabled={pending}
            maxLength={400}
            value={additionalJustification}
            onChange={(event) => setAdditionalJustification(event.target.value)}
            placeholder="Why one more Explorer call is needed"
          />
          <button
            disabled={pending || !additionalExplorer.enabled ||
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
            Approve one more Explorer call
          </button>
        </section>
      )}
      {error && <ErrorNotice error={error} />}
    </section>
  );
}

const stageHeadings: Record<SingleAgentStage, string> = {
  waiting_to_start: "Waiting to start",
  writing: "The agent is working",
  result_submitted_waiting_for_stop: "Result submitted; capturing the work",
  awaiting_selected_checks: "Waiting for the selected checks",
  awaiting_human: "Review the result",
  accepted: "Result accepted",
  probe_evidence_recorded: "Profile probe finished",
  changes_requested: "Changes requested",
  needs_action: "Needs your decision",
  blocked: "Blocked",
};

const evidenceLabels: Record<AcceptanceEvidence["kind"], string> = {
  source: "In the changes",
  check: "Check",
  manual: "Manual",
  not_run: "Not run",
};

// Arguments are shown exactly; one containing spaces or quotes is quoted so
// the displayed command cannot be misread as different arguments.
const commandText = (check: SingleAgentCheck) =>
  check.command_kind === "exact_shell"
    ? check.shell_command ?? ""
    : [check.executable ?? "", ...check.arguments].map((part) =>
      /[\s"'\\]/.test(part) ? JSON.stringify(part) : part
    ).join(" ");

const checkRunLabel = (check: SingleAgentCheck) =>
  check.latest_run_passed === true
    ? "Passed"
    : check.latest_run_passed === false && check.latest_run_status === "finished"
    ? "Failed"
    : check.latest_run_status
    ? check.latest_run_status.replaceAll("_", " ")
    : "Not run yet";

/**
 * A Single agent run's current stage, structured result and the decisions
 * the service currently offers for it. Every decision repeats the exact
 * result, candidate and task version shown here; provider prose or an agent
 * exit never completes the task.
 */
function SingleAgentRunPanel(
  { task, onChanged }: { task: Task; onChanged: () => void },
) {
  const commands = useWorkflowCommands(
    `llmrelay.workflow.commands.${task.id}`,
    onChanged,
  );
  const [feedback, setFeedback] = useState("");
  const [resumeMessage, setResumeMessage] = useState("");
  const [decided, setDecided] = useState("");
  const run = task.workflow?.run ?? null;
  if (!run) return null;
  const result = run.result;
  const offered = new Set(run.available_actions);
  const terminal = task.archived ||
    ["done", "cancelled"].includes(task.lifecycle);
  const candidate = result?.candidate_hash ?? null;
  const decisionKey = result ? `${run.attempt_id}:${result.id}` : "";
  const guidanceHash = run.guidance_changes.length
    ? run.guidance_change_hash
    : null;
  const guidanceBlocked = run.guidance_changes.length > 0 && !guidanceHash;
  const decidable = !terminal && !!result && !!candidate &&
    decided !== decisionKey;
  const accept = async () => {
    if (!result || !candidate || guidanceBlocked) return;
    const outcome = await commands.submit(
      `accept:${decisionKey}`,
      (operation_id) => ({
        kind: "accept_result",
        operation_id,
        task_id: task.id,
        attempt_id: run.attempt_id,
        expected_version: task.version,
        result_binding_id: result.id,
        candidate_hash: candidate,
        guidance_change_hash: guidanceHash,
      }),
    );
    if (outcome) setDecided(decisionKey);
  };
  const requestChanges = async () => {
    if (!result || !candidate || !feedback.trim()) return;
    const outcome = await commands.submit(
      `rework:${decisionKey}`,
      (operation_id) => ({
        kind: "request_rework",
        operation_id,
        task_id: task.id,
        attempt_id: run.attempt_id,
        expected_version: task.version,
        result_binding_id: result.id,
        candidate_hash: candidate,
        feedback: feedback.trim(),
      }),
    );
    if (outcome) {
      setDecided(decisionKey);
      setFeedback("");
    }
  };
  const resumeKey = run.resume
    ? `resume:${run.attempt_id}:${run.resume.session_id}:${run.resume.transcript_epoch}`
    : "";
  // Resuming launches the writer, so while an earlier resume's outcome is
  // unknown its message stays fixed and a retry replays the same request.
  const resumeUncertain = commands.uncertain.has(resumeKey);
  const resume = async () => {
    const writer = run.resume;
    if (!writer) return;
    const outcome = await commands.submit(
      resumeKey,
      (operation_id) => ({
        kind: "resume_run",
        operation_id,
        task_id: task.id,
        attempt_id: run.attempt_id,
        expected_version: task.version,
        session_id: writer.session_id,
        transcript_epoch: writer.transcript_epoch,
        message: resumeMessage.trim() || null,
      }),
    );
    if (outcome) setResumeMessage("");
  };
  const authorize = (
    check: SingleAgentCheck,
    decision: "authorized" | "denied",
    lifetime: "once" | "run",
  ) =>
    void commands.submit(
      `check:${run.attempt_id}:${check.check_id}:${check.exact_command_hash}:${check.scope_hash}`,
      (operation_id) => ({
        kind: "authorize_check",
        operation_id,
        task_id: task.id,
        attempt_id: run.attempt_id,
        check_id: check.check_id,
        exact_command_hash: check.exact_command_hash,
        scope_hash: check.scope_hash,
        decision,
        lifetime,
      }),
    );
  const uncertain = [...commands.uncertain].some((key) =>
    key.includes(run.attempt_id)
  );
  const changedSince = [...commands.reviewed].some((key) =>
    key.includes(run.attempt_id)
  );
  return (
    <section
      className="panel review-panel single-agent-run"
      data-attention-target={`attempt:${run.attempt_id}`}
      tabIndex={-1}
    >
      <span className="eyebrow">Single agent run</span>
      <h3>{stageHeadings[run.stage]}</h3>
      {run.parent_attempt_id && (
        <p className="hint">
          This run continues the task after you requested changes. The earlier
          result and its evidence stay recorded unchanged.
        </p>
      )}
      {run.probe_admission_id && (
        <p className="hint">
          This is a disposable probe of an agent profile. Its result is only
          evidence for that profile's admission; it cannot be accepted or sent
          back for changes.
        </p>
      )}
      {result
        ? (
          <div className="single-agent-result">
            <h4>The agent's result</h4>
            <MarkdownContent text={result.summary} />
            <h4>Acceptance criteria</h4>
            <ul className="acceptance-evidence">
              {result.acceptance_rows.map((row, index) => (
                <li key={`${index}:${row.criterion}`}>
                  <strong>{row.criterion}</strong>{" "}
                  <span
                    className={`badge ${
                      row.status === "met" ? "supported" : "danger"
                    }`}
                  >
                    {row.status === "met" ? "Reported met" : "Reported not met"}
                  </span>
                  <ul>
                    {(row.evidence ?? []).map((item, itemIndex) => (
                      <li key={itemIndex}>
                        <em>{evidenceLabels[item.kind] ?? item.kind}:</em>{" "}
                        {item.detail}
                      </li>
                    ))}
                  </ul>
                </li>
              ))}
            </ul>
            <p className="hint">
              These rows are the agent's own report. LLMRelay captured the files
              it changed as the result; only the selected checks and your review
              verify it.
            </p>
            <TechnicalDetails>
              <p>
                Result {result.id} · {result.state} · candidate{" "}
                {result.candidate_hash ?? "not captured yet"} · snapshot{" "}
                {result.snapshot_id ?? "none"} · check selection revision{" "}
                {result.selected_check_revision} · submitted {result.created_at}
              </p>
            </TechnicalDetails>
          </div>
        )
        : <p className="hint">The agent has not submitted a result yet.</p>}
      {run.checks.length > 0 && (
        <div className="single-agent-checks">
          <h4>Selected checks</h4>
          <p className="hint">
            Selected checks are required. Each runs only after you authorize
            its exact command, and only against the captured result. A failed
            check runs again only after you authorize it again.
          </p>
          {run.checks.map((check) => {
            const authorizable = !terminal &&
              offered.has("authorize_check") &&
              (check.authorization !== "authorized" ||
                check.latest_run_passed === false);
            return (
              <div className="verification-row" key={check.check_id}>
                <div>
                  <strong>{check.original_text || check.check_key}</strong>
                  <code>{commandText(check)}</code>
                  <small>Runs in {check.cwd}</small>
                </div>
                <span
                  className={`badge ${
                    check.latest_run_passed === true
                      ? "supported"
                      : check.latest_run_passed === false
                      ? "danger"
                      : "waiting"
                  }`}
                >
                  {checkRunLabel(check)}
                </span>
                <small>
                  {check.authorization === "authorized"
                    ? "You authorized this command."
                    : check.authorization === "denied"
                    ? "You denied this command."
                    : "Waiting for your authorization."}
                </small>
                {authorizable && (
                  <div className="button-row">
                    <button
                      disabled={commands.busy}
                      onClick={() => authorize(check, "authorized", "once")}
                    >
                      {check.latest_run_passed === false
                        ? "Allow one more run"
                        : "Allow once"}
                    </button>
                    {check.latest_run_passed !== false && (
                      <button
                        disabled={commands.busy}
                        onClick={() => authorize(check, "authorized", "run")}
                      >
                        Allow for this run
                      </button>
                    )}
                    <button
                      className="danger"
                      disabled={commands.busy}
                      onClick={() => authorize(check, "denied", "once")}
                    >
                      Deny
                    </button>
                  </div>
                )}
                <TechnicalDetails>
                  <p>
                    {check.check_key} · command {check.exact_command_hash} ·
                    scope {check.scope_hash}
                    {check.latest_run_id ? ` · run ${check.latest_run_id}` : ""}
                  </p>
                </TechnicalDetails>
              </div>
            );
          })}
        </div>
      )}
      {run.guidance_changes.length > 0 && (
        <div className="warning">
          <p>
            This result changes instruction files that agents read:{" "}
            {run.guidance_changes.map((change) => change.path).join(", ")}.
            Accepting the result also accepts these changes.
          </p>
          <TechnicalDetails>
            {run.guidance_changes.map((change) => (
              <p key={change.path}>
                {change.path} · base {change.base_revision} · approved{" "}
                {change.approved_sha256} · result{" "}
                {change.candidate_sha256 ?? "deleted or not a regular file"}
              </p>
            ))}
          </TechnicalDetails>
        </div>
      )}
      {decidable &&
        (offered.has("accept_result") || offered.has("request_rework")) && (
        <div className="single-agent-decision">
          {offered.has("request_rework") && (
            <>
              <textarea
                aria-label="Changes to request"
                disabled={commands.busy}
                placeholder="Describe what should change (required to request changes)"
                value={feedback}
                onChange={(event) => setFeedback(event.target.value)}
              />
              <p className="hint">
                Request changes starts a linked new run with a fresh agent
                conversation for the changes you describe. Check authorizations
                do not carry over to it.
              </p>
            </>
          )}
          <div className="button-row">
            {offered.has("request_rework") && (
              <button
                disabled={commands.busy || !feedback.trim()}
                onClick={() => void requestChanges()}
              >
                Request changes
              </button>
            )}
            {offered.has("accept_result") && (
              <button
                className="primary"
                disabled={commands.busy || guidanceBlocked}
                onClick={() => void accept()}
              >
                Accept result
              </button>
            )}
          </div>
          {offered.has("accept_result") && (
            <p className="hint">
              Accepting records your decision and completes the task. It does
              not merge or commit anything into your repository.
            </p>
          )}
        </div>
      )}
      {!terminal && run.stage === "awaiting_human" &&
        !offered.has("accept_result") && decided !== decisionKey && (
        <p className="hint">
          LLMRelay does not offer Accept result for this result right now.
          Refresh to see its current state.
        </p>
      )}
      {decided === decisionKey && decisionKey && !terminal && (
        <p role="status">
          Your decision was recorded. Refresh if this page has not updated yet.
        </p>
      )}
      {run.stage === "accepted" && (
        <p className="hint">
          {run.integration_recorded
            ? "Integration recorded: a task that depends on this one verified these accepted changes in its starting commit."
            : "Not integrated yet. After you merge the accepted changes yourself, record the integrated commit in the Changes tab of a task that depends on this one. Dependent tasks wait until then."}
        </p>
      )}
      {!terminal && run.resume && (
        offered.has("resume_run")
          ? (
            <div className="inline-form">
              <label>
                Message for the agent (optional)
                <textarea
                  disabled={commands.busy || resumeUncertain}
                  value={resumeMessage}
                  onChange={(event) => setResumeMessage(event.target.value)}
                />
              </label>
              <button disabled={commands.busy} onClick={() => void resume()}>
                Resume agent
              </button>
              <small>
                Continues this run's own agent conversation once. It never
                starts a different conversation in its place.
              </small>
            </div>
          )
          : ["needs_action", "blocked"].includes(run.stage) && (
            <div className="hint">
              Resume agent is not available for this run right now.
              {run.resume.reason && (
                <TechnicalDetails>
                  <p>{run.resume.reason}</p>
                </TechnicalDetails>
              )}
            </div>
          )
      )}
      {!terminal && offered.has("cancel") && (
        <p className="hint">
          To stop this run instead, choose Cancel in Controls. Its history is
          kept.
        </p>
      )}
      {uncertain && <UncertainRequestNotice changed={changedSince} />}
      {commands.error && <ErrorNotice error={commands.error} />}
    </section>
  );
}
