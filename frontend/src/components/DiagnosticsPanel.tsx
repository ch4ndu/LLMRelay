import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { useEffect, useState } from "react";
import { diagnostics } from "../api";
import type {
  AttentionItem,
  CapabilityEvidence,
  TripSetupState,
} from "../types";
import { roleLabel } from "../types";
import { CompatibilityDetails } from "./RoleSettings";

const providerName = (provider?: string) =>
  provider === "claude" ? "Claude" : provider === "codex" ? "Codex" : provider || "Agent";

const verificationLabel = (status: string) =>
  status === "supported"
    ? "Verified"
    : status === "unsupported"
    ? "Not supported"
    : "Not verified yet";

/** Plain wording for recorded service events; anything else stays generic. */
function eventSummary(event: Record<string, unknown>) {
  const code = String(event.event_code || event.code || "");
  const outcome = String(event.outcome || "");
  if (code === "coordinator.tick" && outcome === "deferred") {
    return {
      title: "A workflow step failed and is being retried",
      impact: "Waiting work does not advance until the step succeeds.",
    };
  }
  if (code === "coordinator.tick") {
    return { title: "A workflow step completed", impact: "" };
  }
  if (code === "recipe.intake" && outcome === "deferred") {
    return {
      title: "A scheduled recipe could not create its draft yet",
      impact: "LLMRelay tries again at the next check.",
    };
  }
  return {
    title: outcome ? `Service event: ${outcome.replaceAll("_", " ")}` : "Service event",
    impact: "",
  };
}

export function DiagnosticsPanel(
  { capabilities = [], setups = [], attention = [] }: {
    capabilities?: CapabilityEvidence[];
    setups?: TripSetupState[];
    /** The current attention list; its paused-service items are shown here. */
    attention?: AttentionItem[];
  },
) {
  // Taken from the service's current state, so a paused step is shown even
  // when its best-effort diagnostic log entry could not be written.
  const paused = attention.filter((item) =>
    item.id === "coordinator_deferred" ||
    item.id.startsWith("coordinator_deferred:")
  );
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
          <p>
            Recent service events, agent verification status and anything that
            is stopping work from advancing.
          </p>
        </div>
      </header>
      {error && <ErrorNotice error={error} />}
      <section className="panel diagnostics-current" aria-labelledby="diagnostics-current-title">
        <h3 id="diagnostics-current-title">Right now</h3>
        {paused.length
          ? paused.map((item) => (
            <article key={item.id} data-diagnostic-id={item.id}>
              <strong>{item.title}</strong>
              {item.task_title && <small>{item.task_title}</small>}
              <p>{item.reason}</p>
              {item.details && (
                <TechnicalDetails>
                  <pre>{item.details}</pre>
                </TechnicalDetails>
              )}
            </article>
          ))
          : <p className="empty">No workflow step is failing right now.</p>}
      </section>
      <div className="diagnostic-list">
        {events.map((event, index) => {
          const summary = eventSummary(event);
          return (
            <article key={index}>
              <time>{String(event.timestamp || event.created_at || "")}</time>
              <strong>{summary.title}</strong>
              {summary.impact && <p>{summary.impact}</p>}
              <TechnicalDetails>
                <pre>
                  {String(event.event_code || event.code || "event")}
                  {"\n"}
                  {JSON.stringify(event.detail || event, null, 2)}
                </pre>
              </TechnicalDetails>
            </article>
          );
        })}
      </div>
      <section className="panel">
        <h3>Agent verification</h3>
        {capabilities.filter((item) => item.compatibility).map((item, index) => (
          <div key={`${item.provider}-${item.role}-${index}`}>
            <strong>
              {providerName(item.provider)} · {roleLabel(item.role)} ·{" "}
              {verificationLabel(item.status)}
            </strong>
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
              <strong>
                {providerName(selection.profile?.provider)} · {roleLabel(selection.role)} ·{" "}
                Not verified yet
              </strong>
              <CompatibilityDetails compatibility={selection.compatibility} />
            </div>
          )))}
        <p>
          To verify an agent, use Project setup or the task's Agent settings.
          They show how many agent calls the check will make and wait for your
          approval before starting it in a separate throwaway workspace. You
          can follow it in Workspace and the approvals list. After it passes
          and the agent has stopped, you choose whether to use the result.
          Setup results are kept as history and do not approve task work.
        </p>
        <p className="hint">
          The command-line verification commands are for troubleshooting. Use
          the dashboard for new setups and task agent changes so each check
          stays tied to the exact project, task and agent it verifies.
        </p>
      </section>
    </section>
  );
}
