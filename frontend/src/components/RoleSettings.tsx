import { useEffect, useRef, useState } from "react";
import {
  ApiError,
  command,
  getRolePreparations,
  operation,
  reuseOperationIdentity,
} from "../api";
import { ModelSelector } from "./ModelSelector";
import {
  type CapabilityEvidence,
  type CompatibilityExplanation,
  type ContinuationAction,
  type ProductionRoleRestriction,
  type Project,
  type Provider,
  type Role,
  type RoleConfig,
  roleLabel,
  type RolePreparation,
  ROLES,
  type Session,
  type SwitchIntent,
  type Task,
  type TripLaneState,
} from "../types";

const compatibilityActions: Record<CompatibilityExplanation["action"], string> = {
  install_supported_provider_version: "Install a reviewed provider version",
  update_llmrelay_release: "Update LLMRelay for a reviewed contract",
  requalify_exact_profile: "Validate this exact profile",
  inspect_local_provider_configuration: "Inspect local provider configuration",
  contact_operator: "Contact the operator",
};

export function CompatibilityDetails({
  compatibility,
}: {
  compatibility?: CompatibilityExplanation | null;
}) {
  if (!compatibility) return null;
  const candidate = compatibility.status === "matched";
  return (
    <div className="compatibility-details">
      <p>
        <strong>{candidate ? "Reviewed contract candidate" : "Compatibility needs attention"}</strong>
        {" · "}{compatibility.message}
      </p>
      <p className="hint">Next step: {compatibilityActions[compatibility.action]}</p>
      <details>
        <summary>Contract details</summary>
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

export function RoleSettings(
  {
    task,
    sessions,
    switches = [],
    lanes = [],
    project,
    productionRestrictions = [],
    capabilities = [],
    actions = [],
    onChanged,
  }: {
    task: Task;
    project?: Project;
    sessions: Session[];
    switches?: SwitchIntent[];
    lanes?: TripLaneState[];
    productionRestrictions?: ProductionRoleRestriction[];
    capabilities?: CapabilityEvidence[];
    actions?: ContinuationAction[];
    onChanged: () => void;
  },
) {
  const terminal = task.archived ||
    ["done", "cancelled"].includes(task.lifecycle);
  const [editing, setEditing] = useState<Role>();
  const [config, setConfig] = useState<RoleConfig>();
  const [error, setError] = useState("");
  const [cmuxSocketPath, setCmuxSocketPath] = useState("");
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
  const [preparationError, setPreparationError] = useState("");
  const [preparationRefresh, setPreparationRefresh] = useState(0);
  useEffect(() => {
    if (terminal) {
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
  }, [task.id, preparationRevisionKey, preparationRefresh, terminal]);
  const refresh = () => {
    setPreparationRefresh((value) => value + 1);
    onChanged();
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

  return (
    <section className="panel role-settings">
      <header>
        <h3>Host manager + five delegated roles</h3>
        <span>Requested · effective · running</span>
      </header>
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
          const switchable = session &&
              (session.status === "running" || session.status === "exited")
            ? session
            : undefined;
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
            <article key={role}>
              <div>
                <strong>{roleLabel(role)}</strong>
                <small>
                  {running
                    ? `${running.provider} running`
                    : session
                    ? `${session.status}, ${session.readiness}`
                    : effective
                    ? "effective, no attached process"
                    : "requested"}
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
                  <div className="compact-fields">
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
                    <ModelSelector
                      provider={value.provider}
                      value={value.model}
                      onChange={(model) => setConfig({ ...value, model })}
                      label={`${roleLabel(role)} exact model`}
                      knownExactModels={knownExactModels(value.provider)}
                    />
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
                    <button onClick={() => save(role)}>Save request</button>
                  </div>
                )
                : (
                  <>
                    <span>
                      requested rev {requested?.revision ?? "draft"}: {value
                        ? `${value.provider} · ${value.model} · ${value.effort}`
                        : "Not configured"}
                      <br />
                      <small>
                        effective rev {effective?.revision ?? "—"}
                        {session
                          ? ` · ${session.id.slice(0, 8)} · ${session.status}`
                          : ""}
                      </small>
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
                        Activate project profiles or save an explicit task
                        override before editing this role.
                      </small>
                    )}
                    {!task.archived &&
                      !["done", "cancelled"].includes(task.lifecycle) &&
                      role !== "final_verifier" && exactResume &&
                      !contractUnavailable &&
                      exactResume.operation !== "runtime_probe_resume" && (
                      <button onClick={() => resume(session, exactResume)}>
                        Resume exact native session
                      </button>
                    )}
                  </>
                )}
              {intent && (
                <small className="badge waiting">
                  Switch {String(intent.state).replaceAll("_", " ")} · lane{" "}
                  {intent.lane_id === "default"
                    ? "default"
                    : intent.lane_id.slice(0, 8)} · checkpoint{" "}
                  {String(intent.checkpoint_snapshot_id).slice(0, 8)}
                </small>
              )}
              {productionRestriction && (
                <small>
                  <span className={`badge ${productionRestriction.status}`}>
                    Production {productionRestriction.status[0].toUpperCase() +
                      productionRestriction.status.slice(1)}
                  </span>{" "}
                  {productionRestriction.status === "unverified"
                    ? "Validation required: "
                    : "Launch blocked: "}
                  {productionRestriction.reason}
                </small>
              )}
              <CompatibilityDetails compatibility={compatibility} />
              {requested && preparation?.task_profile_source && (
                <small>
                  Authority source: {preparation.task_profile_source ===
                      "project_default"
                    ? "project default"
                    : "task override"} · adapter {preparation.adapter ||
                    "unavailable"}
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
                        Enter the exact human-confirmed live endpoint before
                        preparing the fresh runtime proof. The bounded native
                        check makes no cmux control request and preserves its
                        native exit and stderr; an absent or refused socket
                        cannot count as a sandbox denial.
                      </small>
                    </>
                  )}
                  {!runtimeAdmission && (
                    <button
                      disabled={!project}
                      onClick={() =>
                        void prepareRuntime(role, requested.revision)}
                    >
                      Prepare exact runtime proof
                    </button>
                  )}
                  {runtimeAdmission?.state === "pending_approval" && (
                    <>
                      <small>
                        Scope <code>{runtimeAdmission.scope_hash}</code>{" "}
                        · fresh calls:{" "}
                        {runtimeAdmission.fresh_call_count}. No call has
                        started.
                      </small>
                      <button
                        onClick={() =>
                          void authorizeRuntime(
                            runtimeAdmission.id,
                            runtimeAdmission.scope_hash,
                          )}
                      >
                        Approve scoped runtime call
                      </button>
                    </>
                  )}
                  {runtimeAdmission?.probe_state === "authorized" && (
                    <button
                      onClick={() =>
                        void runRuntime(runtimeAdmission.id, role, "launch")}
                    >
                      Launch bounded runtime probe
                    </button>
                  )}
                  {runtimeAdmission &&
                    ["failed", "stale"].includes(
                      runtimeAdmission.probe_state || runtimeAdmission.state,
                    ) && (
                    <button
                      disabled={!project}
                      onClick={() =>
                        void prepareRuntime(role, requested.revision)}
                    >
                      Prepare corrected runtime verification
                    </button>
                  )}
                  {runtimeAdmission?.probe_state === "running" && (
                    <small>
                      Probe {runtimeAdmission.session_status || "reserved"}{" "}
                      · trust {runtimeAdmission.hook_trust || "pending"}{" "}
                      · session{" "}
                      <code>{runtimeAdmission.session_id}</code>. Use the
                      existing Workspace terminal and permission inbox for
                      observable output and human decisions.
                    </small>
                  )}
                  {runtimeExactResume && runtimeSession && (
                    <button
                      onClick={() =>
                        void resume(runtimeSession, runtimeExactResume)}
                    >
                      Resume same native probe
                    </button>
                  )}
                  {runtimeAdmission?.probe_state === "evidence_recorded" && (
                    <button
                      onClick={() =>
                        void runRuntime(runtimeAdmission.id, role, "publish")}
                    >
                      Publish exact ordinary proof
                    </button>
                  )}
                  {runtimeAdmission?.failure_reason && (
                    <small className="error">
                      {runtimeAdmission.failure_reason}
                    </small>
                  )}
                  <small>
                    Setup receipts do not grant this authority. Profile choice
                    never launches paid work or approves installation,
                    implementation, acceptance, retry, or blanket permissions.
                  </small>
                </div>
              )}
              {!terminal && pendingReplacement && !replacementEligible && (
                <small className="badge waiting">
                  Replacement blocked before authority transfer:{" "}
                  {matchesActivated || taskActivated
                    ? "the exact current capability proof is missing or stale."
                    : "validate the exact ordinary capability, then explicitly activate this task-only profile."}
                  {" "}
                  The current role remains effective.
                  {role === "manager"
                    ? " Complete the exact proof and task-profile activation above, then use Change manager at safe boundary or Interrupt and change manager in Workflow controls."
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
                        Pending activation. Complete the existing capability
                        workflow validation for this exact prepared tuple; setup
                        probes do not grant ordinary task authority.
                      </small>
                    )
                )}
              {!terminal && requested && taskActivated && !matchesActivated && (
                <small className="badge supported">
                  Task-only profile activated · {requested.activation?.adapter}
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
              {!terminal && role === "explorer" && requested && effective &&
                pendingReplacement && (
                <small className="badge waiting">
                  Pending Explorer profile change waits for a service-verified
                  workflow boundary; Explorer evidence cannot authorize its own
                  switch.
                </small>
              )}
              {role === "final_verifier" && session?.status === "exited" && (
                <small>
                  Final verification is fresh-only. An interrupted or incomplete
                  final invocation cannot be resumed.
                </small>
              )}
            </article>
          );
        })}
      </div>
      {error && <p className="error" role="alert">{error}</p>}
      <p className="hint">
        An active change stays requested until a verified immutable checkpoint
        and typed handoff are available. Resume reuses only the exact persisted
        native session, role generation, configuration, and invocation. Codex
        native approvals are honored and may avoid a new inbox item; the inbox
        handles requests the CLI actually emits, and Revoke affects only
        LLMRelay-owned rules.
      </p>
    </section>
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
      {error && <small className="error">{error}</small>}
    </div>
  );
}
