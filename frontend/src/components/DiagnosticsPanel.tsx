import { useEffect, useState } from "react";
import { diagnostics } from "../api";
import type { CapabilityEvidence, TripSetupState } from "../types";
import { CompatibilityDetails } from "./RoleSettings";
export function DiagnosticsPanel({ capabilities = [], setups = [] }: {
  capabilities?: CapabilityEvidence[];
  setups?: TripSetupState[];
}) {
  const [events, setEvents] = useState<Array<Record<string, unknown>>>([]);
  const [error, setError] = useState("");
  useEffect(() => {
    diagnostics().then((v) => setEvents(v.events)).catch((e) =>
      setError(e instanceof Error ? e.message : String(e))
    );
  }, []);
  return (
    <section className="diagnostics">
      <header className="page-heading">
        <div>
          <span className="eyebrow">Local service</span>
          <h1>Diagnostics and setup</h1>
          <p>Sanitized local events, capability state, and recovery signals.</p>
        </div>
      </header>
      {error && <p className="error">{error}</p>}
      <div className="diagnostic-list">
        {events.map((event, index) => (
          <article key={index}>
            <time>{String(event.timestamp || event.created_at || "")}</time>
            <strong>{String(event.event_code || event.code || "event")}</strong>
            <pre>{JSON.stringify(event.detail||event,null,2)}</pre>
          </article>
        ))}
      </div>
      <section className="panel">
        <h3>Runtime capability validation</h3>
        {capabilities.filter((item) => item.compatibility).map((item, index) => (
          <div key={`${item.provider}-${item.role}-${index}`}>
            <strong>{item.provider} · {item.role} · Production {item.status}</strong>
            <CompatibilityDetails compatibility={item.compatibility} />
          </div>
        ))}
        {setups.filter((setup, index) => setup.state !== "superseded" &&
          setup.state !== "aborted" && setups.findIndex((item) =>
            item.project_id === setup.project_id) === index)
          .flatMap((setup) => setup.selected_profiles
          .filter((selection) => selection.selection_state === "selected" &&
            selection.compatibility && !capabilities.some((item) =>
              item.provider === selection.profile?.provider && item.role === selection.role))
          .map((selection) => (
            <div key={`${setup.setup_operation_id}-${selection.role}`}>
              <strong>{selection.profile?.provider} · {selection.role} · Production unverified</strong>
              <CompatibilityDetails compatibility={selection.compatibility} />
            </div>
          )))}
        <p>
          Project Setup and task Role Settings prepare an exact scoped runtime
          admission, display the fresh-call count, and require separate human
          authorization before launching app-owned disposable-worktree probes.
          Progress stays visible in the Workspace terminal and permission inbox;
          proof publication is a second explicit action after structured
          evidence and quiescence. Installation receipts remain historical and
          never become ordinary task authority.
        </p>
        <p className="hint">
          Advanced capability CLI commands remain diagnostic interfaces. New
          installations and task profile changes use the dashboard runtime
          action so fixture, role, profile, generation, and proof identity stay
          service-bound.
        </p>
      </section>
    </section>
  );
}
