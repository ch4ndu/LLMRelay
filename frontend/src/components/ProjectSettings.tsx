import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { useEffect, useState } from "react";
import { command, operation, operationId } from "../api";
import type {
  AgentProfile,
  CmuxSessionSurface,
  CmuxViewOutcome,
  ExecutionCheckInput,
  ExecutionConfigurationInput,
  Project,
  ProjectExecution,
  Task,
  TripSetupState,
  TripVerificationCheck,
  ValidationProvider,
  WorkflowCommand,
  WorkflowFamily,
} from "../types";
import { ProjectSetup } from "./ProjectSetup";
import { ModelSelector } from "./ModelSelector";
import { AdmissionControls } from "./RoleSettings";
import {
  UncertainRequestNotice,
  type WorkflowCommandRunner,
  useWorkflowCommands,
} from "./WorkflowControls";

const efforts = ["low", "medium", "high", "xhigh", "max", "ultra"];

type CheckDraft = {
  check_key: string;
  original_text: string;
  command_kind: "exact_argv" | "exact_shell";
  executable: string;
  /** One argument per line, so arguments may contain spaces. */
  arguments: string;
  shell_command: string;
  cwd: string;
  timeout_seconds: number;
};

type ConfigurationDraft = {
  guidance_paths: string;
  testing_coverage: ExecutionConfigurationInput["testing_coverage"];
  documentation_policy: string;
  checks: CheckDraft[];
};

const textField = (value: Record<string, unknown>, key: string) => {
  const item = value[key];
  return typeof item === "string" ? item : "";
};

// The latest saved revision, read back from its untyped record, so a new
// revision starts from what was approved last.
const configurationDraft = (
  execution: ProjectExecution | undefined,
): ConfigurationDraft => {
  const latest = execution?.configurations[0];
  const config = latest?.configuration ?? {};
  const guidance = Array.isArray(config.guidance) ? config.guidance : [];
  const coverage = config.testing_coverage;
  return {
    guidance_paths: guidance.flatMap((item: unknown) =>
      typeof item === "object" && item !== null &&
        typeof Reflect.get(item, "path") === "string"
        ? [String(Reflect.get(item, "path"))]
        : []
    ).join("\n"),
    testing_coverage: coverage === "minimal" || coverage === "extensive"
      ? coverage
      : "moderate",
    documentation_policy: typeof config.documentation_policy === "string"
      ? config.documentation_policy
      : "",
    checks: (latest?.checks ?? []).map((check): CheckDraft => {
      const argumentsValue = check.arguments;
      const timeout = check.timeout_seconds;
      return {
        check_key: textField(check, "check_key"),
        original_text: textField(check, "original_text"),
        command_kind: check.command_kind === "exact_shell"
          ? "exact_shell"
          : "exact_argv",
        executable: textField(check, "executable"),
        arguments: Array.isArray(argumentsValue)
          ? argumentsValue.filter((item) => typeof item === "string").join("\n")
          : "",
        shell_command: textField(check, "shell_command"),
        cwd: textField(check, "cwd") || ".",
        timeout_seconds: typeof timeout === "number" ? timeout : 600,
      };
    }),
  };
};

const checkInput = (check: CheckDraft): ExecutionCheckInput =>
  check.command_kind === "exact_shell"
    ? {
      check_key: check.check_key.trim(),
      command_kind: "exact_shell",
      executable: null,
      arguments: [],
      shell_command: check.shell_command,
      cwd: check.cwd.trim(),
      timeout_seconds: check.timeout_seconds,
      original_text: check.original_text.trim(),
    }
    : {
      check_key: check.check_key.trim(),
      command_kind: "exact_argv",
      executable: check.executable.trim(),
      arguments: check.arguments.split("\n").filter((line) => line !== ""),
      shell_command: null,
      cwd: check.cwd.trim(),
      timeout_seconds: check.timeout_seconds,
      original_text: check.original_text.trim(),
    };

/** Saves a new, separately approved execution configuration revision. */
function ExecutionConfigurationEditor(
  { project, execution, commands }: {
    project: Project;
    execution?: ProjectExecution;
    commands: WorkflowCommandRunner;
  },
) {
  const [draft, setDraft] = useState(() => configurationDraft(execution));
  const latestRevision = execution?.configurations[0]?.revision ?? 0;
  const updateCheck = (index: number, change: Partial<CheckDraft>) =>
    setDraft((current) => ({
      ...current,
      checks: current.checks.map((check, position) =>
        position === index ? { ...check, ...change } : check
      ),
    }));
  const key = `configuration:${project.id}`;
  const save = () =>
    void commands.submit(key, (operation_id) => ({
      kind: "save_execution_configuration",
      operation_id,
      project_id: project.id,
      expected_revision: latestRevision,
      configuration: {
        guidance_paths: draft.guidance_paths.split("\n").map((line) =>
          line.trim()
        ).filter(Boolean),
        testing_coverage: draft.testing_coverage,
        documentation_policy: draft.documentation_policy.trim(),
        checks: draft.checks.map(checkInput),
      },
    }));
  return (
    <fieldset className="execution-configuration" disabled={commands.busy}>
      <legend>
        {latestRevision
          ? `New execution configuration (current revision ${latestRevision})`
          : "First execution configuration"}
      </legend>
      <p className="hint">
        Guidance files are read from the project's registered commit when you
        save; later edits in the folder are not included. Saving creates a new
        revision. Tasks and admissions keep the revision they chose.
      </p>
      <label>
        Guidance files <small>One repository-relative path per line</small>
        <textarea
          value={draft.guidance_paths}
          onChange={(event) =>
            setDraft({ ...draft, guidance_paths: event.target.value })}
        />
      </label>
      <label>
        Testing coverage
        <select
          value={draft.testing_coverage}
          onChange={(event) =>
            setDraft({
              ...draft,
              testing_coverage: event.target.value === "minimal"
                ? "minimal"
                : event.target.value === "extensive"
                ? "extensive"
                : "moderate",
            })}
        >
          <option value="minimal">Minimal</option>
          <option value="moderate">Moderate</option>
          <option value="extensive">Extensive</option>
        </select>
      </label>
      <label>
        Documentation policy
        <textarea
          value={draft.documentation_policy}
          onChange={(event) =>
            setDraft({ ...draft, documentation_policy: event.target.value })}
        />
      </label>
      {draft.checks.map((check, index) => (
        <fieldset className="execution-check" key={index}>
          <legend>Check {index + 1}</legend>
          <label>
            Name <small>Lowercase letters, digits, - or _</small>
            <input
              value={check.check_key}
              onChange={(event) =>
                updateCheck(index, { check_key: event.target.value })}
            />
          </label>
          <label>
            What it verifies
            <input
              value={check.original_text}
              onChange={(event) =>
                updateCheck(index, { original_text: event.target.value })}
            />
          </label>
          <label>
            Command form
            <select
              value={check.command_kind}
              onChange={(event) =>
                updateCheck(index, {
                  command_kind: event.target.value === "exact_shell"
                    ? "exact_shell"
                    : "exact_argv",
                })}
            >
              <option value="exact_argv">Program and arguments</option>
              <option value="exact_shell">One shell command</option>
            </select>
          </label>
          {check.command_kind === "exact_argv"
            ? (
              <>
                <label>
                  Program
                  <input
                    value={check.executable}
                    onChange={(event) =>
                      updateCheck(index, { executable: event.target.value })}
                  />
                </label>
                <label>
                  Arguments <small>One per line</small>
                  <textarea
                    value={check.arguments}
                    onChange={(event) =>
                      updateCheck(index, { arguments: event.target.value })}
                  />
                </label>
              </>
            )
            : (
              <label>
                Shell command
                <input
                  value={check.shell_command}
                  onChange={(event) =>
                    updateCheck(index, { shell_command: event.target.value })}
                />
              </label>
            )}
          <label>
            Folder <small>Repository-relative; . for the root</small>
            <input
              value={check.cwd}
              onChange={(event) =>
                updateCheck(index, { cwd: event.target.value })}
            />
          </label>
          <label>
            Time limit in seconds
            <input
              type="number"
              min={1}
              max={3600}
              value={check.timeout_seconds}
              onChange={(event) =>
                updateCheck(index, {
                  timeout_seconds: Number(event.target.value),
                })}
            />
          </label>
          <button
            type="button"
            onClick={() =>
              setDraft({
                ...draft,
                checks: draft.checks.filter((_, position) => position !== index),
              })}
          >
            Remove check
          </button>
        </fieldset>
      ))}
      <div className="button-row">
        <button
          type="button"
          onClick={() =>
            setDraft({
              ...draft,
              checks: [...draft.checks, {
                check_key: "",
                original_text: "",
                command_kind: "exact_argv",
                executable: "",
                arguments: "",
                shell_command: "",
                cwd: ".",
                timeout_seconds: 600,
              }],
            })}
        >
          Add check
        </button>
        <button
          type="button"
          className="primary"
          disabled={!draft.documentation_policy.trim()}
          onClick={save}
        >
          Save configuration revision
        </button>
      </div>
      {commands.uncertain.has(key) && (
        <UncertainRequestNotice changed={commands.reviewed.has(key)} />
      )}
    </fieldset>
  );
}

type ProfileDraft = {
  profile?: AgentProfile;
  display_name: string;
  provider: ValidationProvider;
  model: string;
  effort: string;
  instructions_text: string;
};

type ProfileFields = Omit<ProfileDraft, "profile">;
type ProfileTarget = { id: string; version: number };

const saveProfileCommand = (
  projectId: string,
  target: ProfileTarget | undefined,
  fields: ProfileFields,
  operation_id: string,
): WorkflowCommand => ({
  kind: "save_agent_profile",
  operation_id,
  project_id: projectId,
  profile_id: target?.id ?? null,
  expected_version: target?.version ?? null,
  profile: {
    display_name: fields.display_name.trim(),
    provider: fields.provider,
    model: fields.model.trim(),
    effort: fields.effort,
    instructions_text: fields.instructions_text,
  },
});

// Reads back only a stored new-profile request for this project, so it can be
// resent unchanged under its original operation ID.
const storedProfileCreation = (
  body: string,
  projectId: string,
): ProfileFields | undefined => {
  const field = (from: unknown, name: string): unknown =>
    typeof from === "object" && from !== null ? Reflect.get(from, name) : undefined;
  let value: unknown;
  try {
    value = JSON.parse(body);
  } catch {
    return undefined;
  }
  const profile = field(value, "profile");
  const display_name = field(profile, "display_name");
  const provider = field(profile, "provider");
  const model = field(profile, "model");
  const effort = field(profile, "effort");
  const instructions_text = field(profile, "instructions_text");
  if (
    field(value, "kind") !== "save_agent_profile" ||
    field(value, "project_id") !== projectId ||
    field(value, "profile_id") !== null || field(value, "expected_version") !== null ||
    typeof display_name !== "string" ||
    (provider !== "codex" && provider !== "claude") ||
    typeof model !== "string" || typeof effort !== "string" ||
    typeof instructions_text !== "string"
  ) return undefined;
  return { display_name, provider, model, effort, instructions_text };
};

/** Creates a profile, or a new revision of one; it never changes old runs. */
function AgentProfileEditor(
  { project, draft, commands, onDone }: {
    project: Project;
    draft: ProfileDraft;
    commands: WorkflowCommandRunner;
    onDone: () => void;
  },
) {
  // A new profile has no version fence: each operation ID creates one profile.
  // While a creation's outcome is unknown, only that exact request may be
  // sent again; a changed one could create a second profile.
  const creationKey = `profile:${project.id}:new`;
  const pendingCreation = draft.profile
    ? undefined
    : commands.unresolved(creationKey);
  const original = pendingCreation &&
    storedProfileCreation(pendingCreation.body, project.id);
  const [value, setValue] = useState<ProfileDraft>(() =>
    original ? { ...draft, ...original } : draft
  );
  const [recovered, setRecovered] = useState<ProfileTarget>();
  const target = recovered ??
    (draft.profile
      ? { id: draft.profile.id, version: draft.profile.version }
      : undefined);
  const key = target ? `profile:${project.id}:${target.id}` : creationKey;
  const save = async () => {
    const saved = await commands.submit(
      key,
      (operation_id) => saveProfileCommand(project.id, target, value, operation_id),
      { keepUnresolved: !target },
    );
    if (saved) onDone();
  };
  const recover = async () => {
    if (!original) return;
    const created = await commands.submit(
      creationKey,
      (operation_id) =>
        saveProfileCommand(project.id, undefined, original, operation_id),
      { keepUnresolved: true },
    );
    if (!created) return;
    if (created.version === null) {
      onDone();
      return;
    }
    setRecovered({ id: created.entity_id, version: created.version });
  };
  return (
    <fieldset className="agent-profile-editor" disabled={commands.busy}>
      <legend>
        {draft.profile
          ? `Edit ${draft.profile.display_name}`
          : recovered
          ? "Edit the recovered profile"
          : "New agent profile"}
      </legend>
      {!target && pendingCreation && !commands.busy && (
        <div className="warning" role="status">
          <p>
            {original
              ? `An earlier request to create the profile “${original.display_name}” (${original.provider} · ${original.model} · ${original.effort}) may already have been applied.`
              : "An earlier profile creation may already have been applied, but its stored request could not be read."}{" "}
            Your edits stay here but are not sent as a new profile until that
            request is settled.
          </p>
          {original && (
            <button type="button" onClick={() => void recover()}>
              Check the earlier profile creation
            </button>
          )}
          <small>
            Checking sends that exact earlier request again; LLMRelay creates
            the profile at most once.
          </small>
        </div>
      )}
      {recovered && (
        <p className="hint" role="status">
          The earlier request created this profile once. Saving now changes that
          profile instead of creating another.
        </p>
      )}
      <label>
        Name
        <input
          value={value.display_name}
          maxLength={120}
          onChange={(event) =>
            setValue({ ...value, display_name: event.target.value })}
        />
      </label>
      <label>
        Provider
        <select
          value={value.provider}
          onChange={(event) =>
            setValue({
              ...value,
              provider: event.target.value === "claude" ? "claude" : "codex",
            })}
        >
          <option value="codex">Codex</option>
          <option value="claude">Claude</option>
        </select>
      </label>
      <ModelSelector
        provider={value.provider}
        value={value.model}
        onChange={(model) => setValue({ ...value, model })}
        label="Profile exact model"
        disabled={commands.busy}
      />
      <label>
        Effort
        <select
          value={value.effort}
          onChange={(event) => setValue({ ...value, effort: event.target.value })}
        >
          {efforts.map((effort) => <option key={effort}>{effort}</option>)}
        </select>
      </label>
      <label>
        Instructions for the agent
        <textarea
          value={value.instructions_text}
          onChange={(event) =>
            setValue({ ...value, instructions_text: event.target.value })}
        />
      </label>
      {draft.profile && (
        <p className="hint">
          Saved instructions are not shown again. Enter the instructions this
          profile should use; any change to the provider, model, effort or
          instructions creates a new revision that needs its own admission.
        </p>
      )}
      <div className="button-row">
        <button
          type="button"
          className="primary"
          disabled={!value.display_name.trim() || !value.model.trim() ||
            (!target && !!pendingCreation)}
          onClick={() => void save()}
        >
          Save profile
        </button>
        <button type="button" onClick={onDone}>Cancel</button>
      </div>
      {target && commands.uncertain.has(key) && (
        <UncertainRequestNotice changed={commands.reviewed.has(key)} />
      )}
    </fieldset>
  );
}

/**
 * The project's default workflow for future tasks, its Single agent profiles,
 * configuration and admissions. Existing tasks keep their own workflow.
 */
function SingleAgentProjectSettings(
  { project, execution, tasks, onChanged }: {
    project: Project;
    execution: ProjectExecution;
    tasks: Task[];
    onChanged: () => void;
  },
) {
  const commands = useWorkflowCommands(
    `llmrelay.workflow.project.${project.id}`,
    onChanged,
  );
  const [defaultFamily, setDefaultFamily] = useState<WorkflowFamily>(
    execution.default_family,
  );
  useEffect(() => setDefaultFamily(execution.default_family), [
    execution.default_family,
    execution.default_version,
  ]);
  const [profileDraft, setProfileDraft] = useState<ProfileDraft>();
  const [editingConfiguration, setEditingConfiguration] = useState(false);
  const latestConfiguration = execution.configurations[0];
  const defaultKey = `default:${project.id}`;
  const saveDefault = () =>
    void commands.submit(defaultKey, (operation_id) => ({
      kind: "set_project_workflow_default",
      operation_id,
      project_id: project.id,
      expected_version: execution.default_version,
      family: defaultFamily,
    }));
  return (
    <div className="single-agent-project">
      <fieldset className="workflow-default" disabled={commands.busy}>
        <legend>Default workflow for new tasks</legend>
        {(["trip", "single_agent"] as const).map((option) => (
          <label className="toggle" key={option}>
            <input
              type="radio"
              name={`workflow-default-${project.id}`}
              checked={defaultFamily === option}
              onChange={() => setDefaultFamily(option)}
            />
            {option === "trip" ? "TRIP" : "Single agent"}
          </label>
        ))}
        <small>
          Only tasks created later start with this choice. Existing tasks,
          recipes and schedules keep their workflow.
        </small>
        <button
          type="button"
          disabled={defaultFamily === execution.default_family}
          onClick={saveDefault}
        >
          Save default workflow
        </button>
        {commands.uncertain.has(defaultKey) && (
          <UncertainRequestNotice changed={commands.reviewed.has(defaultKey)} />
        )}
      </fieldset>
      <details>
        <summary>Single agent profiles and configuration</summary>
        <h4>Execution configuration</h4>
        {latestConfiguration
          ? (
            <p>
              Revision {latestConfiguration.revision}, approved{" "}
              {new Date(latestConfiguration.approved_at).toLocaleString()},
              with {latestConfiguration.checks.length} check
              {latestConfiguration.checks.length === 1 ? "" : "s"}.
            </p>
          )
          : <p className="empty">No execution configuration has been saved.</p>}
        {editingConfiguration
          ? (
            <>
              <ExecutionConfigurationEditor
                key={latestConfiguration?.id ?? "first"}
                project={project}
                execution={execution}
                commands={commands}
              />
              <button
                type="button"
                onClick={() => setEditingConfiguration(false)}
              >
                Close configuration editor
              </button>
            </>
          )
          : (
            <button type="button" onClick={() => setEditingConfiguration(true)}>
              {latestConfiguration
                ? "Create a new configuration revision"
                : "Create an execution configuration"}
            </button>
          )}
        <h4>Agent profiles</h4>
        {execution.profiles.map((profile) => (
          <article className="review-row" key={profile.id}>
            <strong>{profile.display_name}</strong>
            {profile.current_revision
              ? (
                <span>
                  {profile.current_revision.provider === "claude"
                    ? "Claude"
                    : "Codex"} · {profile.current_revision.model} ·{" "}
                  {profile.current_revision.effort}
                </span>
              )
              : <span>No current revision</span>}
            <button
              type="button"
              onClick={() =>
                setProfileDraft({
                  profile,
                  display_name: profile.display_name,
                  provider: profile.current_revision?.provider ?? "codex",
                  model: profile.current_revision?.model ?? "",
                  effort: profile.current_revision?.effort ?? "medium",
                  instructions_text: "",
                })}
            >
              Edit<span className="visually-hidden">: {profile.display_name}</span>
            </button>
            {profile.current_revision && latestConfiguration && (
              <AdmissionControls
                projectId={project.id}
                execution={execution}
                profileRevisionId={profile.current_revision.id}
                configId={latestConfiguration.id}
                tasks={tasks}
                commands={commands}
              />
            )}
            <TechnicalDetails>
              <p>
                profile {profile.id} · version {profile.version}
                {profile.current_revision
                  ? ` · revision ${profile.current_revision.revision} · ${profile.current_revision.profile_hash}`
                  : ""}
              </p>
            </TechnicalDetails>
          </article>
        ))}
        {!execution.profiles.length && (
          <p className="empty">No agent profile has been saved.</p>
        )}
        {profileDraft
          ? (
            <AgentProfileEditor
              key={profileDraft.profile?.id ?? "new"}
              project={project}
              draft={profileDraft}
              commands={commands}
              onDone={() => setProfileDraft(undefined)}
            />
          )
          : (
            <button
              type="button"
              onClick={() =>
                setProfileDraft({
                  display_name: "",
                  provider: "codex",
                  model: "",
                  effort: "medium",
                  instructions_text: "",
                })}
            >
              Add an agent profile
            </button>
          )}
        <p className="hint">
          Admissions here cover the newest configuration revision. A profile
          becomes usable only after its admission is activated, which needs a
          probe run of that exact profile. A probe is offered only when
          LLMRelay reports that it can start one; otherwise the reason is
          shown, for example that this agent version's Single agent mode has
          not been qualified.
        </p>
      </details>
      {commands.error && <ErrorNotice error={commands.error} />}
    </div>
  );
}

export function ProjectSettings(
  {
    project,
    setup,
    tripChecks = [],
    cmuxSurfaces = {},
    execution,
    tasks = [],
    onChanged,
    onViewSession = async () => {
      throw new Error("cmux attachment routing is not available");
    },
  }: {
    project: Project;
    setup?: TripSetupState;
    tripChecks?: TripVerificationCheck[];
    cmuxSurfaces?: Record<string, CmuxSessionSurface>;
    /** This project's Single agent setup; absent from older services. */
    execution?: ProjectExecution;
    /** Every task, so an admission's probe task can be found. */
    tasks?: Task[];
    onChanged: () => Promise<void> | void;
    onViewSession?: (
      sessionId: string,
    ) => Promise<CmuxViewOutcome>;
  },
) {
  const [path, setPath] = useState(project.repository_path);
  const [legacyPath, setLegacyPath] = useState("");
  const [legacyPreview, setLegacyPreview] = useState<Record<string, unknown>>();
  const [error, setError] = useState("");
  const [reportReminders, setReportReminders] = useState(
    project.settings.role_report_reminders === true,
  );
  const [savingReminders, setSavingReminders] = useState(false);
  useEffect(() => setPath(project.repository_path), [project.repository_path]);
  useEffect(() => {
    setReportReminders(project.settings.role_report_reminders === true);
  }, [project.id, project.version, project.settings.role_report_reminders]);

  const saveReportReminders = async () => {
    setSavingReminders(true);
    try {
      await command({
        kind: "update_project_settings",
        operation_id: operationId(),
        project_id: project.id,
        expected_version: project.version,
        settings: { ...project.settings, role_report_reminders: reportReminders },
      });
      setError("");
      await onChanged();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSavingReminders(false);
    }
  };

  const apply = async (body: Record<string, unknown>) => {
    try {
      await command({ ...body, operation_id: operationId() } as never);
      setError("");
      onChanged();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  const previewLegacy = async () => {
    try {
      setLegacyPreview(
        await operation({ kind: "legacy_preview", source: legacyPath }),
      );
      setError("");
    } catch (cause) {
      setLegacyPreview(undefined);
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  const importLegacy = async () => {
    try {
      await operation({
        kind: "legacy_import",
        operation_id: operationId(),
        project_id: project.id,
        expected_project_version: project.version,
        source: legacyPath,
        expected_source_hash: legacyPreview?.source_hash,
      });
      setLegacyPreview(undefined);
      setLegacyPath("");
      onChanged();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  return (
    <section className="panel project-settings">
      <header>
        <div>
          <h3>{project.display_name}</h3>
          <span>{project.repository_identity}</span>
        </div>
        <span
          className={project.queue_paused ? "badge waiting" : "badge supported"}
        >
          {project.queue_paused ? "Pickup paused" : "Pickup on"}
        </span>
      </header>
      <label>
        Repository path<div className="compact-fields">
          <input
            value={path}
            onChange={(event) => setPath(event.target.value)}
          />
          <button
            disabled={!path}
            onClick={() =>
              apply({
                kind: "relink_project",
                project_id: project.id,
                path,
                expected_version: project.version,
              })}
          >
            Validate and relink
          </button>
        </div>
        <small className="hint">
          Relinking checks the folder and makes new tasks start from its
          current commit. Use it after moving the folder or committing changes.
          It is available when no task in this project is running.
        </small>
      </label>
      <ProjectSetup
        project={project}
        setup={setup}
        cmuxSurfaces={cmuxSurfaces}
        onChanged={onChanged}
        onViewSession={onViewSession}
      />
      {execution && (
        <SingleAgentProjectSettings
          project={project}
          execution={execution}
          tasks={tasks}
          onChanged={() => void onChanged()}
        />
      )}
      <div>
        <label>
          <input
            type="checkbox"
            checked={reportReminders}
            disabled={savingReminders}
            onChange={(event) => setReportReminders(event.target.checked)}
          />
          Remind agents to submit reports
        </label>
        <p className="hint">
          Off by default. After an eligible agent finishes a turn without its
          required report, LLMRelay may send an additional prompt and use provider
          quota. Final verification is excluded. Each agent generation has two
          reminder reservations for its entire lifetime, including resumed
          sessions. A reservation stays spent even if it is cancelled or delivery
          is uncertain. Turning this off prevents future writes; prompts already
          written cannot be retracted, and turning it back on does not reset the
          allowance.
        </p>
        <button disabled={savingReminders} onClick={saveReportReminders}>
          {savingReminders ? "Saving reminders…" : "Save report reminders"}
        </button>
      </div>
      <details>
        <summary>TRIP verification checks ({tripChecks.length})</summary>
        <p className="hint">
          These checks come from the project's setup. For each task the manager
          selects the ones that apply, and each check still needs your approval
          for the exact result it runs on.
        </p>
        <div className="check-suite-list">
          {tripChecks.map((check) => (
            <article key={check.id}>
              <div>
                <strong>{check.check_key} · {check.category}</strong>
                <small>
                  {check.original_text} · {check.cwd} · {check.timeout_seconds}s
                </small>
              </div>
              <span>
                {check.enabled ? "Inherited" : "Disabled in revision"}
              </span>
              <small>
                Inputs: {check.relevant_inputs.join(", ") ||
                  "declared by manager at selection"}
              </small>
            </article>
          ))}
        </div>
        {!tripChecks.length && (
          <p className="empty">
            No inherited verification commands are activated. Verified
            completion remains blocked when applicable evidence is required.
          </p>
        )}
      </details>
      <details>
        <summary>Import legacy tasks (optional)</summary>
        <p className="hint">
          Import supported task Markdown. This does not register or change a Git
          project. Preview validates the exact source without changing it;
          import is idempotent for this project and preserves the original
          source, frontmatter, sections, runs, bugs, and unknown fields.
        </p>
        <div className="compact-fields">
          <input
            aria-label="Legacy task source"
            value={legacyPath}
            onChange={(event) => {
              setLegacyPath(event.target.value);
              setLegacyPreview(undefined);
            }}
            placeholder="Absolute .md path"
          />
          <button
            disabled={!legacyPath.startsWith("/")}
            onClick={previewLegacy}
          >
            Preview
          </button>
        </div>
        {legacyPreview && (
          <div className="preview">
            <strong>
              {String(
                (legacyPreview.frontmatter as Record<string, unknown>)?.title ||
                  "Validated legacy task",
              )}
            </strong>
            <small>
              {String(legacyPreview.source_hash)} · {String(
                (legacyPreview.validation_runs as unknown[])?.length || 0,
              )} runs · {String(
                (legacyPreview.validation_bugs as unknown[])?.length || 0,
              )} bugs
            </small>
            <button className="primary" onClick={importLegacy}>
              Import unchanged source
            </button>
          </div>
        )}
      </details>
      {error && <ErrorNotice error={error} />}
    </section>
  );
}
