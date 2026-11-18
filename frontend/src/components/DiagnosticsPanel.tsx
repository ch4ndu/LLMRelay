import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { diagnostics, diagnosticsRuntimeBindings } from "../api";
import { field } from "../apiValidation";
import type {
  AttentionItem,
  CapabilityEvidence,
  DiagnosticsRuntimeBinding,
  Project,
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

const noAttention: AttentionItem[] = [];

function isCurrentCapability(
  candidate: CapabilityEvidence,
  capabilities: CapabilityEvidence[],
) {
  // The snapshot can already contain a newer winner than the on-demand read saw.
  return candidate.status === "supported" && !capabilities.some((item) =>
    item !== candidate && item.provider === candidate.provider &&
    item.role === candidate.role && item.mode === candidate.mode &&
    item.version === candidate.version &&
    ((item.checked_at ?? "") > (candidate.checked_at ?? "") ||
      (item.checked_at === candidate.checked_at &&
        (item.rowid == null || candidate.rowid == null ||
          !Number.isSafeInteger(item.rowid) || !Number.isSafeInteger(candidate.rowid) ||
          item.rowid >= candidate.rowid)))
  );
}

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
  { capabilities = [], setups = [], projects = [], incarnation, attention = noAttention }: {
    capabilities?: CapabilityEvidence[];
    setups?: TripSetupState[];
    projects?: Project[];
    incarnation?: string;
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
  const [refreshVersion, setRefreshVersion] = useState(0);
  const runtimeRead = useRef<AbortController | undefined>(undefined);
  const refreshDiagnostics = useCallback(() => {
    runtimeRead.current?.abort();
    setRefreshVersion((version) => version + 1);
  }, []);
  useEffect(() => {
    const onVisible = () => {
      if (document.visibilityState === "visible") refreshDiagnostics();
    };
    window.addEventListener("focus", refreshDiagnostics);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      window.removeEventListener("focus", refreshDiagnostics);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [refreshDiagnostics]);
  // App supplies a fresh attention list with each accepted dashboard snapshot.
  useEffect(() => {
    let abandoned = false;
    void diagnostics().then((value) => {
      if (abandoned) return;
      setEvents(value.events);
      setError("");
    }).catch((cause: unknown) => {
      if (!abandoned) setError(cause instanceof Error ? cause.message : String(cause));
    });
    return () => { abandoned = true; };
  }, [attention, refreshVersion]);

  const currentSetups = setups.filter((setup) =>
    setup.state !== "superseded" && setup.state !== "aborted"
  );
  const projectsById = new Map(projects.map((project) => [project.id, project]));
  const selectedProfiles = currentSetups.flatMap((setup) => {
    const project = projectsById.get(setup.project_id);
    const trip = project?.trip;
    const selectedSetup = trip?.setup_operation_id ??
      currentSetups.find((item) => item.project_id === setup.project_id)?.setup_operation_id;
    if (setup.setup_operation_id !== selectedSetup) return [];
    return setup.selected_profiles.flatMap((selection) => {
      const { profile, role, profile_hash, current_adapter_hash } = selection;
      if (selection.selection_state !== "selected" || !profile) return [];
      const hasSetupProof = setup.probe_receipts.some((receipt) =>
        !!profile_hash && receipt.profile_hash === profile_hash &&
        !!current_adapter_hash && receipt.adapter_hash === current_adapter_hash &&
        receipt.role === role && receipt.provider === profile.provider &&
        receipt.result === "success" && !!receipt.capability_key
      );
      const revision = trip?.active_config_revision_id;
      const roleSettings = field(project?.settings.roles, role);
      const profileIsActive = field(roleSettings, "provider") === profile.provider &&
        field(roleSettings, "model") === profile.model && field(roleSettings, "effort") === profile.effort;
      const runtimeScope = setup.state === "activated" && trip?.readiness === "ready" &&
          trip.setup_operation_id === setup.setup_operation_id && profileIsActive &&
          revision && profile_hash && current_adapter_hash
        ? {
          project_id: setup.project_id, setup_operation_id: setup.setup_operation_id,
          role, profile_hash, project_config_revision_id: revision, adapter_hash: current_adapter_hash,
        }
        : null;
      const admissions = runtimeScope ? (setup.runtime_admissions ?? []).filter((admission) =>
        admission.project_id === setup.project_id && admission.task_id === null &&
        admission.probes.some((probe) => probe.role === role)
      ) : [];
      return [{ setup, selection, profile, hasSetupProof, runtimeScope, admissions, project }];
    });
  });
  const runtimeCapabilities = capabilities.filter((item) =>
    item.mode === "interactive_pty" && selectedProfiles.some(({ runtimeScope, profile }) =>
      runtimeScope?.role === item.role && profile.provider === item.provider
    )
  );
  // Snapshot updates from the event feed only invalidate proof when its inputs change.
  const runtimeKey = JSON.stringify([
    incarnation,
    selectedProfiles.map(({ runtimeScope, setup, profile, admissions, project }) => [
      runtimeScope, setup.fixture_project_id, profile.provider, profile.model, profile.effort,
      project?.repository_path, project?.repository_identity, project?.trip?.configuration?.configuration_hash,
      admissions.map(({ id, scope_hash, state, probes }) => [
        id, scope_hash, state, probes.map((probe) => [
          probe.role, probe.state, probe.settings_revision, probe.profile_hash, probe.profile,
          probe.project_config_revision_id, probe.project_configuration_hash, probe.adapter_hash,
          probe.capability_id, probe.capability_key,
        ]),
      ]),
    ]),
    runtimeCapabilities.map(({ id, rowid, provider, role, version, config_hash, hook_hash, status, checked_at, proof }) =>
      [id, rowid, provider, role, version, config_hash, hook_hash, status, checked_at, proof?.runtime_scopes]
    ),
  ]);
  // A new request identity also fences a return to an earlier configuration.
  const runtimeRequest = useMemo(() => ({ runtimeKey, refreshVersion }), [runtimeKey, refreshVersion]);
  const [runtimeResult, setRuntimeResult] = useState<{
    request: typeof runtimeRequest;
    bindings: DiagnosticsRuntimeBinding[];
  }>();
  useEffect(() => {
    const controller = new AbortController();
    runtimeRead.current = controller;
    void diagnosticsRuntimeBindings(controller.signal).then((bindings) => {
      if (!controller.signal.aborted) setRuntimeResult({ request: runtimeRequest, bindings });
    }).catch(() => {
      if (!controller.signal.aborted) setRuntimeResult({ request: runtimeRequest, bindings: [] });
    });
    return () => controller.abort();
  }, [runtimeRequest]);
  const bindings = runtimeResult?.request === runtimeRequest ? runtimeResult.bindings : [];
  const profileRows = selectedProfiles.map((row) => {
    const { runtimeScope, admissions, profile, project } = row;
    const configurationHash = project?.trip?.configuration?.configuration_hash;
    const runtimeBinding = runtimeScope && bindings.find((binding) =>
      binding.project_id === runtimeScope.project_id &&
      binding.setup_operation_id === runtimeScope.setup_operation_id &&
      binding.role === runtimeScope.role && binding.profile_hash === runtimeScope.profile_hash &&
      binding.project_config_revision_id === runtimeScope.project_config_revision_id &&
      binding.adapter_hash === runtimeScope.adapter_hash &&
      admissions.some(({ probes }) => probes.some((probe) =>
        probe.state === "published" && probe.settings_revision == null &&
        probe.role === binding.role &&
        probe.profile_hash === binding.runtime_profile_hash &&
        probe.profile.provider === profile.provider && probe.profile.model === profile.model &&
        probe.profile.effort === profile.effort &&
        probe.project_config_revision_id === binding.project_config_revision_id &&
        (!configurationHash || probe.project_configuration_hash === configurationHash) &&
        probe.adapter_hash === binding.adapter_hash &&
        probe.capability_id === binding.capability_id && probe.capability_key === binding.capability_key
      )) &&
      runtimeCapabilities.some((item) =>
        item.id === binding.capability_id && item.config_hash === binding.capability_key &&
        item.provider === profile.provider && item.role === binding.role &&
        isCurrentCapability(item, runtimeCapabilities)
      )
    );
    return { ...row, runtimeBinding };
  });
  const historicalCapabilities = capabilities.filter((item) =>
    !profileRows.some(({ runtimeBinding }) =>
      runtimeBinding && item.id === runtimeBinding.capability_id &&
      item.config_hash === runtimeBinding.capability_key
    )
  );
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
        <button type="button" onClick={refreshDiagnostics}>Refresh diagnostics</button>
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
        {profileRows.map(({ setup, selection, profile, hasSetupProof, runtimeBinding }) => (
          <div key={`${setup.setup_operation_id}-${selection.role}`}>
            <strong>
              {providerName(profile.provider)} · {roleLabel(selection.role)} ·{" "}
              {runtimeBinding
                ? verificationLabel("supported")
                : hasSetupProof
                ? "Setup proof recorded"
                : verificationLabel("unverified")}
            </strong>
            <p>Selected profile · {profile.model} · {profile.effort}</p>
            {runtimeBinding && hasSetupProof && <p>Setup proof recorded</p>}
            <CompatibilityDetails compatibility={selection.compatibility} />
          </div>
        ))}
        {historicalCapabilities.map((item, index) => (
          <div key={`${item.provider}-${item.role}-${index}`}>
            <strong>
              {providerName(item.provider)} · {roleLabel(item.role)} ·{" "}
              Historical evidence · {verificationLabel(item.status)}
            </strong>
            {item.checked_at && <p>Recorded <time>{item.checked_at}</time></p>}
            <CompatibilityDetails
              compatibility={item.compatibility}
              verification={item.status === "supported" ? "saved_record" : undefined}
            />
            {item.config_hash && <TechnicalDetails><p>Capability binding {item.config_hash}</p></TechnicalDetails>}
          </div>
        ))}
        <p>
          To verify an agent, use Project setup or the task's Roles tab.
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
