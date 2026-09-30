import { ErrorNotice } from "./ErrorNotice";
import { useRef, useState } from "react";
import { ApiError, command, operationId } from "../api";
import {
  type AppState,
  type ProfileSet,
  type Project,
  type RecipeSchedule,
  type Role,
  type RoleConfig,
  roleLabel,
  ROLES,
  type TaskRecipe,
} from "../types";

const blankRoles = (): Record<Role, RoleConfig> =>
  Object.fromEntries(
    ROLES.map((
      role,
    ) => [role, { provider: "codex", model: "", effort: "medium" }]),
  ) as Record<Role, RoleConfig>;
const utcLabel = (value: string | null) =>
  value ? `${value} UTC · ${new Date(value).toLocaleString()}` : "Not armed";
type ProfileDraft = {
  id?: string;
  version?: number;
  name: string;
  roles: Record<Role, RoleConfig>;
};
type RecipeDraft = {
  id?: string;
  version?: number;
  name: string;
  title: string;
  description: string;
  criteria: string;
  priority: number;
  profileRevisionId: string;
  checkIds: string[];
};
type ScheduleDraft = {
  id?: string;
  version?: number;
  name: string;
  recipeRevisionId: string;
  cadence: "daily" | "weekly";
  anchorUtc: string;
};
const blankRecipe = (): RecipeDraft => ({
  name: "",
  title: "",
  description: "",
  criteria: "",
  priority: 0,
  profileRevisionId: "",
  checkIds: [],
});
const blankSchedule = (): ScheduleDraft => ({
  name: "",
  recipeRevisionId: "",
  cadence: "daily",
  anchorUtc: "",
});

type PendingRecipeCommand = {
  version: 1;
  resource: string;
  body: { kind: string; project_id: string; operation_id: string; [key: string]: unknown };
};
const pendingStorageKey = "llmrelay.m9.pending-commands.v1";
const commandFields: Record<string, string[]> = {
  upsert_profile_set: ["profile_set_id", "expected_version", "name", "roles"],
  archive_profile_set: ["profile_set_id", "expected_version"],
  upsert_task_recipe: ["recipe_id", "expected_version", "name", "title", "description", "acceptance_criteria", "priority", "profile_revision_id", "required_check_ids"],
  archive_task_recipe: ["recipe_id", "expected_version"],
  create_draft_from_recipe: ["recipe_id", "recipe_revision_id", "expected_recipe_version"],
  upsert_recipe_schedule: ["schedule_id", "expected_version", "name", "recipe_revision_id", "cadence", "anchor_utc"],
  pause_recipe_schedule: ["schedule_id", "expected_version"],
  resume_recipe_schedule: ["schedule_id", "expected_version"],
  archive_recipe_schedule: ["schedule_id", "expected_version"],
};
const resourceFor = (body: PendingRecipeCommand["body"]): string => {
  const type = body.kind.includes("profile_set") ? "profile" :
    body.kind.includes("schedule") ? "schedule" : "recipe";
  const id = type === "profile" ? body.profile_set_id :
    type === "schedule" ? body.schedule_id : body.recipe_id;
  return `${body.project_id}:${type}:${typeof id === "string" ? id : "new"}`;
};
const validPending = (value: unknown): value is PendingRecipeCommand => {
  if (!value || typeof value !== "object") return false;
  const entry = value as Record<string, unknown>;
  if (entry.version !== 1 || !entry.body || typeof entry.body !== "object") return false;
  const body = entry.body as Record<string, unknown>;
  const fields = typeof body.kind === "string" ? commandFields[body.kind] : undefined;
  if (!fields || typeof body.project_id !== "string" || !body.project_id ||
    typeof body.operation_id !== "string" || !body.operation_id ||
    Object.keys(body).sort().join() !== ["kind", "project_id", "operation_id", ...fields].sort().join() ||
    fields.some((field) => body[field] === undefined)) return false;
  const id = body.profile_set_id ?? body.recipe_id ?? body.schedule_id;
  if (id !== undefined && id !== null && typeof id !== "string") return false;
  if (String(body.kind).startsWith("archive_") || body.kind === "pause_recipe_schedule" ||
    body.kind === "resume_recipe_schedule" || body.kind === "create_draft_from_recipe") {
    if (typeof id !== "string" || !id) return false;
  }
  const expected = body.expected_version ?? body.expected_recipe_version;
  if (expected !== undefined && expected !== null &&
    (!Number.isSafeInteger(expected) || Number(expected) < 1)) return false;
  if (String(body.kind).startsWith("archive_") || body.kind === "pause_recipe_schedule" ||
    body.kind === "resume_recipe_schedule" || body.kind === "create_draft_from_recipe") {
    if (typeof expected !== "number") return false;
  }
  if (body.kind === "upsert_profile_set" &&
    (typeof body.name !== "string" || !body.roles || typeof body.roles !== "object" ||
      Array.isArray(body.roles) || Object.keys(body.roles).sort().join() !== [...ROLES].sort().join() ||
      ROLES.some((role) => {
        const config = (body.roles as Record<string, unknown>)[role];
        if (!config || typeof config !== "object" || Array.isArray(config)) return true;
        const fields = config as Record<string, unknown>;
        return Object.keys(fields).sort().join() !== "effort,model,provider" ||
          !["codex", "claude"].includes(String(fields.provider)) ||
          typeof fields.model !== "string" || typeof fields.effort !== "string";
      }))) return false;
  if (body.kind === "upsert_task_recipe" &&
    (["name", "title", "description", "profile_revision_id"].some((field) => typeof body[field] !== "string") ||
      !Array.isArray(body.acceptance_criteria) || body.acceptance_criteria.some((item) => typeof item !== "string") ||
      !Array.isArray(body.required_check_ids) || body.required_check_ids.some((item) => typeof item !== "string") ||
      typeof body.priority !== "number" || !Number.isFinite(body.priority))) return false;
  if (body.kind === "upsert_recipe_schedule" &&
    (["name", "recipe_revision_id", "anchor_utc"].some((field) => typeof body[field] !== "string") ||
      !["daily", "weekly"].includes(String(body.cadence)))) return false;
  if (body.kind === "create_draft_from_recipe" && typeof body.recipe_revision_id !== "string") return false;
  return entry.resource === resourceFor(body as PendingRecipeCommand["body"]);
};
const readPending = (): PendingRecipeCommand[] => {
  const raw = sessionStorage.getItem(pendingStorageKey);
  if (!raw) return [];
  const entries: unknown = JSON.parse(raw);
  if (!Array.isArray(entries) || entries.length > 100 || !entries.every(validPending) ||
    new Set(entries.map((entry) => entry.resource)).size !== entries.length) {
    throw new Error("Saved recipe recovery data is invalid. Mutations are blocked until it is inspected.");
  }
  return entries;
};

export function Recipes({ state, project, onChanged, onOpenTask }: {
  state: AppState;
  project?: Project;
  onChanged: () => void;
  onOpenTask: (taskId: string) => void;
}) {
  const [profile, setProfile] = useState<ProfileDraft>({
    name: "",
    roles: blankRoles(),
  });
  const [recipe, setRecipe] = useState<RecipeDraft>(blankRecipe);
  const [schedule, setSchedule] = useState<ScheduleDraft>(blankSchedule);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [pending, setPending] = useState(false);
  const identities = useRef(new Map<string, string>());
  const projectId = project?.id;
  const [formProjectId, setFormProjectId] = useState(projectId);
  const recovery = useRef<{ entries: PendingRecipeCommand[]; fault: string } | null>(null);
  if (!recovery.current) {
    try {
      recovery.current = { entries: readPending(), fault: "" };
    } catch (cause) {
      recovery.current = { entries: [], fault: cause instanceof Error ? cause.message : String(cause) };
    }
  }
  const [, showRecovery] = useState(0);
  const persistRecovery = (entries: PendingRecipeCommand[]) => {
    sessionStorage.setItem(pendingStorageKey, JSON.stringify(entries));
    recovery.current!.entries = entries;
    showRecovery((value) => value + 1);
  };
  if (formProjectId !== projectId) {
    setFormProjectId(projectId);
    setProfile({ name: "", roles: blankRoles() });
    setRecipe(blankRecipe());
    setSchedule(blankSchedule());
    setError("");
    setNotice("");
    identities.current.clear();
  }
  const profiles = state.profile_sets.filter((item) =>
    item.project_id === projectId
  );
  const recipes = state.task_recipes.filter((item) =>
    item.project_id === projectId
  );
  const schedules = state.recipe_schedules.filter((item) =>
    item.project_id === projectId
  );
  const checks = (state.trip_checks || []).filter((item) =>
    item.project_id === projectId &&
    (item.enabled === true || item.enabled === 1)
  );
  const currentConfig = project?.trip?.active_config_revision_id;
  const enabledChecks = checks.filter((item) =>
    item.config_revision_id === currentConfig
  );
  const ready = project?.trip?.readiness === "ready" && !!currentConfig;
  const submit = async (body: { kind: string; [key: string]: unknown }) => {
    if (!projectId || pending || recovery.current!.fault) return undefined;
    const keyed = { ...body, project_id: projectId };
    const key = JSON.stringify(keyed);
    const resource = resourceFor(keyed as PendingRecipeCommand["body"]);
    if (recovery.current!.entries.some((entry) => entry.resource === resource)) {
      setError("This resource has a pending request. Use Retry exact pending request to recover its result before another action.");
      return undefined;
    }
    const id = identities.current.get(key) || operationId();
    identities.current.set(key, id);
    const request = { ...keyed, operation_id: id } as PendingRecipeCommand["body"];
    const entry: PendingRecipeCommand = { version: 1, resource, body: request };
    try {
      persistRecovery([...recovery.current!.entries, entry]);
    } catch (cause) {
      setError(`Could not retain this request for recovery: ${cause instanceof Error ? cause.message : String(cause)}`);
      return undefined;
    }
    setPending(true);
    try {
      const response = await command(request);
      identities.current.delete(key);
      persistRecovery(recovery.current!.entries.filter((item) => item !== entry));
      setError("");
      setNotice("Saved. The latest state is loading.");
      onChanged();
      return response.result;
    } catch (cause) {
      if (!(cause instanceof ApiError && cause.ambiguous)) {
        identities.current.delete(key);
        persistRecovery(recovery.current!.entries.filter((item) => item !== entry));
      } else {
        onChanged();
      }
      setError(
        `${
          cause instanceof Error ? cause.message : String(cause)
        } Current state is refreshing. The exact request is retained for explicit retry; your edited draft was kept.`,
      );
      return undefined;
    } finally {
      setPending(false);
    }
  };
  const retryPending = async (entry: PendingRecipeCommand) => {
    if (pending || recovery.current!.fault || entry.body.project_id !== projectId) return;
    setPending(true);
    try {
      const response = await command(entry.body);
      persistRecovery(recovery.current!.entries.filter((item) => item !== entry));
      const result = response.result;
      if (typeof result?.entity_id === "string" && typeof result.version === "number") {
        const { entity_id, version } = result;
        const apply = <T extends { id?: string; version?: number }>(
          draft: T, id: unknown, sameCreate: boolean,
        ): T =>
          draft.id === (id ?? undefined) && (draft.version ?? 0) <= version &&
              (id !== null || sameCreate)
            ? { ...draft, id: entity_id, version }
            : draft;
        if (entry.body.kind === "upsert_profile_set") setProfile((draft) => apply(
          draft, entry.body.profile_set_id,
          draft.name === entry.body.name && ROLES.every((role) => {
            const saved = (entry.body.roles as Record<Role, RoleConfig>)[role];
            const current = draft.roles[role];
            return current.provider === saved.provider && current.model === saved.model &&
              current.effort === saved.effort;
          }),
        ));
        if (entry.body.kind === "upsert_task_recipe") setRecipe((draft) => apply(
          draft, entry.body.recipe_id,
          draft.name === entry.body.name && draft.title === entry.body.title &&
            draft.description === entry.body.description && draft.priority === entry.body.priority &&
            draft.profileRevisionId === entry.body.profile_revision_id &&
            JSON.stringify(draft.criteria.split("\n").map((text) => text.trim()).filter(Boolean)) ===
              JSON.stringify(entry.body.acceptance_criteria) &&
            JSON.stringify(draft.checkIds) === JSON.stringify(entry.body.required_check_ids),
        ));
        if (entry.body.kind === "upsert_recipe_schedule") setSchedule((draft) => apply(
          draft, entry.body.schedule_id,
          draft.name === entry.body.name && draft.recipeRevisionId === entry.body.recipe_revision_id &&
            draft.cadence === entry.body.cadence && draft.anchorUtc === entry.body.anchor_utc,
        ));
        if (entry.body.kind === "create_draft_from_recipe") onOpenTask(result.entity_id);
      }
      setError("");
      setNotice(
        "Pending request recovered. The latest state is loading. Select the recovered record before editing it if this form has changed.",
      );
      onChanged();
    } catch (cause) {
      if (!(cause instanceof ApiError && cause.ambiguous)) {
        persistRecovery(recovery.current!.entries.filter((item) => item !== entry));
      }
      setError(`${cause instanceof Error ? cause.message : String(cause)}. Current state is refreshing; inspect it before a new action.`);
      onChanged();
    } finally {
      setPending(false);
    }
  };
  const chooseProfile = (item: ProfileSet) => {
    setProfile({
      id: item.id,
      version: item.version,
      name: item.name,
      roles: structuredClone(item.roles),
    });
    setError("");
  };
  const chooseRecipe = (item: TaskRecipe) => {
    setRecipe({
      id: item.id,
      version: item.version,
      name: item.name,
      title: item.title,
      description: item.description,
      criteria: item.acceptance_criteria.join("\n"),
      priority: item.priority,
      profileRevisionId: profiles.some((profile) =>
          profile.revision_id === item.profile_revision_id && !profile.archived
        )
        ? item.profile_revision_id
        : "",
      checkIds: [],
    });
    setNotice(
      "Editing uses the current check matrix. Select required checks explicitly; historical check IDs were not mapped by name.",
    );
  };
  const chooseSchedule = (item: RecipeSchedule) => {
    setSchedule({
      id: item.id,
      version: item.version,
      name: item.name,
      recipeRevisionId: item.recipe_revision_id,
      cadence: item.cadence,
      anchorUtc: item.anchor_utc,
    });
    setError("");
  };
  const saveProfile = async () => {
    const result = await submit({
      kind: "upsert_profile_set",
      profile_set_id: profile.id ?? null,
      expected_version: profile.version ?? null,
      name: profile.name,
      roles: profile.roles,
    });
    if (
      typeof result?.entity_id === "string" &&
      typeof result.version === "number"
    ) {
      const { entity_id: id, version } = result;
      setProfile((draft) => ({ ...draft, id, version }));
    }
  };
  const saveRecipe = async () => {
    const result = await submit({
      kind: "upsert_task_recipe",
      recipe_id: recipe.id ?? null,
      expected_version: recipe.version ?? null,
      name: recipe.name,
      title: recipe.title,
      description: recipe.description,
      acceptance_criteria: recipe.criteria.split("\n").map((text) =>
        text.trim()
      ).filter(Boolean),
      priority: recipe.priority,
      profile_revision_id: recipe.profileRevisionId,
      required_check_ids: recipe.checkIds,
    });
    if (
      typeof result?.entity_id === "string" &&
      typeof result.version === "number"
    ) {
      const { entity_id: id, version } = result;
      setRecipe((draft) => ({ ...draft, id, version }));
    }
  };
  const saveSchedule = async () => {
    const result = await submit({
      kind: "upsert_recipe_schedule",
      schedule_id: schedule.id ?? null,
      expected_version: schedule.version ?? null,
      name: schedule.name,
      recipe_revision_id: schedule.recipeRevisionId,
      cadence: schedule.cadence,
      anchor_utc: schedule.anchorUtc,
    });
    if (
      typeof result?.entity_id === "string" &&
      typeof result.version === "number"
    ) {
      const { entity_id: id, version } = result;
      setSchedule((draft) => ({ ...draft, id, version }));
    }
  };
  if (!project) {
    return (
      <section className="recipes-page">
        <header className="page-heading">
          <h1>Recipes</h1>
        </header>
        <p>
          Select a project in the sidebar to edit its saved configurations,
          recipes, and schedules.
        </p>
      </section>
    );
  }
  return (
    <section className="recipes-page">
      <header className="page-heading">
        <div>
          <span className="eyebrow">{project.display_name}</span>
          <h1>Recipes</h1>
          <p>
            Saved configurations copy into ordinary editable drafts. A draft
            is queued only when you choose Make Ready.
          </p>
        </div>
      </header>
      {project.queue_paused && (
        <p className="warning">
          Project pickup is paused. Drafts can still be created; they will not
          enter the queue.
        </p>
      )}
      {!ready && (
        <p className="warning">
          Activate this project’s TRIP configuration before saving or creating
          recipes.
        </p>
      )}
      {error && <ErrorNotice error={error} />}
      {notice && <p role="status">{notice}</p>}
      {recovery.current!.fault && <ErrorNotice error={recovery.current!.fault} />}
      {recovery.current!.entries.filter((entry) => entry.body.project_id === projectId).map((entry) => (
        <div className="warning" key={entry.resource}>
          {entry.body.kind} has an unknown result for {entry.resource}. The current state can be inspected,
          but its snapshot does not prove failure. Your edited form stays separate.
          <button disabled={pending || !!recovery.current!.fault} onClick={() => void retryPending(entry)}>
            Retry exact pending request
          </button>
        </div>
      ))}

      <section className="panel">
        <h2>Profile sets</h2>
        <p className="hint">
          These are saved role configurations. Each task still needs its own
          activation before Make Ready; saving does not qualify a model.
        </p>
        <div className="recipe-list">
          {profiles.map((item) => (
            <article key={item.id}>
              <strong>{item.name}</strong> · revision {item.revision} ·{" "}
              {item.archived ? "Archived" : "Available"}
              <p className="hint">
                Pinned configuration {item.config_revision_id} · hash{" "}
                {item.configuration_hash.slice(0, 12)}
              </p>
              <div className="button-row">
                <button
                  disabled={item.archived || pending}
                  onClick={() => chooseProfile(item)}
                >
                  Edit
                </button>
                <button
                  disabled={item.archived || pending}
                  onClick={() =>
                    void submit({
                      kind: "archive_profile_set",
                      profile_set_id: item.id,
                      expected_version: item.version,
                    })}
                >
                  Archive
                </button>
              </div>
            </article>
          ))}
        </div>
        <h3>{profile.id ? "Edit profile set" : "New profile set"}</h3>
        <label>
          Name{" "}
          <input
            value={profile.name}
            onChange={(event) =>
              setProfile({ ...profile, name: event.target.value })}
          />
        </label>
        <div className="recipe-role-grid">
          {ROLES.map((role) => (
            <div key={role} className="recipe-role-row">
              <strong>{roleLabel(role)}</strong>
              <label>
                Provider{" "}
                <select
                  value={profile.roles[role].provider}
                  onChange={(event) =>
                    setProfile({
                      ...profile,
                      roles: {
                        ...profile.roles,
                        [role]: {
                          ...profile.roles[role],
                          provider: event.target
                            .value as RoleConfig["provider"],
                        },
                      },
                    })}
                >
                  <option value="codex">Codex</option>
                  <option value="claude">Claude</option>
                </select>
              </label>
              <label>
                Model{" "}
                <input
                  value={profile.roles[role].model}
                  onChange={(event) =>
                    setProfile({
                      ...profile,
                      roles: {
                        ...profile.roles,
                        [role]: {
                          ...profile.roles[role],
                          model: event.target.value,
                        },
                      },
                    })}
                />
              </label>
              <label>
                Effort{" "}
                <input
                  value={profile.roles[role].effort}
                  onChange={(event) =>
                    setProfile({
                      ...profile,
                      roles: {
                        ...profile.roles,
                        [role]: {
                          ...profile.roles[role],
                          effort: event.target.value,
                        },
                      },
                    })}
                />
              </label>
            </div>
          ))}
        </div>
        <div className="button-row">
          <button
            disabled={!ready || pending}
            onClick={() => void saveProfile()}
          >
            Save profile set
          </button>
          <button
            disabled={pending}
            onClick={() => setProfile({ name: "", roles: blankRoles() })}
          >
            New
          </button>
        </div>
      </section>

      <section className="panel">
        <h2>Task recipes</h2>
        <div className="recipe-list">
          {recipes.map((item) => (
            <article key={item.id}>
              <strong>{item.name}</strong> · revision {item.revision} ·{" "}
              {item.archived ? "Archived" : "Available"}
              <p className="hint">
                Pinned configuration {item.config_revision_id} · workflow{" "}
                {item.workflow_version} · hash {item.workflow_hash.slice(0, 12)}
              </p>
              {item.config_revision_id !== currentConfig && (
                <p className="warning">
                  Pinned configuration is superseded. Save a current profile set
                  and recipe before creating a draft.
                </p>
              )}
              <div className="button-row">
                <button
                  disabled={item.archived || pending}
                  onClick={() => chooseRecipe(item)}
                >
                  Edit
                </button>
                <button
                  disabled={!ready || item.archived ||
                    item.config_revision_id !== currentConfig || pending}
                  onClick={async () => {
                    const result = await submit({
                      kind: "create_draft_from_recipe",
                      recipe_id: item.id,
                      recipe_revision_id: item.revision_id,
                      expected_recipe_version: item.version,
                    });
                    if (typeof result?.entity_id === "string") {
                      onOpenTask(result.entity_id);
                    }
                  }}
                >
                  Create draft
                </button>
                <button
                  disabled={item.archived || pending}
                  onClick={() =>
                    void submit({
                      kind: "archive_task_recipe",
                      recipe_id: item.id,
                      expected_version: item.version,
                    })}
                >
                  Archive
                </button>
              </div>
            </article>
          ))}
        </div>
        <h3>{recipe.id ? "Edit recipe" : "New recipe"}</h3>
        <label>
          Name{" "}
          <input
            value={recipe.name}
            onChange={(event) =>
              setRecipe({ ...recipe, name: event.target.value })}
          />
        </label>
        <label>
          Task title{" "}
          <input
            value={recipe.title}
            onChange={(event) =>
              setRecipe({ ...recipe, title: event.target.value })}
          />
        </label>
        <label>
          Description{" "}
          <textarea
            value={recipe.description}
            onChange={(event) =>
              setRecipe({ ...recipe, description: event.target.value })}
          />
        </label>
        <label>
          Acceptance criteria, one per line{" "}
          <textarea
            value={recipe.criteria}
            onChange={(event) =>
              setRecipe({ ...recipe, criteria: event.target.value })}
          />
        </label>
        <label>
          Priority{" "}
          <input
            type="number"
            value={recipe.priority}
            onChange={(event) =>
              setRecipe({ ...recipe, priority: Number(event.target.value) })}
          />
        </label>
        <label>
          Profile revision{" "}
          <select
            value={recipe.profileRevisionId}
            onChange={(event) =>
              setRecipe({ ...recipe, profileRevisionId: event.target.value })}
          >
            <option value="">Select a current profile revision</option>
            {profiles.filter((item) =>
              !item.archived && item.config_revision_id === currentConfig
            ).map((item) => (
              <option key={item.revision_id} value={item.revision_id}>
                {item.name} · revision {item.revision}
              </option>
            ))}
          </select>
        </label>
        <fieldset>
          <legend>Required verification checks</legend>
          {enabledChecks.map((item) => (
            <label key={item.id}>
              <input
                type="checkbox"
                checked={recipe.checkIds.includes(item.id)}
                onChange={(event) =>
                  setRecipe({
                    ...recipe,
                    checkIds: event.target.checked
                      ? [...recipe.checkIds, item.id]
                      : recipe.checkIds.filter((id) => id !== item.id),
                  })}
              />
              {item.check_key} · {item.category}
            </label>
          ))}
        </fieldset>
        <p className="hint">
          The active configuration and workflow are pinned on save. Check
          selection here constrains the Manager’s later selection; it creates no
          selected-check rows.
        </p>
        <div className="button-row">
          <button
            disabled={!ready || !recipe.profileRevisionId || pending}
            onClick={() => void saveRecipe()}
          >
            Save recipe
          </button>
          <button disabled={pending} onClick={() => setRecipe(blankRecipe())}>
            New
          </button>
        </div>
      </section>

      <section className="panel">
        <h2>Foreground schedules</h2>
        <p className="hint">
          Daily means every 24 hours; weekly means every 7 days from the UTC
          anchor. Fires only create backlog drafts while this service runs.
          Missed time is summarized without catch-up drafts.
        </p>
        <div className="recipe-list">
          {schedules.map((item) => (
            <article key={item.id}>
              <strong>{item.name}</strong> ·{" "}
              {item.archived ? "Archived" : item.paused ? "Paused" : "Enabled"}
              {" "}
              · {item.cadence}
              <p>
                Pinned recipe: {item.recipe_name}
                {item.recipe_archived ? " (archived; cannot enable)" : ""}
                {item.recipe_config_revision_id !== currentConfig
                  ? " (configuration superseded; save a current recipe and edit this schedule)"
                  : ""}
              </p>
              <p>
                Recipe revision {item.recipe_revision_id} · Anchor{" "}
                {utcLabel(item.anchor_utc)} · Next{" "}
                {utcLabel(item.next_fire_utc)}
              </p>
              {item.last_fire && (
                <p>
                  Last {item.last_fire.outcome.replaceAll("_", " ")} at{" "}
                  {item.last_fire.scheduled_for_utc}
                  {item.last_fire.task_id &&
                    ` · task ${item.last_fire.task_id}`}
                  {item.last_fire.reason && ` · ${item.last_fire.reason}`}
                  {item.last_fire.missed_count > 0 &&
                    ` · ${item.last_fire.missed_count} missed (${item.last_fire.missed_first_utc} to ${item.last_fire.missed_last_utc})`}
                </p>
              )}
              <div className="button-row">
                <button
                  disabled={item.archived || pending}
                  onClick={() => chooseSchedule(item)}
                >
                  Edit
                </button>
                <button
                  disabled={item.archived || pending ||
                    (item.paused &&
                      (!ready || item.recipe_archived ||
                        item.recipe_config_revision_id !== currentConfig))}
                  onClick={() =>
                    void submit({
                      kind: item.paused
                        ? "resume_recipe_schedule"
                        : "pause_recipe_schedule",
                      schedule_id: item.id,
                      expected_version: item.version,
                    })}
                >
                  {item.paused ? item.last_fire ? "Resume" : "Enable" : "Pause"}
                </button>
                <button
                  disabled={item.archived || pending}
                  onClick={() =>
                    void submit({
                      kind: "archive_recipe_schedule",
                      schedule_id: item.id,
                      expected_version: item.version,
                    })}
                >
                  Archive
                </button>
              </div>
            </article>
          ))}
        </div>
        <h3>{schedule.id ? "Edit schedule" : "New schedule"}</h3>
        <label>
          Name{" "}
          <input
            value={schedule.name}
            onChange={(event) =>
              setSchedule({ ...schedule, name: event.target.value })}
          />
        </label>
        <label>
          Recipe revision{" "}
          <select
            value={schedule.recipeRevisionId}
            onChange={(event) =>
              setSchedule({
                ...schedule,
                recipeRevisionId: event.target.value,
              })}
          >
            <option value="">Select a recipe revision</option>
            {recipes.filter((item) =>
              !item.archived && item.config_revision_id === currentConfig
            ).map((item) => (
              <option key={item.revision_id} value={item.revision_id}>
                {item.name} · revision {item.revision}
              </option>
            ))}
          </select>
        </label>
        <label>
          Cadence{" "}
          <select
            value={schedule.cadence}
            onChange={(event) =>
              setSchedule({
                ...schedule,
                cadence: event.target.value as ScheduleDraft["cadence"],
              })}
          >
            <option value="daily">Daily</option>
            <option value="weekly">Weekly</option>
          </select>
        </label>
        <label>
          UTC anchor (YYYY-MM-DDTHH:MM:SSZ){" "}
          <input
            placeholder="2026-10-01T09:00:00Z"
            value={schedule.anchorUtc}
            onChange={(event) =>
              setSchedule({ ...schedule, anchorUtc: event.target.value })}
          />
        </label>
        {schedule.anchorUtc && (
          <p className="hint">
            Browser local: {new Date(schedule.anchorUtc).toLocaleString()}
          </p>
        )}
        <div className="button-row">
          <button
            disabled={!ready || !schedule.recipeRevisionId || pending}
            onClick={() => void saveSchedule()}
          >
            Save schedule
          </button>
          <button
            disabled={pending}
            onClick={() => setSchedule(blankSchedule())}
          >
            New
          </button>
        </div>
        <p className="hint">
          New schedules start paused. Enable or Resume anchors the first fire
          strictly after that command.
        </p>
      </section>
    </section>
  );
}
