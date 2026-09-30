import type { Task } from "../types";

const eventLabels: Record<string, string> = {
  "attempt.phase.changed": "Moved to the next step",
  "attempt.attention.changed": "Waiting for you",
  "review.result.applied": "Review result recorded",
  "rework.lineage.created": "Rework requested",
  "control.applied": "Your control was applied",
  "human.command.applied": "Your change was applied",
  "trip.command.applied": "Your workflow decision was applied",
  "task.profile.activated": "Agent profile activated",
  "session.resume.rejected": "An agent session could not be resumed",
  "session.resume.fresh_route.reserved": "A fresh agent session was reserved",
  "session.graceful_stop.deadline_elapsed": "An agent did not stop in time",
  "workspace.reservation.recovery_required": "Workspace preparation needs recovery",
  "workspace.recovery.finalized": "Workspace recovery finished",
  "restart.hold.released": "Continued after restart",
  "coordinator.action.committed": "Workflow step started",
  "decision.explanation.changed": "Waiting reason updated",
  "legacy.imported": "Task imported",
};

const eventLabel = (code: string) =>
  eventLabels[code] ||
  code.replaceAll(".", " ").replaceAll("_", " ").replace(/^./, (first) =>
    first.toUpperCase()
  );

/** Recent recorded events, newest first, with plain names and task titles. */
export function ActivityTimeline(
  { events, tasks = [], limit = 40 }: {
    events: Array<Record<string, unknown>>;
    tasks?: Task[];
    limit?: number;
  },
) {
  const taskTitle = (event: Record<string, unknown>) =>
    event.entity_kind === "task"
      ? tasks.find((task) => task.id === event.entity_id)?.title
      : undefined;
  return (
    <section className="timeline" aria-label="Activity">
      <header>
        <h3>Activity</h3>
      </header>
      <ol>
        {events.slice(0, limit).map((event, index) => {
          const title = taskTitle(event);
          return (
            <li key={String(event.id || index)}>
              <time dateTime={String(event.created_at)}>
                {new Date(String(event.created_at)).toLocaleString([], {
                  month: "short",
                  day: "numeric",
                  hour: "2-digit",
                  minute: "2-digit",
                })}
              </time>
              <div>
                <strong>{eventLabel(String(event.event_code || "event"))}</strong>
                <small>
                  {title ? `${title} · ` : ""}
                  {event.actor_kind === "human" ? "You" : "LLMRelay"}
                </small>
              </div>
            </li>
          );
        })}
      </ol>
      {!events.length && <p className="empty">No recorded activity yet.</p>}
    </section>
  );
}
