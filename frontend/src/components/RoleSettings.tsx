import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { useEffect, useRef, useState } from "react";
import {
  ApiError,
  command,
  getRolePreparations,
  getState,
  operation,
  reuseOperationIdentity,
} from "../api";
import { ModelSelector } from "./ModelSelector";
import {
  type CapabilityEvidence,
  type CompatibilityExplanation,
  type ContinuationAction,
  type ProductionRoleRestriction,
  type ProfileAdmission,
  type Project,
  type ProjectExecution,
  type Provider,
  type Role,
  type RoleConfig,
  roleLabel,
  type RolePreparation,
  type RuntimeAdmissionState,
  ROLES,
  type Session,
  type SwitchIntent,
  type Task,
  type TripLaneState,
  type WorkflowFamily,
} from "../types";
import {
  generationSettled,
  UncertainRequestNotice,
  type WorkflowCommandRunner,
  useWorkflowCommands,
} from "./WorkflowControls";

const compatibilityActions: Record<CompatibilityExplanation["action"], string> = {
  install_supported_provider_version: "Install the supported agent version shown below, or update LLMRelay, then verify the profile again.",
  update_llmrelay_release: "Update LLMRelay to a release that supports this agent version, then verify the profile again.",
  requalify_exact_profile: "Use Project setup or the task's Roles tab to run verification for these agent settings before starting work.",
  inspect_local_provider_configuration: "Review the agent setting named in Technical details and correct it before verifying again.",
  contact_operator: "Ask the person who manages this LLMRelay installation to review Technical details and update or repair it.",
};

export function CompatibilityDetails({
  compatibility,
  verification,
}: {
  compatibility?: CompatibilityExplanation | null;
  verification?: "current_profile" | "saved_record";
}) {
  if (!compatibility) return null;
  const candidate = compatibility.status === "matched";
  const verified = candidate && verification !== undefined;
  return (
    <div className="compatibility-details">
      <p>
        <strong>{verified ? "Agent verification passed" : candidate ? "Agent version recognized" : "Agent verification needs attention"}</strong>
        {" · "}{verified
          ? verification === "current_profile"
            ? "These exact agent settings passed verification."
            : "This saved record passed verification. Current task settings are checked separately."
          : compatibility.status === "unknown_version"
          ? `This agent version (${compatibility.observed_version || "unknown"}) is not supported by this LLMRelay release.`
          : compatibility.status === "contract_changed"
          ? "The agent connection settings have changed since the last verification."
          : compatibility.status === "evidence_stale"
          ? compatibility.missing_evidence.includes(
              "exact_selected_profile_observation",
            )
            ? compatibility.message
            : "Verification for these agent settings is missing or out of date."
          : candidate
          ? "Version recognition alone does not mean this profile is ready to run. Check its verification status."
          : "LLMRelay's agent support information could not be validated."}
      </p>
      {!verified && <p className="hint">Next step: {compatibilityActions[compatibility.action]}</p>}
      <details>
        <summary>Technical details</summary>
        <p>{compatibility.message}</p>
        <small>
          Pack {compatibility.pack_id || "unavailable"} revision {compatibility.pack_revision || "unavailable"}
          {compatibility.observed_version ? ` · observed ${compatibility.observed_version}` : ""}
          {" · "}contract {compatibility.contract_id || "unavailable"} revision {compatibility.contract_revision || "unavailable"}
          {" · "}predicate {compatibility.predicate_id || "unavailable"}
          {" · "}hash {compatibility.short_hash || "unavailable"}
        </small>
        {compatibility.missing_evidence.length > 0 && (
          <small>Missing evidence: {compatibility.missing_evidence.join(" · ")}</small>
        )}
      </details>
    </div>
  );
}

export interface SingleAgentSelection {
  execution_config_revision_id: string;
  profile_revision_id: string;
  check_ids: string[];
}

export const emptySingleAgentSelection = (
  execution?: ProjectExecution,
): SingleAgentSelection => ({
  execution_config_revision_id: execution?.configurations[0]?.id ?? "",
  profile_revision_id: "",
  check_ids: [],
});

const providerName = (provider: Provider) =>
  provider === "claude" ? "Claude" : "Codex";

// Configuration checks arrive as untyped records; only an ID makes one
// selectable.
const configurationChecks = (
  execution: ProjectExecution | undefined,
  configId: string,
) =>
  (execution?.configurations.find((item) => item.id === configId)?.checks ?? [])
    .flatMap((check) => {
      const id = check.id;
      const text = check.original_text;
      const key = check.check_key;
      if (typeof id !== "string") return [];
      return [{
        id,
        label: typeof text === "string" && text.trim()
          ? text
          : typeof key === "string"
          ? key
          : "Check",
      }];
    });

export const latestAdmission = (
  execution: ProjectExecution | undefined,
  profileRevisionId: string,
  configId: string,
): ProfileAdmission | undefined =>
  execution?.admissions.filter((admission) =>
    admission.profile_revision_id === profileRevisionId &&
    admission.execution_config_revision_id === configId
  ).sort((left, right) => right.updated_at.localeCompare(left.updated_at))[0];

const admissionSummary = (admission: ProfileAdmission | undefined) =>
  !admission
    ? "Not admitted yet. Prepare its admission before a task can use this profile with this configuration."
    : admission.state === "activated"
    ? "Admitted for this project with this configuration."
    : admission.state === "pending"
    ? `Admission pending${
      admission.state_reason ? `: ${admission.state_reason}` : "."
    }`
    : `Admission no longer valid${
      admission.state_reason ? `: ${admission.state_reason}` : "."
    } Prepare it again.`;

export function SingleAgentSelectionFields(
  { execution, value, onChange, disabled = false }: {
    execution?: ProjectExecution;
    value: SingleAgentSelection;
    onChange: (value: SingleAgentSelection) => void;
    disabled?: boolean;
  },
) {
  const profiles = (execution?.profiles ?? []).flatMap((profile) =>
    profile.current_revision ? [{ profile, revision: profile.current_revision }] : []
  );
  if (!execution?.configurations.length || !profiles.length) {
    return (
      <p className="hint">
        This project has no Single agent setup yet. Add an execution
        configuration and an agent profile in Project settings first.
      </p>
    );
  }
  const checks = configurationChecks(execution, value.execution_config_revision_id);
  const chosen = !!value.execution_config_revision_id &&
    !!value.profile_revision_id;
  return (
    <fieldset className="single-agent-selection" disabled={disabled}>
      <legend>Single agent setup</legend>
      <label>
        Execution configuration
        <select
          value={value.execution_config_revision_id}
          onChange={(event) =>
            onChange({
              ...value,
              execution_config_revision_id: event.target.value,
              check_ids: [],
            })}
        >
          <option value="">Choose a configuration</option>
          {execution.configurations.map((configuration) => (
            <option key={configuration.id} value={configuration.id}>
              Revision {configuration.revision} · approved{" "}
              {new Date(configuration.approved_at).toLocaleString()}
            </option>
          ))}
        </select>
      </label>
      <label>
        Agent profile
        <select
          value={value.profile_revision_id}
          onChange={(event) =>
            onChange({ ...value, profile_revision_id: event.target.value })}
        >
          <option value="">Choose a profile</option>
          {profiles.map(({ profile, revision }) => (
            <option key={revision.id} value={revision.id}>
              {profile.display_name} · {providerName(revision.provider)} ·{" "}
              {revision.model} · {revision.effort}
            </option>
          ))}
        </select>
      </label>
      {checks.length > 0 && (
        <fieldset>
          <legend>Checks the result must pass</legend>
          {checks.map((check) => (
            <label className="toggle" key={check.id}>
              <input
                type="checkbox"
                checked={value.check_ids.includes(check.id)}
                onChange={(event) =>
                  onChange({
                    ...value,
                    check_ids: event.target.checked
                      ? [...value.check_ids, check.id]
                      : value.check_ids.filter((id) => id !== check.id),
                  })}
              />
              {check.label}
            </label>
          ))}
        </fieldset>
      )}
      {chosen && (
        <p className="hint">
          {admissionSummary(
            latestAdmission(
              execution,
              value.profile_revision_id,
              value.execution_config_revision_id,
            ),
          )}
        </p>
      )}
    </fieldset>
  );
}

/**
 * Prepare, probe and activate one profile revision's admission with one
 * configuration. Probing queues a paid model run, so it needs a second,
 * explicit confirmation; nothing here runs on its own.
 */
export function AdmissionControls(
  { projectId, execution, profileRevisionId, configId, tasks, commands }: {
    projectId: string;
    execution?: ProjectExecution;
    profileRevisionId: string;
    configId: string;
    tasks: Task[];
    commands: WorkflowCommandRunner;
  },
) {
  const [confirmProbe, setConfirmProbe] = useState(false);
  const admission = latestAdmission(execution, profileRevisionId, configId);
  const probeTask = admission?.probe_task_id
    ? tasks.find((task) => task.id === admission.probe_task_id)
    : undefined;
  const probeRun = probeTask?.workflow?.run ?? null;
  const probeOpen = !!probeTask && !probeTask.archived &&
    !["done", "cancelled"].includes(probeTask.lifecycle);
  const evidenceRecorded = probeRun?.stage === "probe_evidence_recorded";
  const key = `${profileRevisionId}:${configId}`;
  const prepare = () =>
    void commands.submit(`prepare:${key}`, (operation_id) => ({
      kind: "prepare_profile_admission",
      operation_id,
      project_id: projectId,
      profile_revision_id: profileRevisionId,
      execution_config_revision_id: configId,
    }));
  const probe = async (admissionId: string) => {
    await commands.submit(`probe:${admissionId}`, (operation_id) => ({
      kind: "probe_profile_admission",
      operation_id,
      project_id: projectId,
      admission_id: admissionId,
    }));
    setConfirmProbe(false);
  };
  const activate = (admissionId: string) =>
    void commands.submit(`activate:${admissionId}`, (operation_id) => ({
      kind: "activate_profile_admission",
      operation_id,
      project_id: projectId,
      admission_id: admissionId,
    }));
  const keys = [`prepare:${key}`, ...(admission
    ? [`probe:${admission.id}`, `activate:${admission.id}`]
    : [])];
  const uncertain = keys.some((item) => commands.uncertain.has(item));
  const changedSince = keys.some((item) => commands.reviewed.has(item));
  return (
    <div className="admission-controls">
      <p>{admissionSummary(admission)}</p>
      {(!admission || admission.state === "invalidated") && (
        <>
          <button disabled={commands.busy} onClick={prepare}>
            Prepare admission
          </button>
          <small>
            Preparing records exactly what would be admitted. It starts
            nothing and makes no model call.
          </small>
        </>
      )}
      {admission?.state === "pending" && !probeOpen && !evidenceRecorded && (
        !admission.probe_available
          ? (
            <>
              <button disabled>Probe this profile…</button>
              <small>
                A probe cannot start now:{" "}
                {admission.probe_unavailable_reason ??
                  "LLMRelay did not report why."}
              </small>
            </>
          )
          : confirmProbe
          ? (
            <div className="warning">
              <p>
                A probe queues one disposable task that runs this agent once in
                its own worktree, which is a paid model call. Its result is
                evidence for this admission only.
              </p>
              <div className="button-row">
                <button
                  disabled={commands.busy}
                  onClick={() => void probe(admission.id)}
                >
                  Queue the probe run
                </button>
                <button onClick={() => setConfirmProbe(false)}>
                  Don't probe
                </button>
              </div>
            </div>
          )
          : (
            <button
              disabled={commands.busy}
              onClick={() => setConfirmProbe(true)}
            >
              Probe this profile…
            </button>
          )
      )}
      {probeOpen && probeTask && (
        <p className="hint">
          Probe task “{probeTask.title}”:{" "}
          {probeRun?.stage_reason ?? "waiting to start."}
        </p>
      )}
      {admission?.state === "pending" && evidenceRecorded && (
        <button
          className="primary"
          disabled={commands.busy}
          onClick={() => activate(admission.id)}
        >
          Activate admission
        </button>
      )}
      {admission?.state === "activated" && admission.activated_at && (
        <small>
          Activated {new Date(admission.activated_at).toLocaleString()}.
        </small>
      )}
      {uncertain && <UncertainRequestNotice changed={changedSince} />}
    </div>
  );
}

/**
 * The workflow choice of a task that has not started. A Ready task returns
 * to the drafts when its choice changes, so Make Ready rechecks it.
 */
function WorkflowChoice(
  { task, execution, commands }: {
    task: Task;
    execution?: ProjectExecution;
    commands: WorkflowCommandRunner;
  },
) {
  const current = task.workflow;
  const [family, setFamily] = useState<WorkflowFamily>(
    current?.family ?? "trip",
  );
  const [selection, setSelection] = useState<SingleAgentSelection>(() =>
    current?.family === "single_agent"
      ? {
        execution_config_revision_id: current.execution_config_revision_id ??
          "",
        profile_revision_id: current.profile_revision_id ?? "",
        check_ids: current.check_ids,
      }
      : emptySingleAgentSelection(execution)
  );
  const unchanged = family === (current?.family ?? "trip") &&
    (family === "trip" ||
      (selection.execution_config_revision_id ===
          current?.execution_config_revision_id &&
        selection.profile_revision_id === current?.profile_revision_id &&
        [...selection.check_ids].sort().join() ===
          [...(current?.check_ids ?? [])].sort().join()));
  const incomplete = family === "single_agent" &&
    (!selection.execution_config_revision_id || !selection.profile_revision_id);
  const key = `selection:${task.id}`;
  const save = () =>
    void commands.submit(key, (operation_id) => ({
      kind: "set_task_workflow",
      operation_id,
      task_id: task.id,
      expected_version: task.version,
      selection: {
        workspace_mode: current?.workspace_mode ?? "isolated_worktree",
        ...(family === "trip" ? { family } : { family, ...selection }),
      },
    }));
  return (
    <section className="panel workflow-choice">
      <h3>Workflow</h3>
      <div className="button-row" role="radiogroup" aria-label="Workflow">
        {(["trip", "single_agent"] as const).map((option) => (
          <label className="toggle" key={option}>
            <input
              type="radio"
              name={`workflow-${task.id}`}
              checked={family === option}
              disabled={commands.busy}
              onChange={() => setFamily(option)}
            />
            {option === "trip"
              ? "TRIP (manager and five delegated roles)"
              : "Single agent (one agent writes the whole task)"}
          </label>
        ))}
      </div>
      {family === "single_agent" && (
        <SingleAgentSelectionFields
          execution={execution}
          value={selection}
          onChange={setSelection}
          disabled={commands.busy}
        />
      )}
      {current?.admission_state === "invalidated" && current.admission_reason && (
        <p className="hint">
          The earlier admission of this task no longer applies:{" "}
          {current.admission_reason}
        </p>
      )}
      {task.lifecycle === "ready" && !unchanged && (
        <p className="hint">
          Saving moves this Ready task back to the drafts. Choose Make Ready
          again afterwards.
        </p>
      )}
      <button
        disabled={commands.busy || unchanged || incomplete}
        onClick={save}
      >
        Save workflow choice
      </button>
      {commands.uncertain.has(key) && (
        <UncertainRequestNotice changed={commands.reviewed.has(key)} />
      )}
    </section>
  );
}

/** A Single agent task's own actor, profile and admission; no TRIP roles. */
function SingleAgentSettings(
  { task, sessions, execution, tasks, commands }: {
    task: Task;
    sessions: Session[];
    execution?: ProjectExecution;
    tasks: Task[];
    commands: WorkflowCommandRunner;
  },
) {
  const workflow = task.workflow;
  const terminal = task.archived ||
    ["done", "cancelled"].includes(task.lifecycle);
  const profileRevisionId = workflow?.run?.profile_revision_id ??
    workflow?.profile_revision_id ?? "";
  const configId = workflow?.run?.execution_config_revision_id ??
    workflow?.execution_config_revision_id ?? "";
  const profile = execution?.profiles.find((item) =>
    item.current_revision?.id === profileRevisionId
  );
  const configuration = execution?.configurations.find((item) =>
    item.id === configId
  );
  const writer = sessions.find((session) =>
    session.task_id === task.id && session.role === "single_agent" &&
    session.attempt_id === task.active_attempt?.id
  );
  return (
    <section className="panel role-settings single-agent-settings">
      <header>
        <h3>Single agent</h3>
        <span>One agent writes the whole task</span>
      </header>
      <dl className="progress-facts">
        <dt>Agent profile</dt>
        <dd>
          {profile?.current_revision
            ? `${profile.display_name} · ${
              providerName(profile.current_revision.provider)
            } · ${profile.current_revision.model} · ${profile.current_revision.effort}`
            : profileRevisionId
            ? "An earlier revision of a profile; its current settings may differ."
            : "Not chosen yet"}
        </dd>
        <dt>Execution configuration</dt>
        <dd>
          {configuration
            ? `Revision ${configuration.revision}`
            : configId
            ? "An earlier revision"
            : "Not chosen yet"}
        </dd>
        <dt>Required checks</dt>
        <dd>
          {(workflow?.run?.selected_check_ids ?? workflow?.check_ids ?? [])
            .length || "None"}
        </dd>
        <dt>Agent session</dt>
        <dd>
          {writer
            ? writer.status === "running" ? "Running now" : "Not running"
            : "Not started yet"}
        </dd>
      </dl>
      {workflow && !workflow.run && (
        <p className="hint">
          {workflow.admission_state === "admitted"
            ? "This task's selection is admitted and queued."
            : workflow.admission_state === "invalidated"
            ? `This task's admission no longer applies${
              workflow.admission_reason ? `: ${workflow.admission_reason}` : "."
            }`
            : "This task is a draft. Make Ready checks its selection and admission."}
        </p>
      )}
      {!terminal && profileRevisionId && configId && (
        <AdmissionControls
          projectId={task.project_id}
          execution={execution}
          profileRevisionId={profileRevisionId}
          configId={configId}
          tasks={tasks}
          commands={commands}
        />
      )}
      <TechnicalDetails>
        <p>
          profile revision {profileRevisionId || "none"} · configuration{" "}
          {configId || "none"} · selection revision{" "}
          {workflow?.selection_revision ?? "none"} · admission{" "}
          {workflow?.admission_state ?? "none"}
          {writer ? ` · session ${writer.id} · ${writer.status}` : ""}
        </p>
      </TechnicalDetails>
    </section>
  );
}

export function RoleSettings(
  {
    task,
    sessions,
    switches = [],
    lanes = [],
    project,
    productionRestrictions = [],
    capabilities = [],
    runtimeAdmissions = [],
    actions = [],
    execution,
    tasks = [],
    onChanged,
  }: {
    task: Task;
    project?: Project;
    sessions: Session[];
    switches?: SwitchIntent[];
    lanes?: TripLaneState[];
    productionRestrictions?: ProductionRoleRestriction[];
    capabilities?: CapabilityEvidence[];
    runtimeAdmissions?: RuntimeAdmissionState[];
    actions?: ContinuationAction[];
    /** This task's project Single agent setup; absent from older services. */
    execution?: ProjectExecution;
    /** Every task, so an admission's probe task can be found. */
    tasks?: Task[];
    onChanged: () => void;
  },
) {
  const terminal = task.archived ||
    ["done", "cancelled"].includes(task.lifecycle);
  const singleAgent = task.workflow?.family === "single_agent";
  const workflowCommands = useWorkflowCommands(
    `llmrelay.workflow.commands.${task.id}`,
    onChanged,
  );
  const workflowEditable = !terminal && !task.active_attempt &&
    ["backlog", "ready"].includes(task.lifecycle) && execution !== undefined;
  const [editing, setEditing] = useState<Role>();
  const [config, setConfig] = useState<RoleConfig>();
  const [error, setError] = useState("");
  const [cmuxSocketPath, setCmuxSocketPath] = useState("");
  const cmuxSocketInstruction = "Enter the live cmux socket path before preparing Claude verification.";
  const operationStorageKey = `llmrelay.role.operations.${task.id}`;
  const browserOperationIdentities = useRef(
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
  const runBrowserOperation = async (
    key: string,
    request: Record<string, unknown> & { kind: string },
  ) => {
    const stable = reuseOperationIdentity(
      browserOperationIdentities.current.get(key),
      request,
    );
    const { id, request: stableRequest } = stable;
    browserOperationIdentities.current.set(key, {
      body: JSON.stringify(stableRequest),
      id,
    });
    localStorage.setItem(
      operationStorageKey,
      JSON.stringify([...browserOperationIdentities.current]),
    );
    try {
      const result = await operation(
        { ...stableRequest, operation_id: id } as never,
      );
      browserOperationIdentities.current.delete(key);
      localStorage.setItem(
        operationStorageKey,
        JSON.stringify([...browserOperationIdentities.current]),
      );
      return result;
    } catch (cause) {
      if (!(cause instanceof ApiError && cause.ambiguous)) {
        browserOperationIdentities.current.delete(key);
        localStorage.setItem(
          operationStorageKey,
          JSON.stringify([...browserOperationIdentities.current]),
        );
      }
      throw cause;
    }
  };
  const runCommand = async (key: string, request: Record<string, unknown>) => {
    const stable = reuseOperationIdentity(
      browserOperationIdentities.current.get(key),
      request,
    );
    const { id, request: stableRequest } = stable;
    browserOperationIdentities.current.set(key, {
      body: JSON.stringify(stableRequest),
      id,
    });
    localStorage.setItem(
      operationStorageKey,
      JSON.stringify([...browserOperationIdentities.current]),
    );
    try {
      const result = await command(
        { ...stableRequest, operation_id: id } as never,
      );
      browserOperationIdentities.current.delete(key);
      localStorage.setItem(
        operationStorageKey,
        JSON.stringify([...browserOperationIdentities.current]),
      );
      return result;
    } catch (cause) {
      if (!(cause instanceof ApiError && cause.ambiguous)) {
        browserOperationIdentities.current.delete(key);
        localStorage.setItem(
          operationStorageKey,
          JSON.stringify([...browserOperationIdentities.current]),
        );
      }
      throw cause;
    }
  };
  const projectRoles =
    project?.settings.roles && typeof project.settings.roles === "object"
      ? project.settings.roles as Partial<Record<Role, RoleConfig>>
      : {};
  const canUseDefaults = !terminal && !task.active_attempt &&
    ["backlog", "ready"].includes(task.lifecycle);
  const defaultsComplete = ROLES.every((role) => {
    const value = projectRoles[role];
    return value && ["codex", "claude"].includes(value.provider) &&
      typeof value.model === "string" && value.model.trim() &&
      typeof value.effort === "string" && value.effort.trim();
  });
  const defaultsPending = useRef(false);
  const [defaultsState, setDefaultsState] = useState<{
    version: number;
    state: "saving" | "saved" | "unknown";
  }>();
  const [defaultsNotice, setDefaultsNotice] = useState("");
  const defaultsBlocked = defaultsState?.state === "saving" ||
    defaultsState?.version === task.version;
  const settings = (role: Role) =>
    [...task.role_settings].filter((value) => value.role === role).sort((
      left,
      right,
    ) => right.revision - left.revision);
  const knownExactModels = (provider: Provider | "") =>
    provider
      ? [
        ...task.role_settings.map((setting) =>
          setting.config.provider === provider ? setting.config.model : ""
        ),
        ...Object.values(projectRoles).map((setting) =>
          setting?.provider === provider ? setting.model : ""
        ),
      ]
      : [];
  const preparationRevisionKey = ROLES.map((role) => settings(role)[0]).map(
    (requested) =>
      requested
        ? `${requested.role}:${requested.revision}:${requested.config.provider}:${requested.config.model}:${requested.config.effort}`
        : "none",
  ).join("|");
  const [preparations, setPreparations] = useState<RolePreparation[]>([]);
  const preparationEvidenceKey = JSON.stringify({
    configuration: project?.settings.trip_config_revision_id,
    capabilities: capabilities.map((item) => [
      item.provider,
      item.role,
      item.mode,
      item.config_hash,
      item.hook_hash,
      item.status,
      item.checked_at,
      item.compatibility,
    ]),
    admissions: runtimeAdmissions.filter((item) =>
      !item.task_id || item.task_id === task.id
    )
      .map((
        item,
      ) => [
        item.id,
        item.state,
        item.updated_at,
        item.probes.map((probe) => [
          probe.role,
          probe.state,
          probe.session_id,
          probe.session_status,
          probe.readiness,
          probe.failure_reason,
          probe.published_at,
        ]),
      ]),
  });
  const [preparationError, setPreparationError] = useState("");
  const [preparationRefresh, setPreparationRefresh] = useState(0);
  useEffect(() => {
    if (terminal || singleAgent) {
      setPreparations([]);
      setPreparationError("");
      return;
    }
    let cancelled = false;
    setPreparations([]);
    setPreparationError("");
    void getRolePreparations(task.id).then((values) => {
      if (!cancelled) setPreparations(values);
    }).catch((cause) => {
      if (!cancelled) {
        setPreparationError(
          cause instanceof Error ? cause.message : String(cause),
        );
      }
    });
    return () => {
      cancelled = true;
    };
  }, [task.id, task.version, preparationRevisionKey, preparationEvidenceKey, preparationRefresh, terminal, singleAgent]);
  const refresh = () => {
    setPreparationRefresh((value) => value + 1);
    onChanged();
  };
  // Decides an unknown save from one authoritative read: the requested role
  // choices either match the project defaults (saved) or the task is still
  // at the version the save was sent for (not applied, safe to send again).
  const checkDefaultsOutcome = async () => {
    const pending = defaultsState;
    if (pending?.state !== "unknown") return;
    try {
      const latest = (await getState()).tasks.find((item) =>
        item.id === task.id
      );
      const latestRequested = (role: Role) =>
        latest?.role_settings.filter((setting) => setting.role === role)
          .sort((left, right) => right.revision - left.revision)[0]?.config;
      const saved = !!latest && ROLES.every((role) => {
        const requested = latestRequested(role);
        const wanted = projectRoles[role];
        return !!requested && !!wanted &&
          requested.provider === wanted.provider &&
          requested.model === wanted.model &&
          requested.effort === wanted.effort;
      });
      setDefaultsState(
        saved && latest ? { version: latest.version, state: "saved" } : undefined,
      );
      setError("");
      setDefaultsNotice(
        saved
          ? ""
          : !latest
          ? "This task is no longer shown, so the save outcome cannot be checked here. Refresh the dashboard."
          : latest.version === pending.version
          ? "The project role choices were not applied. You can choose Use project defaults again."
          : "This task changed since the save was sent, but its role choices do not match the project defaults. Review the role settings before trying again.",
      );
      refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  const save = async (role: Role) => {
    if (!config) return;
    try {
      await runCommand(`settings:${role}`, {
        kind: "set_role_settings",
        task_id: task.id,
        role,
        expected_version: task.version,
        config,
      });
      setEditing(undefined);
      setError("");
      refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  const useProjectDefaults = async () => {
    if (
      !canUseDefaults || !defaultsComplete || defaultsPending.current ||
      defaultsBlocked
    ) return;
    defaultsPending.current = true;
    setDefaultsState({ version: task.version, state: "saving" });
    setDefaultsNotice("");
    setError("");
    try {
      await runCommand(`project-defaults:${task.version}`, {
        kind: "update_task",
        task_id: task.id,
        expected_version: task.version,
        title: task.title,
        description: task.description,
        acceptance_criteria: task.acceptance_criteria,
        priority: task.priority,
        manual_order: task.manual_order,
        role_overrides: Object.fromEntries(
          ROLES.map((role) => [role, projectRoles[role]]),
        ),
      });
      setEditing(undefined);
      setDefaultsState({ version: task.version, state: "saved" });
      refresh();
    } catch (cause) {
      const ambiguous = cause instanceof ApiError && cause.ambiguous;
      setDefaultsState(
        ambiguous ? { version: task.version, state: "unknown" } : undefined,
      );
      setError(cause instanceof Error ? cause.message : String(cause));
      refresh();
    } finally {
      defaultsPending.current = false;
    }
  };
  const activate = async (role: Role, revision: number) => {
    try {
      await runCommand(`activate:${role}:${revision}`, {
        kind: "activate_task_profile",
        task_id: task.id,
        role,
        settings_revision: revision,
        expected_version: task.version,
      });
      setError("");
      refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  const resume = async (session: Session, action: ContinuationAction) => {
    try {
      if (action.operation === "runtime_probe_resume") {
        const admissionId =
          typeof action.binding.runtime_admission_id === "string"
            ? action.binding.runtime_admission_id
            : undefined;
        const role = typeof action.binding.role === "string"
          ? action.binding.role as Role
          : undefined;
        if (!admissionId || !role) {
          throw new Error(
            "The projected runtime-resume binding is incomplete.",
          );
        }
        await runBrowserOperation(`runtime-resume:${admissionId}:${role}`, {
          kind: "runtime_probe_resume",
          admission_id: admissionId,
          role,
        });
      } else {
        await runBrowserOperation(`role-resume:${session.id}`, {
          kind: "role_resume",
          session_id: session.id,
          prompt: "",
        });
      }
      refresh();
    } catch (cause) {
      setError(
        cause instanceof ApiError && cause.ambiguous
          ? `${cause.message} Refresh and reconcile before retrying.`
          : cause instanceof Error
          ? cause.message
          : String(cause),
      );
      refresh();
    }
  };
  const prepareRuntime = async (role: Role, revision: number) => {
    if (!project) return;
    const preparation = preparations.find((item) =>
      item.task_id === task.id && item.role === role &&
      item.requested_revision === revision
    );
    if (preparation?.config.provider === "claude" && !cmuxSocketPath.trim()) {
      setError(cmuxSocketInstruction);
      return;
    }
    const projectScoped = preparation?.task_profile_source ===
      "project_default";
    try {
      await runCommand(`runtime-prepare:${role}:${revision}`, {
        kind: "trip",
        action: "prepare_runtime_admission",
        project_id: project.id,
        ...(projectScoped ? {} : { task_id: task.id }),
        role,
        ...(projectScoped ? {} : { settings_revision: revision }),
        ...(cmuxSocketPath.trim()
          ? { cmux_socket_path: cmuxSocketPath.trim() }
          : {}),
        expected_version: projectScoped ? project.version : task.version,
      });
      setError("");
      refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  const authorizeRuntime = async (id: string, scopeHash: string) => {
    try {
      await runCommand(`runtime-authorize:${id}`, {
        kind: "trip",
        action: "authorize_runtime_admission",
        admission_id: id,
        scope_hash: scopeHash,
      });
      setError("");
      refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  const runRuntime = async (
    id: string,
    role: Role,
    action: "launch" | "resume" | "publish",
  ) => {
    try {
      if (action === "publish") {
        await runCommand(`runtime-publish:${id}:${role}`, {
          kind: "trip",
          action: "publish_runtime_proof",
          admission_id: id,
          role,
        });
      } else {
        await runBrowserOperation(
          `runtime:${id}:${role}:${action}`,
          action === "resume"
            ? {
              kind: "runtime_probe_resume",
              admission_id: id,
              role,
            }
            : {
              kind: "runtime_probe_launch",
              admission_id: id,
              role,
            },
        );
      }
      setError("");
      refresh();
    } catch (cause) {
      setError(
        cause instanceof ApiError && cause.ambiguous
          ? `${cause.message} Refresh and reconcile before retrying.`
          : cause instanceof Error
          ? cause.message
          : String(cause),
      );
      refresh();
    }
  };

  const workflowChoice = workflowEditable && (
    <WorkflowChoice
      key={`${task.id}:${task.version}`}
      task={task}
      execution={execution}
      commands={workflowCommands}
    />
  );
  if (singleAgent) {
    return (
      <>
        {workflowChoice}
        <SingleAgentSettings
          task={task}
          sessions={sessions}
          execution={execution}
          tasks={tasks}
          commands={workflowCommands}
        />
        {workflowCommands.error && <ErrorNotice error={workflowCommands.error} />}
      </>
    );
  }

  return (
    <>
    {workflowChoice}
    {workflowCommands.error && <ErrorNotice error={workflowCommands.error} />}
    <section className="panel role-settings">
      <header>
        <h3>Agent roles</h3>
        <span>The manager plus five delegated roles</span>
      </header>
      {canUseDefaults && (
        <div className="project-role-defaults">
          <p className="hint">
            Use project defaults copies the six project role choices into this
            task, replacing its current choices. Verification is checked
            separately; this does not start agents or make the task Ready.
          </p>
          {!defaultsComplete && (
            <p className="hint">Complete the six role choices in Project setup first.</p>
          )}
          <button
            disabled={!defaultsComplete || defaultsBlocked}
            onClick={() => void useProjectDefaults()}
          >
            {defaultsState?.state === "saving" ? "Applying project defaults…" : "Use project defaults"}
          </button>
          {defaultsState && (
            <p role="status">
              {defaultsState.state === "unknown"
                ? "The save outcome is unknown. Check it before trying again."
                : defaultsState.state === "saved"
                ? "Project role choices saved. Refresh role settings to see their current verification status."
                : "Saving the project role choices…"}
              {" "}<button
                disabled={defaultsState.state === "saving"}
                onClick={defaultsState.state === "unknown" ? () => void checkDefaultsOutcome() : refresh}
              >
                {defaultsState.state === "unknown"
                  ? "Check whether it was saved"
                  : "Refresh role settings"}
              </button>
            </p>
          )}
          {defaultsNotice && <p role="status">{defaultsNotice}</p>}
        </div>
      )}
      <div className="role-list">
        {ROLES.map((role) => {
          const revisions = settings(role);
          const requested = revisions[0];
          const effective = revisions.find((value) =>
            value.effective_generation_id
          );
          const session = sessions.find((value) =>
            value.role_generation_id === effective?.effective_generation_id
          );
          const exactResume = session && actions.find((action) =>
            action.kind === "exact_resume" && action.enabled &&
            action.binding.session_id === session.id
          );
          const running = session?.status === "running" ? session : undefined;
          const switchable = generationSettled(session) ? session : undefined;
          // Between the session's exit and its generation's settled stop,
          // a probe or replacement would race the old generation.
          const stopping = session?.status === "exited" &&
            !generationSettled(session);
          const value = editing === role
            ? config
            : requested?.config || task.role_overrides[role] ||
              projectRoles[role];
          const preparation = value && requested && preparations.find((item) =>
            item.task_id === task.id && item.role === role &&
            item.requested_revision === requested.revision &&
            item.config.provider === value.provider &&
            item.config.model === value.model &&
            item.config.effort === value.effort
          );
          const currentProof = preparation?.status === "supported" &&
              preparation.capability_key
            ? capabilities.find((capability) =>
              capability.provider === value?.provider &&
              capability.role === role &&
              capability.mode === "interactive_pty" &&
              capability.status === "supported" &&
              capability.config_hash === preparation.capability_key
            )
            : undefined;
          const compatibility = preparation?.compatibility;
          const contractUnavailable = compatibility &&
            compatibility.status !== "matched" &&
            compatibility.status !== "evidence_stale";
          const activated = projectRoles[role];
          const matchesActivated = !!value && !!activated &&
            value.provider === activated.provider &&
            value.model === activated.model &&
            value.effort === activated.effort;
          const taskActivated = preparation?.task_profile_activated === true &&
            preparation.task_profile_source === "task_override";
          const replacementEligible = (matchesActivated || taskActivated) &&
            !!currentProof;
          const pendingReplacement = requested && effective &&
            requested.revision !== effective.revision;
          const fixedRestriction = value
            ? productionRestrictions.find(
              (restriction) =>
                restriction.provider === value.provider &&
                restriction.role === role,
            )
            : undefined;
          const productionRestriction = value?.provider === "codex" &&
              role === "implementer" && !currentProof
            ? preparation
              ? {
                provider: value.provider,
                role,
                status: preparation.status === "unsupported"
                  ? "unsupported" as const
                  : "unverified" as const,
                reason: preparation.status === "supported"
                  ? "The current preparation key has not yet appeared in Supported capability evidence"
                  : preparation.reason,
              }
              : editing === role
              ? {
                provider: value.provider,
                role,
                status: "unverified" as const,
                reason:
                  "Edited values have no authoritative current preparation identity; save the request and validate its exact prepared key",
              }
              : fixedRestriction || {
                provider: value.provider,
                role,
                status: "unverified" as const,
                reason: preparationError ||
                  "Authoritative current preparation is required before Supported evidence can be selected",
              }
            : undefined;
          const runtimeAdmission = preparation?.runtime_admission;
          const runtimeExactResume = runtimeAdmission?.session_id
            ? actions.find((action) =>
              action.kind === "exact_resume" && action.enabled &&
              action.operation === "runtime_probe_resume" &&
              action.binding.session_id === runtimeAdmission.session_id
            )
            : undefined;
          const runtimeSession = runtimeAdmission?.session_id
            ? sessions.find((entry) =>
              entry.id === runtimeAdmission.session_id
            )
            : undefined;
          const snapshots = task.snapshots.filter((snapshot) => {
            if (
              snapshot.attempt_id !== task.active_attempt?.id ||
              !snapshot.complete
            ) return false;
            if (role === "manager") {
              return snapshot.kind === "plan" &&
                snapshot.source_role_generation_id ===
                  switchable?.role_generation_id;
            }
            if (role === "implementer") {
              return snapshot.kind === "checkpoint" &&
                snapshot.source_role_generation_id ===
                  switchable?.role_generation_id;
            }
            if (role === "explorer") return false;
            const kind = role === "plan_reviewer" ? "plan" : "candidate";
            return snapshot.kind === kind && task.reviews.some((review) =>
              review.kind ===
                kind.replace(
                  "candidate",
                  role === "final_verifier" ? "final" : "code",
                ) &&
              review.candidate_hash === snapshot.manifest_hash &&
              review.role_generation_id === switchable?.role_generation_id
            );
          });
          const intent = switches.find((item) =>
            item.attempt_id === task.active_attempt?.id && item.role === role &&
            item.old_generation_id === effective?.effective_generation_id
          );
          return (
            <article
              key={role}
              data-attention-target={`role_settings:${task.id}:${role}`}
              tabIndex={-1}
            >
              <div>
                <strong>{roleLabel(role)}</strong>
                <small>
                  {running
                    ? "Running now"
                    : session
                    ? session.status === "exited" ? "Not running" : session.status.replaceAll("_", " ")
                    : effective
                    ? "Active, not running"
                    : "Not started yet"}
                </small>
                {role === "implementer" &&
                  lanes.filter((lane) =>
                    lane.attempt_id === task.active_attempt?.id
                  ).map((lane) => (
                    <small key={lane.id}>
                      Lane {lane.lane_key}: {lane.state} · effective{" "}
                      {lane.effective_generation_id?.slice(0, 8) || "pending"}
                      {lane.pending_settings_revision
                        ? ` · settings rev ${lane.pending_settings_revision} waiting`
                        : ""}
                    </small>
                  ))}
              </div>
              {!terminal && editing === role && value
                ? (
                  <div className="compact-fields role-edit-fields">
                    <label>
                      Provider
                      <select
                        value={value.provider}
                        onChange={(event) =>
                          setConfig({
                            ...value,
                            provider: event.target.value as "codex" | "claude",
                          })}
                      >
                        <option value="codex">Codex</option>
                        <option value="claude">Claude</option>
                      </select>
                    </label>
                    <ModelSelector
                      provider={value.provider}
                      value={value.model}
                      onChange={(model) => setConfig({ ...value, model })}
                      label={`${roleLabel(role)} exact model`}
                      knownExactModels={knownExactModels(value.provider)}
                    />
                    <label>
                      Effort
                      <select
                        value={value.effort}
                        onChange={(event) =>
                          setConfig({ ...value, effort: event.target.value })}
                      >
                        {!["low", "medium", "high", "xhigh", "max", "ultra"]
                          .includes(value.effort) && (
                          <option value={value.effort}>{value.effort}</option>
                        )}
                        <option>low</option>
                        <option>medium</option>
                        <option>high</option>
                        <option>xhigh</option>
                        <option>max</option>
                        <option>ultra</option>
                      </select>
                    </label>
                    <div className="button-row role-edit-actions">
                      <button className="primary" onClick={() => save(role)}>
                        Save request
                      </button>
                      <button onClick={() => setEditing(undefined)}>
                        Cancel
                      </button>
                    </div>
                  </div>
                )
                : (
                  <>
                    <span className="role-profile">
                      {value
                        ? `${value.provider === "claude" ? "Claude" : "Codex"} · ${value.model} · ${value.effort}`
                        : "Not configured"}
                      {pendingReplacement && (
                        <small> · requested change, not active yet</small>
                      )}
                      <TechnicalDetails>
                        <p>
                          requested revision {requested?.revision ?? "draft"} ·
                          effective revision {effective?.revision ?? "none"}
                          {session ? ` · session ${session.id} · ${session.status}` : ""}
                        </p>
                      </TechnicalDetails>
                    </span>
                    {!terminal && value && (
                      <button
                        onClick={() => {
                          setEditing(role);
                          setConfig(value);
                        }}
                      >
                        Edit
                      </button>
                    )}
                    {!terminal && !value && (
                      <small>
                        Finish project setup or save settings for this task
                        before editing this role.
                      </small>
                    )}
                    {!task.archived &&
                      !["done", "cancelled"].includes(task.lifecycle) &&
                      role !== "final_verifier" && exactResume &&
                      !contractUnavailable &&
                      exactResume.operation !== "runtime_probe_resume" && (
                      <button onClick={() => resume(session, exactResume)}>
                        Resume the stopped session
                      </button>
                    )}
                  </>
                )}
              {intent && (
                <small className="badge waiting">
                  Role change: {String(intent.state).replaceAll("_", " ")}
                </small>
              )}
              {productionRestriction && (
                <div className="hint">
                  <span className={`badge ${productionRestriction.status}`}>
                    {productionRestriction.status === "unverified"
                      ? "Needs verification"
                      : "Not supported"}
                  </span>{" "}
                  {productionRestriction.status === "unverified"
                    ? "Verification required: "
                    : "Cannot start: "}
                  {productionRestriction.status === "unverified"
                    ? "Run verification for this agent profile in Project setup or the task's Roles tab before starting work."
                    : "This agent cannot run with the current settings. Review Technical details and choose a supported configuration."}
                  <TechnicalDetails>{productionRestriction.reason}</TechnicalDetails>
                </div>
              )}
              <CompatibilityDetails
                compatibility={compatibility}
                verification={currentProof ? "current_profile" : undefined}
              />
              {requested && preparation?.task_profile_source && (
                <small>
                  Settings from{" "}
                  {preparation.task_profile_source === "project_default"
                    ? "the project"
                    : "this task"}
                </small>
              )}
              {!terminal && requested && preparation?.status === "unverified" &&
                preparation.compatibility?.status === "matched" &&
                !currentProof && (
                <div className="inline-form runtime-proof-control">
                  {value?.provider === "claude" && (
                    <>
                      <label>
                        Live cmux Unix socket for Claude requalification
                        <input
                          value={cmuxSocketPath}
                          onChange={(event) =>
                            setCmuxSocketPath(event.target.value)}
                          placeholder="/absolute/path/to/cmux.sock"
                        />
                      </label>
                      <small>
                        Enter the socket path of the cmux you are running now
                        before preparing the verification. The check does not
                        control cmux; if the socket is missing or refused the
                        verification fails instead of being treated as a
                        sandbox limit.
                      </small>
                    </>
                  )}
                  {stopping && (
                    <small>
                      The {roleLabel(role).toLowerCase()} has exited, but
                      LLMRelay has not yet confirmed that it fully stopped.
                      Verification becomes available once it has.
                    </small>
                  )}
                  {!runtimeAdmission && (
                    <button
                      disabled={!project || stopping}
                      onClick={() =>
                        void prepareRuntime(role, requested.revision)}
                    >
                      Prepare verification
                    </button>
                  )}
                  {runtimeAdmission?.state === "pending_approval" && (
                    <>
                      <small>
                        This verification makes{" "}
                        {runtimeAdmission.fresh_call_count} new agent call
                        {runtimeAdmission.fresh_call_count === 1 ? "" : "s"}.
                        Nothing has started yet.
                      </small>
                      <button
                        disabled={stopping}
                        onClick={() =>
                          void authorizeRuntime(
                            runtimeAdmission.id,
                            runtimeAdmission.scope_hash,
                          )}
                      >
                        Approve verification call
                      </button>
                    </>
                  )}
                  {runtimeAdmission?.probe_state === "authorized" && (
                    <button
                      disabled={stopping}
                      onClick={() =>
                        void runRuntime(runtimeAdmission.id, role, "launch")}
                    >
                      Start verification
                    </button>
                  )}
                  {runtimeAdmission &&
                    ["failed", "stale"].includes(
                      runtimeAdmission.probe_state || runtimeAdmission.state,
                    ) && (
                    <button
                      disabled={!project || stopping}
                      onClick={() =>
                        void prepareRuntime(role, requested.revision)}
                    >
                      Prepare a new verification
                    </button>
                  )}
                  {runtimeAdmission?.probe_state === "running" && (
                    <small>
                      Verification is{" "}
                      {(runtimeAdmission.session_status || "starting").replaceAll("_", " ")}.
                      Watch its output in the Workspace and answer any
                      approvals it asks for there.
                    </small>
                  )}
                  {runtimeExactResume && runtimeSession && (
                    <button
                      onClick={() =>
                        void resume(runtimeSession, runtimeExactResume)}
                    >
                      Resume the verification
                    </button>
                  )}
                  {runtimeAdmission?.probe_state === "evidence_recorded" && (
                    <button
                      onClick={() =>
                        void runRuntime(runtimeAdmission.id, role, "publish")}
                    >
                      Save verification result
                    </button>
                  )}
                  {runtimeAdmission?.failure_reason && (
                    <ErrorNotice error={runtimeAdmission.failure_reason} />
                  )}
                  <small>
                    Choosing a profile starts no paid work and approves nothing
                    else: not installation, implementation, acceptance, retries
                    or blanket permissions.
                  </small>
                </div>
              )}
              {!terminal && pendingReplacement && !replacementEligible && (
                <small className="badge waiting">
                  The requested change is not active yet:{" "}
                  {matchesActivated || taskActivated
                    ? "its verification is missing or out of date."
                    : "verify this profile, then activate it for this task."}
                  {" "}
                  The current settings stay in use until then.
                  {role === "manager"
                    ? " After that, use Change manager at safe boundary or Interrupt and change manager in the task's Controls."
                    : ""}
                </small>
              )}
              {!terminal && requested && !matchesActivated && !taskActivated &&
                (
                  currentProof
                    ? (
                      <button
                        onClick={() => activate(role, requested.revision)}
                      >
                        Activate task profile
                      </button>
                    )
                    : (
                      <small>
                        Not active yet. Verify this exact profile before it can
                        be used for this task; project setup checks alone are
                        not enough.
                      </small>
                    )
                )}
              {!terminal && requested && taskActivated && !matchesActivated && (
                <small className="badge supported">
                  Active for this task only
                </small>
              )}
              {!terminal && switchable && requested && effective &&
                pendingReplacement && replacementEligible &&
                role !== "explorer" && role !== "manager" && (
                <CheckpointSwitch
                  task={task}
                  role={role}
                  snapshots={snapshots}
                  generation={switchable.role_generation_id}
                  sourceSettingsRevision={switchable.config_revision}
                  revision={requested.revision}
                  onChanged={onChanged}
                />
              )}
              {!terminal && stopping && requested && effective &&
                pendingReplacement && replacementEligible &&
                role !== "explorer" && role !== "manager" && (
                <small className="badge waiting">
                  The checkpoint switch becomes available once LLMRelay
                  confirms the current {roleLabel(role).toLowerCase()} has
                  fully stopped.
                </small>
              )}
              {!terminal && role === "explorer" && requested && effective &&
                pendingReplacement && (
                <small className="badge waiting">
                  The Explorer change applies at the next safe point between
                  workflow steps.
                </small>
              )}
              {role === "final_verifier" && session?.status === "exited" && (
                <small>
                  Final verification always runs as a new session, so a stopped
                  one cannot be resumed.
                </small>
              )}
            </article>
          );
        })}
      </div>
      {error && (error === cmuxSocketInstruction
        ? <p className="error" role="alert">{error}</p>
        : <ErrorNotice error={error} />)}
      <p className="hint">
        A change to a running role stays requested until the role reaches a
        safe checkpoint. Resume continues the same agent conversation with the
        same settings. Approvals the agent already granted itself are honored;
        Approvals shows the requests it sends to LLMRelay.
      </p>
    </section>
    </>
  );
}

export function CheckpointSwitch(
  {
    task,
    role,
    snapshots,
    generation,
    sourceSettingsRevision,
    revision,
    onChanged,
  }: {
    task: Task;
    role: Role;
    snapshots: Task["snapshots"];
    generation: string;
    sourceSettingsRevision: number;
    revision: number;
    onChanged: () => void;
  },
) {
  const snapshotKind = role === "manager" || role === "plan_reviewer"
    ? "plan"
    : role === "implementer"
    ? "checkpoint"
    : "candidate";
  const eligibleSnapshots = snapshots.filter((item) => {
    if (
      !item.complete || item.attempt_id !== task.active_attempt?.id ||
      item.kind !== snapshotKind
    ) {
      return false;
    }
    if (role === "manager" || role === "implementer") {
      return item.source_role_generation_id === generation &&
        item.source_settings_revision === sourceSettingsRevision;
    }
    return task.reviews.some((review) =>
      review.role_generation_id === generation &&
      review.candidate_hash === item.manifest_hash &&
      ["delivered", "ambiguous", "finished"].includes(review.delivery_state)
    );
  });
  const [snapshot, setSnapshot] = useState(eligibleSnapshots.at(-1)?.id || "");
  const [handoff, setHandoff] = useState("");
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  const switchOperationStorageKey =
    `llmrelay.role.switch.${task.id}.${role}.${generation}`;
  const switchOperationIdentity = useRef<{ body: string; id: string }>(
    (() => {
      try {
        const value = localStorage.getItem(switchOperationStorageKey);
        return value ? JSON.parse(value) : { body: "", id: "" };
      } catch {
        return { body: "", id: "" };
      }
    })(),
  );
  const freeze = async () => {
    try {
      setStatus("Checking role quiescence and snapshot safety…");
      const frozen = await operation<
        { snapshot_id: string; manifest_hash: string }
      >({
        kind: "snapshot_freeze",
        attempt_id: task.active_attempt?.id,
        snapshot_kind: snapshotKind,
      });
      const verified = await operation<{ verified: boolean }>({
        kind: "snapshot_verify",
        snapshot_id: frozen.snapshot_id,
      });
      if (!verified.verified) {
        throw new Error("The frozen checkpoint did not verify");
      }
      setSnapshot(frozen.snapshot_id);
      setStatus(
        `Safe checkpoint ${frozen.manifest_hash.slice(0, 12)} is ready`,
      );
      onChanged();
    } catch (cause) {
      setStatus("Checkpoint blocked");
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  const submit = async () => {
    try {
      setError("");
      const request = {
        kind: "switch_role_request",
        attempt_id: task.active_attempt?.id,
        role,
        old_generation_id: generation,
        settings_revision: revision,
        snapshot_id: snapshot,
        handoff: { summary: handoff },
        expected_task_version: task.version,
      };
      const prior = switchOperationIdentity.current.id
        ? switchOperationIdentity.current
        : undefined;
      const stable = reuseOperationIdentity(prior, request);
      switchOperationIdentity.current = {
        body: JSON.stringify(stable.request),
        id: stable.id,
      };
      localStorage.setItem(
        switchOperationStorageKey,
        JSON.stringify(switchOperationIdentity.current),
      );
      await operation({ ...stable.request, operation_id: stable.id } as never);
      switchOperationIdentity.current = { body: "", id: "" };
      localStorage.removeItem(switchOperationStorageKey);
      setStatus("Switch requested; old authority is being fenced");
      onChanged();
    } catch (cause) {
      if (!(cause instanceof ApiError && cause.ambiguous)) {
        switchOperationIdentity.current = { body: "", id: "" };
        localStorage.removeItem(switchOperationStorageKey);
      }
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  return (
    <div className="inline-form">
      <button onClick={freeze}>Freeze safe {snapshotKind} checkpoint</button>
      <select
        aria-label={`${roleLabel(role)} checkpoint`}
        value={snapshot}
        onChange={(event) => setSnapshot(event.target.value)}
      >
        <option value="">Choose verified checkpoint</option>
        {eligibleSnapshots.map((item) => (
          <option key={item.id} value={item.id}>
            {item.kind} · {item.manifest_hash.slice(0, 12)}
          </option>
        ))}
      </select>
      <textarea
        aria-label="Checkpoint handoff"
        value={handoff}
        onChange={(event) => setHandoff(event.target.value)}
        placeholder="Immutable checkpoint and pending work"
      />
      <button disabled={!handoff.trim() || !snapshot} onClick={submit}>
        Request checkpoint switch
      </button>
      {status && <small>{status}</small>}
      {error && <ErrorNotice error={error} />}
    </div>
  );
}
