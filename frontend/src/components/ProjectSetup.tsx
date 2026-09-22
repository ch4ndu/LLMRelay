import { useEffect, useMemo, useRef, useState } from "react";
import {
  ApiError,
  command,
  discardUnknownCmuxSurface,
  operation,
  operationId,
  reuseOperationIdentity,
  setCmuxKeyboardControl,
} from "../api";
import {
  cmuxNewestSurface,
  cmuxOutcomeWithDurableSurface,
  cmuxRouteLabel,
  cmuxSurfacePresentation,
  cmuxViewOutcomeFromSurface,
  recordedOutputText,
} from "../cmuxRouting";
import { ModelSelector } from "./ModelSelector";
import type {
  CmuxKeyboardControlAction,
  CmuxSessionSurface,
  CmuxViewOutcome,
  ContinuationAction,
  Project,
  Provider,
  Role,
  RoleConfig,
  RuntimeAdmissionState,
  TripAdapter,
  TripProjectState,
  TripSetupProfile,
  TripSetupProposal,
  TripSetupRecovery,
  TripSetupState,
  TripSetupSuggestion,
} from "../types";

const delegatedRoles: Array<Exclude<Role, "manager">> = [
  "explorer",
  "plan_reviewer",
  "implementer",
  "code_reviewer",
  "final_verifier",
];
const efforts = ["low", "medium", "high", "xhigh", "max", "ultra"];
const builtInAdapters: Record<string, TripAdapter> = {
  llmrelay_codex: {
    kind: "native-agent",
    provider: "codex",
    capabilities: {
      read_only: true,
      workspace_write: true,
      resume: true,
      fresh_session: true,
    },
  },
  llmrelay_claude: {
    kind: "native-agent",
    provider: "claude",
    capabilities: {
      read_only: true,
      workspace_write: true,
      resume: true,
      fresh_session: true,
    },
  },
};
type ProfileDraft = Omit<TripSetupProfile, "provider"> & {
  provider: Provider | "";
};
type SetupDraft = {
  projectName: string;
  guidance: string;
  documentation: string;
  focused: string;
  broad: string;
  cleanup: string;
  verificationContracts: TripSetupProposal["verification_contracts"];
  coverage: "" | "minimal" | "moderate" | "extensive";
  cmux: "auto" | "on" | "off";
  agentsContent: string;
  approveLocalExclude: boolean;
  profiles: Record<Exclude<Role, "manager">, ProfileDraft>;
  profileIds: Record<Exclude<Role, "manager">, string>;
  migrationResolutions: Record<string, string>;
};
type ManagerDraft = { provider: Provider | ""; model: string; effort: string };
type SetupStage =
  | "discover"
  | "project-settings"
  | "agents"
  | "review"
  | "activate";
const setupStages: Array<{ id: SetupStage; label: string }> = [
  { id: "discover", label: "Discover" },
  { id: "project-settings", label: "Project settings" },
  { id: "agents", label: "Agents" },
  { id: "review", label: "Review changes" },
  { id: "activate", label: "Activate" },
];
const stageForProject = (
  setup: TripSetupState | undefined,
  trip: TripProjectState,
): SetupStage => {
  switch (setup?.state) {
    case "draft":
      return setup.proposal_hash ? "agents" : "project-settings";
    case "probing":
      return "agents";
    case "preflight_complete":
    case "finalized":
      return "review";
    case "install_authorized":
    case "applying":
    case "recovery_required":
    case "activated":
      return "activate";
    case "discovery":
    case "workspace_recovery_required":
    case "aborted":
    case "superseded":
      return "discover";
    default:
      return trip.readiness === "ready" ? "activate" : "discover";
  }
};

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);
const hasOnlyKeys = (value: Record<string, unknown>, allowed: string[]) =>
  Object.keys(value).every((key) => allowed.includes(key));
const lines = (value: unknown) =>
  Array.isArray(value)
    ? value.filter((item): item is string => typeof item === "string")
    : [];
const lineText = (value: unknown) => lines(value).join("\n");
const splitLines = (value: string) =>
  value.split("\n").map((item) => item.trim()).filter(Boolean);
const splitCommands = (value: string) =>
  value.split("\n").filter((item) => !!item.trim());
const setupSuggestion = (value: unknown): TripSetupSuggestion | undefined => {
  if (
    !isRecord(value) || Object.keys(value).length === 0 ||
    !hasOnlyKeys(value, [
      "project_name",
      "guidance",
      "documentation",
      "verification",
      "agents_content",
    ])
  ) return undefined;
  const result: TripSetupSuggestion = {};
  let actionable = false;
  if ("project_name" in value) {
    if (
      typeof value.project_name !== "string" ||
      !value.project_name.trim()
    ) return undefined;
    result.project_name = value.project_name;
    actionable = true;
  }
  if ("guidance" in value) {
    if (
      !Array.isArray(value.guidance) ||
      !value.guidance.every((item) => typeof item === "string" && !!item)
    ) return undefined;
    result.guidance = value.guidance as string[];
    actionable = true;
  }
  if ("documentation" in value) {
    if (
      !isRecord(value.documentation) ||
      !hasOnlyKeys(value.documentation, ["no_change_text"]) ||
      Object.keys(value.documentation).length !== 1 ||
      typeof value.documentation.no_change_text !== "string" ||
      !value.documentation.no_change_text.trim()
    ) return undefined;
    result.documentation = {
      no_change_text: value.documentation.no_change_text,
    };
    actionable = true;
  }
  if ("verification" in value) {
    if (
      !isRecord(value.verification) ||
      !hasOnlyKeys(value.verification, ["focused", "broad", "cleanup"])
    ) return undefined;
    const verification: TripSetupSuggestion["verification"] = {};
    for (const category of ["focused", "broad", "cleanup"] as const) {
      if (!(category in value.verification)) continue;
      const commands = value.verification[category];
      if (
        !Array.isArray(commands) ||
        !commands.every((command) =>
          typeof command === "string" && !!command.trim() &&
          !command.includes("\n") && !command.includes("\r")
        )
      ) return undefined;
      verification[category] = commands as string[];
      actionable = true;
    }
    result.verification = verification;
  }
  if ("agents_content" in value) {
    if (
      typeof value.agents_content !== "string" ||
      !value.agents_content.trim()
    ) return undefined;
    result.agents_content = value.agents_content;
    actionable = true;
  }
  return actionable ? result : undefined;
};
const roleName = (role: Role) =>
  role.split("_").map((word) => word[0].toUpperCase() + word.slice(1)).join(
    " ",
  );
const migrationItemDetails = (item: string) => {
  if (item.startsWith("partial:")) {
    return {
      kind: "Incomplete installation",
      path: item.slice("partial:".length),
    };
  }
  if (item.startsWith("alternate_root:")) {
    return {
      kind: "Additional skills folder",
      path: item.slice("alternate_root:".length),
    };
  }
  return { kind: "Existing file conflict", path: item };
};
const fixedContract = (role: Exclude<Role, "manager">) => ({
  authority: role === "implementer"
    ? "workspace-write" as const
    : "read-only" as const,
  session: role === "final_verifier" ? "fresh" as const : "retained" as const,
});
const emptyProfiles = () =>
  Object.fromEntries(delegatedRoles.map((role) => [
    role,
    {
      adapter: "",
      provider: "",
      model: "",
      effort: "",
      service_tier: "",
      ...fixedContract(role),
    },
  ])) as SetupDraft["profiles"];

type PersistedSetupDraft = {
  version: 1;
  scope: string;
  draft: SetupDraft;
  manager: ManagerDraft;
  editingRevision: boolean;
  activeStage: SetupStage;
};
const restoredSetupStage = (
  persisted: PersistedSetupDraft | undefined,
  setup: TripSetupState | undefined,
  trip: TripProjectState,
) => {
  const serverStage = stageForProject(setup, trip);
  if (!persisted) return serverStage;
  const serverStageRequired = !!setup?.recoveries?.length || [
    "workspace_recovery_required",
    "install_authorized",
    "applying",
    "recovery_required",
  ].includes(setup?.state || "");
  if (serverStageRequired) return serverStage;
  return persisted.activeStage;
};

const setupDraftProjectPrefix = (projectId: string) =>
  `llmrelay.trip.draft.v1.${encodeURIComponent(projectId)}.`;
const setupDraftStorageKey = (
  project: Project,
  setup?: TripSetupState,
) => {
  const revision = setup
    ? `${setup.setup_operation_id}:${setup.proposal_hash || "uncommitted"}`
    : `project:${project.version}`;
  return `${setupDraftProjectPrefix(project.id)}${
    encodeURIComponent(revision)
  }`;
};
const removePersistedSetupDraft = (scope: string) => {
  try {
    localStorage.removeItem(scope);
  } catch {
    // Browser storage availability never gates the server-backed workflow.
  }
};
const stringRecord = (value: unknown) =>
  isRecord(value) &&
  Object.values(value).every((item) => typeof item === "string");
const validProfileDraft = (
  value: unknown,
  role: Exclude<Role, "manager">,
): value is ProfileDraft => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "adapter",
      "provider",
      "model",
      "effort",
      "service_tier",
      "authority",
      "session",
    ])
  ) return false;
  const contract = fixedContract(role);
  return typeof value.adapter === "string" &&
    ["", "codex", "claude"].includes(String(value.provider)) &&
    typeof value.model === "string" && typeof value.effort === "string" &&
    (value.service_tier === undefined || value.service_tier === null ||
      typeof value.service_tier === "string") &&
    value.authority === contract.authority &&
    value.session === contract.session;
};
const validSetupDraft = (value: unknown): value is SetupDraft => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "projectName",
      "guidance",
      "documentation",
      "focused",
      "broad",
      "cleanup",
      "verificationContracts",
      "coverage",
      "cmux",
      "agentsContent",
      "approveLocalExclude",
      "profiles",
      "profileIds",
      "migrationResolutions",
    ])
  ) return false;
  if (
    typeof value.projectName !== "string" ||
    typeof value.guidance !== "string" ||
    typeof value.documentation !== "string" ||
    typeof value.focused !== "string" || typeof value.broad !== "string" ||
    typeof value.cleanup !== "string" ||
    !isRecord(value.verificationContracts) ||
    !Object.values(value.verificationContracts).every(isRecord) ||
    !["", "minimal", "moderate", "extensive"].includes(
      String(value.coverage),
    ) || !["auto", "on", "off"].includes(String(value.cmux)) ||
    typeof value.agentsContent !== "string" ||
    typeof value.approveLocalExclude !== "boolean" ||
    !isRecord(value.profiles) || !isRecord(value.profileIds) ||
    !hasOnlyKeys(value.profiles, delegatedRoles) ||
    !hasOnlyKeys(value.profileIds, delegatedRoles) ||
    !stringRecord(value.migrationResolutions)
  ) return false;
  const profiles = value.profiles;
  const profileIds = value.profileIds;
  return delegatedRoles.every((role) =>
    validProfileDraft(profiles[role], role) &&
    typeof profileIds[role] === "string"
  );
};
const loadPersistedSetupDraft = (scope: string) => {
  try {
    const value = JSON.parse(localStorage.getItem(scope) || "null");
    if (
      !isRecord(value) ||
      !hasOnlyKeys(value, [
        "version",
        "scope",
        "draft",
        "manager",
        "editingRevision",
        "activeStage",
      ]) || value.version !== 1 || value.scope !== scope ||
      !validSetupDraft(value.draft) || !isRecord(value.manager) ||
      !hasOnlyKeys(value.manager, ["provider", "model", "effort"]) ||
      !["", "codex", "claude"].includes(String(value.manager.provider)) ||
      typeof value.manager.model !== "string" ||
      typeof value.manager.effort !== "string" ||
      typeof value.editingRevision !== "boolean" ||
      !setupStages.some((stage) => stage.id === value.activeStage)
    ) {
      removePersistedSetupDraft(scope);
      return undefined;
    }
    return value as PersistedSetupDraft;
  } catch {
    removePersistedSetupDraft(scope);
    return undefined;
  }
};
const retireOtherSetupDrafts = (projectId: string, currentScope: string) => {
  try {
    const prefix = setupDraftProjectPrefix(projectId);
    for (let index = localStorage.length - 1; index >= 0; index--) {
      const key = localStorage.key(index);
      if (key?.startsWith(prefix) && key !== currentScope) {
        localStorage.removeItem(key);
      }
    }
  } catch {
    // The in-memory draft remains usable when browser storage is unavailable.
  }
};

function hydratedDraft(project: Project, setup?: TripSetupState): SetupDraft {
  const saved = setup?.proposal;
  const detected = project.trip?.detected?.configuration;
  const source = (saved || detected || {}) as Record<string, unknown>;
  const verification = (source.verification || {}) as Record<string, unknown>;
  const testing = (source.testing || {}) as Record<string, unknown>;
  const observability = (source.observability || {}) as Record<string, unknown>;
  const roles = (source.roles || {}) as Record<string, { profile?: string }>;
  const profiles = (source.profiles || {}) as Record<string, TripSetupProfile>;
  const resultProfiles = emptyProfiles();
  const profileIds = {} as SetupDraft["profileIds"];
  for (const role of delegatedRoles) {
    const profileId = roles[role]?.profile || role;
    profileIds[role] = profileId;
    const profile = profiles[profileId];
    if (profile) resultProfiles[role] = { ...profile };
  }
  const migration = saved?.canonical_migration;
  return {
    projectName: typeof source.project_name === "string"
      ? source.project_name
      : project.display_name,
    guidance: lineText(source.guidance || ["AGENTS.md"]),
    documentation:
      typeof (source.documentation as Record<string, unknown> | undefined)
          ?.no_change_text === "string"
        ? String(
          (source.documentation as Record<string, unknown>).no_change_text,
        )
        : "",
    focused: lineText(verification.focused),
    broad: lineText(verification.broad),
    cleanup: lineText(verification.cleanup),
    verificationContracts: saved?.verification_contracts || {},
    coverage:
      ["minimal", "moderate", "extensive"].includes(String(testing.coverage))
        ? testing.coverage as SetupDraft["coverage"]
        : "",
    cmux: ["auto", "on", "off"].includes(String(observability.cmux))
      ? observability.cmux as SetupDraft["cmux"]
      : "off",
    agentsContent: saved?.agents_file.approved_content ??
      setup?.agents_file.content ?? "",
    approveLocalExclude: saved?.local_exclude.approved ?? false,
    profiles: resultProfiles,
    profileIds,
    migrationResolutions: migration?.resolutions || {},
  };
}

export function ProjectSetup({
  project,
  setup,
  cmuxSurfaces = {},
  onChanged,
  onViewSession,
}: {
  project: Project;
  setup?: TripSetupState;
  cmuxSurfaces?: Record<string, CmuxSessionSurface>;
  onChanged: () => Promise<void> | void;
  onViewSession: (
    sessionId: string,
  ) => Promise<CmuxViewOutcome>;
}) {
  const selectedManager = setup?.selected_profiles.find((item) =>
    item.role === "manager"
  )?.profile as RoleConfig | undefined;
  const managerControl = setup?.manager_control;
  const trip: TripProjectState = project.trip || {
    readiness: "not_initialized",
    reason: "Project setup has not been inspected",
    detected_installation: "unknown",
  };
  const setupDraftKey = setupDraftStorageKey(project, setup);
  const persistedSetupDraft = useMemo(
    () => loadPersistedSetupDraft(setupDraftKey),
    [setupDraftKey],
  );
  const [manager, setManager] = useState<ManagerDraft>(() =>
    persistedSetupDraft?.manager || selectedManager || {
      provider: "",
      model: "",
      effort: "",
    }
  );
  const [draft, setDraft] = useState<SetupDraft>(() =>
    persistedSetupDraft?.draft || hydratedDraft(project, setup)
  );
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [cmuxSocketPath, setCmuxSocketPath] = useState("");
  const [editingRevision, setEditingRevision] = useState(
    persistedSetupDraft?.editingRevision || false,
  );
  const stageSignal = `${setup?.state || "none"}:${
    setup?.proposal_hash || "none"
  }:${trip.readiness}`;
  const [activeStage, setActiveStage] = useState<SetupStage>(() =>
    restoredSetupStage(persistedSetupDraft, setup, trip)
  );
  const stageSignalRef = useRef(stageSignal);
  const setupDraftKeyRef = useRef(setupDraftKey);
  const skipDraftPersistence = useRef(false);
  const operationStorageKey = `llmrelay.trip.operations.${project.id}`;
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
  const commandGuard = useRef(false);
  const dispatchGuard = useRef(false);
  useEffect(() => {
    retireOtherSetupDrafts(project.id, setupDraftKey);
    if (setupDraftKeyRef.current === setupDraftKey) return;
    removePersistedSetupDraft(setupDraftKeyRef.current);
    setupDraftKeyRef.current = setupDraftKey;
    skipDraftPersistence.current = true;
    setDraft(persistedSetupDraft?.draft || hydratedDraft(project, setup));
    setManager(
      persistedSetupDraft?.manager || selectedManager || {
        provider: "",
        model: "",
        effort: "",
      },
    );
    setEditingRevision(persistedSetupDraft?.editingRevision || false);
    setActiveStage(
      restoredSetupStage(persistedSetupDraft, setup, trip),
    );
    stageSignalRef.current = stageSignal;
  }, [
    project.id,
    setupDraftKey,
    persistedSetupDraft,
    selectedManager?.provider,
    selectedManager?.model,
    selectedManager?.effort,
    setup,
    trip,
    stageSignal,
  ]);
  useEffect(() => {
    if (skipDraftPersistence.current) {
      skipDraftPersistence.current = false;
      return;
    }
    if (setupDraftKeyRef.current !== setupDraftKey) return;
    const value: PersistedSetupDraft = {
      version: 1,
      scope: setupDraftKey,
      draft,
      manager,
      editingRevision,
      activeStage,
    };
    try {
      localStorage.setItem(setupDraftKey, JSON.stringify(value));
    } catch {
      // The in-memory draft remains usable when browser storage is unavailable.
    }
  }, [setupDraftKey, draft, manager, editingRevision, activeStage]);
  useEffect(() => {
    if (stageSignalRef.current === stageSignal) return;
    stageSignalRef.current = stageSignal;
    setActiveStage(stageForProject(setup, trip));
  }, [stageSignal, setup, trip]);

  const detected = trip.detected || {};
  const detectedKind = String(
    detected.kind || trip.detected_installation || "unknown",
  );
  const migrationItems = useMemo(() => [
    ...((detected.conflicts as string[] | undefined) || []),
    ...((detected.partial_paths as string[] | undefined) || []).map((path) =>
      `partial:${path}`
    ),
    ...((detected.alternate_roots as string[] | undefined) || []).map((path) =>
      `alternate_root:${path}`
    ),
  ], [detected]);
  const existingAdapters =
    (setup?.proposal?.adapters || detected.adapters || { adapters: {} })
      .adapters || {};
  const adapterOptions = { ...builtInAdapters, ...existingAdapters };

  const runTrip = async (key: string, action: Record<string, unknown>) => {
    if (commandGuard.current) return;
    commandGuard.current = true;
    const stable = reuseOperationIdentity(
      commandIdentities.current.get(key),
      action,
    );
    const { id, request } = stable;
    commandIdentities.current.set(key, { body: JSON.stringify(request), id });
    localStorage.setItem(
      operationStorageKey,
      JSON.stringify([...commandIdentities.current]),
    );
    setBusy(key);
    setError("");
    setNotice("");
    try {
      const response = await command({
        kind: "trip",
        operation_id: id,
        ...request,
      });
      commandIdentities.current.delete(key);
      localStorage.setItem(
        operationStorageKey,
        JSON.stringify([...commandIdentities.current]),
      );
      setNotice(String(response.result.state || "Saved"));
      await onChanged();
      return response.result;
    } catch (cause) {
      setError(
        cause instanceof ApiError && cause.ambiguous
          ? `${cause.message} Refresh and reconcile before retrying.`
          : cause instanceof Error
          ? cause.message
          : String(cause),
      );
      await onChanged();
    } finally {
      commandGuard.current = false;
      setBusy("");
    }
  };
  const runBrowserOperation = async (
    key: string,
    request: Record<string, unknown> & { kind: string },
  ) => {
    const stable = reuseOperationIdentity(
      commandIdentities.current.get(key),
      request,
    );
    const { id, request: stableRequest } = stable;
    commandIdentities.current.set(key, {
      body: JSON.stringify(stableRequest),
      id,
    });
    localStorage.setItem(
      operationStorageKey,
      JSON.stringify([...commandIdentities.current]),
    );
    try {
      const result = await operation(
        { ...stableRequest, operation_id: id } as never,
      );
      commandIdentities.current.delete(key);
      localStorage.setItem(
        operationStorageKey,
        JSON.stringify([...commandIdentities.current]),
      );
      return result;
    } catch (cause) {
      if (!(cause instanceof ApiError && cause.ambiguous)) {
        commandIdentities.current.delete(key);
        localStorage.setItem(
          operationStorageKey,
          JSON.stringify([...commandIdentities.current]),
        );
      }
      throw cause;
    }
  };
  const clearPersistedSetupDraft = (suppressNextWrite = false) => {
    removePersistedSetupDraft(setupDraftKey);
    if (suppressNextWrite) skipDraftPersistence.current = true;
  };
  const inspect = () =>
    runTrip("inspect", { action: "inspect_project", project_id: project.id });
  const begin = () =>
    runTrip("begin", {
      action: "begin_setup",
      project_id: project.id,
      expected_project_version: project.version,
      host_manager: manager,
    });
  const stopSetupManager = () =>
    setup && runTrip("stop-setup-manager", {
      action: "stop_setup_manager",
      setup_operation_id: setup.setup_operation_id,
      expected_project_version: project.version,
    });
  const changeSetupManager = () =>
    setup && runTrip("change-setup-manager", {
      action: "change_setup_manager",
      setup_operation_id: setup.setup_operation_id,
      expected_project_version: project.version,
      host_manager: manager,
    });
  const proposal = (): TripSetupProposal | undefined => {
    if (!manager.provider || !draft.coverage) return undefined;
    const profiles: Record<string, TripSetupProfile> = {};
    const roles = {} as TripSetupProposal["roles"];
    const proposalAdapters: Record<string, TripAdapter> = {
      ...existingAdapters,
    };
    for (const role of delegatedRoles) {
      const profile = draft.profiles[role];
      if (profile.service_tier?.trim()) {
        return undefined;
      }
      if (!profile.provider) return undefined;
      const profileId = draft.profileIds[role] || role;
      profiles[profileId] = {
        ...profile,
        provider: profile.provider,
        service_tier: profile.service_tier || null,
      };
      roles[role] = { profile: profileId };
      if (
        !proposalAdapters[profile.adapter] && builtInAdapters[profile.adapter]
      ) {
        proposalAdapters[profile.adapter] = builtInAdapters[profile.adapter];
      }
    }
    const canonicalMigration = migrationItems.length
      ? {
        observed_kind: detectedKind,
        observation_hash: String(detected.observation_hash || ""),
        unresolved_conflicts: migrationItems.filter((item) =>
          !draft.migrationResolutions[item]
        ),
        resolutions: draft.migrationResolutions,
      }
      : undefined;
    return {
      project_name: draft.projectName.trim(),
      host_manager: manager as RoleConfig,
      guidance: Array.from(
        new Set(["AGENTS.md", ...splitLines(draft.guidance)]),
      ),
      documentation: { no_change_text: draft.documentation },
      verification: {
        focused: splitCommands(draft.focused),
        broad: splitCommands(draft.broad),
        cleanup: splitCommands(draft.cleanup),
      },
      verification_contracts: draft.verificationContracts,
      testing: { coverage: draft.coverage },
      observability: { cmux: draft.cmux },
      roles,
      profiles,
      adapters: { adapters: proposalAdapters },
      agents_file: {
        relative_path: "AGENTS.md",
        approved_content: draft.agentsContent,
      },
      local_exclude: {
        pattern: "/.local/trip-explorer/",
        approved: draft.approveLocalExclude,
      },
      ...(canonicalMigration
        ? { canonical_migration: canonicalMigration }
        : {}),
    };
  };
  const proposalValue = proposal();
  const proposalProblem = (() => {
    if (managerControl?.hold) {
      return "Replace the held discovery manager and explicitly launch the fresh revision before saving a proposal.";
    }
    if (!manager.provider || !manager.model.trim() || !manager.effort) {
      return "Complete the exact host manager profile.";
    }
    if (!setup?.probe_receipts.some((receipt) => receipt.role === "manager")) {
      return "Complete the retained host-manager discovery and record its role-bound receipt first.";
    }
    if (!draft.projectName.trim()) return "Enter the project name.";
    if (!draft.coverage) {
      return "Choose minimal, moderate, or extensive test coverage.";
    }
    if (!draft.documentation.trim()) {
      return "Enter the documentation no-change text.";
    }
    if (!draft.agentsContent.trim()) {
      return "Review and approve the exact AGENTS.md content.";
    }
    for (const role of delegatedRoles) {
      const profile = draft.profiles[role];
      if (profile.service_tier?.trim()) {
        return `${
          roleName(role)
        } has an unsupported service tier. Clear it before preflight.`;
      }
      if (
        !profile.adapter || !profile.provider || !profile.model.trim() ||
        !profile.effort
      ) return `Complete the exact ${roleName(role)} profile.`;
      const adapter = adapterOptions[profile.adapter];
      if (
        !adapter ||
        !["native-agent", "builtin-cli"].includes(String(adapter.kind))
      ) {
        return `${
          roleName(role)
        } uses a preserved adapter that this release cannot launch.`;
      }
      if (adapter.provider !== profile.provider) {
        return `${
          roleName(role)
        } provider does not match its selected adapter.`;
      }
      const requiredAuthority = role === "implementer"
        ? "workspace_write"
        : "read_only";
      const requiredSession = role === "final_verifier"
        ? "fresh_session"
        : "resume";
      if (
        adapter.capabilities?.[requiredAuthority] !== true ||
        adapter.capabilities?.[requiredSession] !== true
      ) {
        return `${
          roleName(role)
        } uses an adapter that cannot enforce its required ${
          fixedContract(role).authority
        } and ${fixedContract(role).session} contract.`;
      }
    }
    if (migrationItems.length && !detected.observation_hash) {
      return "Run discovery again to bind the existing-installation observation before choosing migration resolutions.";
    }
    if (migrationItems.some((item) => !draft.migrationResolutions[item])) {
      return "Choose an explicit preservation resolution for every existing-installation conflict.";
    }
    return "";
  })();
  const saveDraft = async () => {
    if (!setup || !proposalValue) return;
    const result = await runTrip("save-draft", {
      action: editingRevision ? "revise_setup_draft" : "save_setup_draft",
      setup_operation_id: setup.setup_operation_id,
      expected_project_version: project.version,
      proposal: proposalValue,
    });
    if (result) clearPersistedSetupDraft();
  };
  const authorizeProbes = () =>
    setup?.proposal_hash && runTrip("authorize-probes", {
      action: "authorize_setup_probes",
      setup_operation_id: setup.setup_operation_id,
      proposal_hash: setup.proposal_hash,
    });
  const authorizeInstall = () =>
    setup?.proposal_hash && setup.approved_preimages_hash &&
    setup.final_source_set_hash &&
    runTrip("authorize-install", {
      action: "authorize_installation",
      setup_operation_id: setup.setup_operation_id,
      proposal_hash: setup.proposal_hash,
      approved_preimages_hash: setup.approved_preimages_hash,
      final_source_set_hash: setup.final_source_set_hash,
    });
  const finalizeInstall = () =>
    setup?.proposal_hash && runTrip("finalize-install", {
      action: "finalize_installation",
      setup_operation_id: setup.setup_operation_id,
      proposal_hash: setup.proposal_hash,
    });
  const applyInstall = () =>
    setup?.proposal_hash && runTrip("apply-install", {
      action: "apply_installation",
      setup_operation_id: setup.setup_operation_id,
      proposal_hash: setup.proposal_hash,
    });
  const recoverInstall = () =>
    setup && runTrip("recover-install", {
      action: "recover_installation",
      setup_operation_id: setup.setup_operation_id,
    });
  const adopt = () =>
    setup?.proposal && runTrip("adopt", {
      action: "adopt_installation",
      project_id: project.id,
      expected_project_version: project.version,
      configuration: setup.proposal,
    });
  const prepareRuntime = (role?: Role) =>
    runTrip(
      role ? `prepare-runtime:${role}` : "prepare-runtime",
      {
        action: "prepare_runtime_admission",
        project_id: project.id,
        expected_version: project.version,
        ...(role ? { role } : {}),
        ...(cmuxSocketPath.trim()
          ? { cmux_socket_path: cmuxSocketPath.trim() }
          : {}),
      },
    );
  const authorizeRuntime = (admissionId: string, scopeHash: string) =>
    runTrip(`authorize-runtime:${admissionId}`, {
      action: "authorize_runtime_admission",
      admission_id: admissionId,
      scope_hash: scopeHash,
    });
  const publishRuntime = (admissionId: string, role: Role) =>
    runTrip(`publish-runtime:${admissionId}:${role}`, {
      action: "publish_runtime_proof",
      admission_id: admissionId,
      role,
    });
  const runtimeDispatch = async (
    admissionId: string,
    role: Role,
    resume = false,
  ) => {
    if (dispatchGuard.current) return;
    dispatchGuard.current = true;
    setBusy(`runtime:${admissionId}:${role}`);
    setError("");
    try {
      await runBrowserOperation(
        `runtime:${admissionId}:${role}:${resume ? "resume" : "launch"}`,
        resume
          ? {
            kind: "runtime_probe_resume",
            admission_id: admissionId,
            role,
          }
          : {
            kind: "runtime_probe_launch",
            admission_id: admissionId,
            role,
          },
      );
      await onChanged();
    } catch (cause) {
      setError(
        cause instanceof ApiError && cause.ambiguous
          ? `${cause.message} Refresh and reconcile before retrying.`
          : cause instanceof Error
          ? cause.message
          : String(cause),
      );
      await onChanged();
    } finally {
      dispatchGuard.current = false;
      setBusy("");
    }
  };
  const dispatch = async (
    attemptId: string,
    role: Role,
    sessionId?: string,
  ) => {
    if (dispatchGuard.current) return;
    dispatchGuard.current = true;
    setBusy(`${attemptId}:${role}`);
    setError("");
    try {
      if (sessionId) {
        await runBrowserOperation(`role-resume:${sessionId}`, {
          kind: "role_resume",
          session_id: sessionId,
          prompt: "",
        });
      } else {
        await runBrowserOperation(`setup-dispatch:${attemptId}:${role}`, {
          kind: "trip_setup_dispatch",
          attempt_id: attemptId,
          role,
        });
      }
      await onChanged();
    } catch (cause) {
      setError(
        cause instanceof ApiError && cause.ambiguous
          ? `${cause.message} Refresh and reconcile before retrying.`
          : cause instanceof Error
          ? cause.message
          : String(cause),
      );
      await onChanged();
    } finally {
      dispatchGuard.current = false;
      setBusy("");
    }
  };
  const dispatchFreshSetup = async (action: ContinuationAction) => {
    const attemptId = action.binding.attempt_id;
    const role = action.binding.role;
    const rejectionEventId = action.binding.rejection_event_id;
    if (
      typeof attemptId !== "string" ||
      (role !== "manager" &&
        !delegatedRoles.includes(role as Exclude<Role, "manager">)) ||
      typeof rejectionEventId !== "string"
    ) {
      setError(
        "The fresh setup action is missing its immutable authority binding. Refresh and reconcile before retrying.",
      );
      return;
    }
    if (dispatchGuard.current) return;
    dispatchGuard.current = true;
    setBusy(`fresh-setup:${attemptId}:${role}`);
    setError("");
    try {
      await runBrowserOperation(`setup-fresh-resume:${rejectionEventId}`, {
        kind: "trip_setup_dispatch",
        attempt_id: attemptId,
        role: role as Role,
        fresh_resume_rejection: action.binding,
      });
      await onChanged();
    } catch (cause) {
      setError(
        cause instanceof ApiError && cause.ambiguous
          ? `${cause.message} Refresh and reconcile before retrying.`
          : cause instanceof Error
          ? cause.message
          : String(cause),
      );
      await onChanged();
    } finally {
      dispatchGuard.current = false;
      setBusy("");
    }
  };

  const managerValid = !!manager.provider && !!manager.model.trim() &&
    !!manager.effort;
  const managerMatchesCurrent = !!managerControl?.current &&
    manager.provider === managerControl.current.provider &&
    manager.model.trim() === managerControl.current.model &&
    manager.effort === managerControl.current.effort;
  const managerProfileLabel = (profile?: RoleConfig | null) =>
    profile
      ? `provider ${profile.provider} · model ${profile.model} · effort ${profile.effort}`
      : "not launched";
  const knownExactModels = (provider: Provider | "") =>
    provider
      ? [
        manager.provider === provider ? manager.model : "",
        ...delegatedRoles.map((role) => {
          const profile = draft.profiles[role];
          return profile.provider === provider ? profile.model : "";
        }),
        ...(setup?.selected_profiles || []).map((selection) => {
          const profile = selection.profile;
          return profile?.provider === provider ? profile.model : "";
        }),
      ]
      : [];
  const updateProfile = (
    role: Exclude<Role, "manager">,
    next: ProfileDraft,
  ) => {
    setDraft((current) => {
      const profileId = current.profileIds[role];
      const profiles = { ...current.profiles };
      for (const candidate of delegatedRoles) {
        if (current.profileIds[candidate] === profileId) {
          profiles[candidate] = next;
        }
      }
      return { ...current, profiles };
    });
  };
  const receiptsByRole = new Map(
    setup?.probe_receipts.filter((receipt) =>
      receipt.result === "success" && receipt.capability_key &&
      receipt.adapter_hash
    ).map((receipt) => [receipt.role, receipt]) || [],
  );
  const managerReceipts =
    setup?.probe_receipts.filter((receipt) => receipt.role === "manager") || [];
  const discoveryManagerHeld = !!managerControl?.hold;
  const usableManagerReceipts = discoveryManagerHeld ? [] : managerReceipts;
  const hasUsableManagerReceipt = receiptsByRole.has("manager") &&
    !discoveryManagerHeld;
  const discoverySuggestion = usableManagerReceipts
    .map((receipt) => setupSuggestion(receipt.evidence.setup_proposal_summary))
    .find((suggestion) => suggestion !== undefined);
  const suggestedPolicyLabels = discoverySuggestion
    ? [
      discoverySuggestion.project_name !== undefined ? "project name" : "",
      discoverySuggestion.guidance !== undefined ? "guidance paths" : "",
      discoverySuggestion.documentation !== undefined
        ? "documentation no-change text"
        : "",
      discoverySuggestion.verification?.focused !== undefined
        ? "focused commands"
        : "",
      discoverySuggestion.verification?.broad !== undefined
        ? "broad commands"
        : "",
      discoverySuggestion.verification?.cleanup !== undefined
        ? "cleanup commands"
        : "",
      discoverySuggestion.agents_content !== undefined ? "AGENTS.md draft" : "",
    ].filter(Boolean)
    : [];
  const applyDiscoverySuggestion = () => {
    if (!discoverySuggestion) return;
    const hasVerificationSuggestion =
      discoverySuggestion.verification?.focused !== undefined ||
      discoverySuggestion.verification?.broad !== undefined ||
      discoverySuggestion.verification?.cleanup !== undefined;
    setDraft((current) => ({
      ...current,
      projectName: discoverySuggestion.project_name ?? current.projectName,
      guidance: discoverySuggestion.guidance !== undefined
        ? discoverySuggestion.guidance.join("\n")
        : current.guidance,
      documentation: discoverySuggestion.documentation?.no_change_text ??
        current.documentation,
      focused: discoverySuggestion.verification?.focused !== undefined
        ? discoverySuggestion.verification.focused.join("\n")
        : current.focused,
      broad: discoverySuggestion.verification?.broad !== undefined
        ? discoverySuggestion.verification.broad.join("\n")
        : current.broad,
      cleanup: discoverySuggestion.verification?.cleanup !== undefined
        ? discoverySuggestion.verification.cleanup.join("\n")
        : current.cleanup,
      verificationContracts: hasVerificationSuggestion
        ? {}
        : current.verificationContracts,
      agentsContent: discoverySuggestion.agents_content ??
        current.agentsContent,
    }));
    setError("");
    setNotice(
      `Applied ${
        suggestedPolicyLabels.join(", ")
      } to the local draft. Nothing was saved or approved.`,
    );
  };
  const allReceipts = ["manager", ...delegatedRoles].every((role) =>
    receiptsByRole.has(role as Role)
  );
  const revisionEligible = !!setup && [
    "probing",
    "preflight_complete",
    "finalized",
    "install_authorized",
    "activated",
  ].includes(setup.state);
  const showProposalEditor = !!setup &&
    !discoveryManagerHeld &&
    (editingRevision || ["discovery", "draft"].includes(setup.state));
  const runtimeAdmission = setup?.runtime_admissions?.find((item) =>
    !item.task_id
  );
  const setupContinuationActions = (setup?.continuation_actions || []).filter(
    (action) => action.operation === "trip_setup_dispatch",
  );
  const recoverSetupApply = (setup?.continuation_actions || []).find(
    (action) =>
      action.kind === "recover_setup_apply" &&
      action.operation === "recover_installation",
  );
  const serverStage = stageForProject(setup, trip);
  const stageProgressLabel = (stage: SetupStage) => {
    if (editingRevision) {
      if (stage === "project-settings") return "Correction draft";
      if (stage === "activate") return "Active revision";
      return stage === activeStage ? "Viewing" : "Available";
    }
    switch (stage) {
      case "discover":
        return hasUsableManagerReceipt
          ? "Complete"
          : serverStage === stage
          ? "Current"
          : "Available";
      case "project-settings":
        return setup?.proposal_hash
          ? "Saved"
          : usableManagerReceipts.length > 0
          ? "Next"
          : serverStage === stage
          ? "Current"
          : "Upcoming";
      case "agents":
        return allReceipts
          ? "Verified"
          : serverStage === stage
          ? "Current"
          : setup?.proposal_hash
          ? "Next"
          : "Upcoming";
      case "review":
        return [
            "install_authorized",
            "applying",
            "recovery_required",
            "activated",
          ].includes(setup?.state || "")
          ? "Approved"
          : serverStage === stage
          ? "Current"
          : allReceipts
          ? "Next"
          : "Upcoming";
      case "activate":
        return trip.readiness === "ready"
          ? "Active"
          : serverStage === stage
          ? "Current"
          : "Upcoming";
    }
  };
  const proposalProblemStage: SetupStage = proposalProblem.includes(
      "host-manager discovery",
    )
    ? "discover"
    : proposalProblem.includes("Run discovery again")
    ? "discover"
    : proposalProblem.includes("host manager profile")
    ? "project-settings"
    : proposalProblem.includes("profile") ||
        proposalProblem.includes("adapter") ||
        proposalProblem.includes("provider") ||
        proposalProblem.includes("service tier")
    ? "agents"
    : proposalProblem.includes("preservation") ||
        proposalProblem.includes("existing-installation")
    ? "review"
    : "project-settings";
  return (
    <section
      className="trip-setup"
      aria-label={`TRIP Explorer setup for ${project.display_name}`}
    >
      <header>
        <div>
          <span className="eyebrow">Guided project setup</span>
          <h4>Set up {project.display_name}</h4>
          <p>{trip.reason}</p>
        </div>
        <span
          className={`badge ${
            trip.readiness === "ready" ? "supported" : "waiting"
          }`}
        >
          {trip.readiness.replaceAll("_", " ")}
        </span>
      </header>
      <div className="setup-summary">
        <span>
          Detected: <strong>{detectedKind.replaceAll("_", " ")}</strong>
        </span>
        <span>
          Workflow: <strong>{trip.workflow_id || "not activated"}</strong>
        </span>
        <span>
          Source:{" "}
          <code>{trip.upstream_source_hash?.slice(0, 12) || "pending"}</code>
        </span>
      </div>
      <nav className="setup-stage-nav" aria-label="Project setup stages">
        {setupStages.map((stage, index) => (
          <button
            type="button"
            key={stage.id}
            className={activeStage === stage.id ? "active" : ""}
            aria-current={activeStage === stage.id ? "step" : undefined}
            onClick={() => setActiveStage(stage.id)}
          >
            <span aria-hidden="true">{index + 1}</span>
            <strong>{stage.label}</strong>
            <small>{stageProgressLabel(stage.id)}</small>
          </button>
        ))}
      </nav>
      {(detected.conflicts as string[] | undefined)?.map((item) => (
        <p className="warning" key={item}>{item}</p>
      ))}
      {detected.configuration_error && (
        <p className="error">{detected.configuration_error}</p>
      )}
      {activeStage === "discover" && (
        <div className="setup-stage-heading">
          <div>
            <span className="eyebrow">Stage 1 of 5</span>
            <h5>Discover the existing project setup</h5>
            <p className="hint">
              Inspect the repository, then let the selected manager suggest a
              local draft. Discovery does not authorize installation.
            </p>
          </div>
          <button disabled={!!busy} onClick={() => void inspect()}>
            Inspect existing setup
          </button>
        </div>
      )}
      {activeStage === "discover" && !setup && trip.readiness !== "ready" && (
        <fieldset className="setup-step">
          <legend>Select the host manager for discovery</legend>
          <p className="hint">
            This is the app-host manager, separate from the five delegated TRIP
            roles. Suggested values are not consent; choose every value
            explicitly.
          </p>
          <div className="form-grid three">
            <label>
              Provider<select
                value={manager.provider}
                onChange={(event) =>
                  setManager({
                    ...manager,
                    provider: event.target.value as Provider | "",
                  })}
              >
                <option value="">Choose provider</option>
                <option value="codex">Codex</option>
                <option value="claude">Claude</option>
              </select>
            </label>
            <ModelSelector
              provider={manager.provider}
              value={manager.model}
              onChange={(model) => setManager({ ...manager, model })}
              label="Exact model"
              knownExactModels={knownExactModels(manager.provider)}
            />
            <label>
              Reasoning effort<select
                value={manager.effort}
                onChange={(event) =>
                  setManager({ ...manager, effort: event.target.value })}
              >
                <option value="">Choose effort</option>
                {manager.effort && !efforts.includes(manager.effort) && (
                  <option value={manager.effort}>{manager.effort}</option>
                )}
                {efforts.map((value) => <option key={value}>{value}</option>)}
              </select>
            </label>
          </div>
          <button
            className="primary"
            disabled={!managerValid || detectedKind === "unknown" || !!busy}
            title={detectedKind === "unknown"
              ? "Discover the existing installation first"
              : undefined}
            onClick={() => void begin()}
          >
            Start project discovery
          </button>
        </fieldset>
      )}
      {setup && (
        <>
          <p className="setup-identity">
            <strong>Setup operation:</strong>{" "}
            <code>{setup.setup_operation_id}</code> · state{" "}
            {setup.state.replaceAll("_", " ")}
          </p>
          <div className="setup-summary">
            {setup.discovery_status && (
              <span>
                Discovery: <strong>{setup.discovery_status.status}</strong>{" "}
                · workspace{" "}
                {setup.discovery_status.workspace_state || "unknown"} · permits
                {" "}
                {setup.discovery_status
                  .consumed_permits}/{setup.discovery_status.issued_permits +
                  setup.discovery_status.consumed_permits}
              </span>
            )}
            {setup.probe_status && (
              <span>
                Probe attempt: <strong>{setup.probe_status.status}</strong>{" "}
                · workspace {setup.probe_status.workspace_state || "unknown"}
                {" "}
                · permits {setup.probe_status
                  .consumed_permits}/{setup.probe_status.issued_permits +
                  setup.probe_status.consumed_permits}
              </span>
            )}
          </div>
          {setup.error && <p className="error" role="alert">{setup.error}</p>}
          {setupContinuationActions.map((action) => (
            <section
              className="setup-step"
              key={`${action.kind}:${
                action.binding.rejection_event_id || action.reason
              }`}
            >
              <span className="eyebrow">Setup session recovery</span>
              <h5>
                {action.kind === "fresh_accounted_retry"
                  ? "Start a fresh accounted setup session"
                  : action.kind.replaceAll("_", " ")}
              </h5>
              <p className="hint">{action.reason}</p>
              {action.accounting_note && (
                <p className="hint">{action.accounting_note}</p>
              )}
              {action.kind === "fresh_accounted_retry" && (
                <button
                  className="primary"
                  disabled={!action.enabled || !!busy}
                  onClick={() => void dispatchFreshSetup(action)}
                >
                  Start fresh accounted setup session
                </button>
              )}
            </section>
          ))}
          {activeStage === "discover" && !setup.proposal_hash &&
            managerControl && (
            <section className="setup-step discovery-manager-control">
              <span className="eyebrow">Discovery manager control</span>
              <h5>Stop or replace the exact discovery manager</h5>
              <dl>
                <dt>Current profile</dt>
                <dd>{managerProfileLabel(managerControl.current)}</dd>
                <dt>Requested profile</dt>
                <dd>{managerProfileLabel(managerControl.requested)}</dd>
                <dt>Effective launch profile</dt>
                <dd>{managerProfileLabel(managerControl.effective)}</dd>
                <dt>Interrupt state</dt>
                <dd>
                  {managerControl.interrupt_requested
                    ? "interrupt requested; exit is not inferred"
                    : "no interrupt request recorded"}
                </dd>
                <dt>Process state</dt>
                <dd>
                  {managerControl.quiescent
                    ? "positively quiescent"
                    : "not yet positively quiescent"}
                </dd>
              </dl>
              <p className="hint">
                <strong>
                  Next action: {managerControl.next_action.action}
                </strong>
                {" · "}
                {managerControl.next_action.reason}
              </p>
              {managerControl.hold && (
                <p className="warning">
                  Durable manager dispatch hold:{" "}
                  {managerControl.hold.state}. It remains in force across
                  refresh and restart until this replacement is superseded.
                </p>
              )}
              {managerControl.next_action.action === "stop" && (
                <button
                  disabled={!!busy}
                  onClick={() => void stopSetupManager()}
                >
                  Stop discovery manager
                </button>
              )}
              {managerControl.next_action.action === "retry_stop" && (
                <button
                  disabled={!!busy}
                  onClick={() => void stopSetupManager()}
                >
                  Retry Stop discovery manager
                </button>
              )}
              {managerControl.next_action.action === "waiting" && (
                <p className="warning">
                  Discovery dispatch remains held. A requested interrupt is not
                  proof that the manager process group has exited.
                </p>
              )}
              {["change", "launch"].includes(
                managerControl.next_action.action,
              ) && (
                <fieldset className="setup-step">
                  <legend>
                    {managerControl.next_action.action === "launch"
                      ? "Correct discovery manager before launch"
                      : "Replacement manager profile"}
                  </legend>
                  <p className="hint">
                    {managerControl.next_action.action === "launch"
                      ? "No discovery manager has launched. Saving a different exact profile creates a fresh immutable revision only; choose Launch manager discovery separately."
                      : "Changing this profile creates a fresh immutable discovery revision. The old manager permit and credential stay retired; the replacement launches only when you explicitly choose Launch manager discovery afterwards."}
                  </p>
                  <div className="form-grid three">
                    <label>
                      Provider<select
                        value={manager.provider}
                        onChange={(event) =>
                          setManager({
                            ...manager,
                            provider: event.target.value as Provider | "",
                          })}
                      >
                        <option value="">Choose provider</option>
                        <option value="codex">Codex</option>
                        <option value="claude">Claude</option>
                      </select>
                    </label>
                    <ModelSelector
                      provider={manager.provider}
                      value={manager.model}
                      onChange={(model) => setManager({ ...manager, model })}
                      label={managerControl.next_action.action === "launch"
                        ? "Exact corrected model"
                        : "Exact replacement model"}
                      knownExactModels={knownExactModels(manager.provider)}
                    />
                    <label>
                      Reasoning effort<select
                        value={manager.effort}
                        onChange={(event) =>
                          setManager({
                            ...manager,
                            effort: event.target.value,
                          })}
                      >
                        <option value="">Choose effort</option>
                        {manager.effort && !efforts.includes(manager.effort) &&
                          (
                            <option value={manager.effort}>
                              {manager.effort}
                            </option>
                          )}
                        {efforts.map((value) => (
                          <option key={value}>{value}</option>
                        ))}
                      </select>
                    </label>
                  </div>
                  <button
                    className="primary"
                    disabled={!managerValid || managerMatchesCurrent || !!busy}
                    title={managerMatchesCurrent
                      ? "Choose a different exact provider, model, or reasoning effort"
                      : undefined}
                    onClick={() => void changeSetupManager()}
                  >
                    {managerControl.next_action.action === "launch"
                      ? "Save corrected discovery manager"
                      : "Change discovery manager"}
                  </button>
                  {managerMatchesCurrent && (
                    <p className="hint">
                      Change is disabled because the replacement profile is
                      unchanged. To keep this provider, model, and effort, use
                      the available Retry or Launch manager discovery action.
                    </p>
                  )}
                </fieldset>
              )}
            </section>
          )}
          {revisionEligible && !editingRevision && (
            <button
              disabled={!!busy}
              onClick={() => {
                setEditingRevision(true);
                setActiveStage("project-settings");
              }}
            >
              Propose reviewed configuration correction
            </button>
          )}
          {activeStage === "discover" &&
            setup.state === "workspace_recovery_required" && (
            <button
              disabled={!!busy}
              onClick={() =>
                setup.probe_attempt_id ? void authorizeProbes() : void begin()}
            >
              {setup.probe_attempt_id
                ? "Retry probe workspace reservation"
                : "Retry discovery workspace reservation"}
            </button>
          )}
          {activeStage === "discover" && setup.discovery_attempt_id &&
            !setup.proposal_hash && (
            <div className="discovery-workspace">
              <section className="setup-step discovery-status-panel">
                <span className="eyebrow">Discovery status</span>
                <h5>
                  {setup.discovery_status?.status.replaceAll("_", " ") ||
                    "Ready to launch"}
                </h5>
                <p className="hint">
                  Workspace {setup.discovery_status?.workspace_state ||
                    "not reserved"}
                  {setup.discovery_status && (
                    <>
                      {" · "}
                      {setup.discovery_status.consumed_permits}/
                      {setup.discovery_status.issued_permits +
                        setup.discovery_status.consumed_permits} permit uses
                    </>
                  )}
                </p>
              </section>
              <section className="setup-step discovery-output-panel">
                <div>
                  <span className="eyebrow">Manager session</span>
                  <h5>Discovery output</h5>
                </div>
                <SetupInvocation
                  role="manager"
                  purpose="discovery"
                  attemptId={setup.discovery_attempt_id}
                  sessions={setup.sessions}
                  receipt={hasUsableManagerReceipt}
                  busy={busy}
                  onDispatch={dispatch}
                  onViewSession={onViewSession}
                  cmuxSurfaces={cmuxSurfaces}
                  recoveries={setup.recoveries || []}
                  onChanged={onChanged}
                  dispatchHeld={!!managerControl?.hold}
                />
                <p className="hint">
                  View output opens this exact manager session in cmux. Native
                  prompts use the attachment lease; service approvals remain in
                  the dashboard.
                </p>
              </section>
            </div>
          )}
          {activeStage === "discover" && usableManagerReceipts.length > 0 && (
            <details className="setup-step">
              <summary>Discovery suggestion (non-authoritative)</summary>
              {discoverySuggestion
                ? (
                  <>
                    <p className="hint">
                      This retained-manager output is policy text only. Applying
                      it replaces:{" "}
                      {suggestedPolicyLabels.join(", ")}. It leaves the manager,
                      delegated roles and profiles, test coverage,
                      observability, local-exclude consent, and migration
                      resolutions unchanged.
                    </p>
                    <dl>
                      {discoverySuggestion.project_name !== undefined && (
                        <>
                          <dt>Project name</dt>
                          <dd>{discoverySuggestion.project_name}</dd>
                        </>
                      )}
                      {discoverySuggestion.guidance !== undefined && (
                        <>
                          <dt>Guidance paths</dt>
                          <dd>
                            {discoverySuggestion.guidance.length
                              ? (
                                <ul>
                                  {discoverySuggestion.guidance.map((path) => (
                                    <li key={path}>
                                      <code>{path}</code>
                                    </li>
                                  ))}
                                </ul>
                              )
                              : (
                                <span className="hint">No paths suggested</span>
                              )}
                          </dd>
                        </>
                      )}
                      {discoverySuggestion.documentation !== undefined && (
                        <>
                          <dt>Documentation no-change text</dt>
                          <dd>
                            {discoverySuggestion.documentation.no_change_text}
                          </dd>
                        </>
                      )}
                      {(["focused", "broad", "cleanup"] as const).map(
                        (category) => {
                          const commands = discoverySuggestion.verification
                            ?.[category];
                          if (commands === undefined) return null;
                          return (
                            <div key={category}>
                              <dt>
                                {category[0].toUpperCase() + category.slice(1)}
                                {" "}
                                commands
                              </dt>
                              <dd>
                                {commands.length
                                  ? (
                                    <ul>
                                      {commands.map((command, index) => (
                                        <li key={`${category}:${index}`}>
                                          <code>{command}</code>
                                        </li>
                                      ))}
                                    </ul>
                                  )
                                  : (
                                    <span className="hint">
                                      No commands suggested
                                    </span>
                                  )}
                              </dd>
                            </div>
                          );
                        },
                      )}
                      {discoverySuggestion.agents_content !== undefined && (
                        <>
                          <dt>Proposed complete AGENTS.md draft</dt>
                          <dd>
                            <pre>{discoverySuggestion.agents_content}</pre>
                          </dd>
                        </>
                      )}
                    </dl>
                    <p className="hint">
                      Applying changes only this unsaved browser draft. It runs
                      no command, changes no server state, and saves, approves,
                      probes, or installs nothing. AGENTS.md remains subject to
                      preservation checks, explicit save, preflight, and final
                      installation approval.
                    </p>
                    {showProposalEditor && (
                      <button
                        type="button"
                        disabled={!!busy}
                        onClick={applyDiscoverySuggestion}
                      >
                        Apply suggestions to draft
                      </button>
                    )}
                  </>
                )
                : (
                  <p className="hint">
                    This historical manager receipt has no usable typed setup
                    suggestion. Existing draft values are unchanged; discovery
                    is not rerun automatically.
                  </p>
                )}
            </details>
          )}
          {activeStage === "discover" && usableManagerReceipts.length > 0 &&
            showProposalEditor && (
            <div className="setup-next-action">
              <div>
                <strong>Discovery result recorded</strong>
                <span>
                  Review the suggested policy and project-specific commands in
                  the local draft. Nothing has been saved or approved.
                </span>
              </div>
              <button
                className="primary"
                onClick={() => setActiveStage("project-settings")}
              >
                Review project settings
              </button>
            </div>
          )}
          {showProposalEditor && ["project-settings", "agents", "review"]
            .includes(activeStage) &&
            (
              <fieldset className="setup-step">
                <legend>
                  {activeStage === "project-settings"
                    ? "Project settings"
                    : activeStage === "agents"
                    ? "Agent profiles"
                    : "Review and save exact changes"}
                </legend>
                {activeStage === "project-settings" && (
                  <>
                    {editingRevision && (
                      <p className="warning">
                        This creates a new immutable draft. The active
                        configuration, prior sessions, receipts, and approvals
                        remain unchanged until the corrected revision is
                        separately probed, approved, and applied.
                      </p>
                    )}
                    <h5>Host manager profile</h5>
                    <div className="form-grid three">
                      <label>
                        Provider<select
                          value={manager.provider}
                          disabled={!editingRevision}
                          onChange={(event) =>
                            setManager({
                              ...manager,
                              provider: event.target.value as Provider | "",
                            })}
                        >
                          <option value="">Choose provider</option>
                          <option value="codex">Codex</option>
                          <option value="claude">Claude</option>
                        </select>
                      </label>
                      <ModelSelector
                        provider={manager.provider}
                        value={manager.model}
                        disabled={!editingRevision}
                        onChange={(model) => setManager({ ...manager, model })}
                        label="Exact model"
                        knownExactModels={knownExactModels(manager.provider)}
                      />
                      <label>
                        Reasoning effort<select
                          value={manager.effort}
                          disabled={!editingRevision}
                          onChange={(event) =>
                            setManager({
                              ...manager,
                              effort: event.target.value,
                            })}
                        >
                          <option value="">Choose effort</option>
                          {manager.effort &&
                            !efforts.includes(manager.effort) && (
                            <option value={manager.effort}>
                              {manager.effort}
                            </option>
                          )}
                          {efforts.map((value) => (
                            <option key={value}>{value}</option>
                          ))}
                        </select>
                      </label>
                    </div>
                    <div className="form-grid">
                      <label>
                        Project name<input
                          value={draft.projectName}
                          onChange={(event) =>
                            setDraft({
                              ...draft,
                              projectName: event.target.value,
                            })}
                        />
                      </label>
                      <label>
                        Test coverage<select
                          value={draft.coverage}
                          onChange={(event) =>
                            setDraft({
                              ...draft,
                              coverage: event.target
                                .value as SetupDraft["coverage"],
                            })}
                        >
                          <option value="">Choose explicitly</option>
                          <option value="minimal">Minimal</option>
                          <option value="moderate">Moderate</option>
                          <option value="extensive">Extensive</option>
                        </select>
                      </label>
                      <label>
                        Observability<select
                          value={draft.cmux}
                          onChange={(event) =>
                            setDraft({
                              ...draft,
                              cmux: event.target.value as SetupDraft["cmux"],
                            })}
                        >
                          <option value="off">Off for app host</option>
                          <option value="auto">Auto</option>
                          <option value="on">On</option>
                        </select>
                      </label>
                    </div>
                    <label>
                      Guidance paths{" "}
                      <small>
                        One contained project-relative path per line; AGENTS.md
                        is always included.
                      </small>
                      <textarea
                        value={draft.guidance}
                        onChange={(event) =>
                          setDraft({ ...draft, guidance: event.target.value })}
                      />
                    </label>
                    <label>
                      Documentation no-change text<textarea
                        value={draft.documentation}
                        onChange={(event) =>
                          setDraft({
                            ...draft,
                            documentation: event.target.value,
                          })}
                      />
                    </label>
                    <div className="form-grid three">
                      <label>
                        Focused exact shell commands<textarea
                          value={draft.focused}
                          onChange={(event) =>
                            setDraft({
                              ...draft,
                              focused: event.target.value,
                              verificationContracts: {},
                            })}
                        />
                      </label>
                      <label>
                        Broad exact shell commands<textarea
                          value={draft.broad}
                          onChange={(event) =>
                            setDraft({
                              ...draft,
                              broad: event.target.value,
                              verificationContracts: {},
                            })}
                        />
                      </label>
                      <label>
                        Cleanup exact shell commands<textarea
                          value={draft.cleanup}
                          onChange={(event) =>
                            setDraft({
                              ...draft,
                              cleanup: event.target.value,
                              verificationContracts: {},
                            })}
                        />
                      </label>
                    </div>
                    <p className="hint">
                      Each line is an exact <code>/bin/sh -lc</code>{" "}
                      command. A task manager selects the applicable matrix;
                      structured argv contracts must name an absolute
                      executable, and every command still requires exact human
                      authorization before execution.
                    </p>
                    <label>
                      Exact approved AGENTS.md content<textarea
                        className="guidance-content"
                        value={draft.agentsContent}
                        onChange={(event) =>
                          setDraft({
                            ...draft,
                            agentsContent: event.target.value,
                          })}
                      />
                    </label>
                    {setup.agents_file.error && (
                      <p className="error">{setup.agents_file.error}</p>
                    )}
                    <label className="toggle">
                      <input
                        type="checkbox"
                        checked={draft.approveLocalExclude}
                        onChange={(event) =>
                          setDraft({
                            ...draft,
                            approveLocalExclude: event.target.checked,
                          })}
                      />Approve adding exactly{" "}
                      <code>/.local/trip-explorer/</code>{" "}
                      to the repository-local Git exclude only if needed
                    </label>
                  </>
                )}
                {activeStage === "agents" && (
                  <>
                    <h5>Five delegated role profiles</h5>
                    <p className="hint">
                      The service supports selected Codex and Claude native
                      profiles only after live preflight. Preserved custom
                      adapters stay visible but cannot be selected when their
                      contract is unsupported.
                    </p>
                    <div className="profile-list">
                      {delegatedRoles.map((role) => {
                        const value = draft.profiles[role];
                        return (
                          <fieldset key={role}>
                            <legend>
                              {roleName(role)} · {value.authority} ·{" "}
                              {value.session}
                            </legend>
                            <div className="form-grid three">
                              <label>
                                Adapter<select
                                  value={value.adapter}
                                  onChange={(event) => {
                                    const adapter =
                                      adapterOptions[event.target.value];
                                    updateProfile(role, {
                                      ...value,
                                      adapter: event.target.value,
                                      provider: adapter?.provider === "codex" ||
                                          adapter?.provider === "claude"
                                        ? adapter.provider
                                        : "",
                                    });
                                  }}
                                >
                                  <option value="">Choose adapter</option>
                                  {Object.entries(adapterOptions).map(
                                    ([id, adapter]) => {
                                      const requiredAuthority =
                                        role === "implementer"
                                          ? "workspace_write"
                                          : "read_only";
                                      const requiredSession =
                                        role === "final_verifier"
                                          ? "fresh_session"
                                          : "resume";
                                      const supported =
                                        ["native-agent", "builtin-cli"]
                                          .includes(
                                            String(adapter.kind),
                                          ) &&
                                        ["codex", "claude"].includes(
                                          String(adapter.provider),
                                        ) &&
                                        adapter.capabilities
                                            ?.[requiredAuthority] === true &&
                                        adapter.capabilities
                                            ?.[requiredSession] ===
                                          true;
                                      return (
                                        <option
                                          key={id}
                                          value={id}
                                          disabled={!supported}
                                        >
                                          {id} ·{" "}
                                          {String(adapter.kind || "unknown")}
                                          {supported
                                            ? ""
                                            : " · preserved, unsupported"}
                                        </option>
                                      );
                                    },
                                  )}
                                </select>
                              </label>
                              <label>
                                Provider<input
                                  readOnly
                                  value={value.provider}
                                  placeholder="from adapter"
                                />
                              </label>
                              <ModelSelector
                                provider={value.provider}
                                value={value.model}
                                onChange={(model) =>
                                  updateProfile(role, { ...value, model })}
                                label="Exact model"
                                knownExactModels={knownExactModels(
                                  value.provider,
                                )}
                              />
                              <label>
                                Reasoning effort<select
                                  value={value.effort}
                                  onChange={(event) =>
                                    updateProfile(role, {
                                      ...value,
                                      effort: event.target.value,
                                    })}
                                >
                                  <option value="">Choose effort</option>
                                  {value.effort &&
                                    !efforts.includes(value.effort) && (
                                    <option value={value.effort}>
                                      {value.effort}
                                    </option>
                                  )}
                                  {efforts.map((effort) => (
                                    <option key={effort}>{effort}</option>
                                  ))}
                                </select>
                              </label>
                              <label>
                                Service tier (unsupported)<input
                                  disabled
                                  value={value.service_tier || ""}
                                />
                                {value.service_tier && (
                                  <button
                                    type="button"
                                    onClick={() =>
                                      updateProfile(role, {
                                        ...value,
                                        service_tier: null,
                                      })}
                                  >
                                    Clear unsupported tier
                                  </button>
                                )}
                              </label>
                            </div>
                          </fieldset>
                        );
                      })}
                    </div>
                  </>
                )}
                {activeStage === "review" && (
                  <>
                    {migrationItems.length > 0 && (
                      <fieldset>
                        <legend>Existing customization preservation</legend>
                        <p className="hint">
                          Both choices install the pinned files and preserve the
                          originals they replace as backups. LLMRelay does not
                          merge custom content automatically; review and
                          reconcile any preserved original yourself.
                        </p>
                        {migrationItems.map((item) => (
                          <label key={item}>
                            <span>
                              <strong>{migrationItemDetails(item).kind}</strong>
                              {" · "}
                              <code>{migrationItemDetails(item).path}</code>
                            </span>
                            <select
                              value={draft.migrationResolutions[item] || ""}
                              onChange={(event) =>
                                setDraft({
                                  ...draft,
                                  migrationResolutions: {
                                    ...draft.migrationResolutions,
                                    [item]: event.target.value,
                                  },
                                })}
                            >
                              <option value="">Choose explicitly</option>
                              <option value="use_pinned">
                                Use pinned files here and save replaced files as
                                backups
                              </option>
                              <option value="copy_pinned_to_canonical_preserve_original">
                                Install pinned files in the standard folder and
                                preserve existing files as backups
                              </option>
                            </select>
                          </label>
                        ))}
                      </fieldset>
                    )}
                    <button
                      className="primary"
                      disabled={!proposalValue || !!proposalProblem || !!busy}
                      title={proposalProblem || undefined}
                      onClick={() => void saveDraft()}
                    >
                      {editingRevision
                        ? "Create corrected immutable revision"
                        : "Save setup proposal"}
                    </button>
                    {editingRevision && (
                      <button
                        disabled={!!busy}
                        onClick={() => {
                          clearPersistedSetupDraft(true);
                          setDraft(hydratedDraft(project, setup));
                          setManager(
                            selectedManager || {
                              provider: "",
                              model: "",
                              effort: "",
                            },
                          );
                          setEditingRevision(false);
                          setActiveStage(stageForProject(setup, trip));
                        }}
                      >
                        Cancel correction
                      </button>
                    )}
                    {proposalProblem && (
                      <div className="setup-prerequisite" role="status">
                        <div>
                          <strong>Before this proposal can be saved</strong>
                          <span>{proposalProblem}</span>
                        </div>
                        <button
                          type="button"
                          onClick={() =>
                            setActiveStage(proposalProblemStage)}
                        >
                          Go to {setupStages.find((stage) =>
                            stage.id === proposalProblemStage
                          )?.label}
                        </button>
                      </div>
                    )}
                  </>
                )}
              </fieldset>
            )}
          {activeStage === "agents" && setup.state === "draft" &&
            setup.proposal_hash && (
            <fieldset className="setup-step">
              <legend>Authorize paid live probes</legend>
              <p>
                This separate authorization launches the exact selected manager
                and five delegated profiles in the private empty fixture. Calls
                use the selected native CLI accounts and their available
                allowance. It does not authorize installation.
              </p>
              <code>{setup.proposal_hash}</code>
              <button
                className="primary"
                disabled={!!busy}
                onClick={() => void authorizeProbes()}
              >
                Authorize affected profile probes and exact unchanged reuse
              </button>
            </fieldset>
          )}
          {activeStage === "agents" && setup.probe_attempt_id &&
            ["probing", "preflight_complete"].includes(setup.state) && (
            <fieldset className="setup-step">
              <legend>Run and inspect live probes</legend>
              {(["manager", ...delegatedRoles] as Role[]).map((role) => (
                <SetupInvocation
                  key={role}
                  role={role}
                  purpose="probe"
                  attemptId={role === "manager" && receiptsByRole.has(role)
                    ? setup.discovery_attempt_id!
                    : setup.probe_attempt_id!}
                  sessions={setup.sessions}
                  receipt={receiptsByRole.has(role)}
                  busy={busy}
                  onDispatch={dispatch}
                  onViewSession={onViewSession}
                  cmuxSurfaces={cmuxSurfaces}
                  recoveries={setup.recoveries || []}
                  onChanged={onChanged}
                />
              ))}
              <p>
                {receiptsByRole.size}/6 exact capability and adapter-bound
                receipts recorded. Retained roles report only after their exact
                session resumes; the final verifier is always fresh. Historical
                receipts without identity bindings remain visible but
                unverified.
              </p>
              {setup.probe_receipts.filter((receipt) =>
                receipt.reused_from_receipt_id
              )
                .map((receipt) => (
                  <small key={receipt.role}>
                    {roleName(receipt.role)} reuses exact unchanged proof{" "}
                    {receipt.reused_from_receipt_id}; changed profiles still
                    require this revision's explicit probe authorization.
                  </small>
                ))}
            </fieldset>
          )}
          {activeStage === "review" && allReceipts && setup.proposal_hash &&
            ["probing", "preflight_complete", "finalized"].includes(
              setup.state,
            ) && (
            <fieldset className="setup-step">
              <legend>Approve exact installation</legend>
              {setup.destination_preview_error && (
                <p className="error">{setup.destination_preview_error}</p>
              )}
              <div className="destination-list">
                {setup.destination_preview?.map((entry) => (
                  <div key={entry.relative_path}>
                    <code>{entry.relative_path}</code>
                    <small>
                      draft source {entry.source_sha256.slice(0, 12)} · preimage
                      {" "}
                      {entry.preimage_sha256?.slice(0, 12) || "absent"}
                    </small>
                  </div>
                ))}
              </div>
              <p>
                Proposal <code>{setup.proposal_hash}</code>
                <br />Preimages <code>{setup.approved_preimages_hash}</code>
                <br />Final source set{" "}
                <code>{setup.final_source_set_hash || "not frozen"}</code>
              </p>
              {detectedKind === "compatible" &&
                  !setup.supersedes_setup_operation_id
                ? (
                  <button
                    className="primary"
                    disabled={!!busy || !setup.proposal}
                    onClick={() => void adopt()}
                  >
                    Adopt exact compatible installation
                  </button>
                )
                : (
                  <>
                    {!setup.installation_source_binding_complete && (
                      <button
                        className="primary"
                        disabled={!!busy}
                        onClick={() => void finalizeInstall()}
                      >
                        Freeze exact final files and destination preimages
                      </button>
                    )}
                    <p className="hint">
                      {setup.installation_source_binding_reason}
                    </p>
                    {setup.final_files.map((entry) => (
                      <details key={entry.relative_path}>
                        <summary>
                          <code>{entry.relative_path}</code> · source{" "}
                          {entry.source_sha256.slice(0, 12)} · preimage{" "}
                          {entry.preimage_sha256?.slice(0, 12) || "absent"}
                        </summary>
                        <strong>Before</strong>
                        <pre>{entry.preimage_content ?? "(file absent)"}</pre>
                        <strong>After</strong>
                        <pre>{entry.content}</pre>
                      </details>
                    ))}
                    <button
                      className="primary"
                      disabled={!setup.installation_source_binding_complete ||
                        !!busy || !setup.final_files.length}
                      onClick={() => void authorizeInstall()}
                    >
                      Approve exact final files and preimages
                    </button>
                  </>
                )}
            </fieldset>
          )}
          {activeStage === "activate" &&
            setup.state === "install_authorized" && (
            <button
              className="primary"
              disabled={!!busy}
              onClick={() => void applyInstall()}
            >
              Apply staged authorized installation
            </button>
          )}
          {activeStage === "activate" && recoverSetupApply && (
            <section className="setup-step">
              <p className="hint">{recoverSetupApply.reason}</p>
              <button
                disabled={!recoverSetupApply.enabled || !!busy}
                onClick={() => void recoverInstall()}
              >
                Recover the authorized apply journal
              </button>
            </section>
          )}
        </>
      )}
      {activeStage === "project-settings" && !showProposalEditor && (
        <section className="setup-step setup-stage-empty">
          <h5>
            {setup
              ? "Project settings are locked to the saved revision"
              : "Project settings follow discovery"}
          </h5>
          <p className="hint">
            {setup
              ? "The current setup operation has moved past draft editing. Start a reviewed configuration correction to change these values without altering the active revision or its approvals."
              : "Inspect the repository and complete manager discovery before reviewing the project policy draft."}
          </p>
          {!setup && (
            <button onClick={() => setActiveStage("discover")}>
              Go to Discover
            </button>
          )}
          {revisionEligible && !editingRevision && (
            <button
              onClick={() => {
                setEditingRevision(true);
                setActiveStage("project-settings");
              }}
            >
              Propose reviewed configuration correction
            </button>
          )}
        </section>
      )}
      {activeStage === "agents" && !setup && (
        <section className="setup-step setup-stage-empty">
          <h5>Agent profiles follow discovery</h5>
          <p className="hint">
            Inspect the repository and start manager discovery before choosing
            the five delegated profiles.
          </p>
          <button onClick={() => setActiveStage("discover")}>
            Go to Discover
          </button>
        </section>
      )}
      {activeStage === "review" && !setup && (
        <section className="setup-step setup-stage-empty">
          <h5>There are no proposed changes to review yet</h5>
          <p className="hint">
            Discovery creates the local draft that can be reviewed here. No
            project files change during discovery.
          </p>
          <button onClick={() => setActiveStage("discover")}>
            Go to Discover
          </button>
        </section>
      )}
      {activeStage === "activate" && trip.readiness !== "ready" &&
        !["install_authorized", "applying", "recovery_required"].includes(
          setup?.state || "",
        ) && (
        <section className="setup-step setup-stage-empty">
          <h5>Activation is not authorized yet</h5>
          <p className="hint">
            Complete the current server-backed setup stage first. Visiting this
            page does not approve probes, installation, or runtime access.
          </p>
          <button onClick={() => setActiveStage(serverStage)}>
            Return to{" "}
            {setupStages.find((stage) => stage.id === serverStage)?.label}
          </button>
        </section>
      )}
      {activeStage === "activate" && trip.configuration && (
        <ActiveConfiguration
          project={project}
          runtimeAdmission={runtimeAdmission}
        />
      )}
      {activeStage === "activate" && trip.readiness === "ready" && (
        <fieldset className="setup-step">
          <legend>Runtime capability readiness</legend>
          <p>
            Installation is complete. Ordinary task dispatch remains separate
            until every selected profile has current executable, policy, hook,
            role, model, effort, confinement, and native-session proof. Setup
            receipts are never promoted into this authority.
          </p>
          <label>
            Live cmux Unix socket for Claude requalification
            <input
              value={cmuxSocketPath}
              onChange={(event) => setCmuxSocketPath(event.target.value)}
              placeholder="/absolute/path/to/cmux.sock"
            />
          </label>
          <p className="hint">
            Required when this admission includes Claude. Enter the exact
            human-confirmed live socket path before preparing it. The frozen
            probe is a zero-I/O connection attempt only: it never sends a cmux
            control request or changes cmux, and preserves its native exit and
            stderr. A missing or refused path is not evidence of sandbox denial.
          </p>
          {!runtimeAdmission && (
            <button
              className="primary"
              disabled={!!busy}
              onClick={() => void prepareRuntime()}
            >
              Prepare exact runtime verification
            </button>
          )}
          {runtimeAdmission && (
            <>
              <RuntimeAdmission
                admission={runtimeAdmission}
                busy={busy}
                onAuthorize={authorizeRuntime}
                onLaunch={(role) => runtimeDispatch(runtimeAdmission.id, role)}
                onResume={(role) =>
                  runtimeDispatch(runtimeAdmission.id, role, true)}
                onPublish={(role) => publishRuntime(runtimeAdmission.id, role)}
                onCorrect={(role) => prepareRuntime(role)}
                onViewSession={onViewSession}
                cmuxSurfaces={cmuxSurfaces}
                recoveries={setup?.recoveries || []}
                onChanged={onChanged}
              />
            </>
          )}
        </fieldset>
      )}
      {notice && (
        <p className="success" aria-live="polite">
          {notice.replaceAll("_", " ")}
        </p>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
          <small>
            Inputs and the operation identity are retained for an unchanged
            retry.
          </small>
        </p>
      )}
    </section>
  );
}

function CmuxRouteNotice(
  { outcome, onRetry, onDiscard, discarding = false }: {
    outcome?: CmuxViewOutcome;
    onRetry?: () => void;
    onDiscard?: () => void;
    discarding?: boolean;
  },
) {
  if (!outcome) return null;
  const output = recordedOutputText(outcome);
  const presentation = cmuxSurfacePresentation(
    outcome.surface,
    outcome.state === "pending",
  );
  const retryAvailable = outcome.surface
    ? presentation.retryAvailable
    : outcome.retry_available;
  return (
    <div className={`cmux-route ${outcome.state}`} role="status">
      <strong>cmux presentation {cmuxRouteLabel(outcome.state)}</strong> ·{" "}
      {outcome.message}
      {outcome.surface && (
        <small>
          route revision {outcome.surface.binding_revision} · surface{" "}
          {outcome.surface.surface_state} · attachment{" "}
          {outcome.surface.attachment_state} · desired{" "}
          {outcome.surface.desired_input_state} · actual{" "}
          {outcome.surface.actual_input_state} · control revision{" "}
          {outcome.surface.applied_revision}/{outcome.surface.control_revision}
        </small>
      )}
      {output && <pre className="recorded-output">{output}</pre>}
      {outcome.surface && presentation.diagnostic && (
        <small className="warning">
          Durable cmux diagnostic: {presentation.diagnostic}
        </small>
      )}
      {outcome.surface && presentation.guidance && (
        <small className="warning">{presentation.guidance}</small>
      )}
      {outcome.surface && presentation.discardAvailable && onDiscard && (
        <button disabled={discarding} onClick={onDiscard}>
          {discarding
            ? "Discarding unknown reservation…"
            : "Discard unknown reservation"}
        </button>
      )}
      {retryAvailable && onRetry && (
        <button onClick={onRetry}>
          {outcome.surface ? presentation.viewLabel : "View output again"}
        </button>
      )}
    </div>
  );
}

const keyboardOutcome = (
  result: Awaited<ReturnType<typeof setCmuxKeyboardControl>>,
): CmuxViewOutcome =>
  cmuxViewOutcomeFromSurface({
    state: result.state === "retired" ? "failed" : result.state,
    message: result.message,
    retry_available: result.state !== "retired",
    surface: result.surface,
  });

const requestCmuxKeyboardControl = async (
  sessionId: string,
  surface: CmuxSessionSurface,
  action: CmuxKeyboardControlAction,
) =>
  keyboardOutcome(
    await setCmuxKeyboardControl(
      sessionId,
      surface.id,
      surface.binding_revision,
      surface.control_revision,
      action,
      operationId(),
    ),
  );

function SetupInvocation(
  {
    role,
    purpose,
    attemptId,
    sessions,
    cmuxSurfaces,
    receipt,
    busy,
    onDispatch,
    onViewSession,
    recoveries,
    onChanged,
    dispatchHeld = false,
  }: {
    role: Role;
    purpose: "discovery" | "probe";
    attemptId: string;
    sessions: TripSetupState["sessions"];
    cmuxSurfaces: Record<string, CmuxSessionSurface>;
    receipt: boolean;
    busy: string;
    onDispatch: (attemptId: string, role: Role, sessionId?: string) => void;
    onViewSession: (
      sessionId: string,
    ) => Promise<CmuxViewOutcome>;
    recoveries: TripSetupRecovery[];
    onChanged: () => Promise<void> | void;
    dispatchHeld?: boolean;
  },
) {
  const [viewOutcome, setViewOutcome] = useState<CmuxViewOutcome>();
  const [discarding, setDiscarding] = useState(false);
  const discardOperation = useRef<
    | { surfaceRouteId: string; sessionId: string; operationId: string }
    | undefined
  >(undefined);
  const durableSurfaceRef = useRef<{
    sessionId?: string;
    surface?: CmuxSessionSurface;
  }>({});
  const session = [...sessions].reverse().find((item) =>
    item.attempt_id === attemptId && item.role === role
  );
  const durableSurface = session?.cmux_surface ||
    (session ? cmuxSurfaces[session.id] : undefined);
  if (durableSurfaceRef.current.sessionId !== session?.id) {
    durableSurfaceRef.current = {
      sessionId: session?.id,
      surface: durableSurface,
    };
  } else {
    durableSurfaceRef.current.surface = cmuxNewestSurface(
      durableSurfaceRef.current.surface,
      durableSurface,
    );
  }
  const commitViewOutcome = (outcome: CmuxViewOutcome) => {
    const committed = cmuxOutcomeWithDurableSurface(
      outcome,
      durableSurfaceRef.current.surface,
    ) || outcome;
    setViewOutcome(committed);
    return committed;
  };
  const presentationOutcome = cmuxOutcomeWithDurableSurface(
    viewOutcome,
    durableSurfaceRef.current.surface,
  );
  const presentationActionability = cmuxSurfacePresentation(
    presentationOutcome?.surface,
    presentationOutcome?.state === "pending",
  );
  const recovery = recoveries.find((item) => item.session_id === session?.id);
  const resumable = !dispatchHeld && role !== "final_verifier" &&
    session?.status === "exited" &&
    session.has_native_session && session.resume_count === 0 && !receipt;
  const retainedResumeSpent = role !== "final_verifier" &&
    !!session && session.resume_count > 0 && !receipt;
  const freshRetry = !dispatchHeld && !receipt && !!session &&
    ["launch_failed", "exited"].includes(session.status) && !resumable;
  const keyboardControlActive = !!session?.input_control;
  const sessionStatus = !session
    ? "not launched"
    : keyboardControlActive && session.status === "running"
    ? "waiting for keyboard control release"
    : role !== "final_verifier" && session.resume_count === 0 &&
        session.readiness === "idle_candidate"
    ? session.status === "exited"
      ? "first pass complete · retained resume ready"
      : session.status === "interrupt_requested"
      ? "first pass complete · stopping for retained resume"
      : "first pass complete · verified idle"
    : `${session.status} · ${session.readiness}`;
  const viewOutput = async () => {
    if (!session) return undefined;
    setViewOutcome({
      state: "pending",
      message: "Reserving the exact persistent cmux presentation…",
      retry_available: false,
    });
    try {
      const outcome = commitViewOutcome(
        cmuxViewOutcomeFromSurface(await onViewSession(session.id)),
      );
      await onChanged();
      return outcome;
    } catch (cause) {
      const outcome: CmuxViewOutcome = {
        state: "failed",
        message: cause instanceof Error ? cause.message : String(cause),
        retry_available: true,
      };
      return commitViewOutcome(outcome);
    }
  };
  const setKeyboardControl = async (
    action: CmuxKeyboardControlAction,
    requestedSurface = presentationOutcome?.surface,
  ) => {
    const currentSurface = cmuxNewestSurface(
      durableSurfaceRef.current.surface,
      requestedSurface,
    );
    const actionability = cmuxSurfacePresentation(currentSurface);
    if (
      !session || !currentSurface ||
      (action === "acquire"
        ? !actionability.takeAvailable
        : !actionability.releaseAvailable)
    ) return;
    try {
      commitViewOutcome(
        await requestCmuxKeyboardControl(
          session.id,
          currentSurface,
          action,
        ),
      );
      await onChanged();
    } catch (cause) {
      commitViewOutcome({
        state: "failed",
        message: cause instanceof Error ? cause.message : String(cause),
        retry_available: true,
        surface: currentSurface,
      });
    }
  };
  const takeKeyboardControl = async () => {
    const outcome = await viewOutput();
    if (
      !outcome?.surface ||
      !cmuxSurfacePresentation(outcome.surface).takeAvailable
    ) return;
    await setKeyboardControl("acquire", outcome.surface);
  };
  const discardUnknown = async () => {
    const surface = presentationOutcome?.surface;
    if (
      !session || !surface || !cmuxSurfacePresentation(surface).discardAvailable
    ) return;
    const prior = discardOperation.current;
    const retry =
      prior?.surfaceRouteId === surface.id && prior.sessionId === session.id
        ? prior
        : {
          surfaceRouteId: surface.id,
          sessionId: session.id,
          operationId: operationId(),
        };
    discardOperation.current = retry;
    setDiscarding(true);
    try {
      const result = await discardUnknownCmuxSurface(
        session.id,
        surface.id,
        retry.operationId,
      );
      discardOperation.current = undefined;
      commitViewOutcome(cmuxViewOutcomeFromSurface(result));
      await onChanged();
    } catch (cause) {
      commitViewOutcome({
        ...(presentationOutcome || {
          state: "failed" as const,
          retry_available: true,
        }),
        message: cause instanceof Error ? cause.message : String(cause),
      });
    } finally {
      setDiscarding(false);
    }
  };
  return (
    <div className="setup-invocation">
      <div>
        <strong>{roleName(role)}</strong>
        <small>
          {receipt ? "verified receipt" : sessionStatus}
        </small>
      </div>
      {session && (
        <>
          <button
            disabled={session.status === "running" &&
              !presentationActionability.viewAvailable}
            onClick={() => void viewOutput()}
          >
            {presentationActionability.viewLabel}
          </button>
          {session.status === "running" && (
            <button
              disabled={!presentationActionability.takeAvailable}
              onClick={() => void takeKeyboardControl()}
            >
              Take keyboard control
            </button>
          )}
          {session.status === "running" &&
            presentationActionability.releaseAvailable && (
            <button onClick={() => void setKeyboardControl("release")}>
              Release keyboard control
            </button>
          )}
        </>
      )}
      <CmuxRouteNotice
        outcome={presentationOutcome}
        onRetry={() => void viewOutput()}
        onDiscard={() => void discardUnknown()}
        discarding={discarding}
      />
      {keyboardControlActive && session?.status === "running" && (
        <small className="warning input-control-hold" role="status">
          Automatic manager progress is paused while a cmux keyboard-control
          attachment owns input. Return to that pane and press Ctrl-] (or close
          it) to release control. LLMRelay will continue automatically after the
          lease ends.
        </small>
      )}
      {recovery && <SetupRecovery recovery={recovery} onChanged={onChanged} />}
      {dispatchHeld && (
        <small className="warning">
          Discovery manager dispatch and resume are held until this replacement
          is completed or superseded.
        </small>
      )}
      {!receipt && !session && (
        <button
          disabled={!!busy || dispatchHeld}
          onClick={() => onDispatch(attemptId, role)}
        >
          {purpose === "discovery"
            ? "Launch manager discovery"
            : "Launch bounded probe"}
        </button>
      )}
      {resumable && !recovery && (
        <>
          <small>
            The service completed the first turn without spending this resume.
            Resume starts the retained reporting turn explicitly.
          </small>
          <button
            disabled={!!busy}
            onClick={() => onDispatch(attemptId, role, session.id)}
          >
            Resume retained session
          </button>
        </>
      )}
      {freshRetry && !recovery && (
        <button
          disabled={!!busy}
          onClick={() => onDispatch(attemptId, role)}
        >
          {purpose === "discovery"
            ? "Retry manager discovery"
            : "Launch exact-profile retry"}
        </button>
      )}
      {retainedResumeSpent && session?.status === "exited" && !recovery && (
        <small className="warning">
          The retained reporting turn has ended without a verified receipt. Its
          one resume is spent; it cannot be resumed again. Use the explicit
          setup recovery or retry path after reviewing the failure.
        </small>
      )}
      {role === "final_verifier" && session && !receipt && (
        <small>
          Fresh final invocation is never resumed. A replacement is a separate
          explicit paid retry after the failure cause is corrected.
        </small>
      )}
    </div>
  );
}

function SetupRecovery(
  { recovery, onChanged }: {
    recovery: TripSetupRecovery;
    onChanged: () => Promise<void> | void;
  },
) {
  const [evidence, setEvidence] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const retry = useRef<{ body: string; id: string } | undefined>(undefined);
  const resolve = async (decision: "confirm_quiescent" | "cancel") => {
    if (!evidence.trim() || busy) return;
    const request = {
      task_id: recovery.task_id,
      attempt_id: recovery.attempt_id,
      session_id: recovery.session_id,
      expected_version: recovery.task_version,
      decision,
      evidence,
    };
    const stable = reuseOperationIdentity(retry.current, request);
    const { id, request: stableRequest } = stable;
    retry.current = { body: JSON.stringify(stableRequest), id };
    setBusy(true);
    setError("");
    try {
      await command({
        kind: "resolve_recovery",
        operation_id: id,
        ...stableRequest,
      });
      retry.current = undefined;
      await onChanged();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="setup-recovery">
      <p className="warning">
        Process ownership is unresolved for this {roleName(recovery.role)}
        {recovery.validation_cell === "trip_runtime_probe"
          ? " runtime admission"
          : " setup invocation"}. The service verifies the recorded operating
        system identities; this annotation is not proof. After verification and
        refresh, use the next action shown for this exact invocation. Retained
        roles resume this session; the fresh-only final verifier instead uses a
        separate explicit retry.
      </p>
      <textarea
        aria-label={`Recovery evidence for ${recovery.role}`}
        value={evidence}
        onChange={(event) => setEvidence(event.target.value)}
      />
      <div className="button-row">
        <button
          disabled={busy || !evidence.trim()}
          onClick={() => void resolve("confirm_quiescent")}
        >
          Verify quiescence and reconcile
        </button>
        <button
          className="danger"
          disabled={busy || !evidence.trim()}
          onClick={() => void resolve("cancel")}
        >
          Verify and cancel
        </button>
      </div>
      {error && <p className="error" role="alert">{error}</p>}
    </div>
  );
}

function RuntimeAdmission(
  {
    admission,
    busy,
    onAuthorize,
    onLaunch,
    onResume,
    onPublish,
    onCorrect,
    onViewSession,
    cmuxSurfaces,
    recoveries,
    onChanged,
  }: {
    admission: RuntimeAdmissionState;
    busy: string;
    onAuthorize: (id: string, scopeHash: string) => void;
    onLaunch: (role: Role) => void;
    onResume: (role: Role) => void;
    onPublish: (role: Role) => void;
    onCorrect: (role: Role) => void;
    onViewSession: (
      sessionId: string,
    ) => Promise<CmuxViewOutcome>;
    cmuxSurfaces: Record<string, CmuxSessionSurface>;
    recoveries: TripSetupRecovery[];
    onChanged: () => Promise<void> | void;
  },
) {
  const [viewOutcomes, setViewOutcomes] = useState<
    Record<string, CmuxViewOutcome>
  >({});
  const [discarding, setDiscarding] = useState<Record<string, boolean>>({});
  const discardOperations = useRef<
    Record<
      string,
      { surfaceRouteId: string; sessionId: string; operationId: string }
    >
  >({});
  const durableSurfaceRefs = useRef<Record<string, CmuxSessionSurface>>({});
  const durableSurfaceForSession = (sessionId: string) => {
    const probe = admission.probes.find((item) =>
      item.session_id === sessionId
    );
    const latest = cmuxNewestSurface(
      durableSurfaceRefs.current[sessionId],
      probe?.cmux_surface || cmuxSurfaces[sessionId],
    );
    if (latest) durableSurfaceRefs.current[sessionId] = latest;
    return latest;
  };
  const commitViewOutcome = (sessionId: string, outcome: CmuxViewOutcome) => {
    const committed = cmuxOutcomeWithDurableSurface(
      outcome,
      durableSurfaceForSession(sessionId),
    ) || outcome;
    setViewOutcomes((current) => ({ ...current, [sessionId]: committed }));
    return committed;
  };
  const presentationOutcomeForSession = (sessionId: string) =>
    cmuxOutcomeWithDurableSurface(
      viewOutcomes[sessionId],
      durableSurfaceForSession(sessionId),
    );
  const viewOutput = async (sessionId: string) => {
    setViewOutcomes((current) => ({
      ...current,
      [sessionId]: {
        state: "pending",
        message: "Reserving the exact persistent cmux presentation…",
        retry_available: false,
      },
    }));
    try {
      const outcome = commitViewOutcome(
        sessionId,
        cmuxViewOutcomeFromSurface(await onViewSession(sessionId)),
      );
      await onChanged();
      return outcome;
    } catch (cause) {
      const outcome: CmuxViewOutcome = {
        state: "failed",
        message: cause instanceof Error ? cause.message : String(cause),
        retry_available: true,
      };
      return commitViewOutcome(sessionId, outcome);
    }
  };
  const setKeyboardControl = async (
    sessionId: string,
    surface: CmuxSessionSurface,
    action: CmuxKeyboardControlAction,
  ) => {
    const currentSurface = cmuxNewestSurface(
      durableSurfaceForSession(sessionId),
      surface,
    ) || surface;
    const actionability = cmuxSurfacePresentation(currentSurface);
    if (
      action === "acquire"
        ? !actionability.takeAvailable
        : !actionability.releaseAvailable
    ) return;
    try {
      commitViewOutcome(
        sessionId,
        await requestCmuxKeyboardControl(sessionId, currentSurface, action),
      );
      await onChanged();
    } catch (cause) {
      commitViewOutcome(
        sessionId,
        cmuxViewOutcomeFromSurface({
          state: "failed",
          message: cause instanceof Error ? cause.message : String(cause),
          retry_available: true,
          surface: currentSurface,
        }),
      );
    }
  };
  const takeKeyboardControl = async (sessionId: string) => {
    const outcome = await viewOutput(sessionId);
    if (
      !outcome?.surface ||
      !cmuxSurfacePresentation(outcome.surface).takeAvailable
    ) return;
    await setKeyboardControl(sessionId, outcome.surface, "acquire");
  };
  const discardUnknown = async (sessionId: string) => {
    const outcome = presentationOutcomeForSession(sessionId);
    const surface = outcome?.surface;
    if (!surface || !cmuxSurfacePresentation(surface).discardAvailable) return;
    const prior = discardOperations.current[sessionId];
    const retry =
      prior?.surfaceRouteId === surface.id && prior.sessionId === sessionId
        ? prior
        : { surfaceRouteId: surface.id, sessionId, operationId: operationId() };
    discardOperations.current[sessionId] = retry;
    setDiscarding((current) => ({ ...current, [sessionId]: true }));
    try {
      const result = await discardUnknownCmuxSurface(
        sessionId,
        surface.id,
        retry.operationId,
      );
      delete discardOperations.current[sessionId];
      commitViewOutcome(sessionId, cmuxViewOutcomeFromSurface(result));
      await onChanged();
    } catch (cause) {
      if (outcome) {
        commitViewOutcome(sessionId, {
          ...outcome,
          message: cause instanceof Error ? cause.message : String(cause),
        });
      }
    } finally {
      setDiscarding((current) => ({ ...current, [sessionId]: false }));
    }
  };
  return (
    <div className="runtime-admission">
      <p>
        <strong>{admission.state.replaceAll("_", " ")}</strong>{" "}
        · fresh calls before launch:{" "}
        <strong>{admission.fresh_call_count}</strong>
        <br />Scope <code>{admission.scope_hash}</code>
      </p>
      {admission.state === "pending_approval" && (
        <button
          className="primary"
          disabled={!!busy}
          onClick={() => onAuthorize(admission.id, admission.scope_hash)}
        >
          Approve {admission.fresh_call_count} scoped fresh runtime calls
        </button>
      )}
      {admission.failure_reason && (
        <p className="error">{admission.failure_reason}</p>
      )}
      <div className="profile-list">
        {admission.probes.map((probe) => {
          const recovery = recoveries.find((item) =>
            item.session_id === probe.session_id &&
            item.runtime_admission_id === admission.id
          );
          const retainedResume = probe.role !== "final_verifier" &&
            probe.session_status === "exited" && probe.has_native_session &&
            probe.state === "running";
          const publish = probe.state === "evidence_recorded" &&
            probe.session_status === "exited";
          const launch = probe.state === "authorized";
          const cmuxOutcome = probe.session_id
            ? presentationOutcomeForSession(probe.session_id)
            : undefined;
          const cmuxPresentation = cmuxSurfacePresentation(
            cmuxOutcome?.surface,
            cmuxOutcome?.state === "pending",
          );
          const cmuxSurface = cmuxOutcome?.surface;
          return (
            <div key={probe.role}>
              <strong>{roleName(probe.role)}</strong>
              <small>
                {probe.profile.provider} · {probe.profile.model} · {probe
                  .profile.effort} · {probe.state.replaceAll("_", " ")}
                {probe.session_status
                  ? ` · ${probe.session_status} · ${
                    probe.readiness || "unknown trust"
                  }`
                  : ""}
              </small>
              <small>
                adapter {probe.adapter} · capability{" "}
                <code>{probe.capability_key.slice(0, 12)}</code>
              </small>
              {probe.failure_reason && (
                <small className="error">{probe.failure_reason}</small>
              )}
              {probe.session_id && (
                <>
                  <button
                    disabled={probe.session_status === "running" &&
                      !cmuxPresentation.viewAvailable}
                    onClick={() => void viewOutput(probe.session_id!)}
                  >
                    {cmuxPresentation.viewLabel}
                  </button>
                  {probe.session_status === "running" && (
                    <button
                      disabled={!cmuxPresentation.takeAvailable}
                      onClick={() =>
                        void takeKeyboardControl(probe.session_id!)}
                    >
                      Take keyboard control
                    </button>
                  )}
                  {probe.session_status === "running" &&
                    cmuxSurface && cmuxPresentation.releaseAvailable && (
                    <button
                      onClick={() =>
                        void setKeyboardControl(
                          probe.session_id!,
                          cmuxSurface,
                          "release",
                        )}
                    >
                      Release keyboard control
                    </button>
                  )}
                </>
              )}
              {probe.session_id && (
                <CmuxRouteNotice
                  outcome={cmuxOutcome}
                  onRetry={() => void viewOutput(probe.session_id!)}
                  onDiscard={() => void discardUnknown(probe.session_id!)}
                  discarding={discarding[probe.session_id]}
                />
              )}
              {recovery && (
                <SetupRecovery recovery={recovery} onChanged={onChanged} />
              )}
              {launch && (
                <button
                  disabled={!!busy}
                  onClick={() => onLaunch(probe.role)}
                >
                  Launch bounded probe
                </button>
              )}
              {retainedResume && !recovery && (
                <button
                  disabled={!!busy}
                  onClick={() => onResume(probe.role)}
                >
                  Resume same native session
                </button>
              )}
              {publish && (
                <button
                  className="primary"
                  disabled={!!busy}
                  onClick={() => onPublish(probe.role)}
                >
                  Publish exact ordinary proof
                </button>
              )}
              {["failed", "stale"].includes(probe.state) && (
                <button
                  className="primary"
                  disabled={!!busy}
                  onClick={() => onCorrect(probe.role)}
                >
                  Prepare corrected runtime verification
                </button>
              )}
              {probe.role === "final_verifier" && probe.session_id &&
                !["current", "published"].includes(probe.state) && (
                <small>
                  Fresh-only: this session is never resumed. A new call requires
                  a separate explicit corrected retry.
                </small>
              )}
            </div>
          );
        })}
      </div>
      <p className="hint">
        Authorization covers only these displayed disposable-fixture probes. It
        grants no installation, implementation, acceptance, retry, task launch,
        or blanket agent permission.
      </p>
    </div>
  );
}

function ActiveConfiguration(
  { project, runtimeAdmission }: {
    project: Project;
    runtimeAdmission?: RuntimeAdmissionState;
  },
) {
  const configuration = project.trip!.configuration!;
  const config = configuration.config;
  const verification = config.verification || configuration.verification;
  const projectRoles = isRecord(project.settings.roles)
    ? project.settings.roles
    : {};
  const profiles = config.profiles || {};
  const roleProfiles = config.roles || {};
  const currentRuntimeProofs =
    runtimeAdmission?.probes.filter((probe) =>
      ["current", "published"].includes(probe.state)
    ).length || 0;
  return (
    <section className="active-configuration">
      <header>
        <div>
          <span className="eyebrow">Current configuration</span>
          <h5>Project setup is activated</h5>
        </div>
        <span className="badge supported">Installed</span>
      </header>
      <div className="configuration-cards">
        <article>
          <small>Configuration</small>
          <strong>Revision {configuration.revision}</strong>
          <span>
            {configuration.preflight.receipt_count} preflight receipts
          </span>
        </article>
        <article>
          <small>Runtime readiness</small>
          <strong>
            {runtimeAdmission
              ? runtimeAdmission.state.replaceAll("_", " ")
              : "Not verified"}
          </strong>
          <span>
            {runtimeAdmission
              ? `${currentRuntimeProofs}/${runtimeAdmission.probes.length} current proofs`
              : "Installation does not grant runtime authority"}
          </span>
        </article>
        <article>
          <small>Agent profiles</small>
          <strong>{Object.keys(roleProfiles).length + 1} configured</strong>
          <span>Host manager plus delegated roles</span>
        </article>
      </div>
      <details>
        <summary>View exact activated configuration</summary>
        <p>
          <strong>Guidance:</strong>{" "}
          {(config.guidance || []).join(" · ") || "None"}
        </p>
        <p>
          <strong>Coverage:</strong>{" "}
          {config.testing?.coverage || "Not recorded"} ·{" "}
          <strong>Observability:</strong>{" "}
          {String(config.observability?.cmux || "not recorded")} ·{" "}
          <strong>Preflight receipts:</strong>{" "}
          {configuration.preflight.receipt_count}
        </p>
        <p>
          <strong>Documentation:</strong>{" "}
          {String(config.documentation?.no_change_text || "")}
        </p>
        <div className="profile-list">
          <div>
            <strong>Host manager</strong>
            <small>
              {isRecord(projectRoles.manager)
                ? `${String(projectRoles.manager.provider)} · ${
                  String(projectRoles.manager.model)
                } · ${String(projectRoles.manager.effort)}`
                : "Not recorded"}
            </small>
          </div>
          {delegatedRoles.map((role) => {
            const profileId = roleProfiles[role]?.profile;
            const profile = profileId ? profiles[profileId] : undefined;
            return (
              <div key={role}>
                <strong>{roleName(role)}</strong>
                <small>
                  {profile
                    ? `${profile.provider} · ${profile.model} · ${profile.effort} · ${profile.authority} · ${profile.session} · adapter ${profile.adapter}`
                    : "Not recorded"}
                </small>
              </div>
            );
          })}
        </div>
        <p>
          <strong>Adapters:</strong>{" "}
          {Object.keys(configuration.adapters.adapters || {}).join(" · ") ||
            "None"}
        </p>
        <div className="form-grid three">
          <div>
            <strong>Focused</strong>
            <pre>{(verification?.focused || []).join("\n") || "None"}</pre>
          </div>
          <div>
            <strong>Broad</strong>
            <pre>{(verification?.broad || []).join("\n") || "None"}</pre>
          </div>
          <div>
            <strong>Cleanup</strong>
            <pre>{(verification?.cleanup || []).join("\n") || "None"}</pre>
          </div>
        </div>
        <p>
          <code>{configuration.configuration_hash}</code>
        </p>
      </details>
    </section>
  );
}
