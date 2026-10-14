import { TechnicalDetails } from "./ErrorNotice";
import { type RefObject, useRef } from "react";
import { type ContinuationAction, type Task } from "../types";
import { useDialogFocus } from "../useDialogFocus";
import {
  currentPlanDecision,
  currentPlanView,
  type PlanView,
} from "./ReviewPanel";
import { MarkdownContent } from "./MarkdownContent";

/**
 * The complete plan the person chose to open, read-only. It shows only the
 * exact snapshot it was opened with; once the task's attempt, plan or content
 * revision moves on, the old text is withdrawn rather than shown under the
 * newer identity. A decision it was opened for is shown only while that same
 * decision still names this plan.
 */
export function PlanViewer(
  { view, task, actions, returnFocus, fallbackFocus, onReopen, onClose }: {
    view: PlanView;
    task: Task;
    actions: ContinuationAction[];
    returnFocus: RefObject<HTMLElement | null>;
    fallbackFocus: RefObject<HTMLElement | null>;
    onReopen: (view: PlanView) => void;
    onClose: () => void;
  },
) {
  const dialog = useRef<HTMLElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);
  const trapTab = useDialogFocus(
    dialog,
    closeButton,
    onClose,
    returnFocus,
    fallbackFocus,
  );
  const liveDecision = currentPlanDecision(task, actions);
  const decisionEnded = !!view.decision &&
    liveDecision?.phase !== view.decision.phase;
  const current = currentPlanView(
    task,
    view.decision ? liveDecision : undefined,
  );
  const unchanged = !!current && current.task_id === view.task_id &&
    current.attempt_id === view.attempt_id &&
    current.plan_hash === view.plan_hash &&
    current.content_revision === view.content_revision &&
    current.text === view.text;
  return (
    <div className="plan-viewer-backdrop">
      <section
        ref={dialog}
        className="plan-viewer"
        role="dialog"
        aria-modal="true"
        aria-labelledby="plan-viewer-title"
        tabIndex={-1}
        onKeyDown={trapTab}
      >
        <header className="plan-viewer-header">
          <div>
            <span className="eyebrow">Plan · read only</span>
            <h2 id="plan-viewer-title">{task.title}</h2>
          </div>
          <button
            ref={closeButton}
            className="icon-button plan-viewer-close"
            aria-label="Close plan"
            onClick={onClose}
          >
            <span aria-hidden="true">×</span>
          </button>
        </header>
        <div className="plan-viewer-body">
          {unchanged
            ? (
              <>
                {view.decision && !decisionEnded
                  ? (
                    <>
                      <p>
                        <strong>Opened for:</strong> {view.decision.title}
                      </p>
                      <p className="hint">
                        This is the complete plan that decision applies to. Your
                        decision covers the whole plan.
                      </p>
                    </>
                  )
                  : decisionEnded
                  ? (
                    <p className="warning" role="status">
                      The decision you opened this plan for is no longer waiting
                      for you, so it is shown only as the task's current plan.
                      Check Your decision for what the task needs now.
                    </p>
                  )
                  : (
                    <p className="hint">
                      This is the task's current plan, shown as context. It is
                      not linked to any particular report or question; only an
                      agent's own words say which plan or part of it they mean.
                    </p>
                  )}
                <MarkdownContent className="plan-content" text={view.text} />
              </>
            )
            : (
              <div className="warning" role="status">
                <p>
                  This plan changed after you opened it, so the earlier text is
                  no longer shown. Open the current plan to read it.
                </p>
                {current && (
                  <button onClick={() => onReopen(current)}>
                    Show the current plan
                  </button>
                )}
              </div>
            )}
        </div>
        <footer className="plan-viewer-footer">
          <span className="hint">
            Reading the plan does not approve it. Decide in the task's Your
            decision panel.
          </span>
          <TechnicalDetails>
            <dl className="technical-list">
              <div>
                <dt>Task</dt>
                <dd>{view.task_id}</dd>
              </div>
              <div>
                <dt>Attempt</dt>
                <dd>{view.attempt_id}</dd>
              </div>
              <div>
                <dt>Plan</dt>
                <dd>{view.plan_hash ?? "not reported"}</dd>
              </div>
              <div>
                <dt>Content revision</dt>
                <dd>{view.content_revision ?? "not reported"}</dd>
              </div>
            </dl>
          </TechnicalDetails>
        </footer>
      </section>
    </div>
  );
}
