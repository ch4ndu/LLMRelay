export function ActivityTimeline(
  { events }: { events: Array<Record<string, unknown>> },
) {
  return (
    <section className="panel timeline">
      <header>
        <h3>Activity</h3>
        <span>Authoritative events</span>
      </header>
      <ol>
        {events.slice(0, 40).map((event, index) => (
          <li key={String(event.id || index)}>
            <time>
              {new Date(String(event.created_at)).toLocaleTimeString([], {
                hour: "2-digit",
                minute: "2-digit",
              })}
            </time>
            <div>
              <strong>
                {String(event.event_code || "event").replaceAll(".", " · ")}
              </strong>
              <small>
                {String(event.actor_kind || "service")} ·{" "}
                {String(event.entity_id || "").slice(0, 12)}
              </small>
            </div>
          </li>
        ))}
      </ol>
      {!events.length && <p className="empty">No recorded activity.</p>}
    </section>
  );
}
