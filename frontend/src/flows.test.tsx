/// <reference lib="deno.ns" />
import { Window } from "happy-dom";
import type { ReactNode } from "react";
import type { Root } from "react-dom/client";
import { ROLES } from "./types";
import type {
  AppState,
  AttentionItem,
  AttentionTarget,
  CmuxSessionSurface,
  CmuxViewOutcome,
  DecisionExplanation,
  PermissionRequest,
  Project,
  Session,
  StateCursor,
  StateWaitResult,
  Task,
  TripSetupState,
  TripTaskVerification,
} from "./types";
import type { LiveEnvironment, LiveStatus } from "./liveState";

const window = new Window({ url: "http://127.0.0.1/" });
for (
  const [name, value] of Object.entries({
    window,
    document: window.document,
    localStorage: window.localStorage,
    sessionStorage: window.sessionStorage,
    Event: window.Event,
    Node: window.Node,
    HTMLElement: window.HTMLElement,
    KeyboardEvent: window.KeyboardEvent,
    PointerEvent: window.PointerEvent,
    IS_REACT_ACT_ENVIRONMENT: true,
  })
) {
  Object.defineProperty(globalThis, name, {
    configurable: true,
    writable: true,
    value,
  });
}

const { act, useState } = await import("react");
const { createRoot } = await import("react-dom/client");
const { AttentionInbox } = await import("./components/AttentionInbox");
const { ApprovalInbox } = await import("./components/ApprovalInbox");
const { ReviewPanel } = await import("./components/ReviewPanel");
const { ResourceStatus } = await import("./components/ResourceStatus");
const { RoleSettings } = await import("./components/RoleSettings");
const { DiagnosticsPanel } = await import("./components/DiagnosticsPanel");
const { RecoveryPanel } = await import("./components/RecoveryPanel");
const { ProjectSetup } = await import("./components/ProjectSetup");
const { ProjectPicker } = await import("./components/ProjectPicker");
const { TaskForm } = await import("./components/TaskForm");
const { TaskDetail } = await import("./components/TaskDetail");
const { TaskBoard } = await import("./components/TaskBoard");
const { History } = await import("./components/History");
const { Recipes } = await import("./components/Recipes");
const { WorkflowControls } = await import("./components/WorkflowControls");
const { ModelSelector } = await import("./components/ModelSelector");
const { Workspace } = await import("./components/Workspace");
const { ApiError, ProtocolError, command, getState, transportTimeouts } = await import("./api");
const { liveEnvironment, liveTiming, startLiveState } = await import(
  "./liveState"
);
const { attentionTargetProblem } = await import("./components/AttentionInbox");
const { App } = await import("./App");
const nativeFetch = globalThis.fetch;
const fixtureSource = Symbol("protocol fixture source");
type FixtureFetch = typeof fetch & { [fixtureSource]?: typeof fetch };
const protocolFixture = () => new Response(JSON.stringify({
  generation: 1,
  server_version: "fixture",
  instance_id: "test-instance",
  supported_features: ["http_operational_v1"],
}), { headers: { "content-type": "application/json" } });
let protocolFixtureResponder = () => Promise.resolve(protocolFixture());
const wrapFetch = (source: typeof fetch): FixtureFetch => {
  const wrapped = ((input: string | URL | Request, init?: RequestInit) =>
    String(input) === "/api/protocol"
      ? protocolFixtureResponder()
      : source(input, init)) as FixtureFetch;
  wrapped[fixtureSource] = source;
  return wrapped;
};
let fixtureFetch = wrapFetch(nativeFetch);
Object.defineProperty(globalThis, "fetch", {
  configurable: true,
  get: () => fixtureFetch,
  set: (value: FixtureFetch) => {
    fixtureFetch = wrapFetch(value[fixtureSource] ?? value);
  },
});

Deno.test("M9 Recipes creates an exact draft and explicitly arms a paused schedule", async () => {
  sessionStorage.clear();
  const requests: Record<string, unknown>[] = [];
  const opened: string[] = [];
  let refreshes = 0;
  const refreshed = () => { refreshes += 1; };
  let failNextSave = false;
  let failNextConflict = false;
  const currentProject: Project = {
    ...initialized,
    trip: { ...initialized.trip!, active_config_revision_id: "config-1" },
  };
  const state: AppState = {
    ...liveBase,
    projects: [currentProject],
    profile_sets: [{ id: "profile-1", project_id: "p1", name: "Saved roles", version: 1,
      archived: false, revision: 1, revision_id: "profile-rev-1",
      roles: inheritedRoles as Record<(typeof ROLES)[number], { provider: "codex"; model: string; effort: string }>,
      config_revision_id: "config-1", configuration_hash: "hash-1" }],
    task_recipes: [{ id: "recipe-1", project_id: "p1", name: "Review", version: 2,
      archived: false, revision: 2, revision_id: "recipe-rev-2", title: "Review change",
      description: "", acceptance_criteria: ["Evidence"], priority: 0,
      profile_revision_id: "profile-rev-1", required_check_ids: ["old-check"],
      config_revision_id: "config-1", configuration_hash: "hash-1",
      workflow_version: "workflow-1", workflow_hash: "workflow-hash" }],
    recipe_schedules: [{ id: "schedule-1", project_id: "p1", name: "Morning", version: 1,
      archived: false, paused: true, recipe_revision_id: "recipe-rev-2", recipe_name: "Review",
      recipe_config_revision_id: "config-1",
      recipe_archived: false, cadence: "daily", anchor_utc: "2026-10-01T09:00:00Z",
      next_fire_utc: null, last_fire: null }],
  };
  globalThis.fetch = (async (_input: string | URL | Request, init?: RequestInit) => {
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    requests.push(body);
    if (failNextSave) {
      failNextSave = false;
      throw new ApiError("unknown save outcome", 0, String(body.operation_id), true);
    }
    if (failNextConflict) {
      failNextConflict = false;
      throw new ApiError("version conflict", 409);
    }
    return new Response(JSON.stringify({ result: { entity_id: "AJ-created", version: 1 } }));
  }) as typeof fetch;
  try {
    mount(<Recipes state={state} project={currentProject} onChanged={refreshed} onOpenTask={(id) => opened.push(id)} />);
    field("Name", "Unsaved profile edits");
    rerender(<Recipes state={{ ...state, revision: "2" }} project={currentProject} onChanged={noop} onOpenTask={(id) => opened.push(id)} />);
    check(findField("Name").value === "Unsaved profile edits", "polling reset the dirty profile form");
    click("Save profile set");
    await settle();
    const profileSave = requests.find((body) => body.kind === "upsert_profile_set");
    check(profileSave?.name === "Unsaved profile edits" && profileSave.profile_set_id === null &&
      profileSave.expected_version === null && Object.keys(profileSave.roles as object).length === 6,
      "profile save did not submit the six-role configuration");
    const sections = [...document.querySelectorAll("section.panel")];
    const profilePanel = sections.find((element) => element.querySelector("h2")?.textContent === "Profile sets")!;
    act(() => profilePanel.querySelector<HTMLButtonElement>("article button")!.click());
    field("Name", "Edited saved roles");
    click("Save profile set");
    await settle();
    check(requests.some((body) => body.kind === "upsert_profile_set" && body.profile_set_id === "profile-1" &&
      body.expected_version === 1 && body.name === "Edited saved roles"),
      "profile edit lost its identity or version");
    act(() => profilePanel.querySelectorAll<HTMLButtonElement>("article button")[1].click());
    await settle();
    check(requests.some((body) => body.kind === "archive_profile_set" &&
      body.profile_set_id === "profile-1" && body.expected_version === 1),
      "profile Archive lost its identity or version");
    const recipePanel = sections.find((element) => element.querySelector("h2")?.textContent === "Task recipes")!;
    const recipeFields = recipePanel.querySelectorAll<HTMLInputElement>("input");
    change(recipeFields[0], "New review");
    change(recipeFields[1], "Review exact revision");
    change(recipePanel.querySelector<HTMLSelectElement>("select")!, "profile-rev-1");
    click("Save recipe");
    await settle();
    const recipeSave = requests.find((body) => body.kind === "upsert_task_recipe");
    check(recipeSave?.title === "Review exact revision" && recipeSave.profile_revision_id === "profile-rev-1" &&
      recipeSave.recipe_id === null && recipeSave.expected_version === null,
      "recipe save did not submit the selected profile revision");
    act(() => recipePanel.querySelector<HTMLButtonElement>("article button")!.click());
    click("Save recipe");
    await settle();
    check(requests.some((body) => body.kind === "upsert_task_recipe" &&
      body.recipe_id === "recipe-1" && body.expected_version === 2 &&
      Array.isArray(body.required_check_ids) && body.required_check_ids.length === 0),
      "recipe edit reused a historical check instead of requiring reselection");
    const schedulePanel = sections.find((element) => element.querySelector("h2")?.textContent === "Foreground schedules")!;
    const scheduleInputs = schedulePanel.querySelectorAll<HTMLInputElement>("input");
    change(scheduleInputs[0], "Weekly review");
    change(schedulePanel.querySelector<HTMLSelectElement>("select")!, "recipe-rev-2");
    change(schedulePanel.querySelectorAll<HTMLSelectElement>("select")[1], "weekly");
    change(scheduleInputs[1], "2026-10-01T09:00:00Z");
    click("Save schedule");
    await settle();
    const scheduleSave = requests.find((body) => body.kind === "upsert_recipe_schedule");
    check(scheduleSave?.cadence === "weekly" && scheduleSave.recipe_revision_id === "recipe-rev-2" &&
      scheduleSave.schedule_id === null && scheduleSave.expected_version === null,
      "schedule create did not submit the selected revision and cadence");
    click("Create draft");
    await settle();
    const draft = requests.find((body) => body.kind === "create_draft_from_recipe");
    check(draft?.recipe_id === "recipe-1" && draft.recipe_revision_id === "recipe-rev-2" &&
      draft.expected_recipe_version === 2 && opened[0] === "AJ-created",
      "manual draft did not use exact recipe identity or navigate to the task");
    act(() => recipePanel.querySelectorAll<HTMLButtonElement>("article button")[2].click());
    await settle();
    check(requests.some((body) => body.kind === "archive_task_recipe" &&
      body.recipe_id === "recipe-1" && body.expected_version === 2),
      "recipe Archive lost its identity or version");
    click("Enable");
    await settle();
    const enable = requests.find((body) => body.kind === "resume_recipe_schedule");
    check(enable?.schedule_id === "schedule-1" && enable.expected_version === 1,
      "Enable did not submit the sole arming command");
    act(() => schedulePanel.querySelector<HTMLButtonElement>("article button")!.click());
    change(schedulePanel.querySelectorAll<HTMLSelectElement>("select")[1], "weekly");
    click("Save schedule");
    await settle();
    check(requests.some((body) => body.kind === "upsert_recipe_schedule" &&
      body.schedule_id === "schedule-1" && body.expected_version === 1 && body.cadence === "weekly"),
      "schedule edit lost its identity or expected version");
    rerender(<Recipes state={{ ...state, recipe_schedules: [{ ...state.recipe_schedules[0], paused: false, version: 2 }] }}
      project={currentProject} onChanged={noop} onOpenTask={noop} />);
    click("Pause");
    await settle();
    check(requests.some((body) => body.kind === "pause_recipe_schedule" &&
      body.schedule_id === "schedule-1" && body.expected_version === 2),
      "Pause did not submit the schedule version");
    rerender(<Recipes state={{ ...state, recipe_schedules: [{ ...state.recipe_schedules[0], paused: true,
      version: 3, last_fire: { outcome: "missed", scheduled_for_utc: "2026-10-01T09:00:00Z",
        task_id: null, reason: null, missed_count: 1, missed_first_utc: "2026-10-01T09:00:00Z",
        missed_last_utc: "2026-10-01T09:00:00Z" } }] }}
      project={currentProject} onChanged={refreshed} onOpenTask={noop} />);
    click("Resume");
    await settle();
    check(requests.some((body) => body.kind === "resume_recipe_schedule" &&
      body.schedule_id === "schedule-1" && body.expected_version === 3),
      "Resume did not submit the paused schedule version");
    const activeSchedule = [...document.querySelectorAll("section.panel")].find((element) =>
      element.querySelector("h2")?.textContent === "Foreground schedules")!;
    act(() => activeSchedule.querySelectorAll<HTMLButtonElement>("article button")[2].click());
    await settle();
    check(requests.some((body) => body.kind === "archive_recipe_schedule" &&
      body.schedule_id === "schedule-1" && body.expected_version === 3),
      "schedule Archive did not submit the exact version");
    act(() => profilePanel.querySelector<HTMLButtonElement>("article button")!.click());
    field("Name", "Unknown profile result");
    failNextSave = true;
    const refreshBefore = refreshes;
    click("Save profile set");
    await settle();
    const ambiguousSave = requests.at(-1)!;
    check(refreshes > refreshBefore, "unknown result did not refresh current state");
    field("Name", "Changed after unknown result");
    const beforeBlocked = requests.length;
    click("Save profile set");
    await settle();
    check(requests.length === beforeBlocked && document.body.textContent?.includes("unknown result"),
      "editing an ambiguous save dispatched a second operation");
    act(() => profilePanel.querySelectorAll<HTMLButtonElement>("article button")[1].click());
    await settle();
    check(requests.length === beforeBlocked, "cross-action Archive bypassed the pending profile edit");
    const refreshedState = { ...state, profile_sets: [{ ...state.profile_sets[0], version: 2,
      name: "Unknown profile result" }] };
    rerender(<Recipes state={refreshedState} project={currentProject} onChanged={refreshed} onOpenTask={noop} />);
    act(() => profilePanel.querySelector<HTMLButtonElement>("article button")!.click());
    check(findField("Name").value === "Unknown profile result", "refreshed profile was not selectable");
    unmount();
    mount(<Recipes state={refreshedState} project={currentProject} onChanged={refreshed} onOpenTask={noop} />);
    const remountedProfile = [...document.querySelectorAll("section.panel")].find((element) =>
      element.querySelector("h2")?.textContent === "Profile sets")!;
    act(() => remountedProfile.querySelector<HTMLButtonElement>("article button")!.click());
    field("Name", "Modified after remount");
    click("Save profile set");
    await settle();
    check(requests.length === beforeBlocked && document.body.textContent?.includes("Retry exact pending request"),
      "remount lost the pending guard");
    click("Retry exact pending request");
    await settle();
    check(requests.at(-1)?.operation_id === ambiguousSave.operation_id &&
      JSON.stringify(requests.at(-1)) === JSON.stringify(ambiguousSave),
      "explicit retry changed the pending body or operation identity");
    click("Save profile set");
    await settle();
    check(requests.at(-1)?.name === "Modified after remount" &&
      requests.at(-1)?.profile_set_id === "profile-1" &&
      requests.at(-1)?.operation_id !== ambiguousSave.operation_id,
      "existing-ID edit did not remain bound after exact retry receipt");
    field("Name", "Conflict profile result");
    failNextConflict = true;
    click("Save profile set");
    await settle();
    const conflictId = requests.at(-1)?.operation_id;
    field("Name", "Edited after conflict");
    click("Save profile set");
    await settle();
    check(requests.at(-1)?.operation_id !== conflictId &&
      requests.at(-1)?.name === "Edited after conflict",
      "conflict retry failed to submit the edited intent with a fresh identity");
    click("New");
    field("Name", "Creation with lost response");
    failNextSave = true;
    click("Save profile set");
    await settle();
    const unknownCreate = requests.at(-1)!;
    const beforeCreateRetry = requests.length;
    check(unknownCreate.profile_set_id === null &&
      sessionStorage.getItem("llmrelay.m9.pending-commands.v1")?.includes(String(unknownCreate.operation_id)),
      "unknown create was not retained before navigation");
    const otherProject = { ...currentProject, id: "p2", display_name: "Other project" };
    rerender(<Recipes state={{ ...state, projects: [currentProject, otherProject] }}
      project={otherProject} onChanged={refreshed} onOpenTask={noop} />);
    rerender(<Recipes state={state} project={currentProject} onChanged={refreshed} onOpenTask={noop} />);
    unmount();
    mount(<Recipes state={state} project={currentProject} onChanged={refreshed} onOpenTask={noop} />);
    field("Name", "Another creation after reload");
    click("Save profile set");
    await settle();
    check(requests.length === beforeCreateRetry, "unknown create became a fresh create after remount");
    click("Retry exact pending request");
    await settle();
    check(requests.length === beforeCreateRetry + 1 &&
      JSON.stringify(requests.at(-1)) === JSON.stringify(unknownCreate) &&
      document.body.textContent?.includes("New profile set") &&
      findField("Name").value === "Another creation after reload",
      "unknown create retry changed the request or rebound an unrelated New profile");
    field("Name", "Project one draft");
    rerender(<Recipes state={{ ...state, projects: [currentProject, otherProject] }}
      project={otherProject} onChanged={noop} onOpenTask={noop} />);
    check(findField("Name").value === "", "project change retained another project's dirty form");
    check(requests.every((body) => !["make_ready", "scheduler_run_once", "approve_plan"].includes(String(body.kind))),
      "recipe controls submitted execution or approval authority");
  } finally {
    unmount();
    sessionStorage.clear();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("A05 recipe New controls preserve each form identity during deferred saves", async () => {
  sessionStorage.clear();
  const currentProject: Project = {
    ...initialized,
    trip: { ...initialized.trip!, active_config_revision_id: "config-1" },
  };
  const state: AppState = {
    ...liveBase,
    projects: [currentProject],
    profile_sets: [{
      id: "profile-1",
      project_id: "p1",
      name: "Saved profile",
      version: 1,
      archived: false,
      revision: 1,
      revision_id: "profile-rev-1",
      roles: inheritedRoles as Record<
        (typeof ROLES)[number],
        { provider: "codex"; model: string; effort: string }
      >,
      config_revision_id: "config-1",
      configuration_hash: "profile-hash",
    }],
    task_recipes: [{
      id: "recipe-1",
      project_id: "p1",
      name: "Saved recipe",
      version: 1,
      archived: false,
      revision: 1,
      revision_id: "recipe-rev-1",
      title: "Saved task",
      description: "",
      acceptance_criteria: [],
      priority: 0,
      profile_revision_id: "profile-rev-1",
      required_check_ids: [],
      config_revision_id: "config-1",
      configuration_hash: "recipe-hash",
      workflow_version: "workflow-1",
      workflow_hash: "workflow-hash",
    }],
    recipe_schedules: [],
  };
  const deferred: Array<{
    body: Record<string, unknown>;
    resolve: (response: Response) => void;
  }> = [];
  globalThis.fetch = (async (
    _input: string | URL | Request,
    init?: RequestInit,
  ) => {
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    return await new Promise<Response>((resolve) => {
      deferred.push({ body, resolve });
    });
  }) as typeof fetch;
  const panel = (heading: string) =>
    [...document.querySelectorAll<HTMLElement>("section.panel")].find(
      (element) => element.querySelector("h2")?.textContent === heading,
    )!;
  const newButton = (owner: HTMLElement) =>
    [...owner.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "New",
    )!;
  try {
    mount(
      <Recipes
        state={state}
        project={currentProject}
        onChanged={noop}
        onOpenTask={noop}
      />,
    );

    const profilePanel = panel("Profile sets");
    field("Name", "Profile being saved");
    click("Save profile set");
    await settle();
    const profileNew = newButton(profilePanel);
    check(profileNew.disabled, "profile New remained enabled during its save");
    act(() => profileNew.click());
    check(
      findField("Name").value === "Profile being saved",
      "profile New changed identity while its save was pending",
    );
    field("Name", "Profile edit made while saving");
    deferred[0].resolve(new Response(JSON.stringify({
      result: { entity_id: "profile-created", version: 1 },
    })));
    await settle();
    check(
      profilePanel.querySelector("h3")?.textContent === "Edit profile set" &&
        findField("Name").value === "Profile edit made while saving",
      "profile response lost a same-form edit or attached to another draft",
    );

    const recipePanel = panel("Task recipes");
    const recipeInputs = recipePanel.querySelectorAll<HTMLInputElement>("input");
    change(recipeInputs[0], "Recipe being saved");
    change(recipeInputs[1], "Original recipe task");
    change(
      recipePanel.querySelector<HTMLSelectElement>("select")!,
      "profile-rev-1",
    );
    click("Save recipe");
    await settle();
    const recipeNew = newButton(recipePanel);
    check(recipeNew.disabled, "recipe New remained enabled during its save");
    act(() => recipeNew.click());
    check(
      recipeInputs[0].value === "Recipe being saved",
      "recipe New changed identity while its save was pending",
    );
    change(recipeInputs[0], "Recipe edit made while saving");
    deferred[1].resolve(new Response(JSON.stringify({
      result: { entity_id: "recipe-created", version: 1 },
    })));
    await settle();
    check(
      recipePanel.querySelector("h3")?.textContent === "Edit recipe" &&
        recipeInputs[0].value === "Recipe edit made while saving",
      "recipe response lost a same-form edit or attached to another draft",
    );

    const schedulePanel = panel("Foreground schedules");
    const scheduleInputs = schedulePanel.querySelectorAll<HTMLInputElement>(
      "input",
    );
    change(scheduleInputs[0], "Schedule being saved");
    change(
      schedulePanel.querySelector<HTMLSelectElement>("select")!,
      "recipe-rev-1",
    );
    click("Save schedule");
    await settle();
    const scheduleNew = newButton(schedulePanel);
    check(scheduleNew.disabled, "schedule New remained enabled during its save");
    act(() => scheduleNew.click());
    check(
      scheduleInputs[0].value === "Schedule being saved",
      "schedule New changed identity while its save was pending",
    );
    change(scheduleInputs[0], "Schedule edit made while saving");
    deferred[2].resolve(new Response(JSON.stringify({
      result: { entity_id: "schedule-created", version: 1 },
    })));
    await settle();
    check(
      schedulePanel.querySelector("h3")?.textContent === "Edit schedule" &&
        scheduleInputs[0].value === "Schedule edit made while saving",
      "schedule response lost a same-form edit or attached to another draft",
    );
    check(
      deferred.map((entry) => entry.body.kind).join(",") ===
        "upsert_profile_set,upsert_task_recipe,upsert_recipe_schedule",
      "deferred saves did not exercise all three ordinary form paths",
    );
  } finally {
    unmount();
    sessionStorage.clear();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("M9 App opens created and recovered recipe drafts after authoritative refresh", async () => {
  const projectWithRecipe = {
    ...initialized,
    trip: { ...initialized.trip!, active_config_revision_id: "config-1" },
  };
  const initial: AppState = {
    ...liveBase,
    projects: [projectWithRecipe],
    tasks: [],
    task_recipes: [{ id: "recipe-1", project_id: "p1", name: "Review", version: 2,
      archived: false, revision: 2, revision_id: "recipe-rev-2", title: "Review change",
      description: "", acceptance_criteria: [], priority: 0,
      profile_revision_id: "profile-rev-1", required_check_ids: [],
      config_revision_id: "config-1", configuration_hash: "hash-1",
      workflow_version: "workflow-1", workflow_hash: "workflow-hash" }],
  };
  const created: AppState = { ...initial, revision: "2", tasks: [{
    ...task, id: "AJ-created", title: "Review change", lifecycle: "backlog",
    active_attempt: undefined,
  }] };
  const priorEnvironment = { ...liveEnvironment };
  const requests: Record<string, unknown>[] = [];
  globalThis.fetch = (async (_input: string | URL | Request, init?: RequestInit) => {
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    requests.push(body);
    return new Response(JSON.stringify({ result: { entity_id: "AJ-created", version: 1 } }));
  }) as typeof fetch;
  try {
    for (const outcome of ["created", "recovered", "missing", "left"] as const) {
      const recovered = outcome === "recovered";
      const live = liveHarness();
      liveEnvironment.transport = live.environment.transport;
      liveEnvironment.scheduler = live.environment.scheduler;
      localStorage.clear();
      localStorage.setItem("agenticjira.page", "recipes");
      localStorage.setItem("agenticjira.project", "p1");
      sessionStorage.clear();
      if (recovered) {
        const body = { kind: "create_draft_from_recipe", project_id: "p1",
          operation_id: "pending-create", recipe_id: "recipe-1",
          recipe_revision_id: "recipe-rev-2", expected_recipe_version: 2 };
        sessionStorage.setItem("llmrelay.m9.pending-commands.v1", JSON.stringify([{
          version: 1, resource: "p1:recipe:recipe-1", body,
        }]));
      }
      mount(<App />);
      await settle();
      live.reads[0].resolve(initial);
      await settle();
      click(recovered ? "Retry exact pending request" : "Create draft");
      await settle();
      check(!document.querySelector("aside.detail") && live.reads.length >= 2,
        "task detail opened before the authoritative read");
      if (outcome === "left") click("Board");
      const next = outcome === "missing" ? initial : created;
      live.reads[1].resolve(next);
      await settle();
      for (let index = 2; index < live.reads.length; index++) {
        live.reads[index].resolve(next);
        await settle();
      }
      check(requests.at(-1)?.kind === "create_draft_from_recipe" &&
        (!recovered || requests.at(-1)?.operation_id === "pending-create"),
        `${outcome} draft did not submit the exact command`);
      if (outcome === "created" || outcome === "recovered") {
        check(document.querySelector("aside.detail header .eyebrow")?.textContent === "AJ-created",
          `${outcome} draft did not open its task detail`);
      } else {
        check(!document.querySelector("aside.detail") &&
          (outcome === "left" || document.body.textContent?.includes("not in the latest state")),
          `${outcome} draft crossed navigation or selected a task missing from the refresh`);
      }
      unmount();
    }
  } finally {
    unmount();
    liveEnvironment.transport = priorEnvironment.transport;
    liveEnvironment.scheduler = priorEnvironment.scheduler;
    sessionStorage.clear();
    localStorage.clear();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("M9 exact create retry leaves unrelated recipe and schedule New forms unbound", async () => {
  const requests: Record<string, unknown>[] = [];
  const currentProject = { ...initialized,
    trip: { ...initialized.trip!, active_config_revision_id: "config-1" } };
  const state: AppState = { ...liveBase, projects: [currentProject], tasks: [] };
  globalThis.fetch = (async (_input: string | URL | Request, init?: RequestInit) => {
    requests.push(JSON.parse(String(init?.body)));
    return new Response(JSON.stringify({ result: { entity_id: "recovered", version: 1 } }));
  }) as typeof fetch;
  try {
    for (const kind of ["upsert_task_recipe", "upsert_recipe_schedule"] as const) {
      sessionStorage.clear();
      const recipe = kind === "upsert_task_recipe";
      const body = recipe
        ? { kind, project_id: "p1", operation_id: "pending-recipe", recipe_id: null,
          expected_version: null, name: "Original", title: "Original task",
          description: "", acceptance_criteria: [], priority: 0,
          profile_revision_id: "profile-rev-1", required_check_ids: [] }
        : { kind, project_id: "p1", operation_id: "pending-schedule", schedule_id: null,
          expected_version: null, name: "Original", recipe_revision_id: "recipe-rev-2",
          cadence: "daily", anchor_utc: "2026-10-01T09:00:00Z" };
      sessionStorage.setItem("llmrelay.m9.pending-commands.v1", JSON.stringify([{
        version: 1, resource: recipe ? "p1:recipe:new" : "p1:schedule:new", body,
      }]));
      mount(<Recipes state={state} project={currentProject} onChanged={noop} onOpenTask={noop} />);
      const heading = recipe ? "Task recipes" : "Foreground schedules";
      const panel = [...document.querySelectorAll("section.panel")].find((element) =>
        element.querySelector("h2")?.textContent === heading)!;
      change(panel.querySelector<HTMLInputElement>("input")!, "Unrelated");
      click("Retry exact pending request");
      await settle();
      check(panel.querySelector("h3")?.textContent === (recipe ? "New recipe" : "New schedule") &&
        panel.querySelector<HTMLInputElement>("input")?.value === "Unrelated" &&
        requests.at(-1)?.operation_id === body.operation_id &&
        document.body.textContent?.includes("Pending request recovered"),
        `${heading} exact retry rebound or erased an unrelated New form`);
      unmount();
    }
  } finally {
    unmount();
    sessionStorage.clear();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("M9 archived backlog drafts stay visible in History for Restore", async () => {
  const requests: Record<string, unknown>[] = [];
  const archivedDraft: Task = { ...task, id: "AJ-archived", lifecycle: "backlog",
    archived: true, active_attempt: undefined, version: 3, can_archive: false };
  globalThis.fetch = (async (_input: string | URL | Request, init?: RequestInit) => {
    if (init?.body) requests.push(JSON.parse(String(init.body)));
    return new Response(JSON.stringify({ result: { entity_id: archivedDraft.id, version: 4 } }));
  }) as typeof fetch;
  try {
    mount(<History tasks={[archivedDraft]} projects={[initialized]} onOpen={noop} onChanged={noop} />);
    check(document.body.textContent?.includes("AJ-archived"), "archived draft is missing from History");
    click("Restore");
    await settle();
    check(requests[0]?.kind === "restore" && requests[0]?.task_id === archivedDraft.id &&
      requests[0]?.expected_version === 3, "History did not submit the ordinary Restore command");
  } finally {
    unmount();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("M9 task detail Archive submits the never-attempted draft version", async () => {
  const requests: Record<string, unknown>[] = [];
  const draft: Task = { ...task, id: "AJ-draft", lifecycle: "backlog", archived: false,
    active_attempt: undefined, version: 4, can_archive: true };
  globalThis.fetch = (async (_input: string | URL | Request, init?: RequestInit) => {
    if (init?.body) requests.push(JSON.parse(String(init.body)));
    return new Response(JSON.stringify({ result: { entity_id: draft.id, version: 5 } }));
  }) as typeof fetch;
  try {
    mount(<TaskDetail task={draft} state={{ ...liveBase, tasks: [draft] }} onClose={noop} onChanged={noop} />);
    click("Archive");
    await settle();
    check(requests.some((body) => body.kind === "archive" && body.task_id === draft.id &&
      body.expected_version === 4), "Task detail did not archive the exact draft version");
  } finally {
    unmount();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("M9 malformed can_archive state is rejected before controls render", async () => {
  globalThis.fetch = (async () => new Response(JSON.stringify({
    ...liveBase,
    tasks: [{ ...task, can_archive: "false" }],
  }))) as typeof fetch;
  try {
    let refused = false;
    try {
      await getState();
    } catch (error) {
      refused = error instanceof Error && error.message.includes("unsupported recipe state");
    }
    check(refused, "non-boolean can_archive crossed the snapshot boundary");
    mount(<TaskDetail task={task} state={liveBase} onClose={noop} onChanged={noop} />);
    const archive = [...document.querySelectorAll<HTMLButtonElement>("button")].find((button) =>
      button.textContent?.includes("Archive"));
    check(archive?.disabled, "last known good state unexpectedly enabled Archive");
  } finally {
    unmount();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("M9 board and detail display exact scheduled draft provenance", () => {
  const scheduled: Task = { ...task, id: "AJ-scheduled", lifecycle: "backlog", can_archive: true,
    recipe_provenance: {
      recipe_id: "recipe-1", recipe_name: "Review", recipe_revision_id: "revision-2",
      recipe_revision: 2, profile_revision_id: "profile-revision-3", required_check_ids: [],
      config_revision_id: "config-1", configuration_hash: "hash-1",
      workflow_version: "workflow-1", workflow_hash: "workflow-hash",
      schedule_id: "schedule-1", scheduled_for_utc: "2026-09-25T09:00:00Z",
    } };
  try {
    mount(<TaskBoard tasks={[scheduled]} projects={[initialized]} onOpen={noop} onEdit={noop} onChanged={noop} />);
    check(document.body.textContent?.includes("Scheduled draft · Review revision 2"),
      "board omitted the scheduled recipe revision");
    rerender(<TaskDetail task={scheduled} state={{ ...liveBase, tasks: [scheduled] }} onClose={noop} onChanged={noop} />);
    check(document.body.textContent?.includes("Exact recipe revision revision-2") &&
      document.body.textContent?.includes("Created by schedule schedule-1 at 2026-09-25T09:00:00Z"),
      "detail omitted exact scheduled provenance");
  } finally {
    unmount();
  }
});

Deno.test("M7 unknown Claude contract explains the release route and blocks setup verification", () => {
  const compatibility = {
    status: "unknown_version" as const,
    observed_version: null,
    pack_id: "claude",
    pack_revision: "1",
    contract_id: null,
    contract_revision: null,
    short_hash: null,
    predicate_id: null,
    missing_evidence: ["reviewed_exact_version_predicate"],
    action: "update_llmrelay_release" as const,
    message: "This provider version has no reviewed contract in this release.",
  };
  try {
    mount(<ProjectSetup
      project={initialized}
      setup={{
        setup_operation_id: "claude-unknown", project_id: initialized.id,
        state: "activated", target_inventory: {}, final_files: [],
        installation_source_binding_complete: true,
        installation_source_binding_reason: "activated",
        selected_profiles: [{
          role: "explorer", selection_state: "selected",
          profile: { provider: "claude", model: "advisory-model", effort: "high" },
          compatibility,
        }],
        probe_receipts: [], sessions: [], runtime_admissions: [],
        agents_file: {}, created_at: "2026-01-01T00:00:00Z",
        updated_at: "2026-01-01T00:00:00Z",
      }}
      onChanged={noop}
      onViewSession={async () => ({ state: "failed", message: "unavailable", retry_available: false })}
    />);
    if (!document.body.textContent?.includes("Update LLMRelay for a reviewed contract") ||
      !document.body.textContent?.includes("reviewed_exact_version_predicate") ||
      ![...document.querySelectorAll("button")].some((button) =>
        button.textContent?.includes("Prepare exact runtime verification") && button.disabled)) {
      throw new Error("unknown Claude contract was presented as qualification eligible");
    }
    unmount();
    mount(<ProjectSetup
      project={initialized}
      setup={{
        setup_operation_id: "claude-discovery", project_id: initialized.id,
        state: "discovery", discovery_attempt_id: "claude-attempt",
        target_inventory: {}, final_files: [],
        installation_source_binding_complete: false,
        installation_source_binding_reason: "discovery",
        selected_profiles: [{
          role: "manager", selection_state: "selected",
          profile: { provider: "claude", model: "advisory-model", effort: "high" },
          compatibility,
        }],
        probe_receipts: [], sessions: [], runtime_admissions: [],
        agents_file: {}, created_at: "2026-01-01T00:00:00Z",
        updated_at: "2026-01-01T00:00:00Z",
      }}
      onChanged={noop}
      onViewSession={async () => ({ state: "failed", message: "unavailable", retry_available: false })}
    />);
    if ([...document.querySelectorAll("button")].some((button) =>
      button.textContent?.includes("Launch manager discovery"))) {
      throw new Error("unknown Claude contract exposed a failing discovery launch");
    }
    unmount();
    mount(<DiagnosticsPanel capabilities={[{
      provider: "claude", role: "explorer", mode: "interactive_pty",
      status: "unverified", gaps: [], compatibility,
    }]} />);
    if (!document.body.textContent?.includes("Compatibility needs attention") ||
      !document.body.textContent?.includes("Update LLMRelay for a reviewed contract")) {
      throw new Error("diagnostics omitted the structured compatibility action");
    }
  } finally {
    unmount();
  }
});
Deno.test("M7 fresh qualification stays reachable while stale exact resume is held", async () => {
  localStorage.clear();
  const priorFetch = globalThis.fetch;
  const requests: Record<string, unknown>[] = [];
  globalThis.fetch = (async (_input: string | URL | Request, init?: RequestInit) => {
    requests.push(JSON.parse(String(init?.body)));
    return new Response(JSON.stringify({ result: { state: "ready" } }));
  }) as typeof fetch;
  const compatibility = (status: "matched" | "evidence_stale" | "contract_changed" | "unknown_version" | "ambiguous_manifest" | "manifest_invalid") => ({
    status,
    observed_version: status === "evidence_stale" ? null : "codex-cli 0.155.1",
    pack_id: status === "evidence_stale" ? null : "codex",
    pack_revision: status === "evidence_stale" ? null : "1",
    contract_id: status === "evidence_stale" ? null : "codex-explorer",
    contract_revision: status === "evidence_stale" ? null : "1",
    short_hash: status === "evidence_stale" ? null : "123456789abc",
    predicate_id: status === "evidence_stale" ? null : "codex-cli-0.155.1",
    missing_evidence: status === "matched" ? [] : ["exact_selected_profile_observation"],
    action: "requalify_exact_profile" as const,
    message: status === "matched" ? "Candidate is reviewed." : "Exact observation is missing.",
  });
  const probe = (role: "explorer" | "plan_reviewer") => ({
    role,
    profile: { provider: "codex" as const, model: "model", effort: "high" },
    profile_hash: `${role}-profile`,
    project_config_revision_id: `${role}-config`,
    project_configuration_hash: `${role}-configuration`,
    adapter: "llmrelay_codex",
    adapter_hash: `${role}-adapter`,
    capability_key: `${role}-capability`,
    nonce: "fixture-only",
    state: "authorized" as const,
    has_native_session: false,
  });
  const setup: TripSetupState = {
    setup_operation_id: "m7-role-scope", project_id: initialized.id,
    state: "activated", target_inventory: {}, final_files: [],
    installation_source_binding_complete: true,
    installation_source_binding_reason: "activated",
    selected_profiles: [
      { role: "explorer", selection_state: "selected", profile: probe("explorer").profile, compatibility: compatibility("evidence_stale") },
      { role: "plan_reviewer", selection_state: "selected", profile: probe("plan_reviewer").profile, compatibility: compatibility("matched") },
    ],
    probe_receipts: [], sessions: [],
    runtime_admissions: [{ id: "m7-admission", scope_hash: "m7-scope", state: "running", fresh_call_count: 2, probes: [probe("explorer"), probe("plan_reviewer")] }],
    agents_file: {}, created_at: "2026-01-01T00:00:00Z", updated_at: "2026-01-01T00:00:00Z",
  };
  try {
    mount(<ProjectSetup project={initialized} setup={setup} onChanged={noop} onViewSession={cmuxFixture} />);
    const probeRows = [...document.querySelectorAll(".profile-list > div")];
    const explorerRow = probeRows.find((row) => row.querySelector("strong")?.textContent === "Explorer");
    const reviewerRow = probeRows.find((row) => row.querySelector("strong")?.textContent === "Plan Reviewer");
    if (!explorerRow?.textContent?.includes("Launch bounded probe") ||
      !reviewerRow?.textContent?.includes("Launch bounded probe")) {
      throw new Error(`fresh stale and unaffected roles did not retain bounded launches: ${document.body.textContent?.slice(-1800)}`);
    }
    const staleLaunch = [...explorerRow.querySelectorAll("button")].find((button) => button.textContent?.includes("Launch bounded probe"))!;
    act(() => staleLaunch.click());
    await settle();
    if (requests.at(-1)?.kind !== "runtime_probe_launch" || requests.at(-1)?.role !== "explorer") {
      throw new Error("fresh stale role launch was not routed to backend admission");
    }
    unmount();

    for (const status of ["unknown_version", "ambiguous_manifest", "manifest_invalid"] as const) {
      mount(<ProjectSetup project={initialized} setup={{ ...setup, runtime_admissions: [], selected_profiles: [
        ...setup.selected_profiles,
        { role: "manager", selection_state: "selected", profile: probe("explorer").profile, compatibility: compatibility(status) },
      ] }} onChanged={noop} onViewSession={cmuxFixture} />);
      const prepare = [...document.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent?.includes("Prepare exact runtime verification"));
      if (!prepare?.disabled) throw new Error(`${status} enabled runtime preparation`);
      unmount();
    }

    mount(<ProjectSetup project={initialized} setup={{ ...setup, runtime_admissions: [] }} onChanged={noop} onViewSession={cmuxFixture} />);
    click("Prepare exact runtime verification");
    await settle();
    if (requests.at(-1)?.action !== "prepare_runtime_admission") {
      throw new Error("stale observation blocked fresh runtime preparation");
    }
    unmount();

    const retainedProbe = {
      ...probe("explorer"), state: "running" as const,
      session_id: "retained-explorer", session_status: "exited",
      has_native_session: true,
    };
    const retainedSetup = {
      ...setup, runtime_admissions: [{
        ...setup.runtime_admissions[0], probes: [retainedProbe, probe("plan_reviewer")],
      }],
    };
    mount(<ProjectSetup project={initialized} setup={retainedSetup} onChanged={noop} onViewSession={cmuxFixture} />);
    const staleRow = [...document.querySelectorAll(".profile-list > div")].find((row) => row.querySelector("strong")?.textContent === "Explorer");
    if (staleRow?.textContent?.includes("Resume same native session")) {
      throw new Error("stale runtime binding exposed exact native resume");
    }
    unmount();

    mount(<ProjectSetup project={initialized} setup={{ ...retainedSetup, selected_profiles: [
      { ...setup.selected_profiles[0], compatibility: compatibility("matched") },
      setup.selected_profiles[1],
    ] }} onChanged={noop} onViewSession={cmuxFixture} />);
    const matchedRow = [...document.querySelectorAll(".profile-list > div")].find((row) => row.querySelector("strong")?.textContent === "Explorer");
    const resume = [...matchedRow?.querySelectorAll("button") || []].find((button) => button.textContent?.includes("Resume same native session"));
    if (!resume) throw new Error("matched role lost exact runtime resume");
    act(() => resume.click());
    await settle();
    if (requests.at(-1)?.kind !== "runtime_probe_resume" || requests.at(-1)?.role !== "explorer") {
      throw new Error("matched exact resume did not retain its role binding");
    }
    unmount();

    mount(<ProjectSetup project={initialized} setup={{ ...retainedSetup, selected_profiles: [
      { ...setup.selected_profiles[0], compatibility: compatibility("contract_changed") },
      setup.selected_profiles[1],
    ] }} onChanged={noop} onViewSession={cmuxFixture} />);
    const changedRow = [...document.querySelectorAll(".profile-list > div")].find((row) => row.querySelector("strong")?.textContent === "Explorer");
    if (changedRow?.textContent?.includes("Resume same native session")) {
      throw new Error("changed contract exposed exact native resume");
    }
    unmount();

    mount(<ProjectSetup project={initialized} setup={{ ...retainedSetup, runtime_admissions: [], selected_profiles: [
      { ...setup.selected_profiles[0], compatibility: compatibility("contract_changed") },
      setup.selected_profiles[1],
    ] }} onChanged={noop} onViewSession={cmuxFixture} />);
    click("Prepare exact runtime verification");
    await settle();
    if (requests.at(-1)?.action !== "prepare_runtime_admission") {
      throw new Error("changed contract could not request fresh backend admission");
    }
  } finally {
    unmount();
    globalThis.fetch = priorFetch;
  }
});
Deno.test("M7 fresh Codex discovery and stale probe retry preserve exact resume hold", async () => {
  localStorage.clear();
  const priorFetch = globalThis.fetch;
  const requests: Record<string, unknown>[] = [];
  globalThis.fetch = (async (_input: string | URL | Request, init?: RequestInit) => {
    requests.push(JSON.parse(String(init?.body)));
    return new Response(JSON.stringify({ result: {} }));
  }) as typeof fetch;
  const profile = { provider: "codex" as const, model: "gpt-6-sol", effort: "medium" };
  const compatibility = {
    status: "evidence_stale" as const, observed_version: null,
    pack_id: null, pack_revision: null, contract_id: null,
    contract_revision: null, short_hash: null, predicate_id: null,
    missing_evidence: ["exact_selected_profile_observation"],
    action: "requalify_exact_profile" as const,
    message: "No exact compatibility observation is recorded for this selected profile.",
  };
  const setup: TripSetupState = {
    setup_operation_id: "fresh-codex", project_id: initialized.id,
    state: "discovery", discovery_attempt_id: "discovery-attempt",
    target_inventory: {}, final_files: [],
    installation_source_binding_complete: false,
    installation_source_binding_reason: "discovery",
    selected_profiles: [{ role: "manager", selection_state: "selected", profile, compatibility }],
    probe_receipts: [], sessions: [], runtime_admissions: [], agents_file: {},
    created_at: "2026-01-01T00:00:00Z", updated_at: "2026-01-01T00:00:00Z",
  };
  const session: TripSetupState["sessions"][number] = {
    id: "stale-probe-session", attempt_id: "probe-attempt", role: "explorer",
    provider: "codex", generation: 1, lane_id: "probe-lane",
    status: "exited", launch_state: "started", readiness: "idle_candidate",
    capture_state: "complete", has_native_session: true, resume_count: 0,
    updated_at: "2026-01-01T00:00:00Z",
  };
  try {
    mount(<ProjectSetup project={initialized} setup={setup} onChanged={noop} onViewSession={cmuxFixture} />);
    click("Launch manager discovery");
    await settle();
    if (requests.at(-1)?.kind !== "trip_setup_dispatch" ||
      requests.at(-1)?.attempt_id !== "discovery-attempt" || requests.at(-1)?.role !== "manager") {
      throw new Error("fresh Codex manager discovery did not reach its exact backend route");
    }
    unmount();

    const probing: TripSetupState = {
      ...setup, state: "probing", probe_attempt_id: "probe-attempt",
      selected_profiles: [{ role: "explorer", selection_state: "selected", profile, compatibility }],
    };
    mount(<ProjectSetup project={initialized} setup={probing} onChanged={noop} onViewSession={cmuxFixture} />);
    click("Agents");
    const explorer = [...document.querySelectorAll(".setup-invocation")].find((row) => row.querySelector("strong")?.textContent === "Explorer");
    const firstProbe = [...explorer?.querySelectorAll("button") || []].find((button) => button.textContent?.includes("Launch bounded probe"));
    if (!firstProbe) throw new Error(`fresh delegated Codex probe was hidden without a receipt: ${document.body.textContent?.slice(-1800)}`);
    act(() => firstProbe.click());
    await settle();
    if (requests.at(-1)?.kind !== "trip_setup_dispatch" ||
      requests.at(-1)?.attempt_id !== "probe-attempt" || requests.at(-1)?.role !== "explorer") {
      throw new Error("first delegated probe did not retain exact role routing");
    }
    unmount();

    mount(<ProjectSetup project={initialized} setup={{ ...probing, sessions: [session] }} onChanged={noop} onViewSession={cmuxFixture} />);
    click("Agents");
    const staleRow = [...document.querySelectorAll(".setup-invocation")].find((row) => row.querySelector("strong")?.textContent === "Explorer");
    if (!staleRow?.textContent?.includes("Launch exact-profile retry") ||
      staleRow.textContent.includes("Resume retained session")) {
      throw new Error("stale probe did not offer fresh retry while holding exact resume");
    }
    const retry = [...staleRow.querySelectorAll("button")].find((button) => button.textContent?.includes("Launch exact-profile retry"))!;
    act(() => retry.click());
    await settle();
    if (requests.at(-1)?.kind !== "trip_setup_dispatch" ||
      requests.at(-1)?.attempt_id !== "probe-attempt" || requests.at(-1)?.role !== "explorer") {
      throw new Error("stale retry did not route the selected profile to backend validation");
    }
  } finally {
    unmount();
    globalThis.fetch = priorFetch;
  }
});
const noop = () => {};
const encodedBytes = (bytes: Uint8Array) => {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
};
const persistentSurface = (
  id = "00000000-0000-0000-0000-000000000041",
  overrides: Partial<CmuxSessionSurface> = {},
): CmuxSessionSurface => ({
  id,
  task_workspace_id: "00000000-0000-0000-0000-000000000001",
  workspace_id: "00000000-0000-0000-0000-000000000002",
  surface_id: "00000000-0000-0000-0000-000000000003",
  binding_revision: 1,
  surface_state: "open",
  attachment_state: "live",
  desired_input_state: "view_only",
  actual_input_state: "view_only",
  control_revision: 0,
  applied_revision: 0,
  updated_at: "now",
  ...overrides,
});
const cmuxFixture = async (_sessionId = ""): Promise<CmuxViewOutcome> => ({
  state: "recorded_output" as const,
  message: "fixture recorded output only",
  retry_available: false,
});

const project: Project = {
  id: "p1",
  display_name: "Fixture",
  repository_path: "/tmp/fixture",
  repository_identity: "/tmp/fixture/.git",
  base_revision: "abc",
  queue_paused: false,
  version: 1,
  settings: {},
};
const inheritedRoles = Object.fromEntries(
  ROLES.map((role) => [
    role,
    { provider: "codex", model: "gpt-5.6-sol", effort: "high" },
  ]),
);
const initialized: Project = {
  ...project,
  settings: { roles: inheritedRoles },
  trip: {
    readiness: "ready",
    reason: "Fixture has an activated TRIP configuration",
    detected_installation: "compatible",
  },
};
const task: Task = {
  id: "AJ-1",
  project_id: "p1",
  title: "Existing",
  description: "",
  acceptance_criteria: ["works"],
  priority: 0,
  manual_order: 0,
  lifecycle: "in_progress",
  attention: "none",
  version: 7,
  archived: false,
  can_archive: false,
  permission_waiting: false,
  role_overrides: {},
  dependencies: [],
  active_attempt: {
    id: "a1",
    phase: "implementation",
    status: "running",
    base_revision: "abc",
  },
  role_settings: [],
  reviews: [],
  snapshots: [],
  review_budgets: [],
  legacy: {},
};
let root: Root | undefined;

function mount(node: ReactNode) {
  unmount();
  document.body.innerHTML = '<div id="root"></div>';
  const mounted = createRoot(document.getElementById("root")!);
  root = mounted;
  act(() => mounted.render(node));
}
function rerender(node: ReactNode) {
  if (!root) throw new Error("no root to rerender");
  act(() => root!.render(node));
}
function unmount() {
  if (!root) return;
  const mounted = root;
  root = undefined;
  act(() => mounted.unmount());
}
function change(
  element: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement,
  value: string,
) {
  const setter = Object.getOwnPropertyDescriptor(
    Object.getPrototypeOf(element),
    "value",
  )?.set;
  act(() => {
    setter?.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
    element.dispatchEvent(new Event("change", { bubbles: true }));
  });
}
function findField(label: string) {
  const owner = [...document.querySelectorAll("label")].find((item) =>
    item.textContent?.startsWith(label)
  );
  const element = owner?.querySelector<HTMLInputElement | HTMLTextAreaElement>(
    "input, textarea",
  ) ?? [...document.querySelectorAll<HTMLInputElement | HTMLTextAreaElement>(
    "input, textarea",
  )].find((item) => item.getAttribute("aria-label") === label);
  if (!element) throw new Error(`field not found: ${label}`);
  return element;
}
function field(label: string, value: string) {
  const element = findField(label);
  change(element, value);
  return element;
}
function findSelect(label: string) {
  const owner = [...document.querySelectorAll("label")].find((item) =>
    item.textContent?.startsWith(label)
  );
  const element = owner?.querySelector<HTMLSelectElement>("select");
  if (!element) throw new Error(`select not found: ${label}`);
  return element;
}
function click(text: string) {
  const button = [...document.querySelectorAll("button")].find((value) =>
    value.textContent?.includes(text)
  ) as HTMLButtonElement;
  if (!button) {
    throw new Error(
      `button not found: ${text}; available: ${
        [...document.querySelectorAll("button")].map((value) =>
          value.textContent
        ).join(" | ")
      }`,
    );
  }
  act(() => button.click());
}
async function settle() {
  await act(async () => await new Promise((resolve) => setTimeout(resolve, 0)));
}
function assertLiveCmuxLossFence() {
  if (
    [...document.querySelectorAll<HTMLButtonElement>("button")].some((button) =>
      /View output|Take keyboard control|Release keyboard control|Discard unknown/
        .test(
          button.textContent || "",
        ) && !button.disabled
    ) ||
    !document.body.textContent?.includes(
      "fixture validated cmux loss observation",
    ) ||
    !document.body.textContent.includes("durable retirement interval")
  ) throw new Error("stale cmux result replaced durable live-loss retirement");
}
async function verifyDeferredCmuxView(
  card: (
    surface: CmuxSessionSurface,
    onView: (sessionId: string) => Promise<CmuxViewOutcome>,
  ) => ReactNode,
  open: CmuxSessionSurface,
  lostLive: CmuxSessionSurface,
  assertLost = () => {},
  localSurface = open,
) {
  let resolveView!: (outcome: CmuxViewOutcome) => void;
  const pendingView = new Promise<CmuxViewOutcome>((resolve) =>
    resolveView = resolve
  );
  const onView = () => pendingView;
  mount(card(open, onView));
  click("View output");
  rerender(card(lostLive, onView));
  await settle();
  resolveView({
    state: "view_only",
    message: "stale open response",
    retry_available: true,
    surface: localSurface,
  });
  await settle();
  assertLost();
  assertLiveCmuxLossFence();
}

Deno.test("T14 New task inherits roles, submits explicit overrides, and retains failed draft", async () => {
  const requests: Record<string, unknown>[] = [];
  const provider = "claude" as const;
  const roleOverride = { provider, model: "fable", effort: "medium" };
  let fail = true;
  localStorage.clear();
  localStorage.setItem(
    "agenticjira.new-task",
    JSON.stringify({
      project_id: "",
      title: "Preserve this draft",
      description: "Keep this text",
      criteria: "first\nsecond",
      roles: {},
    }),
  );
  globalThis.fetch =
    (async (_input: string | URL | Request, init?: RequestInit) => {
      requests.push(JSON.parse(String(init?.body)));
      return fail
        ? new Response(JSON.stringify({ error: "base revision changed" }), {
          status: 409,
        })
        : new Response(JSON.stringify({ result: { state: "ready" } }));
    }) as typeof fetch;
  try {
    mount(
      <TaskForm projects={[initialized]} onSaved={noop} onClose={noop} />,
    );
    if (findSelect("Project").value !== "") {
      throw new Error("restored blank project was visually retargeted");
    }
    click("Save draft");
    await settle();
    if (
      requests.length !== 0 ||
      !document.body.textContent?.includes("Choose a project")
    ) {
      throw new Error("blank project submitted or lacked a visible reason");
    }
    unmount();

    localStorage.setItem(
      "agenticjira.new-task",
      JSON.stringify({
        project_id: "removed-project",
        title: "Preserve this draft",
        description: "Keep this text",
        criteria: "first\nsecond",
        roles: { implementer: roleOverride },
        roleOverrides: { implementer: true },
      }),
    );
    mount(
      <TaskForm projects={[initialized]} onSaved={noop} onClose={noop} />,
    );
    const projectSelect = findSelect("Project");
    if (
      projectSelect.value !== "removed-project" ||
      !projectSelect.selectedOptions[0]?.textContent?.includes(
        "Choose an available project",
      )
    ) throw new Error("stale project selection was hidden or retargeted");
    click("Save draft");
    await settle();
    if (requests.length !== 0) throw new Error("stale project was submitted");

    change(projectSelect, "p1");
    click("Save draft");
    await settle();
    if (
      (document.body.textContent?.match(/Inherited:/g) || []).length !== 5 ||
      !document.body.textContent?.includes("Your draft is still here") ||
      findField("Title").value !== "Preserve this draft"
    ) throw new Error("failed submission lost the draft or inherited roles");
    if (
      requests.at(-1)?.kind !== "create_task" ||
      requests.at(-1)?.ready !== false ||
      requests.at(-1)?.project_id !== "p1" ||
      requests.at(-1)?.title !== "Preserve this draft" ||
      JSON.stringify(requests.at(-1)?.acceptance_criteria) !==
        JSON.stringify(["first", "second"]) ||
      JSON.stringify(requests.at(-1)?.role_overrides) !==
      JSON.stringify({ implementer: roleOverride })
    ) throw new Error("repaired draft command did not preserve exact fields");
    const rejectedCreateOperation = requests.at(-1)?.operation_id;
    fail = false;
    if (
      !document.querySelector<HTMLButtonElement>(".primary")!.disabled ||
      !document.body.textContent?.includes("differs from the project default")
    ) throw new Error("unpreflighted override was presented as Ready");
    click("Save draft");
    await settle();
    if (
      requests.at(-1)?.kind !== "create_task" ||
      requests.at(-1)?.ready !== false ||
      requests.at(-1)?.operation_id === rejectedCreateOperation ||
      JSON.stringify(requests.at(-1)?.role_overrides) !==
        JSON.stringify({ implementer: roleOverride })
    ) {
      throw new Error(
        "definitively rejected create did not retain the draft with a fresh identity",
      );
    }

    unmount();
    localStorage.removeItem("agenticjira.new-task");
    mount(
      <TaskForm projects={[project]} onSaved={noop} onClose={noop} />,
    );
    field("Title", "Uninitialized draft");
    const ready = document.querySelector<HTMLButtonElement>(".primary")!;
    click("Save draft");
    await settle();
    if (
      !ready.disabled || requests.at(-1)?.ready !== false ||
      requests.at(-1)?.title !== "Uninitialized draft"
    ) throw new Error("uninitialized project did not allow only a draft");

    unmount();
    const editing = {
      ...task,
      role_overrides: { implementer: roleOverride },
    };
    mount(
      <TaskForm
        projects={[initialized]}
        editing={editing}
        onSaved={noop}
        onClose={noop}
      />,
    );
    field("Title", "Edited without role mutation");
    click("Save changes");
    await settle();
    if (
      requests.at(-1)?.kind !== "update_task" ||
      requests.at(-1)?.role_overrides !== null
    ) throw new Error("task edit attempted to replace role settings");

    unmount();
    localStorage.removeItem("agenticjira.new-task");
    const otherProject = { ...initialized, id: "p-other" };
    mount(
      <TaskForm
        projects={[otherProject, initialized]}
        selectedProjectId="p1"
        onSaved={noop}
        onClose={noop}
      />,
    );
    if (findSelect("Project").value !== "p1") {
      throw new Error("fresh task ignored the explicitly selected project");
    }
    field("Title", "Fresh selected project task");
    click("Save draft");
    await settle();
    if (
      requests.at(-1)?.project_id !== "p1" ||
      requests.at(-1)?.title !== "Fresh selected project task"
    ) {
      throw new Error(
        "fresh task did not submit the selected project or title",
      );
    }
  } finally {
    unmount();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("A09 unresolved task create blocks edited intent and reloads its exact reconciliation", async () => {
  localStorage.clear();
  const requests: Record<string, unknown>[] = [];
  const committedOperations = new Set<string>();
  let committedCreates = 0;
  let saved = 0;
  globalThis.fetch = (async (
    _input: string | URL | Request,
    init?: RequestInit,
  ) => {
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    requests.push(body);
    const operation = String(body.operation_id);
    if (!committedOperations.has(operation)) {
      committedOperations.add(operation);
      committedCreates++;
    }
    if (requests.length === 1) {
      throw new ApiError(
        "task committed but its response was lost",
        0,
        operation,
        true,
      );
    }
    return new Response(JSON.stringify({
      result: { entity_id: "AJ-ambiguous", version: 1 },
    }));
  }) as typeof fetch;
  const form = () => (
    <TaskForm
      projects={[initialized]}
      onSaved={() => saved++}
      onClose={noop}
    />
  );
  try {
    mount(form());
    field("Title", "Original unresolved task");
    click("Save draft");
    await settle();
    const original = requests[0];
    check(
      original?.kind === "create_task" && original.ready === false &&
        document.body.textContent?.includes("may already be committed") &&
        localStorage.getItem("agenticjira.new-task.operation")?.includes(
          String(original.operation_id),
        ),
      "ambiguous task create did not retain its exact persisted request",
    );

    field("Title", "Edited after response loss");
    unmount();
    mount(form());
    check(
      findField("Title").value === "Edited after response loss" &&
        document.body.textContent?.includes("Retry exact unresolved create"),
      "reload lost the edited draft or unresolved-create recovery",
    );
    click("Save draft");
    await settle();
    check(
      requests.length === 1 && committedCreates === 1 &&
        document.body.textContent?.includes(
          "earlier task create still has an unknown result",
        ),
      "edited task submission bypassed the unresolved-create guard",
    );
    field("Title", "Original unresolved task");
    click("Create Ready task");
    await settle();
    check(
      requests.length === 1 && committedCreates === 1,
      "a different Ready mode bypassed the unresolved-create guard",
    );

    field("Title", "Edited after response loss");
    click("Retry exact unresolved create");
    await settle();
    check(
      requests.length === 2 && committedCreates === 1 && saved === 1 &&
        JSON.stringify(requests[1]) === JSON.stringify(original) &&
        localStorage.getItem("agenticjira.new-task.operation") === null &&
        localStorage.getItem("agenticjira.new-task")?.includes(
          "Edited after response loss",
        ),
      "exact task-create reconciliation changed intent, duplicated the commit, stayed pending, or discarded edited draft fields",
    );
  } finally {
    unmount();
    localStorage.clear();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("A04 setup clears definite stale identity but preserves an ambiguous exact retry", async () => {
  localStorage.clear();
  const definiteRequests: Record<string, unknown>[] = [];
  let definiteRefreshes = 0;
  globalThis.fetch = (async (
    _input: string | URL | Request,
    init?: RequestInit,
  ) => {
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    definiteRequests.push(body);
    return definiteRequests.length === 1
      ? new Response(JSON.stringify({ error: "stale project version" }), {
        status: 409,
      })
      : new Response(JSON.stringify({ result: { state: "prepared" } }));
  }) as typeof fetch;
  try {
    mount(
      <ProjectSetup
        project={{ ...initialized, version: 1 }}
        onChanged={() => {
          definiteRefreshes++;
        }}
        onViewSession={cmuxFixture}
      />,
    );
    click("Prepare exact runtime verification");
    await settle();
    const stale = definiteRequests[0];
    check(
      stale?.expected_version === 1 && definiteRefreshes === 1 &&
        document.body.textContent?.includes("stale project version"),
      "definite setup rejection did not surface and refresh current state",
    );
    rerender(
      <ProjectSetup
        project={{ ...initialized, version: 2 }}
        onChanged={() => {
          definiteRefreshes++;
        }}
        onViewSession={cmuxFixture}
      />,
    );
    await settle();
    click("Prepare exact runtime verification");
    await settle();
    check(
      definiteRequests[1]?.expected_version === 2 &&
        definiteRequests[1]?.operation_id !== stale.operation_id &&
        definiteRefreshes === 2,
      "same setup action replayed a definitively rejected stale version",
    );

    unmount();
    localStorage.clear();
    const ambiguousRequests: Record<string, unknown>[] = [];
    let ambiguousRefreshes = 0;
    globalThis.fetch = (async (
      _input: string | URL | Request,
      init?: RequestInit,
    ) => {
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
      ambiguousRequests.push(body);
      if (ambiguousRequests.length === 1) {
        throw new ApiError(
          "setup response was lost",
          0,
          String(body.operation_id),
          true,
        );
      }
      return new Response(JSON.stringify({ result: { state: "prepared" } }));
    }) as typeof fetch;
    mount(
      <ProjectSetup
        project={{ ...initialized, version: 3 }}
        onChanged={() => {
          ambiguousRefreshes++;
        }}
        onViewSession={cmuxFixture}
      />,
    );
    click("Prepare exact runtime verification");
    await settle();
    unmount();
    mount(
      <ProjectSetup
        project={{ ...initialized, version: 3 }}
        onChanged={() => {
          ambiguousRefreshes++;
        }}
        onViewSession={cmuxFixture}
      />,
    );
    click("Prepare exact runtime verification");
    await settle();
    check(
      ambiguousRequests.length === 2 && ambiguousRefreshes === 2 &&
        JSON.stringify(ambiguousRequests[1]) ===
          JSON.stringify(ambiguousRequests[0]),
      "ambiguous setup retry did not preserve the exact request and identity",
    );
  } finally {
    unmount();
    localStorage.clear();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("A13 setup storage failure stays visible and releases the command guard", async () => {
  localStorage.clear();
  const originalSetItem = localStorage.setItem;
  const operationKey = "llmrelay.trip.operations.p1";
  let operationStorageAttempts = 0;
  let requests = 0;
  let refreshes = 0;
  Object.defineProperty(localStorage, "setItem", {
    configurable: true,
    writable: true,
    value: (key: string, value: string) => {
      if (key === operationKey) {
        operationStorageAttempts++;
        throw new Error("fixture browser storage denied");
      }
      originalSetItem(key, value);
    },
  });
  globalThis.fetch = (async () => {
    requests++;
    return new Response(JSON.stringify({ result: { state: "prepared" } }));
  }) as typeof fetch;
  try {
    mount(
      <ProjectSetup
        project={initialized}
        onChanged={() => {
          refreshes++;
        }}
        onViewSession={cmuxFixture}
      />,
    );
    click("Prepare exact runtime verification");
    await settle();
    check(
      document.body.textContent?.includes("fixture browser storage denied") &&
        requests === 0 && refreshes === 1,
      "setup storage failure was hidden or allowed an unretained request",
    );
    click("Prepare exact runtime verification");
    await settle();
    const retry = [...document.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) =>
        button.textContent?.includes("Prepare exact runtime verification")
      );
    check(
      operationStorageAttempts >= 2 && requests === 0 && refreshes === 2 &&
        retry !== undefined && !retry.disabled,
      "setup storage failure left the real command guard latched",
    );
  } finally {
    unmount();
    Object.defineProperty(localStorage, "setItem", {
      configurable: true,
      writable: true,
      value: originalSetItem,
    });
    localStorage.clear();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("Project add validates, guards repeats, and selects the created ID", async () => {
  const requests: Record<string, unknown>[] = [];
  const requestCount = () => requests.length;
  const events: string[] = [];
  let respond: (() => void) | undefined;
  const created = { ...project, id: "p-created", display_name: "JellyScope" };
  globalThis.fetch =
    (async (_input: string | URL | Request, init?: RequestInit) => {
      requests.push(JSON.parse(String(init?.body)));
      return await new Promise<Response>((resolve) => {
        respond = () =>
          resolve(
            new Response(JSON.stringify({
              result: { entity_id: "p-created", state: "created" },
            })),
          );
      });
    }) as typeof fetch;
  function Harness() {
    const [projects, setProjects] = useState<Project[]>([]);
    return (
      <ProjectPicker
        projects={projects}
        onSelect={(id) => events.push(`select:${id}`)}
        onChanged={() => {
          events.push("refresh");
          setProjects([created]);
        }}
      />
    );
  }
  try {
    mount(<Harness />);
    click("Add project");
    if (
      !document.body.textContent?.includes("Project name") ||
      !document.body.textContent?.includes("Repository folder") ||
      !document.body.textContent?.includes("existing local Git folder")
    ) throw new Error("project form labels or path guidance are missing");

    field("Project name", "   ");
    field("Repository folder", "/tmp/JellyScope");
    click("Add project");
    await settle();
    if (requestCount() !== 0) throw new Error("blank project name submitted");

    field("Project name", " JellyScope ");
    field("Repository folder", "relative/JellyScope");
    click("Add project");
    await settle();
    if (requestCount() !== 0) {
      throw new Error("relative project path submitted");
    }

    const fullPath = "/Users/example/Projects/JellyScope/with/a/long/path";
    field("Repository folder", ` ${fullPath} `);
    if (
      !document.querySelector(".path-preview")?.textContent?.includes(fullPath)
    ) {
      throw new Error("full repository path preview is missing");
    }
    const submit = [...document.querySelectorAll("button")].find((button) =>
      button.textContent === "Add project"
    )!;
    act(() => {
      submit.click();
      submit.click();
    });
    await settle();
    if (
      requestCount() !== 1 ||
      requests[0].kind !== "add_project" ||
      requests[0].display_name !== "JellyScope" ||
      requests[0].path !== fullPath ||
      !document.body.textContent?.includes("Checking repository…")
    ) {
      throw new Error(
        "project add body, busy state, or duplicate guard drifted",
      );
    }
    respond?.();
    await settle();
    if (events.join(",") !== "refresh,select:p-created") {
      throw new Error(`created project selection order drifted: ${events}`);
    }
  } finally {
    unmount();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("T15 role control and human review dispatch exact authority without fabricating state", async () => {
  const requests: Record<string, unknown>[] = [];
  const currentPreparationKey = "current-complete-preparation-key";
  let failedRuntimeView = false;
  const codexCatalogRequests: Array<{
    resolve: (slug: string) => void;
    reject: (error: Error) => void;
  }> = [];
  let codexCatalogResolved = false;
  const cmuxDiscardResponses: Record<string, number> = {};
  let preparationSource: "project_default" | "task_override" =
    "project_default";
  globalThis.fetch =
    (async (input: string | URL | Request, init?: RequestInit) => {
      if (String(input).includes("/model-catalog?provider=codex")) {
        return await new Promise<Response>((resolve, reject) => {
          codexCatalogRequests.push({
            resolve: (slug) => {
              if (slug === "old-codex-suggestion") {
                codexCatalogResolved = true;
              }
              resolve(
                new Response(JSON.stringify({
                  provider: "codex",
                  state: "available",
                  advisory_only: true,
                  models: [{ slug, efforts: [] }],
                })),
              );
            },
            reject,
          });
        });
      }
      if (String(input).includes("/model-catalog?provider=claude")) {
        return new Response(
          JSON.stringify({
            provider: "claude",
            state: "unavailable",
            advisory_only: true,
            models: [],
            reason: "No Claude catalog",
          }),
        );
      }
      if (String(input).includes("/role-preparations")) {
        if (failedRuntimeView) {
          return new Response(JSON.stringify([{
            task_id: "AJ-1",
            role: "implementer",
            requested_revision: 1,
            config: {
              provider: "codex",
              model: "gpt-5.6-sol",
              effort: "high",
            },
            capability_key: currentPreparationKey,
            generic_capability_supported: true,
            exact_runtime_authority: false,
            task_profile_source: "task_override",
            adapter: "codex",
            status: "unverified",
            reason: "fresh corrected verification required",
            compatibility: {
              status: "matched", observed_version: "codex-cli 0.155.1",
              pack_id: "codex", pack_revision: "1", contract_id: "manager",
              contract_revision: "1", short_hash: "123456789abc",
              predicate_id: "codex-exact", missing_evidence: ["native_proof"],
              action: "requalify_exact_profile",
              message: "The exact contract is eligible for capability qualification; proof is pending.",
            },
            runtime_admission: {
              id: "failed-task-runtime",
              scope_hash: "failed-task-scope",
              state: "failed",
              fresh_call_count: 1,
              probe_state: "failed",
              failure_category: "model_refusal",
            },
          }]));
        }
        const proofReady = requests.some((value) =>
          value.action === "prepare_runtime_admission"
        );
        return new Response(JSON.stringify([{
          task_id: "AJ-1",
          role: "implementer",
          requested_revision: 1,
          config: {
            provider: "codex",
            model: "gpt-5.6-sol",
            effort: "high",
          },
          capability_key: currentPreparationKey,
          generic_capability_supported: true,
          exact_runtime_authority: proofReady,
          task_profile_source: preparationSource,
          adapter: "codex",
          status: proofReady ? "supported" : "unverified",
          compatibility: {
            status: "matched", observed_version: "codex-cli 0.155.1",
            pack_id: "codex", pack_revision: "1", contract_id: "manager",
            contract_revision: "1", short_hash: "123456789abc",
            predicate_id: "codex-exact", missing_evidence: ["native_proof"],
            action: "requalify_exact_profile",
            message: "The exact contract is eligible for capability qualification; proof is pending.",
          },
          reason: proofReady
            ? "Exact current runtime-scoped authority is supported"
            : "The generic prepared tuple is Supported, but exact current scoped authority is missing; it is not a usable replacement until corrected",
        }]));
      }
      const request = JSON.parse(String(init?.body));
      requests.push(request);
      if (request.kind === "cmux_discard_unknown") {
        const discardSession = String(request.session_id);
        cmuxDiscardResponses[discardSession] =
          (cmuxDiscardResponses[discardSession] || 0) + 1;
        if (cmuxDiscardResponses[discardSession] === 1) {
          throw new Error("cmux discard response was lost");
        }
        return new Response(JSON.stringify({
          result: {
            state: "failed",
            message: "unknown reservation discarded without closing a surface",
            retry_available: true,
            surface: persistentSurface(String(request.surface_route_id), {
              surface_state: "retired",
              attachment_state: "failed",
            }),
          },
        }));
      }
      if (request.kind === "cmux_set_keyboard_control") {
        const acquire = request.action === "acquire";
        const revision = Number(request.expected_control_revision) + 1;
        return new Response(JSON.stringify({
          result: {
            state: acquire ? "control" : "view_only",
            message: acquire
              ? "keyboard control applied on the authenticated attachment"
              : "keyboard-control release applied on the authenticated attachment",
            surface: persistentSurface(String(request.surface_route_id), {
              binding_revision: Number(request.expected_binding_revision),
              desired_input_state: acquire ? "control" : "view_only",
              actual_input_state: acquire ? "control" : "view_only",
              control_revision: revision,
              applied_revision: revision,
            }),
          },
        }));
      }
      return new Response(JSON.stringify({ result: { state: "requested" } }));
    }) as typeof fetch;
  try {
    mount(<WorkflowControls task={task} onChanged={() => {}} />);
    click("Pause after role");
    await settle();
    if (
      requests.at(-1)?.kind !== "control" ||
      requests.at(-1)?.task_id !== "AJ-1" ||
      requests.at(-1)?.expected_version !== 7 ||
      requests.at(-1)?.action !== "pause_after_role"
    ) throw new Error("control target or version drifted");
    unmount();

    const freshDispatchTask: Task = {
      ...task,
      id: "AJ-fresh-dispatch",
      attention: "restart_parked",
      version: 11,
    };
    let freshDispatchRefreshes = 0;
    mount(
      <WorkflowControls
        task={freshDispatchTask}
        actions={[{
          kind: "continue_fresh_dispatch",
          enabled: true,
          reason:
            "A verified quiescent stop requires a separately confirmed fresh dispatch.",
          owner: "human",
          operation: "continue",
          binding: {
            task_id: freshDispatchTask.id,
            attempt_id: "a1",
            session_id: "settled-stop-session",
          },
        }]}
        onChanged={() => {
          freshDispatchRefreshes++;
        }}
      />,
    );
    const gatedPause = [...document.querySelectorAll("button")].find((button) =>
      button.textContent?.includes("Pause after role")
    ) as HTMLButtonElement | undefined;
    if (!gatedPause?.disabled) {
      throw new Error("fresh dispatch recovery did not gate ordinary controls");
    }
    click("Continue with fresh dispatch");
    await settle();
    if (
      requests.at(-1)?.kind !== "control" ||
      requests.at(-1)?.task_id !== freshDispatchTask.id ||
      requests.at(-1)?.expected_version !== freshDispatchTask.version ||
      requests.at(-1)?.action !== "continue" ||
      JSON.stringify(requests.at(-1)?.payload) !== "{}" ||
      freshDispatchRefreshes !== 1
    ) {
      throw new Error(
        "fresh dispatch click did not submit the current ordinary control exactly once",
      );
    }
    rerender(
      <WorkflowControls
        task={{ ...freshDispatchTask, attention: "none", version: 12 }}
        actions={[]}
        onChanged={() => {}}
      />,
    );
    const releasedPause = [...document.querySelectorAll("button")].find((
      button,
    ) => button.textContent?.includes("Pause after role")) as
      | HTMLButtonElement
      | undefined;
    if (releasedPause?.disabled) {
      throw new Error(
        "fresh dispatch success did not restore current ordinary controls",
      );
    }
    unmount();

    const normalizedLegacyTask: Task = {
      ...task,
      id: "AJ-legacy-normalized",
      lifecycle: "backlog",
      attention: "needs_input",
      version: 8,
      active_attempt: undefined,
      legacy: {
        source_status: "implemented",
        llmrelay_normalization: {
          state: "normalized",
          normalized_at: "2026-01-01T00:00:00Z",
        },
      },
    };
    mount(
      <WorkflowControls
        task={normalizedLegacyTask}
        project={initialized}
        onChanged={() => {}}
      />,
    );
    if (
      !document.body.textContent?.includes("Make Ready") ||
      document.body.textContent.includes("Start a fresh managed attempt")
    ) {
      throw new Error(
        "normalized legacy work did not return to normal Ready admission",
      );
    }
    click("Make Ready");
    await settle();
    if (
      requests.at(-1)?.kind !== "make_ready" ||
      requests.at(-1)?.task_id !== normalizedLegacyTask.id ||
      requests.at(-1)?.expected_version !== normalizedLegacyTask.version
    ) {
      throw new Error(
        "normalized legacy Ready action drifted from normal admission",
      );
    }
    unmount();

    const config = {
      provider: "codex" as const,
      model: "gpt-5.6-sol",
      effort: "high",
    };
    const configured: Task = {
      ...task,
      role_settings: [{
        id: "rs1",
        role: "manager",
        revision: 1,
        config,
        effective_generation_id: "g1",
      }, {
        id: "rs2",
        role: "manager",
        revision: 2,
        config: { ...config, model: "next" },
      }, {
        id: "rs3",
        role: "implementer",
        revision: 1,
        config,
      }],
    };
    const session = {
      id: "s1",
      role_generation_id: "g1",
      provider: "codex",
      status: "running",
      readiness: "busy",
      capture_state: "capturing",
      updated_at: "",
      task_id: "AJ-1",
      attempt_id: "a1",
      role: "manager",
      generation: 1,
      config_revision: 1,
      launch: {
        model: config.model,
        effort: config.effort,
        permission_policy: "read-only",
        security_policy: {},
      },
    } as Session;
    mount(
      <RoleSettings
        task={configured}
        project={initialized}
        sessions={[session]}
        productionRestrictions={[{
          provider: "codex",
          role: "implementer",
          status: "unverified",
          reason:
            "Codex Implementer requires exact current native validation, including a delivered PermissionRequest decision; native approvals may be reused without an inbox request and app revocation affects only app-owned rules",
        }]}
        onChanged={() => {}}
      />,
    );
    await settle();
    if (
      !document.body.textContent?.includes("requested rev 2") ||
      !document.body.textContent?.includes("effective rev 1") ||
      !document.body.textContent?.includes("running") ||
      !document.body.textContent?.includes("Production Unverified") ||
      !document.body.textContent?.includes("not a usable replacement") ||
      !document.body.textContent?.includes(
        "Authority source: project default · adapter codex",
      ) ||
      !document.body.textContent?.includes(
        "native approvals are honored and may avoid a new inbox item",
      )
    ) {
      throw new Error(
        "requested, effective, running, or fixed production restriction state collapsed",
      );
    }
    await settle();
    click("Prepare exact runtime proof");
    await settle();
    if (
      requests.at(-1)?.action !== "prepare_runtime_admission" ||
      requests.at(-1)?.role !== "implementer" ||
      requests.at(-1)?.expected_version !== initialized.version ||
      "task_id" in (requests.at(-1) || {}) ||
      "settings_revision" in (requests.at(-1) || {})
    ) {
      throw new Error(
        "project-default runtime correction was not project scoped",
      );
    }
    const implementerRow = [...document.querySelectorAll("article")].find((
      row,
    ) => row.querySelector("strong")?.textContent === "Implementer");
    const implementerEdit = [...implementerRow!.querySelectorAll("button")]
      .find(
        (button) => button.textContent === "Edit",
      );
    act(() => implementerEdit!.click());
    if (
      !implementerRow?.textContent?.includes("Production Unverified") ||
      !implementerRow.textContent.includes("Validation required")
    ) {
      throw new Error(
        "Codex Implementer edit selection hid its validation gate",
      );
    }
    click("Edit");
    click("Save request");
    await settle();
    if (
      requests.at(-1)?.kind !== "set_role_settings" ||
      requests.at(-1)?.role !== "manager" ||
      requests.at(-1)?.expected_version !== 7
    ) throw new Error("role request authority drifted");
    unmount();
    preparationSource = "task_override";
    failedRuntimeView = true;
    mount(
      <RoleSettings
        task={configured}
        project={initialized}
        sessions={[]}
        productionRestrictions={[]}
        onChanged={() => {}}
      />,
    );
    await settle();
    click("Prepare corrected runtime verification");
    await settle();
    if (
      requests.at(-1)?.action !== "prepare_runtime_admission" ||
      requests.at(-1)?.task_id !== "AJ-1" ||
      requests.at(-1)?.role !== "implementer" ||
      requests.some((request) => request.kind === "runtime_probe_launch")
    ) {
      throw new Error(
        "failed task runtime admission was relaunched instead of freshly prepared",
      );
    }
    unmount();
    mount(
      <ProjectSetup
        project={initialized}
        setup={{
          setup_operation_id: "setup-current",
          project_id: initialized.id,
          state: "activated",
          target_inventory: {},
          final_files: [],
          installation_source_binding_complete: true,
          installation_source_binding_reason: "activated",
          selected_profiles: [],
          probe_receipts: [],
          sessions: [],
          runtime_admissions: [{
            id: "failed-project-runtime",
            scope_hash: "failed-project-scope",
            state: "authorized",
            fresh_call_count: 2,
            failure_category: "model_refusal",
            probes: [{
              role: "explorer",
              profile: config,
              profile_hash: "explorer-profile",
              project_config_revision_id: "config-1",
              project_configuration_hash: "configuration-1",
              adapter: "codex",
              adapter_hash: "adapter-1",
              capability_key: "explorer-capability-key",
              nonce: "not-rendered",
              state: "failed",
              failure_category: "model_refusal",
              has_native_session: true,
            }, {
              role: "manager",
              profile: config,
              profile_hash: "manager-profile",
              project_config_revision_id: "config-1",
              project_configuration_hash: "configuration-1",
              adapter: "llmrelay_service_native_manager",
              adapter_hash: "manager-adapter",
              capability_key: "manager-capability-key",
              nonce: "not-rendered",
              state: "authorized",
              has_native_session: false,
            }],
          }],
          agents_file: {},
          created_at: "2026-01-01T00:00:00Z",
          updated_at: "2026-01-01T00:00:00Z",
        }}
        onChanged={() => {}}
        onViewSession={cmuxFixture}
      />,
    );
    click("Prepare corrected runtime verification");
    await settle();
    if (
      requests.at(-1)?.action !== "prepare_runtime_admission" ||
      requests.at(-1)?.project_id !== initialized.id ||
      requests.at(-1)?.role !== "explorer" ||
      "task_id" in (requests.at(-1) || {})
    ) {
      throw new Error(
        "failed project role did not prepare a role-local corrected scope",
      );
    }
    unmount();
    mount(
      <ProjectSetup
        project={initialized}
        setup={{
          setup_operation_id: "setup-failed-stop",
          project_id: initialized.id,
          state: "discovery",
          discovery_attempt_id: "setup-failed-stop-attempt",
          manager_control: {
            hold: {
              id: "setup-failed-stop-control",
              state: "held",
              requested_operation_id: "original-stop",
              created_at: "2026-01-01T00:00:00Z",
              updated_at: "2026-01-01T00:00:00Z",
            },
            current: config,
            requested: config,
            effective: config,
            interrupt_requested: false,
            quiescent: false,
            next_action: {
              action: "retry_stop" as const,
              reason:
                "The prior manager-only Stop delivery failed: exact manager process is unavailable. A fresh human Retry Stop will recheck the exact recorded manager identity; no automatic retry or replacement is scheduled.",
            },
          },
          target_inventory: {},
          final_files: [],
          installation_source_binding_complete: false,
          installation_source_binding_reason: "discovery",
          selected_profiles: [{
            role: "manager" as const,
            selection_state: "selected" as const,
            profile: config,
          }],
          probe_receipts: [],
          sessions: [],
          runtime_admissions: [],
          agents_file: {},
          created_at: "2026-01-01T00:00:00Z",
          updated_at: "2026-01-01T00:00:00Z",
        }}
        onChanged={() => {}}
        onViewSession={cmuxFixture}
      />,
    );
    if (
      !document.body.textContent?.includes(
        "exact manager process is unavailable",
      ) ||
      !document.body.textContent.includes("no automatic retry or replacement")
    ) {
      throw new Error("failed setup manager delivery reason was not shown");
    }
    click("Retry Stop discovery manager");
    await settle();
    if (
      requests.at(-1)?.kind !== "trip" ||
      requests.at(-1)?.action !== "stop_setup_manager" ||
      requests.at(-1)?.setup_operation_id !== "setup-failed-stop" ||
      requests.at(-1)?.expected_project_version !== initialized.version
    ) {
      throw new Error(
        "retry Stop did not submit fresh versioned setup authority",
      );
    }
    unmount();
    mount(
      <ProjectSetup
        project={initialized}
        setup={{
          setup_operation_id: "setup-unchanged-manager",
          project_id: initialized.id,
          state: "discovery",
          discovery_attempt_id: "setup-unchanged-manager-attempt",
          manager_control: {
            current: config,
            requested: config,
            effective: config,
            interrupt_requested: false,
            quiescent: true,
            next_action: {
              action: "change" as const,
              reason: "Choose the exact replacement manager profile.",
            },
          },
          target_inventory: {},
          final_files: [],
          installation_source_binding_complete: false,
          installation_source_binding_reason: "discovery",
          selected_profiles: [{
            role: "manager" as const,
            selection_state: "selected" as const,
            profile: config,
          }],
          probe_receipts: [],
          sessions: [],
          runtime_admissions: [],
          agents_file: {},
          created_at: "2026-01-01T00:00:00Z",
          updated_at: "2026-01-01T00:00:00Z",
        }}
        onChanged={() => {}}
        onViewSession={cmuxFixture}
      />,
    );
    const unchangedChange = [...document.querySelectorAll("button")].find((
      button,
    ) => button.textContent === "Change discovery manager") as
      | HTMLButtonElement
      | undefined;
    const requestsBeforeDisabledChange = requests.length;
    if (
      !unchangedChange?.disabled ||
      !document.body.textContent?.includes(
        "Change is disabled because the replacement profile is unchanged",
      ) ||
      !document.body.textContent.includes(
        "Retry or Launch manager discovery",
      )
    ) {
      throw new Error(
        "unchanged discovery manager did not explain its disabled Change action",
      );
    }
    act(() => unchangedChange.click());
    if (requests.length !== requestsBeforeDisabledChange) {
      throw new Error("disabled unchanged manager Change dispatched a request");
    }
    field("Exact replacement model", "gpt-5.6-terra");
    const changedManager = [...document.querySelectorAll("button")].find((
      button,
    ) => button.textContent === "Change discovery manager") as
      | HTMLButtonElement
      | undefined;
    if (!changedManager || changedManager.disabled) {
      throw new Error("changed manager profile did not enable explicit Change");
    }
    act(() => changedManager.click());
    await settle();
    const changedHostManager = requests.at(-1)?.host_manager as
      | Record<string, unknown>
      | undefined;
    if (
      requests.at(-1)?.action !== "change_setup_manager" ||
      requests.at(-1)?.setup_operation_id !== "setup-unchanged-manager" ||
      changedHostManager?.model !== "gpt-5.6-terra"
    ) {
      throw new Error(
        "enabled discovery manager Change sent the wrong profile",
      );
    }
    unmount();
    let recoveryRefreshes = 0;
    const recoveryViewedSessions: string[] = [];
    const recoveryRefreshCount = () => recoveryRefreshes;
    const recoverySession = {
      id: "setup-recovery-session-current-50",
      attempt_id: "setup-recovery-attempt",
      role: "manager" as const,
      provider: "codex" as const,
      generation: 50,
      lane_id: "default",
      status: "recovery_required",
      launch_state: "delivery_unknown",
      readiness: "unknown",
      capture_state: "idle",
      has_native_session: true,
      resume_count: 0,
      updated_at: "2026-01-01T00:00:50Z",
    };
    const oldRecoverySession = {
      ...recoverySession,
      id: "setup-recovery-session-old-49",
      generation: 49,
      status: "exited",
      launch_state: "finished",
      readiness: "idle",
      updated_at: "2026-01-01T00:00:49Z",
    };
    const setupRecovery = {
      record_id: "setup-recovery-record",
      session_id: recoverySession.id,
      attempt_id: recoverySession.attempt_id,
      task_id: "hidden-setup-task",
      task_version: 50,
      role: "manager" as const,
      validation_cell: "trip_setup_discovery" as const,
      state: "attention_required" as const,
      session_status: "recovery_required",
      attempt_status: "needs_recovery",
      task_attention: "needs_recovery",
      ownership_state: "recorded_process_verification_required" as const,
      created_at: "2026-01-01T00:00:00Z",
      updated_at: "2026-01-01T00:00:00Z",
    };
    const setupRecoveryView = {
      setup_operation_id: "setup-recovery",
      project_id: initialized.id,
      state: "discovery" as const,
      discovery_attempt_id: recoverySession.attempt_id,
      target_inventory: {},
      final_files: [],
      installation_source_binding_complete: false,
      installation_source_binding_reason: "discovery",
      selected_profiles: [],
      probe_receipts: [],
      sessions: [oldRecoverySession, recoverySession],
      recoveries: [setupRecovery],
      runtime_admissions: [],
      agents_file: {},
      created_at: "2026-01-01T00:00:00Z",
      updated_at: "2026-01-01T00:00:00Z",
    };
    mount(
      <ProjectSetup
        project={initialized}
        setup={{
          ...setupRecoveryView,
          sessions: [{
            ...recoverySession,
            status: "running",
            launch_state: "started",
            readiness: "busy_unresolved_hook_work",
            input_control: {
              owner_kind: "human" as const,
              expires_at: "9999-01-01T00:00:00Z",
            },
          }],
          recoveries: [],
        }}
        onChanged={() => {}}
        onViewSession={cmuxFixture}
      />,
    );
    if (
      !document.body.textContent?.includes(
        "waiting for keyboard control release",
      ) ||
      !document.body.textContent.includes(
        "Automatic manager progress is paused",
      ) ||
      !document.body.textContent.includes("press Ctrl-]") ||
      document.body.textContent.includes(
        "running · busy_unresolved_hook_work",
      )
    ) {
      throw new Error(
        "active setup keyboard control was not presented as the progress hold",
      );
    }
    unmount();
    mount(
      <ProjectSetup
        project={initialized}
        setup={setupRecoveryView}
        onChanged={() => {
          recoveryRefreshes++;
        }}
        onViewSession={async (sessionId) => {
          recoveryViewedSessions.push(sessionId);
          if (recoveryViewedSessions.length > 1) {
            return {
              state: "unknown" as const,
              message: "setup cmux create result is unknown",
              retry_available: false,
              surface: persistentSurface(
                "00000000-0000-0000-0000-000000000043",
                {
                  surface_state: "unknown" as const,
                  attachment_state: "pending" as const,
                },
              ),
            };
          }
          return cmuxFixture(sessionId);
        }}
      />,
    );
    click("View output");
    await settle();
    if (
      recoveryViewedSessions.at(-1) !== recoverySession.id ||
      !document.body.textContent?.includes(
        "service verifies the recorded operating system identities",
      ) ||
      !document.body.textContent.includes("fresh-only final verifier") ||
      document.body.textContent.includes("Resume retained session")
    ) {
      throw new Error(
        "current setup output/recovery identity or conditional guidance drifted",
      );
    }
    // Recovery-required sessions are intentionally view-only. A second explicit
    // View may surface an unknown durable reservation, but must not turn a
    // recovery record into a keyboard-control target.
    if (
      [...document.querySelectorAll("button")].some((button) =>
        button.textContent?.includes("Take keyboard control")
      )
    ) {
      throw new Error(
        "recovery-required setup session exposed keyboard control",
      );
    }
    click("View output");
    await settle();
    if (
      recoveryViewedSessions.at(-1) !== recoverySession.id
    ) {
      throw new Error(
        "discovery output did not retain its exact current session",
      );
    }
    if (
      !document.body.textContent?.includes(
        "setup cmux create result is unknown",
      )
    ) {
      throw new Error(
        `setup cmux notice missing: ${document.body.textContent}`,
      );
    }
    click("Discard unknown reservation");
    await settle();
    click("Discard unknown reservation");
    await settle();
    const setupDiscards = requests.filter((request) =>
      request.kind === "cmux_discard_unknown" &&
      request.session_id === recoverySession.id
    );
    if (
      setupDiscards.length !== 2 ||
      setupDiscards[0].operation_id !== setupDiscards[1].operation_id ||
      setupDiscards[1].surface_route_id !==
        "00000000-0000-0000-0000-000000000043"
    ) {
      throw new Error(
        "setup unknown-route retry changed its operation identity",
      );
    }
    field(
      "Recovery evidence for manager",
      "Operator observed the stopped setup invocation",
    );
    click("Verify quiescence and reconcile");
    await settle();
    if (
      requests.at(-1)?.kind !== "resolve_recovery" ||
      requests.at(-1)?.task_id !== "hidden-setup-task" ||
      requests.at(-1)?.recovery_id !== "setup-recovery-record" ||
      requests.at(-1)?.attempt_id !== recoverySession.attempt_id ||
      requests.at(-1)?.session_id !== recoverySession.id ||
      requests.at(-1)?.expected_version !== 50 ||
      requests.at(-1)?.decision !== "confirm_quiescent" ||
      recoveryRefreshCount() !== 4
    ) {
      throw new Error(
        "setup recovery click did not execute and refresh exact authority",
      );
    }
    unmount();
    mount(
      <ProjectSetup
        project={initialized}
        setup={{
          ...setupRecoveryView,
          sessions: [oldRecoverySession, {
            ...recoverySession,
            status: "exited",
            launch_state: "finished",
            readiness: "idle_candidate",
          }],
          recoveries: [],
        }}
        onChanged={() => {
          recoveryRefreshes++;
        }}
        onViewSession={cmuxFixture}
      />,
    );
    if (
      !document.body.textContent?.includes(
        "first pass complete · retained resume ready",
      ) || !document.body.textContent.includes(
        "completed the first turn without spending this resume",
      )
    ) {
      throw new Error("retained setup completion state was not explained");
    }
    click("Resume retained session");
    await settle();
    if (
      requests.at(-1)?.kind !== "role_resume" ||
      requests.at(-1)?.session_id !== recoverySession.id ||
      requests.at(-1)?.prompt !== "" || recoveryRefreshCount() !== 5
    ) {
      throw new Error(
        "verified setup recovery did not expose exact public resume",
      );
    }
    unmount();
    mount(
      <ProjectSetup
        project={initialized}
        setup={{
          ...setupRecoveryView,
          sessions: [oldRecoverySession, {
            ...recoverySession,
            status: "exited",
            launch_state: "finished",
            readiness: "idle_candidate",
            resume_count: 1,
          }],
          recoveries: [],
        }}
        onChanged={() => {
          recoveryRefreshes++;
        }}
        onViewSession={cmuxFixture}
      />,
    );
    if (
      document.body.textContent?.includes("Resume retained session") ||
      document.body.textContent?.includes(
        "first pass complete · retained resume ready",
      ) || !document.body.textContent?.includes(
        "Its one resume is spent; it cannot be resumed again",
      )
    ) {
      throw new Error("spent retained setup resume was presented as reusable");
    }
    unmount();
    const runtimeSession = {
      ...recoverySession,
      id: "runtime-recovery-session",
      attempt_id: "runtime-recovery-attempt",
      role: "plan_reviewer" as const,
    };
    const runtimeRecovery = {
      ...setupRecovery,
      record_id: "runtime-recovery-record",
      session_id: runtimeSession.id,
      attempt_id: runtimeSession.attempt_id,
      role: "plan_reviewer" as const,
      validation_cell: "trip_runtime_probe" as const,
      runtime_admission_id: "runtime-recovery-admission",
    };
    const runtimeRecoveryView = {
      ...setupRecoveryView,
      setup_operation_id: "setup-activated",
      state: "activated" as const,
      discovery_attempt_id: undefined,
      sessions: [runtimeSession],
      recoveries: [runtimeRecovery],
      installation_source_binding_complete: true,
      installation_source_binding_reason: "activated",
      runtime_admissions: [{
        id: "runtime-recovery-admission",
        scope_hash: "runtime-recovery-scope",
        state: "running",
        fresh_call_count: 6,
        probes: [{
          role: "plan_reviewer" as const,
          profile: config,
          profile_hash: "plan-reviewer-profile",
          project_config_revision_id: "config-1",
          project_configuration_hash: "configuration-1",
          adapter: "codex",
          adapter_hash: "adapter-1",
          capability_key: "plan-reviewer-capability-key",
          nonce: "not-rendered",
          state: "running",
          session_id: runtimeSession.id,
          session_status: "running",
          readiness: "unknown",
          has_native_session: true,
        }],
      }],
    };
    const runtimeViewedSessions: string[] = [];
    const refreshesBeforeRuntime = recoveryRefreshCount();
    mount(
      <ProjectSetup
        project={initialized}
        setup={runtimeRecoveryView}
        onChanged={() => {
          recoveryRefreshes++;
        }}
        onViewSession={async (sessionId) => {
          runtimeViewedSessions.push(sessionId);
          if (runtimeViewedSessions.length > 1) {
            return {
              state: "view_only" as const,
              message:
                "runtime cmux surface is ready for explicit keyboard control",
              retry_available: true,
              surface: persistentSurface(
                "00000000-0000-0000-0000-000000000044",
                { attachment_state: "live" as const },
              ),
            };
          }
          return cmuxFixture(sessionId);
        }}
      />,
    );
    click("View output");
    await settle();
    click("Take keyboard control");
    await settle();
    if (
      JSON.stringify(runtimeViewedSessions) !==
        JSON.stringify([
          runtimeSession.id,
          runtimeSession.id,
        ])
    ) {
      throw new Error(
        "runtime output did not preserve its exact current session",
      );
    }
    click("Release keyboard control");
    await settle();
    const runtimeControls = requests.filter((request) =>
      request.kind === "cmux_set_keyboard_control" &&
      request.session_id === runtimeSession.id
    );
    if (
      runtimeControls.length !== 2 ||
      JSON.stringify(runtimeControls.map((request) => request.action)) !==
        JSON.stringify(["acquire", "release"]) ||
      runtimeControls.some((request) =>
        request.surface_route_id !== "00000000-0000-0000-0000-000000000044" ||
        request.expected_binding_revision !== 1 ||
        "lease" in request || "secret" in request
      )
    ) {
      throw new Error(
        "runtime keyboard-control requests drifted from the revision-bound protocol",
      );
    }
    field(
      "Recovery evidence for plan_reviewer",
      "Operator observed the stopped runtime probe",
    );
    click("Verify quiescence and reconcile");
    await settle();
    if (
      requests.at(-1)?.kind !== "resolve_recovery" ||
      requests.at(-1)?.recovery_id !== "runtime-recovery-record" ||
      requests.at(-1)?.session_id !== runtimeSession.id ||
      requests.at(-1)?.attempt_id !== runtimeSession.attempt_id ||
      recoveryRefreshCount() !== refreshesBeforeRuntime + 5
    ) {
      throw new Error(
        "runtime admission recovery was not reachable from setup",
      );
    }
    const refreshesAfterRuntime = recoveryRefreshCount();
    unmount();
    mount(
      <ProjectSetup
        project={initialized}
        setup={{
          ...runtimeRecoveryView,
          recoveries: [],
          runtime_admissions: [{
            ...runtimeRecoveryView.runtime_admissions[0],
            probes: [{
              ...runtimeRecoveryView.runtime_admissions[0].probes[0],
              session_status: "exited",
            }],
          }],
        }}
        onChanged={() => {
          recoveryRefreshes++;
        }}
        onViewSession={cmuxFixture}
      />,
    );
    click("Resume same native session");
    await settle();
    if (
      requests.at(-1)?.kind !== "runtime_probe_resume" ||
      requests.at(-1)?.admission_id !== "runtime-recovery-admission" ||
      requests.at(-1)?.role !== "plan_reviewer" ||
      recoveryRefreshCount() !== refreshesAfterRuntime + 1
    ) {
      throw new Error(
        "verified runtime recovery did not expose dedicated resume",
      );
    }
    unmount();
    failedRuntimeView = false;
    mount(
      <RoleSettings
        task={configured}
        sessions={[session]}
        productionRestrictions={[{
          provider: "codex",
          role: "implementer",
          status: "unverified",
          reason: "exact native validation required",
        }]}
        capabilities={[{
          provider: "codex",
          role: "implementer",
          mode: "interactive_pty",
          status: "supported",
          config_hash: "stale-complete-preparation-key",
          proof: {
            model: "gpt-5.6-sol",
            effort: "high",
            local_mcp_coverage_revision: "codex-local-mcp-coverage-v1-0.154.0",
            native_approval_ownership_revision:
              "codex-native-approval-ownership-v1",
          },
          gaps: [],
        }]}
        onChanged={() => {}}
      />,
    );
    await settle();
    if (
      !document.body.textContent?.includes("Validation required") ||
      !document.body.textContent?.includes(
        "current preparation key has not yet appeared",
      )
    ) {
      throw new Error(
        "stale Supported evidence with identical labels hid the exact current preparation gate",
      );
    }
    unmount();
    mount(
      <RoleSettings
        task={configured}
        sessions={[session]}
        productionRestrictions={[{
          provider: "codex",
          role: "implementer",
          status: "unverified",
          reason: "exact native validation required",
        }]}
        capabilities={[{
          provider: "codex",
          role: "implementer",
          mode: "interactive_pty",
          status: "supported",
          config_hash: currentPreparationKey,
          proof: {
            model: "gpt-5.6-sol",
            effort: "high",
            local_mcp_coverage_revision: "codex-local-mcp-coverage-v1-0.155.1",
            native_approval_ownership_revision:
              "codex-native-approval-ownership-v1",
          },
          gaps: [],
        }]}
        onChanged={() => {}}
      />,
    );
    await settle();
    if (
      document.body.textContent?.includes("exact native validation required")
    ) {
      throw new Error(
        "exact current Supported preparation remained validation-required",
      );
    }
    click("Activate task profile");
    await settle();
    if (
      requests.at(-1)?.kind !== "activate_task_profile" ||
      requests.at(-1)?.role !== "implementer" ||
      requests.at(-1)?.settings_revision !== 1 ||
      requests.at(-1)?.expected_version !== 7
    ) throw new Error("task-profile activation authority drifted");
    unmount();

    let selectProvider: (provider: "codex" | "claude") => void = () => {};
    const CatalogRace = () => {
      const [provider, setProvider] = useState<"codex" | "claude">("codex");
      selectProvider = setProvider;
      return (
        <ModelSelector
          provider={provider}
          value="manual-exact-model"
          onChange={noop}
        />
      );
    };
    mount(<CatalogRace />);
    click("Refresh local suggestions");
    act(() => selectProvider("claude"));
    if (!codexCatalogRequests[0]) {
      throw new Error("Codex catalog request did not start");
    }
    codexCatalogRequests[0].resolve("old-codex-suggestion");
    await settle();
    if (
      !codexCatalogResolved ||
      document.body.textContent?.includes("old-codex-suggestion") ||
      findField("Exact model").value !== "manual-exact-model"
    ) {
      throw new Error(
        "provider-switch catalog response changed scoped suggestions or the exact model",
      );
    }
    act(() => selectProvider("codex"));
    click("Refresh local suggestions");
    act(() => selectProvider("claude"));
    act(() => selectProvider("codex"));
    click("Refresh local suggestions");
    if (!codexCatalogRequests[1] || !codexCatalogRequests[2]) {
      throw new Error("Codex ABA catalog requests did not both start");
    }
    codexCatalogRequests[2].resolve("latest-codex-suggestion");
    await settle();
    codexCatalogRequests[1].resolve("stale-codex-a-suggestion");
    await settle();
    if (
      !document.body.textContent?.includes("latest-codex-suggestion") ||
      document.body.textContent?.includes("old-codex-suggestion") ||
      document.body.textContent?.includes("stale-codex-a-suggestion") ||
      findField("Exact model").value !== "manual-exact-model"
    ) {
      throw new Error(
        "stale Codex-A success replaced the newer Codex-B catalog or exact model",
      );
    }
    act(() => selectProvider("claude"));
    act(() => selectProvider("codex"));
    click("Refresh local suggestions");
    act(() => selectProvider("claude"));
    act(() => selectProvider("codex"));
    click("Refresh local suggestions");
    if (!codexCatalogRequests[3] || !codexCatalogRequests[4]) {
      throw new Error(
        "Codex stale-error ABA catalog requests did not both start",
      );
    }
    codexCatalogRequests[4].resolve("latest-codex-after-error");
    await settle();
    codexCatalogRequests[3].reject(new Error("stale Codex catalog failure"));
    await settle();
    if (
      !document.body.textContent?.includes("latest-codex-after-error") ||
      document.body.textContent?.includes("stale Codex catalog failure") ||
      findField("Exact model").value !== "manual-exact-model"
    ) {
      throw new Error(
        "stale Codex error replaced the newer catalog or exact model",
      );
    }

    const terminal: Task = {
      ...configured,
      lifecycle: "done",
      archived: true,
    };
    mount(<WorkflowControls task={terminal} onChanged={() => {}} />);
    if (
      document.body.textContent?.includes("Continue") ||
      document.body.textContent?.includes("Run next")
    ) throw new Error("terminal workflow controls remained actionable");
    unmount();
    mount(
      <RoleSettings
        task={terminal}
        sessions={[{ ...session, status: "exited" }]}
        onChanged={() => {}}
      />,
    );
    if (
      [...document.querySelectorAll("button")].some((button) =>
        [
          "Edit",
          "Resume exact native session",
          "Freeze safe plan checkpoint",
          "Request checkpoint switch",
        ].includes(button.textContent || "")
      )
    ) throw new Error("terminal history exposed role mutation or resume");
    unmount();

    const terminalHistory: Task = {
      ...terminal,
      dependencies: [{ task_id: "AJ-0", verified_at: null }],
      review_budgets: [{
        id: "budget-1",
        attempt_id: "a1",
        kind: "code",
        initial_allowance: 2,
        extension_allowance: 1,
        spent: 2,
        remaining: 1,
        version: 1,
      }],
      reviews: [{
        id: "review-1",
        kind: "code",
        candidate_hash: "candidate",
        delivery_state: "finished",
        verdict: "approved",
      }],
    };
    const terminalState = {
      schema: 2,
      generated_at: "",
      incarnation: "fixture",
      revision: "1",
      attention: [],
      projects: [project],
      tasks: [terminalHistory],
      production_role_restrictions: [],
      capabilities: [],
      active_sessions: [],
      controls: [{
        id: "proposal-1",
        attempt_id: "a1",
        kind: "transition_proposal",
        state: "proposed",
        payload: { phase: "checks" },
        updated_at: "2026-09-18T00:00:00.000Z",
      }],
      guidance: [],
      check_suites: [{
        id: "suite-1",
        project_id: "p1",
        enabled: true,
        name: "contracts",
        executable: "cargo",
      }],
      checks: [],
      switches: [],
      recovery: [],
      history: [],
      profile_sets: [],
      task_recipes: [],
      recipe_schedules: [],
      continuation_actions: [],
      decisions: [],
      instance_settings: {
        version: 1,
        auto_resume_eligible: false,
        updated_at: "",
      },
      restart_candidates: [],
      permission_requests: [],
      permission_rules: [],
      resources: {
        active_sessions: 0,
        active_controls: 0,
        queued_guidance: 0,
        running_checks: 0,
        observed_at: "",
        processes: [],
      },
    } as AppState;
    mount(
      <TaskDetail
        task={terminalHistory}
        state={terminalState}
        onClose={() => {}}
        onChanged={() => {}}
      />,
    );
    for (
      const historical of [
        "Manager transition proposal",
        "contracts",
        "AJ-0",
        "2 spent · 1 remaining",
        "approved",
      ]
    ) {
      if (!document.body.textContent?.includes(historical)) {
        throw new Error(`terminal history hid ${historical}`);
      }
    }
    for (
      const action of [
        "Apply proposed transition",
        "Run",
        "Record",
        "Add",
        "Extend +1",
      ]
    ) {
      if (
        [...document.querySelectorAll("button")].some((button) =>
          button.textContent?.trim() === action
        )
      ) throw new Error(`terminal task exposed ${action}`);
    }
    if (
      document.querySelector('[aria-label="Dependency task"]') ||
      document.querySelector('[aria-label^="Integration ref for"]')
    ) throw new Error("terminal task exposed dependency mutation fields");
    unmount();

    mount(
      <ResourceStatus
        resources={{
          observed_at: "now",
          active_sessions: 3,
          running_checks: 0,
          queued_guidance: 0,
          processes: [],
          capacity: {
            policy: "up to four occupied invocations",
            occupied_global: 3,
            global_processes: 4,
            occupied_by_provider: { codex: 2, claude: 1 },
            active_invocations_per_provider: 2,
            occupied_managers: 2,
            occupied_managers_by_provider: { codex: 1, claude: 1 },
            managers_per_provider: 1,
            issued_reservations: 0,
          },
        }}
      />,
    );
    for (
      const truthful of [
        "global 3/4",
        "Codex 2/2",
        "Claude 1/2",
        "reserved 0",
        "Codex managers 1/1",
        "Claude managers 1/1",
      ]
    ) {
      if (!document.body.textContent?.includes(truthful)) {
        throw new Error(`capacity status hid ${truthful}`);
      }
    }
    unmount();

    localStorage.setItem("agenticjira.workspace.sideWidth", "330");
    const restoreState = {
      schema: 2,
      generated_at: "",
      incarnation: "fixture",
      revision: "1",
      attention: [],
      projects: [project],
      tasks: [task],
      production_role_restrictions: [],
      capabilities: [],
      active_sessions: [
        {
          ...session,
          id: "restore-s",
          setup_operation_id: "setup-workspace-session",
          input_control: {
            owner_kind: "human",
            expires_at: "9999-01-01T00:00:00Z",
          },
        },
        {
          ...session,
          id: "restore-s-2",
          role: "implementer",
          status: "exited",
          exit_code: 1,
          exit_reason: `error: invalid model identifier gpt-missing ${
            "diagnostic context ".repeat(80)
          }`,
        },
        {
          ...session,
          id: "direct-take-s",
          role: "explorer",
        },
      ],
      controls: [],
      guidance: [],
      check_suites: [],
      checks: [],
      switches: [],
      recovery: [],
      history: [],
      profile_sets: [],
      task_recipes: [],
      recipe_schedules: [],
      continuation_actions: [{
        kind: "exact_resume",
        enabled: true,
        reason:
          "The exact retained restart session can resume after current authority rechecks.",
        owner: "human",
        operation: "restart_resume",
        binding: {
          task_id: "AJ-1",
          attempt_id: "a1",
          session_id: "restore-s",
        },
        accounting_note: "Reuses the retained native session.",
      }],
      decisions: [],
      instance_settings: {
        version: 4,
        auto_resume_eligible: false,
        updated_at: "now",
      },
      restart_candidates: [{
        session_id: "restore-s",
        attempt_id: "a1",
        task_id: "AJ-1",
        source: "planned_shutdown",
        state: "parked",
        reason: "eligible exact native binding is parked",
        result: {},
        updated_at: "now",
      }],
      permission_requests: [],
      permission_rules: [],
      resources: {
        active_sessions: 0,
        active_controls: 0,
        queued_guidance: 0,
        running_checks: 0,
        observed_at: "now",
        processes: [],
      },
    } as AppState;
    const taskOutputRequests: string[] = [];
    const directTakeViews: string[] = [];
    const directTakeControls: Array<{
      sessionId: string;
      surfaceRouteId: string;
      bindingRevision: number;
      controlRevision: number;
      action: string;
    }> = [];
    const directTakeSurface = persistentSurface(
      "00000000-0000-0000-0000-000000000046",
      { binding_revision: 17, control_revision: 23, applied_revision: 23 },
    );
    const openedSetupProjects: string[] = [];
    const textEncoder = new TextEncoder();
    const longRawOsc = new Uint8Array([
      0x9d,
      ...textEncoder.encode("SECRET-LONG-RAW-OSC".repeat(1200)),
    ]);
    const rawControlEnd = new Uint8Array([
      0x9c,
      ...textEncoder.encode("safe after long raw OSC\n"),
      0x90,
      ...textEncoder.encode("SECRET-RAW-DCS\u0007SECRET-AFTER-BEL"),
      0x9c,
      0x98,
      ...textEncoder.encode("SECRET-RAW-SOS"),
      0x9c,
      0x9f,
      ...textEncoder.encode("SECRET-RAW-APC"),
      0x9c,
      0x9e,
      ...textEncoder.encode("SECRET-RAW-PM"),
      0x9c,
      0x9b,
      ...textEncoder.encode("31mraw CSI safe\n"),
      0xc2,
    ]);
    const splitUtf8C1 = new Uint8Array([
      0x9d,
      ...textEncoder.encode("SECRET-SPLIT-UTF8-OSC"),
      0xc2,
      0x9c,
      ...textEncoder.encode("safe after raw controls\n"),
    ]);
    const escapeIntermediatesStart = new Uint8Array([
      ...textEncoder.encode("escape-left"),
      0x1b,
      0x28,
    ]);
    const escapeIntermediatesEnd = new Uint8Array([
      0x42,
      ...textEncoder.encode("escape-middle"),
      0x1b,
      0x29,
      0x30,
      ...textEncoder.encode("escape-next"),
      0x1b,
      0x23,
      0x38,
      ...textEncoder.encode("escape-safe"),
      0x7f,
      0x85,
      ...textEncoder.encode("control-filter-safe\n"),
    ]);
    const doubleEscapeStart = new Uint8Array([
      ...textEncoder.encode("double-escape-left"),
      0x1b,
    ]);
    const doubleEscapeEnd = new Uint8Array([
      0x1b,
      0x5d,
      ...textEncoder.encode("SECRET-DOUBLE-ESC-OSC"),
      0x07,
      ...textEncoder.encode("double-escape-safe\n"),
    ]);
    let restoreViewCount = 0;
    let restoreHistoricalViewCount = 0;
    let selectedRecoveryTask: Task | undefined;
    mount(
      <Workspace
        state={restoreState}
        onSelect={(selected) => {
          selectedRecoveryTask = selected;
        }}
        onChanged={() => {}}
        onOpenSetup={(projectId) => openedSetupProjects.push(projectId)}
        onViewCmuxSession={async (sessionId) => {
          if (sessionId === "direct-take-s") {
            directTakeViews.push(sessionId);
            return {
              state: "view_only" as const,
              message: "direct Take received one exact fresh view route",
              retry_available: true,
              surface: directTakeSurface,
            };
          }
          taskOutputRequests.push(sessionId);
          if (sessionId === "restore-s") {
            restoreViewCount++;
          }
          if (sessionId === "restore-s" && restoreViewCount === 1) {
            throw new Error("watch attachment unavailable");
          }
          if (sessionId === "restore-s" && restoreViewCount === 2) {
            return {
              state: "view_only" as const,
              message:
                "exact persistent cmux surface is ready for explicit keyboard control",
              retry_available: true,
              surface: persistentSurface(),
            };
          }
          if (sessionId === "restore-s-2") {
            restoreHistoricalViewCount++;
          }
          if (sessionId === "restore-s-2" && restoreHistoricalViewCount === 2) {
            return {
              state: "unknown_live" as const,
              message:
                "exact attachment client is live while the surface identity remains unknown",
              retry_available: false,
              surface: persistentSurface(
                "00000000-0000-0000-0000-000000000045",
                {
                  surface_state: "unknown" as const,
                  attachment_state: "live" as const,
                },
              ),
            };
          }
          if (sessionId === "restore-s" && restoreViewCount >= 3) {
            return {
              state: "unknown" as const,
              message: "cmux create result is unknown",
              retry_available: false,
              surface: persistentSurface(
                "00000000-0000-0000-0000-000000000042",
                {
                  surface_state: "unknown" as const,
                  attachment_state: "pending" as const,
                },
              ),
            };
          }
          return {
            state: "recorded_output" as const,
            message:
              "exact retained history only; no provider resume was requested",
            retry_available: false,
            recorded_output: {
              frames: [{
                epoch: "recorded-epoch",
                sequence: 1,
                captured_at: "2026-01-01T00:00:00Z",
                encoding: "base64" as const,
                data: "4oI=",
                gap: false,
              }, {
                epoch: "recorded-epoch",
                sequence: 2,
                captured_at: "2026-01-01T00:00:01Z",
                encoding: "base64" as const,
                data: "rBtb",
                gap: false,
              }, {
                epoch: "recorded-epoch",
                sequence: 3,
                captured_at: "2026-01-01T00:00:02Z",
                encoding: "base64" as const,
                data: "MzFtcmVkG1swbQ==",
                gap: false,
              }, {
                epoch: "recorded-epoch",
                sequence: 4,
                captured_at: "2026-01-01T00:00:03Z",
                encoding: "base64" as const,
                data: encodedBytes(longRawOsc),
                gap: false,
              }, {
                epoch: "recorded-epoch",
                sequence: 5,
                captured_at: "2026-01-01T00:00:04Z",
                encoding: "base64" as const,
                data: encodedBytes(rawControlEnd),
                gap: false,
              }, {
                epoch: "recorded-epoch",
                sequence: 6,
                captured_at: "2026-01-01T00:00:05Z",
                encoding: "base64" as const,
                data: encodedBytes(splitUtf8C1),
                gap: false,
              }, {
                epoch: "recorded-epoch",
                sequence: 7,
                captured_at: "2026-01-01T00:00:06Z",
                encoding: "utf8" as const,
                data: "\u001b]0;SECRET-OSC",
                gap: false,
              }, {
                epoch: "recorded-epoch",
                sequence: 8,
                captured_at: "2026-01-01T00:00:07Z",
                encoding: "utf8" as const,
                data:
                  "-CONT\u0007\u001bPSECRET-DCS\u0007SECRET-DCS-AFTER-BEL\u001b\\\u001b_SECRET-APC\u001b\\\u001b^SECRET-PM\u001b\\\u009dSECRET-C1-OSC\u009c\u0090SECRET-C1-DCS\u009c\u009fSECRET-C1-APC\u009csafe after controls",
                gap: false,
              }, {
                epoch: "recorded-epoch",
                sequence: 9,
                captured_at: "2026-01-01T00:00:08Z",
                encoding: "base64" as const,
                data: encodedBytes(doubleEscapeStart),
                gap: false,
              }, {
                epoch: "recorded-epoch",
                sequence: 10,
                captured_at: "2026-01-01T00:00:09Z",
                encoding: "base64" as const,
                data: encodedBytes(doubleEscapeEnd),
                gap: false,
              }, {
                epoch: "recorded-epoch",
                sequence: 11,
                captured_at: "2026-01-01T00:00:10Z",
                encoding: "base64" as const,
                data: encodedBytes(escapeIntermediatesStart),
                gap: false,
              }, {
                epoch: "recorded-epoch",
                sequence: 12,
                captured_at: "2026-01-01T00:00:11Z",
                encoding: "base64" as const,
                data: encodedBytes(escapeIntermediatesEnd),
                gap: false,
              }, {
                epoch: "recorded-epoch",
                sequence: 13,
                captured_at: "2026-01-01T00:00:12Z",
                encoding: "utf8" as const,
                data: "older output was retained only in part",
                gap: true,
              }, {
                epoch: "replacement-epoch",
                sequence: 1,
                captured_at: "2026-01-01T00:00:13Z",
                encoding: "utf8" as const,
                data: "after exact epoch replacement",
                gap: false,
              }],
              next_epoch: "replacement-epoch",
              next_sequence: 1,
              has_more: false,
            },
          };
        }}
        onSetCmuxKeyboardControl={async (sessionId, surface, action) => {
          directTakeControls.push({
            sessionId,
            surfaceRouteId: surface.id,
            bindingRevision: surface.binding_revision,
            controlRevision: surface.control_revision,
            action,
          });
          return {
            state: "pending" as const,
            message: "exact direct keyboard-control request is pending",
            surface: {
              ...surface,
              desired_input_state: action === "acquire"
                ? "control"
                : "view_only",
              control_revision: surface.control_revision + 1,
              updated_at: "2026-09-20T00:00:00Z",
            },
          };
        }}
        onDiscardCmuxSurface={async (
          sessionId,
          surfaceRouteId,
          operationId,
        ) => {
          const response = await fetch("/api/operation", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              kind: "cmux_discard_unknown",
              operation_id: operationId,
              session_id: sessionId,
              surface_route_id: surfaceRouteId,
            }),
          });
          const body = await response.json() as {
            result?: CmuxViewOutcome;
            error?: string;
          };
          if (!response.ok || !body.result) {
            throw new Error(
              body.error || "cmux discard did not return an outcome",
            );
          }
          return body.result;
        }}
      />,
    );
    if (
      !document.body.textContent?.includes("Restart candidates") ||
      !document.body.textContent?.includes(
        "eligible exact native binding is parked",
      )
    ) {
      throw new Error(
        "restart reason was not restored",
      );
    }
    const fetchBeforePreview = globalThis.fetch;
    const previewPaths: string[] = [];
    globalThis.fetch = (async (input: string | URL | Request) => {
      previewPaths.push(String(input));
      return new Response(JSON.stringify({
        decision_schema: 1,
        snapshot: {
          captured_at: "now",
          process_inventory: "satisfied",
          boot_identity: "satisfied",
          dispatch_enabled: true,
          draining: false,
          revalidation_required: true,
          notice: "No drain or resume was requested",
        },
        sessions: [{
          classification: "fresh_only",
          can_resume_now: false,
          could_resume_after_confirmed_shutdown: false,
          decision: {
            decision_schema: 1,
            reason_code: "restart.fresh_only",
            disposition: "waiting",
            subject: { session_id: "restore-s" },
            observed_revision: {},
            prerequisites: [],
            ownership: { owner: "human", state: "recorded", binding: {} },
            control_policy: { allowed_controls: [] },
          },
        }],
      }));
    }) as typeof fetch;
    if (previewPaths.length !== 0) {
      throw new Error("restart preview was polled on render");
    }
    click("Preview restart");
    await settle();
    if (
      previewPaths.join(",") !== "/api/restart-preview" ||
      !document.body.textContent?.includes("fresh only") ||
      !document.body.textContent.includes("No current resume route")
    ) {
      throw new Error(
        "explicit preview did not render its read-only classification",
      );
    }
    globalThis.fetch =
      (async (input: string | URL | Request, init?: RequestInit) => {
        if (String(input) !== "/api/operation") {
          throw new Error("resume used an unexpected route");
        }
        const request = JSON.parse(String(init?.body));
        if (request.kind !== "restart_resume" || "session_ids" in request) {
          throw new Error(
            "Resume eligible did not use its explicit bounded route",
          );
        }
        return new Response(JSON.stringify({
          result: {
            operation_id: request.operation_id,
            mode: "eligible",
            state: "queued",
            selected_ids: [],
            queued_ids: ["restore-s"],
            omitted_ids: ["restore-s-2"],
            omitted_count: 1,
            outcomes: [
              {
                session_id: "restore-s",
                state: "queued",
                reason: "serialized admission",
              },
              {
                session_id: "restore-s-2",
                state: "omitted",
                reason: "bounded batch",
              },
            ],
          },
        }));
      }) as typeof fetch;
    click("Resume eligible");
    await settle();
    if (
      !document.body.textContent?.includes("Queued 1: restore-s") ||
      !document.body.textContent.includes("Omitted 1: restore-s-2") ||
      document.body.textContent.includes("Restart resume · resumed")
    ) {
      throw new Error("bulk result hid queued or omitted work");
    }
    globalThis.fetch = fetchBeforePreview;
    if (
      !document.body.textContent.includes("Project setup session") ||
      !document.body.textContent.includes(
        "Stop or Replace remains an explicit human action",
      ) ||
      !document.body.textContent.includes(
        "Keyboard control is active. Automatic progress is paused",
      ) ||
      !document.body.textContent.includes("Ctrl-] detaches") ||
      document.body.textContent.includes(
        "error: invalid model identifier gpt-missing",
      ) ||
      document.querySelector(".workspace-instructions")?.hasAttribute("open")
    ) {
      throw new Error(
        "workspace setup action, collapsed instructions, or exit disclosure drifted",
      );
    }
    const directTakeRow = [
      ...document.querySelectorAll(".session-tree article"),
    ]
      .find((row) => row.textContent?.includes("explorer · codex"));
    const directTake = [...(directTakeRow?.querySelectorAll("button") || [])]
      .find((button) => button.textContent?.includes("Take keyboard control"));
    if (!directTake || (directTake as HTMLButtonElement).disabled) {
      throw new Error(
        "a running session without a surface did not expose direct Take",
      );
    }
    act(() => directTake.click());
    await settle();
    if (
      JSON.stringify(directTakeViews) !== '["direct-take-s"]' ||
      directTakeControls.length !== 1 ||
      JSON.stringify(directTakeControls[0]) !== JSON.stringify({
          sessionId: "direct-take-s",
          surfaceRouteId: "00000000-0000-0000-0000-000000000046",
          bindingRevision: 17,
          controlRevision: 23,
          action: "acquire",
        })
    ) {
      throw new Error(
        "direct Take did not View once then acquire the exact returned route and revisions",
      );
    }
    click("Open project setup controls");
    if (JSON.stringify(openedSetupProjects) !== JSON.stringify(["p1"])) {
      throw new Error("setup session did not open its project setup controls");
    }
    click("Show exit details");
    if (
      !document.body.textContent.includes(
        "error: invalid model identifier gpt-missing",
      )
    ) {
      throw new Error("exit details disclosure did not reveal the reason");
    }
    click("Hide exit details");
    if (
      document.body.textContent.includes(
        "error: invalid model identifier gpt-missing",
      )
    ) {
      throw new Error("exit details did not collapse again");
    }
    const roleResumesBeforeOutput = requests.filter((request) =>
      request.kind === "role_resume" || request.kind === "runtime_probe_resume"
    ).length;
    click("View output");
    await settle();
    if (!document.body.textContent?.includes("watch attachment unavailable")) {
      throw new Error(
        "watch request did not retain its foreground error for retry",
      );
    }
    click("View output again");
    await settle();
    click("Take keyboard control");
    await settle();
    if (
      !document.body.textContent?.includes("Exit 1") ||
      !document.body.textContent?.includes("Discard unknown reservation")
    ) {
      throw new Error(
        "session exit reason or explicit unknown-reservation action was hidden",
      );
    }
    click("Discard unknown reservation");
    await settle();
    if (
      !document.body.textContent?.includes("cmux discard response was lost")
    ) {
      throw new Error("ambiguous discard did not retain its retry action");
    }
    click("Discard unknown reservation");
    await settle();
    const discards = requests.filter((request) =>
      request.kind === "cmux_discard_unknown" &&
      request.session_id === "restore-s"
    );
    const discard = discards.at(-1);
    if (
      JSON.stringify(taskOutputRequests) !==
        JSON.stringify(["restore-s", "restore-s", "restore-s"]) ||
      discard?.surface_route_id !== "00000000-0000-0000-0000-000000000042" ||
      discard?.session_id !== "restore-s" ||
      discards.length !== 2 ||
      discards[0].operation_id !== discards[1].operation_id ||
      !document.body.textContent?.includes("unknown reservation discarded") ||
      requests.filter((request) =>
          request.kind === "role_resume" ||
          request.kind === "runtime_probe_resume"
        ).length !== roleResumesBeforeOutput
    ) {
      throw new Error(
        "task cmux view, retry, or read-only recorded-history contract drifted",
      );
    }
    const implementerSession = [
      ...document.querySelectorAll(".session-tree article"),
    ]
      .find((row) =>
        row.textContent?.includes("implementer · codex")
      );
    const liveView = [
      ...(implementerSession?.querySelectorAll("button") || []),
    ]
      .find((button) => button.textContent?.includes("View output"));
    act(() => liveView!.click());
    await settle();
    if (
      !document.body.textContent?.includes("€red") ||
      !document.body.textContent?.includes("safe after controls") ||
      !document.body.textContent?.includes("safe after raw controls") ||
      !document.body.textContent?.includes("raw CSI safe") ||
      !document.body.textContent?.includes(
        "double-escape-leftdouble-escape-safe",
      ) ||
      !document.body.textContent?.includes(
        "escape-leftescape-middleescape-nextescape-safecontrol-filter-safe",
      ) ||
      document.body.textContent?.includes("Bescape-middle") ||
      document.body.textContent?.includes("0escape-next") ||
      document.body.textContent?.includes("8escape-safe") ||
      document.body.textContent?.includes("\u007f") ||
      document.body.textContent?.includes("\u0085") ||
      document.body.textContent?.includes("SECRET-") ||
      !document.body.textContent.includes("recorded output gap") ||
      !document.body.textContent.includes("recorded output changed")
    ) {
      throw new Error(
        "historical recorded cmux output was not safely rendered",
      );
    }
    const unknownLiveView = [
      ...(implementerSession?.querySelectorAll("button") || []),
    ].find((button) => button.textContent?.includes("View output"));
    act(() => unknownLiveView!.click());
    await settle();
    if (
      !document.body.textContent?.includes(
        "exact attachment client is live while the surface identity remains unknown",
      ) ||
      [...document.querySelectorAll("button")].some((button) =>
        button.textContent?.includes("Discard unknown reservation")
      ) ||
      unknownLiveView?.hasAttribute("disabled") ||
      [...(implementerSession?.querySelectorAll("button") || [])]
        .some((button) => button.textContent?.includes("Take keyboard control"))
    ) {
      throw new Error(
        "an exited unknown presentation did not preserve recorded-only access",
      );
    }
    act(() => unknownLiveView!.click());
    await settle();
    if (
      !document.body.textContent?.includes(
        "exact retained history only; no provider resume was requested",
      )
    ) {
      throw new Error(
        "non-running unknown presentation did not return to recorded output",
      );
    }
    const checkbox = document.querySelector<HTMLInputElement>(
      'input[type="checkbox"]',
    )!;
    act(() => checkbox.click());
    await settle();
    if (
      requests.at(-1)?.kind !== "set_auto_resume" ||
      requests.at(-1)?.expected_version !== 4 ||
      requests.at(-1)?.enabled !== true
    ) throw new Error("versioned auto-resume setting was not submitted");
    click("Open task recovery");
    if (selectedRecoveryTask?.id !== "AJ-1") {
      throw new Error("restart candidate did not open its exact task recovery");
    }
    const sideDivider = document.querySelector<HTMLElement>(
      '[aria-label="Resize workspace details"]',
    )!;
    act(() =>
      sideDivider.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }),
      )
    );
    await settle();
    if (
      localStorage.getItem("agenticjira.workspace.sideWidth") !== "346" ||
      sideDivider.getAttribute("aria-valuenow") !== "346"
    ) {
      throw new Error(
        "bounded workspace side divider did not persist",
      );
    }
    unmount();
    mount(
      <TaskDetail
        task={selectedRecoveryTask}
        state={restoreState}
        onClose={noop}
        onChanged={noop}
      />,
    );
    click("Resume retained restart session");
    await settle();
    if (
      requests.at(-1)?.kind !== "restart_resume" ||
      JSON.stringify(requests.at(-1)?.session_ids) !== '["restore-s"]'
    ) {
      throw new Error(
        "projected retained restart action did not submit its exact session ID",
      );
    }
    unmount();
    mount(
      <Workspace
        state={restoreState}
        onSelect={() => {}}
        onChanged={() => {}}
      />,
    );
    if (
      document.querySelector('[aria-label="Resize workspace details"]')
        ?.getAttribute("aria-valuenow") !== "346"
    ) throw new Error("workspace side divider did not survive remount");
    unmount();

    const raceOpen = persistentSurface(
      "00000000-0000-0000-0000-000000000047",
      { updated_at: "2026-09-20T00:00:09Z" },
    );
    const raceState = (surface: CmuxSessionSurface) =>
      ({
        ...restoreState,
        generated_at: surface.updated_at,
        active_sessions: [{
          ...session,
          id: "workspace-race-s",
          role: "explorer",
          updated_at: surface.updated_at,
          cmux_surface: surface,
        }],
        restart_candidates: [],
      }) as AppState;
    const raceLost = {
      ...raceOpen,
      updated_at: "2026-09-20T00:00:10Z",
      surface_state: "lost" as const,
      desired_input_state: "view_only" as const,
      actual_input_state: "lost" as const,
      last_error: "fixture validated cmux loss observation",
    };
    await verifyDeferredCmuxView(
      (surface, onView) => (
        <Workspace
          state={raceState(surface)}
          onSelect={noop}
          onChanged={noop}
          onViewCmuxSession={onView}
        />
      ),
      raceOpen,
      raceLost,
    );
    unmount();

    const reviewing: Task = {
      ...task,
      lifecycle: "awaiting_review",
      active_attempt: {
        id: "a1",
        phase: "awaiting_human_review",
        status: "running",
        base_revision: "abc",
        candidate_hash: "candidate",
      },
    };
    mount(<ReviewPanel task={reviewing} onChanged={() => {}} />);
    field("Review or rework feedback", "Fix the exact edge case");
    click("Request rework");
    await settle();
    if (
      requests.at(-1)?.kind !== "human_review" ||
      requests.at(-1)?.decision !== "request_changes" ||
      requests.at(-1)?.expected_version !== 7 ||
      requests.at(-1)?.carry_plan_approval !== false
    ) throw new Error("rework target, version, or explicit carry drifted");
    click("Accept result");
    await settle();
    if (
      requests.at(-1)?.decision !== "accept" ||
      requests.at(-1)?.task_id !== "AJ-1" ||
      requests.at(-1)?.attempt_id !== "a1"
    ) throw new Error("accept target drifted");
    unmount();

    const state = {
      schema: 2,
      generated_at: "",
      incarnation: "fixture",
      revision: "1",
      attention: [],
      projects: [project],
      tasks: [task],
      production_role_restrictions: [],
      capabilities: [],
      active_sessions: [],
      controls: [],
      guidance: [{ id: "g1", body: "wait", state: "queued" }, {
        id: "g2",
        body: "sent",
        state: "submitted",
      }, { id: "g3", body: "seen", state: "acknowledged" }],
      check_suites: [],
      checks: [],
      switches: [],
      recovery: [],
      history: [],
      profile_sets: [],
      task_recipes: [],
      recipe_schedules: [],
      continuation_actions: [],
      decisions: [],
      instance_settings: {
        version: 1,
        auto_resume_eligible: false,
        updated_at: "",
      },
      restart_candidates: [],
      permission_requests: [],
      permission_rules: [],
      resources: {
        active_sessions: 0,
        active_controls: 0,
        queued_guidance: 1,
        running_checks: 0,
        observed_at: "",
        processes: [],
      },
    } as AppState;
    mount(
      <AttentionInbox
        state={state}
        onNavigate={() => undefined}
        onChanged={() => {}}
      />,
    );
    for (const label of ["queued", "submitted", "acknowledged"]) {
      if (!document.body.textContent?.includes(label)) {
        throw new Error(`${label} guidance state hidden`);
      }
    }
  } finally {
    unmount();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("T19 permission inbox scopes exact actions, refreshes conflicts, and revokes", async () => {
  const requests: Record<string, unknown>[] = [];
  const selections: Array<[string, string | undefined]> = [];
  let conflict = false;
  let protocolConflict = false;
  let changed = 0;
  globalThis.fetch =
    (async (_input: string | URL | Request, init?: RequestInit) => {
      requests.push(JSON.parse(String(init?.body)));
      return protocolConflict
        ? new Response(JSON.stringify({
          error: "protocol mismatch",
          protocol_error: {
            reason: "incompatible_generation",
            observed_generation: 1,
            expected_generation: 2,
            guidance: "Reload the dashboard for the matching protocol.",
          },
        }), { status: 409 })
        : conflict
        ? new Response(JSON.stringify({ error: "stale request" }), {
          status: 409,
        })
        : new Response(JSON.stringify({ result: { state: "applied" } }));
    }) as typeof fetch;
  const decidedAt = "2026-09-13T18:30:00.000Z";
  const deliveredAt = "2026-09-13T18:30:01.000Z";
  const unknownAt = "2026-09-13T18:30:02.000Z";
  const permission = {
    id: "permission-1",
    project_id: "p1",
    task_id: "AJ-1",
    attempt_id: "a1",
    session_id: "session-1",
    role_generation_id: "generation-1",
    role: "implementer",
    provider: "codex",
    native_session_id: "native-1",
    tool_name: "shell",
    input: {
      command: "./gradlew build",
      cwd: "/managed/a1",
      sandbox_permissions: "require_escalated",
    },
    requested_access: { sandbox_permissions: "require_escalated" },
    command_display: "./gradlew build",
    family_preview: {
      session: {
        lifetime: "session",
        command_family: "./gradlew with all arguments",
        arguments: "all",
        provider: "codex",
        role: "implementer",
        native_session: "native-1",
        worktree: "/managed/a1",
        coverage: "this exact managed role/native session and worktree",
        configuration_binding:
          "The complete validated frozen configuration fingerprint must remain compatible, including model and effort.",
        warning:
          "The requested rule trusts this executable family with all current and future arguments. The enforced Codex profile remains active; Codex Implementer still requires exact current native validation. Native approvals may be reused without an inbox request, and app Revoke affects app-owned rules only. This request does not itself establish provider capability or production support.",
      },
      project: {
        lifetime: "project",
        command_family: "./gradlew with all arguments",
        arguments: "all",
        provider: "codex",
        role: "implementer",
        registered_root: "/registered",
        coverage: "current and future ready engine-owned worktrees",
        configuration_binding:
          "The complete validated frozen configuration fingerprint must remain compatible, including model and effort.",
        warning:
          "The requested rule trusts this executable family with all current and future arguments. The enforced Codex profile remains active; Codex Implementer still requires exact current native validation. Native approvals may be reused without an inbox request, and app Revoke affects app-owned rules only. This request does not itself establish provider capability or production support.",
      },
    },
    created_at: new Date().toISOString(),
    deadline_at: new Date(Date.now() + 60_000).toISOString(),
    state: "pending",
    revision: 4,
    delivery_state: "not_reserved",
  } as const;
  const state = {
    schema: 3,
    generated_at: "",
    incarnation: "fixture",
    revision: "1",
    attention: [],
    projects: [project],
    tasks: [{ ...task, permission_waiting: true }],
    production_role_restrictions: [],
    capabilities: [],
    active_sessions: [],
    controls: [],
    guidance: [],
    check_suites: [],
    checks: [],
    switches: [],
    recovery: [],
    history: [],
      profile_sets: [],
      task_recipes: [],
      recipe_schedules: [],
    continuation_actions: [],
    decisions: [],
    instance_settings: {
      version: 1,
      auto_resume_eligible: false,
      updated_at: "",
    },
    restart_candidates: [],
    resources: {
      active_sessions: 1,
      active_controls: 0,
      queued_guidance: 0,
      running_checks: 0,
      observed_at: "",
      processes: [],
    },
    permission_requests: [
      permission,
      {
        ...permission,
        id: "permission-reserved",
        state: "approved_once",
        revision: 6,
        decision_kind: "approve_once",
        decision_actor: "authenticated_human",
        decision_reason: "approved for this exact action",
        decided_at: decidedAt,
        delivery_state: "reserved",
        delivery_reserved_at: decidedAt,
        reserved_behavior: "allow",
      },
      {
        ...permission,
        id: "permission-delivered",
        state: "approved_rule",
        revision: 8,
        decision_kind: "matching_rule",
        decision_actor: "human_rule",
        decision_reason: "matched active human-created command-family rule",
        decided_at: decidedAt,
        matching_rule_id: "rule-1",
        delivery_state: "delivered",
        delivery_reserved_at: decidedAt,
        reserved_behavior: "allow",
        delivered_at: deliveredAt,
        delivery_reason:
          "response bytes written and flushed to the authenticated local hook connection; native command execution is not proven",
      },
      {
        ...permission,
        id: "permission-unknown",
        state: "approved_once",
        revision: 7,
        decision_kind: "approve_once",
        decision_actor: "authenticated_human",
        decided_at: decidedAt,
        delivery_state: "unknown",
        delivery_reserved_at: decidedAt,
        reserved_behavior: "allow",
        delivery_unknown_at: unknownAt,
        delivery_reason: "socket write or flush failed; replay is prohibited",
      },
    ],
    permission_rules: [{
      id: "rule-1",
      provider: "codex",
      project_id: "p1",
      role: "implementer",
      lifetime: "session",
      session_id: "session-1",
      display_family: "./gradlew with all arguments",
      scope: {
        registered_root: "/registered",
        repository_identity: "git:/registered/.git",
        worktree: "/managed/a1",
        executable_kind: "project_relative",
        executable: "gradlew",
        policy_fingerprint: "permission-policy-v3",
        native_session: "native-1",
        coverage: "this exact managed role/native session and worktree",
        configuration_binding:
          "The complete validated frozen configuration fingerprint must remain compatible, including provider executable version, hook and security policy, launch arguments and environment keys, model, and effort.",
      },
      created_at: new Date().toISOString(),
      use_count: 2,
      revision: 3,
    }],
  } as AppState;
  try {
    mount(
      <ApprovalInbox
        state={state}
        onSelect={(selected, sessionId) =>
          selections.push([selected.id, sessionId])}
        onChanged={() => changed++}
      />,
    );
    document.querySelector<HTMLButtonElement>(".approval-target")!.click();
    if (
      JSON.stringify(selections) !== JSON.stringify([["AJ-1", "session-1"]])
    ) throw new Error("permission request selected the wrong task or session");
    const completeInput = [...document.querySelectorAll("details")].find((
      item,
    ) =>
      item.querySelector("summary")?.textContent?.includes(
        "Complete structured input",
      )
    )!;
    completeInput.querySelector<HTMLElement>("summary")!.click();
    if (
      !completeInput.open ||
      !completeInput.textContent?.includes("sandbox_permissions") ||
      !completeInput.textContent?.includes("/managed/a1")
    ) {
      throw new Error(
        "complete structured permission input was not expandable",
      );
    }
    const actions = [...document.querySelectorAll(".approval-actions button")]
      .map((button) => button.textContent?.trim());
    if (
      JSON.stringify(actions) !== JSON.stringify([
        "Approve once",
        "Always approve matching actions",
        "Deny",
      ])
    ) {
      throw new Error(
        "permission request did not expose exactly three actions",
      );
    }
    click("Always approve matching actions");
    if (requests.length !== 0) {
      throw new Error("opening the project suggestion submitted a decision");
    }
    if (
      !document.body.textContent?.includes("future ready engine-owned") ||
      !document.body.textContent?.includes("Project (recommended)") ||
      !document.body.textContent?.includes("changed filenames, counts") ||
      !document.body.textContent?.includes("/registered")
    ) throw new Error("recommended Project scope preview was not explicit");
    const scope = document.querySelector<HTMLSelectElement>(
      '[aria-label="Always approval lifetime"]',
    )!;
    if (scope.value !== "project") {
      throw new Error("Project scope was not the backend-backed default");
    }
    change(scope as unknown as HTMLInputElement, "session");
    if (
      !document.body.textContent?.includes(
        "this exact managed role/native session and worktree",
      ) || !document.body.textContent?.includes("/managed/a1")
    ) throw new Error("selectable Session scope preview was not explicit");
    if (requests.length !== 0) {
      throw new Error(
        "switching scope submitted a decision before confirmation",
      );
    }
    const audit = document.body.textContent || "";
    for (
      const expected of [
        "authenticated_human",
        "human_rule",
        "rule-1",
        new Date(decidedAt).toLocaleString(),
        "response reserved",
        "response delivered",
        "response unknown",
        "does not prove the native command executed",
        "provider executable version, hook and security policy",
        "authorized 2 reserved responses",
        "Codex Implementer still requires exact current native validation",
        "Codex may reuse an approval already granted natively",
        "does not revoke native Codex approvals",
      ]
    ) {
      if (!audit.includes(expected)) {
        throw new Error(`permission audit omitted ${expected}`);
      }
    }
    click("Confirm always approval");
    await settle();
    if (
      requests.at(-1)?.kind !== "decide_permission" ||
      requests.at(-1)?.request_id !== "permission-1" ||
      requests.at(-1)?.expected_revision !== 4 ||
      requests.at(-1)?.decision !== "always_approve" ||
      requests.at(-1)?.lifetime !== "session"
    ) throw new Error("always approval target, revision, or scope drifted");
    click("Revoke");
    await settle();
    if (
      requests.at(-1)?.kind !== "revoke_permission_rule" ||
      requests.at(-1)?.rule_id !== "rule-1" ||
      requests.at(-1)?.expected_revision !== 3
    ) throw new Error("rule revocation target drifted");
    conflict = true;
    click("Approve once");
    await settle();
    if (
      changed < 3 ||
      !document.body.textContent?.includes("State was refreshed")
    ) throw new Error("stale decision did not request a conflict refresh");
    conflict = false;
    protocolConflict = true;
    const changesBeforeProtocol = changed;
    const requestsBeforeProtocol = requests.length;
    click("Approve once");
    await settle();
    if (changed !== changesBeforeProtocol ||
      requests.length !== requestsBeforeProtocol + 1 ||
      !document.body.textContent?.includes("Reload the dashboard for the matching protocol.") ||
      document.body.textContent?.includes("The request changed in another view")) {
      throw new Error("protocol refusal entered the ordinary 409 revision branch");
    }
    unmount();
    protocolConflict = false;
    conflict = false;
    const serviceCheck: TripTaskVerification = {
      attempt_id: "a1",
      selected_revision: 2,
      check_id: "focused-check",
      required: true,
      candidate_hash: "candidate",
      exact_command_hash: "command-hash",
      scope_hash: "scope-hash",
      command: {
        kind: "structured_argv",
        executable: "./gradlew",
        arguments: ["test", "--tests", "Focused"],
        cwd: ".",
      },
      authorization: {
        once_available: false,
        reusable: false,
        family: false,
        authorized: false,
        action_state: "actionable",
        state: "pending",
        source: "service_check",
        family_preview: {
          source: "service_check",
          command_family: "./gradlew with all arguments",
          current_arguments: ["test", "--tests", "Focused"],
          repository_identity: "git:/registered/.git",
          worktree: "/managed/a1",
          warning:
            "Selection, candidate, inputs, freshness, and build ownership remain mandatory.",
        },
      },
    };
    const serviceState = {
      ...state,
      permission_requests: [],
      trip_checks: [{
        id: "focused-check",
        project_id: "p1",
        config_revision_id: "config-1",
        check_key: "focused",
        category: "focused",
        command_kind: "structured_argv",
        executable: "./gradlew",
        arguments: ["test", "--tests", "Focused"],
        cwd: ".",
        timeout_seconds: 60,
        acceptance_rows: ["focused behavior"],
        relevant_inputs: ["src/focused.ts"],
        invalidation: {},
        original_text: "./gradlew test --tests Focused",
        enabled: true,
      }],
      trip_task_verification: [serviceCheck, {
        ...serviceCheck,
        check_id: "planning-history-check",
        authorization: {
          ...serviceCheck.authorization,
          action_state: "inactive",
          inactive_reason: "Decisions are available only during checks.",
        },
      }, {
        ...serviceCheck,
        check_id: "fresh-check",
        authorization: {
          ...serviceCheck.authorization,
          action_state: "current_receipt",
        },
      }],
    } as AppState;
    mount(
      <ApprovalInbox
        state={serviceState}
        onSelect={() => {}}
        onChanged={() => changed++}
      />,
    );
    const serviceText = document.body.textContent || "";
    if (document.querySelectorAll(".approval-request").length !== 1) {
      throw new Error("service-check inbox included non-actionable history");
    }
    for (
      const expected of [
        "service check · current reviewed selection",
        "Source: service-owned selected check",
        "./gradlew with all arguments",
        "git:/registered/.git",
      ]
    ) {
      if (!serviceText.includes(expected)) {
        throw new Error(`service-check inbox omitted ${expected}`);
      }
    }
    click("Always approve matching actions");
    await settle();
    if (
      requests.at(-1)?.kind !== "trip" ||
      requests.at(-1)?.action !== "authorize_check" ||
      requests.at(-1)?.decision !== "approved" ||
      requests.at(-1)?.lifetime !== "family" ||
      requests.at(-1)?.scope_hash !== "scope-hash"
    ) {
      throw new Error(
        "service-check family decision lost exact selection scope",
      );
    }
    unmount();

    const inactiveState = {
      ...serviceState,
      trip_task_verification: [{
        ...serviceCheck,
        authorization: {
          ...serviceCheck.authorization,
          family: true,
          authorized: true,
          state: "approved_family",
          action_state: "inactive",
          inactive_reason: "Decisions are available only during checks.",
        },
      }],
    } as AppState;
    mount(
      <TaskDetail
        task={task}
        state={inactiveState}
        onClose={() => {}}
        onChanged={() => changed++}
      />,
    );
    const inactiveActions = [...document.querySelectorAll<HTMLButtonElement>(
      ".service-check-permission .approval-actions button",
    )];
    if (
      inactiveActions.length !== 3 ||
      inactiveActions.some((button) => !button.disabled) ||
      !document.body.textContent?.includes(
        "Decisions are available only during checks.",
      )
    ) throw new Error("inactive service-check controls were not truthful");
    const inactiveRun = [...document.querySelectorAll<HTMLButtonElement>(
      ".verification-row > .button-row button",
    )][0];
    const inactiveRequestCount = requests.length;
    if (!inactiveRun.disabled) {
      throw new Error("inactive authorized check exposed Run");
    }
    inactiveRun.click();
    await settle();
    if (requests.length !== inactiveRequestCount) {
      throw new Error("inactive authorized Run submitted an operation");
    }
    unmount();

    const matchedState = {
      ...serviceState,
      trip_task_verification: [{
        ...serviceCheck,
        authorization: {
          ...serviceCheck.authorization,
          family: true,
          authorized: true,
          action_state: "current_receipt",
          inactive_reason:
            "A fresh successful receipt already satisfies this candidate and selection; approve again here only to rerun.",
          state: "approved_family",
          matching_rule: {
            id: "service-rule-1",
            revision: 3,
            display_family: "./gradlew with all arguments",
            source: "service_check",
          },
        },
      }],
    } as AppState;
    mount(
      <TaskDetail
        task={task}
        state={matchedState}
        onClose={() => {}}
        onChanged={() => changed++}
      />,
    );
    if (!document.body.textContent?.includes("Revoke service-check rule")) {
      throw new Error("task detail hid service-check revocation");
    }
    if (
      !document.body.textContent?.includes("Rerun approved check") ||
      !document.body.textContent?.includes("Approve once to rerun")
    ) throw new Error("task detail hid explicit service-check rerun controls");
    const matchedFamilyApproval = [
      ...document.querySelectorAll<HTMLButtonElement>(
        ".service-check-permission .approval-actions button",
      ),
    ].find((item) => item.textContent?.includes("Always approve"));
    if (!matchedFamilyApproval?.disabled) {
      throw new Error("covered family exposed redundant approval");
    }
    click("Revoke service-check rule");
    await settle();
    if (
      requests.at(-1)?.action !== "revoke_check_permission_rule" ||
      requests.at(-1)?.rule_id !== "service-rule-1" ||
      requests.at(-1)?.expected_revision !== 3
    ) throw new Error("service-check revocation identity drifted");
  } finally {
    unmount();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("T20 durable cmux projections fence stale View results and local control", async () => {
  localStorage.clear();
  const setupSessionId = "setup-durable-session";
  const runtimeSessionId = "runtime-durable-session";
  const setupState = (surface: CmuxSessionSurface): TripSetupState => ({
    setup_operation_id: "setup-durable-card",
    project_id: initialized.id,
    state: "discovery",
    discovery_attempt_id: "setup-durable-attempt",
    target_inventory: {},
    final_files: [],
    installation_source_binding_complete: false,
    installation_source_binding_reason: "discovery",
    selected_profiles: [],
    probe_receipts: [],
    sessions: [{
      id: setupSessionId,
      attempt_id: "setup-durable-attempt",
      role: "manager",
      provider: "codex",
      generation: 1,
      lane_id: "default",
      status: "running",
      launch_state: "started",
      readiness: "running",
      capture_state: "idle",
      has_native_session: true,
      resume_count: 0,
      updated_at: surface.updated_at,
      cmux_surface: surface,
    }],
    runtime_admissions: [],
    agents_file: {},
    created_at: "2026-09-20T00:00:00Z",
    updated_at: "2026-09-20T00:00:00Z",
  });
  const runtimeState = (surface: CmuxSessionSurface): TripSetupState => ({
    setup_operation_id: "runtime-durable-card",
    project_id: initialized.id,
    state: "activated",
    target_inventory: {},
    final_files: [],
    installation_source_binding_complete: true,
    installation_source_binding_reason: "activated",
    selected_profiles: [],
    probe_receipts: [],
    sessions: [],
    runtime_admissions: [{
      id: "runtime-durable-admission",
      scope_hash: "runtime-durable-scope",
      state: "running",
      fresh_call_count: 1,
      probes: [{
        role: "explorer",
        profile: { provider: "codex", model: "gpt-5.6-terra", effort: "max" },
        profile_hash: "runtime-durable-profile",
        project_config_revision_id: "runtime-durable-config",
        project_configuration_hash: "runtime-durable-configuration",
        adapter: "llmrelay_codex",
        adapter_hash: "runtime-durable-adapter",
        capability_key: "runtime-durable-capability",
        nonce: "not-rendered",
        state: "running",
        session_id: runtimeSessionId,
        session_status: "running",
        readiness: "running",
        has_native_session: true,
        cmux_surface: surface,
      }],
    }],
    agents_file: {},
    created_at: "2026-09-20T00:00:00Z",
    updated_at: "2026-09-20T00:00:00Z",
  });
  const durableSurface = (
    id: string,
    updatedAt: string,
    changes: Partial<CmuxSessionSurface>,
  ) =>
    persistentSurface(id, {
      desired_input_state: "control",
      actual_input_state: "control",
      control_revision: 4,
      applied_revision: 3,
      updated_at: updatedAt,
      ...changes,
    });
  const cardState = () =>
    document.querySelector(".cmux-route strong")?.textContent || "";
  const assertCardState = (expected: string) => {
    if (!cardState().includes(`cmux presentation ${expected}`)) {
      throw new Error(
        `durable cmux card rendered ${
          JSON.stringify(cardState())
        }, expected ${expected}`,
      );
    }
  };
  const assertPendingBlocksInput = () => {
    const control = [...document.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent?.includes("Take keyboard control"));
    if (!control?.disabled) {
      throw new Error(
        "pending durable revision allowed local keyboard control",
      );
    }
    if (
      !document.body.textContent?.includes(
        "prior durable actual state is not treated as new local input authority",
      )
    ) {
      throw new Error(
        "pending durable revision did not explain authority hold",
      );
    }
  };
  const setupCard = (
    surface: CmuxSessionSurface,
    onViewSession: (sessionId: string) => Promise<CmuxViewOutcome> =
      cmuxFixture,
  ) => (
    <ProjectSetup
      project={initialized}
      setup={setupState(surface)}
      onChanged={noop}
      onViewSession={onViewSession}
    />
  );
  const renderSetup = async (surface: CmuxSessionSurface) => {
    mount(setupCard(surface));
    await settle();
  };
  const runtimeCard = (
    surface: CmuxSessionSurface,
    onViewSession: (sessionId: string) => Promise<CmuxViewOutcome> =
      cmuxFixture,
  ) => (
    <ProjectSetup
      project={initialized}
      setup={runtimeState(surface)}
      onChanged={noop}
      onViewSession={onViewSession}
    />
  );
  const renderRuntime = async (surface: CmuxSessionSurface) => {
    mount(runtimeCard(surface));
    await settle();
  };
  const verifyLifecycle = async (
    render: (surface: CmuxSessionSurface) => Promise<void>,
    prefix: string,
  ) => {
    const pendingControl = durableSurface(
      `00000000-0000-0000-0000-0000000000${prefix}1`,
      "2026-09-20T00:00:01Z",
      {},
    );
    await render(pendingControl);
    assertCardState("pending");
    assertPendingBlocksInput();

    await render({
      ...pendingControl,
      applied_revision: 4,
      updated_at: "2026-09-20T00:00:02Z",
    });
    assertCardState("control");
    if (!document.body.textContent?.includes("Release keyboard control")) {
      throw new Error(
        "current durable control state did not reload as releasable",
      );
    }

    const pendingBlocked = durableSurface(
      pendingControl.id,
      "2026-09-20T00:00:03Z",
      { control_revision: 5, applied_revision: 4 },
    );
    await render(pendingBlocked);
    assertCardState("pending");
    assertPendingBlocksInput();
    await render({
      ...pendingBlocked,
      actual_input_state: "blocked",
      applied_revision: 5,
      updated_at: "2026-09-20T00:00:04Z",
    });
    assertCardState("blocked");

    const pendingRelease = durableSurface(
      pendingControl.id,
      "2026-09-20T00:00:05Z",
      {
        desired_input_state: "view_only",
        actual_input_state: "control",
        control_revision: 6,
        applied_revision: 5,
      },
    );
    await render(pendingRelease);
    assertCardState("pending");
    assertPendingBlocksInput();
    await render({
      ...pendingRelease,
      actual_input_state: "view_only",
      applied_revision: 6,
      updated_at: "2026-09-20T00:00:06Z",
    });
    assertCardState("view only");
    if (document.body.textContent?.includes("Release keyboard control")) {
      throw new Error("view-only durable release still exposed local control");
    }

    const lostLive = durableSurface(
      pendingControl.id,
      "2026-09-20T00:00:07Z",
      {
        surface_state: "lost",
        attachment_state: "live",
        desired_input_state: "view_only",
        actual_input_state: "lost",
        control_revision: 6,
        applied_revision: 6,
        last_error: "fixture validated cmux loss observation",
      },
    );
    await render(lostLive);
    assertCardState("lost");
    const liveRetirementActions = ["View output", "Take keyboard control"].map(
      (label) =>
        [...document.querySelectorAll<HTMLButtonElement>("button")]
          .find((button) => button.textContent?.includes(label)),
    );
    if (
      liveRetirementActions.some((button) => !button?.disabled) ||
      document.body.textContent?.includes("View fresh view-only surface") ||
      !document.body.textContent?.includes(
        "fixture validated cmux loss observation",
      ) ||
      !document.body.textContent?.includes("durable retirement interval")
    ) {
      throw new Error(
        "live loss retirement exposed replacement or hid its causal evidence",
      );
    }

    await render({
      ...lostLive,
      attachment_state: "ended",
      updated_at: "2026-09-20T00:00:08Z",
    });
    assertCardState("lost");
    const freshView = [
      ...document.querySelectorAll<HTMLButtonElement>("button"),
    ]
      .find((button) =>
        button.textContent?.includes("View fresh view-only surface")
      );
    const retiredTake = [
      ...document.querySelectorAll<HTMLButtonElement>("button"),
    ]
      .find((button) => button.textContent?.includes("Take keyboard control"));
    if (
      !freshView || freshView.disabled || !retiredTake?.disabled ||
      !document.body.textContent?.includes(
        "prior terminal is durably historical",
      )
    ) {
      throw new Error(
        "durably retired loss did not expose only fresh view-only access",
      );
    }
  };
  const verifyStaleView = async (
    card: (
      surface: CmuxSessionSurface,
      onView: (sessionId: string) => Promise<CmuxViewOutcome>,
    ) => ReactNode,
    prefix: string,
    assertLost = () => assertCardState("lost"),
  ) => {
    const open = durableSurface(
      `00000000-0000-0000-0000-0000000000${prefix}9`,
      "2026-09-20T00:00:09Z",
      {
        desired_input_state: "view_only",
        actual_input_state: "view_only",
        applied_revision: 4,
      },
    );
    const lostLive = {
      ...open,
      updated_at: "2026-09-20T00:00:10Z",
      surface_state: "lost" as const,
      desired_input_state: "view_only" as const,
      actual_input_state: "lost" as const,
      last_error: "fixture validated cmux loss observation",
    };
    await verifyDeferredCmuxView(card, open, lostLive, assertLost);
    await verifyDeferredCmuxView(
      card,
      open,
      lostLive,
      assertLost,
      { ...open, updated_at: lostLive.updated_at },
    );
  };
  try {
    await verifyLifecycle(renderSetup, "5");
    await verifyLifecycle(renderRuntime, "6");
    await verifyStaleView(setupCard, "7");
    await verifyStaleView(runtimeCard, "8");
  } finally {
    unmount();
  }
});

Deno.test("T21 ambiguous mutation transport failures preserve durable operation identity for reconciliation", async () => {
  const priorTimeout = transportTimeouts.mutation;
  const request = { kind: "retry", operation_id: "timeout-operation-id" };
  const operationIds: string[] = [];
  transportTimeouts.mutation = 5;
  globalThis.fetch =
    ((_input: string | URL | Request, init?: RequestInit) =>
      new Promise<Response>((_resolve, reject) => {
        operationIds.push(
          (JSON.parse(String(init?.body)) as { operation_id: string })
            .operation_id,
        );
        init?.signal?.addEventListener(
          "abort",
          () => reject(new Error("aborted")),
          {
            once: true,
          },
        );
      })) as typeof fetch;
  try {
    let failure: unknown;
    try {
      await command(request);
    } catch (error) {
      failure = error;
    }
    if (
      !(failure instanceof ApiError) || !failure.ambiguous ||
      failure.operationId !== request.operation_id
    ) {
      throw new Error(
        "timed-out mutation did not retain its operation identity",
      );
    }

    globalThis.fetch =
      (async (_input: string | URL | Request, init?: RequestInit) => {
        operationIds.push(
          (JSON.parse(String(init?.body)) as { operation_id: string })
            .operation_id,
        );
        return new Response(
          JSON.stringify({ result: { state: "reconciled" } }),
        );
      }) as typeof fetch;
    await command(request);
    if (
      operationIds.join(",") !== "timeout-operation-id,timeout-operation-id"
    ) {
      throw new Error(
        "reconciliation did not reuse the timed-out operation identity",
      );
    }

    const lostFetch = {
      kind: "retry",
      operation_id: "lost-fetch-operation-id",
    };
    globalThis.fetch =
      (async (_input: string | URL | Request, init?: RequestInit) => {
        operationIds.push(
          (JSON.parse(String(init?.body)) as { operation_id: string })
            .operation_id,
        );
        throw new TypeError("connection reset after mutation dispatch");
      }) as typeof fetch;
    let fetchFailure: unknown;
    try {
      await command(lostFetch);
    } catch (error) {
      fetchFailure = error;
    }
    if (
      !(fetchFailure instanceof ApiError) || !fetchFailure.ambiguous ||
      fetchFailure.operationId !== lostFetch.operation_id
    ) {
      throw new Error(
        "lost mutation fetch did not retain its operation identity",
      );
    }

    const lostResponse = {
      kind: "retry",
      operation_id: "lost-response-operation-id",
    };
    globalThis.fetch =
      (async (_input: string | URL | Request, init?: RequestInit) => {
        operationIds.push(
          (JSON.parse(String(init?.body)) as { operation_id: string })
            .operation_id,
        );
        return {
          ok: true,
          status: 200,
          statusText: "OK",
          text: async () => {
            throw new TypeError("response stream ended after server commit");
          },
        } as unknown as Response;
      }) as typeof fetch;
    let lostFailure: unknown;
    try {
      await command(lostResponse);
    } catch (error) {
      lostFailure = error;
    }
    if (
      !(lostFailure instanceof ApiError) || !lostFailure.ambiguous ||
      lostFailure.operationId !== lostResponse.operation_id
    ) {
      throw new Error(
        "lost mutation response did not retain its operation identity",
      );
    }

    globalThis.fetch =
      (async (_input: string | URL | Request, init?: RequestInit) => {
        operationIds.push(
          (JSON.parse(String(init?.body)) as { operation_id: string })
            .operation_id,
        );
        return new Response(
          JSON.stringify({ result: { state: "reconciled" } }),
        );
      }) as typeof fetch;
    await command(lostResponse);
    if (
      operationIds.join(",") !==
        "timeout-operation-id,timeout-operation-id,lost-fetch-operation-id,lost-response-operation-id,lost-response-operation-id"
    ) {
      throw new Error(
        "reconciliation did not reuse the lost-response operation identity",
      );
    }

    let genericRecoveryRefreshes = 0;
    mount(
      <RecoveryPanel
        task={task}
        records={[{
          id: "settled-graceful-stop",
          attempt_id: "a1",
          state: "resolved_graceful_stop_quiescent",
          detail: {
            kind: "graceful_stop_deadline",
          },
        }]}
        onChanged={() => {
          genericRecoveryRefreshes++;
        }}
      />,
    );
    if (document.body.textContent?.includes("Recovery decision")) {
      throw new Error(
        "settled exact recovery was still rendered as actionable",
      );
    }
    unmount();
    mount(
      <RecoveryPanel
        task={task}
        records={[{
          id: "historical-control-failure",
          attempt_id: "a1",
          state: "attention_required",
          detail: {
            kind: "control_failure",
            control_id: "rejected-control",
          },
        }]}
        onChanged={() => {
          genericRecoveryRefreshes++;
        }}
      />,
    );
    if (
      document.body.textContent?.includes("Verify quiescence and reconcile") ||
      document.body.textContent?.includes("Verify and cancel")
    ) {
      throw new Error(
        "recordless historical control failure exposed guaranteed-reject recovery commands",
      );
    }
    click("Refresh and review corrected control");
    if (genericRecoveryRefreshes !== 1) {
      throw new Error(
        "recordless historical control failure became actionless",
      );
    }
    unmount();
  } finally {
    transportTimeouts.mutation = priorTimeout;
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("M8A protocol refusal is definitive before mutation reconciliation", async () => {
  const priorFetch = globalThis.fetch;
  let businessCalls = 0;
  let expectedGeneration = 2;
  let observedGeneration: number | null = 1;
  globalThis.fetch = (async (_input: string | URL | Request, init?: RequestInit) => {
    businessCalls++;
    const declaration = JSON.parse(String(new Headers(init?.headers).get("x-llmrelay-protocol")));
    if (declaration.generation !== 1 || declaration.client_kind !== "browser" ||
      declaration.required_features[0] !== "http_operational_v1") {
      throw new Error("business request omitted the protocol declaration");
    }
    return new Response(JSON.stringify({
      error: "protocol mismatch",
      protocol_error: {
        reason: "incompatible_generation",
        observed_generation: observedGeneration,
        expected_generation: expectedGeneration,
        guidance: "Reload the dashboard for the matching protocol.",
      },
    }), { status: 409 });
  }) as typeof fetch;
  try {
    let failure: unknown;
    try {
      await command({ kind: "retry", operation_id: "protocol-refused" });
    } catch (error) {
      failure = error;
    }
    if (!(failure instanceof ProtocolError) || failure instanceof ApiError ||
      failure.message !== "Reload the dashboard for the matching protocol." ||
      businessCalls !== 1) {
      throw new Error("protocol refusal became an ambiguous or replayed mutation");
    }
    for (const [expected, observed] of [[-1, 1], [0x1_0000_0000, 1], [2, -1], [2, 0x1_0000_0000]]) {
      expectedGeneration = expected;
      observedGeneration = observed;
      let invalidFailure: unknown;
      try {
        await command({ kind: "retry", operation_id: "invalid-protocol-wire" });
      } catch (error) {
        invalidFailure = error;
      }
      if (!(invalidFailure instanceof ApiError) || invalidFailure.status !== 409) {
        throw new Error("out-of-range protocol generation was accepted as a structured refusal");
      }
    }
    const requestCount = () => businessCalls;
    if (requestCount() !== 5) {
      throw new Error("protocol refusals triggered an automatic replay");
    }
  } finally {
    globalThis.fetch = priorFetch;
  }
});

Deno.test("M5 decision policy keeps the permitted control and explains the blocked one", async () => {
  mount(
    <WorkflowControls
      task={task}
      onChanged={() => {}}
      decision={{
        decision_schema: 1,
        reason_code: "workflow.waiting_for_exact_review",
        disposition: "waiting",
        subject: { task_id: task.id },
        observed_revision: { task_version: task.version },
        primary_blocker: {
          code: "review",
          state: "unknown",
          owner: "human",
          evidence: null,
          message: "Review evidence is not current",
        },
        prerequisites: [],
        ownership: { owner: "human", state: "awaiting_review", binding: {} },
        next_action: null,
        control_policy: { allowed_controls: ["pause_now"] },
      }}
    />,
  );
  const retry = [...document.querySelectorAll<HTMLButtonElement>("button")]
    .find((button) => button.textContent?.trim() === "Retry");
  const pause = [...document.querySelectorAll<HTMLButtonElement>("button")]
    .find((button) => button.textContent?.trim() === "Pause now");
  if (
    !retry?.disabled || pause?.disabled ||
    !document.body.textContent?.includes("Review evidence is not current")
  ) {
    throw new Error("backend policy did not disable only the refused control");
  }
  unmount();
  const requests: Record<string, unknown>[] = [];
  const priorFetch = globalThis.fetch;
  globalThis.fetch =
    (async (_input: string | URL | Request, init?: RequestInit) => {
      requests.push(JSON.parse(String(init?.body)));
      return new Response(JSON.stringify({ result: { state: "ready" } }));
    }) as typeof fetch;
  try {
    const backlogTask = {
      ...task,
      lifecycle: "backlog",
      active_attempt: undefined,
    };
    const readiness: DecisionExplanation = {
      decision_schema: 1,
      reason_code: "task.backlog_readiness",
      disposition: "waiting",
      subject: { task_id: task.id },
      observed_revision: { task_version: task.version },
      primary_blocker: null,
      prerequisites: [],
      ownership: {
        owner: "human",
        state: "recorded_task_status",
        binding: { task_id: task.id, expected_task_version: task.version },
      },
      next_action: {
        operation: "make_ready",
        enabled: true,
        owner: "human",
        binding: { task_id: task.id, expected_task_version: task.version },
      },
      control_policy: { allowed_controls: ["make_ready"] },
    };
    mount(
      <WorkflowControls
        task={backlogTask}
        project={initialized}
        decision={readiness}
        onChanged={() => {}}
      />,
    );
    const ready = [...document.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent?.trim() === "Make Ready");
    if (!ready || ready.disabled) {
      throw new Error("backend permitted Backlog action was disabled");
    }
    click("Make Ready");
    await settle();
    if (
      requests.at(-1)?.kind !== "make_ready" ||
      requests.at(-1)?.task_id !== task.id ||
      requests.at(-1)?.expected_version !== task.version
    ) {
      throw new Error("Backlog action did not send exact task and version");
    }
    unmount();
    mount(
      <WorkflowControls
        task={backlogTask}
        project={initialized}
        decision={{
          ...readiness,
          primary_blocker: {
            code: "task.six_role_settings",
            state: "missing",
            owner: "human",
            evidence: null,
            message: "Configure six app roles",
          },
          control_policy: {
            allowed_controls: [],
            disabled_reason_code: "task.six_role_settings",
          },
        }}
        onChanged={() => {}}
      />,
    );
    const blocked = [...document.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent?.trim() === "Make Ready");
    if (!blocked?.disabled || blocked.title !== "Configure six app roles") {
      throw new Error("Backlog missing-prerequisite policy was not shown");
    }
    unmount();
    const readyTask: Task = {
      ...task,
      lifecycle: "ready",
      active_attempt: undefined,
    };
    mount(
      <WorkflowControls
        task={readyTask}
        decision={{
          ...readiness,
          reason_code: "scheduler.queue_paused",
          control_policy: { allowed_controls: ["run_next"] },
        }}
        onChanged={() => {}}
      />,
    );
    const runNext = [...document.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent?.trim() === "Run next");
    if (!runNext || runNext.disabled) {
      throw new Error("Ready Run next policy was disabled");
    }
    click("Run next");
    await settle();
    if (
      requests.at(-1)?.kind !== "control" ||
      requests.at(-1)?.action !== "run_next" ||
      requests.at(-1)?.task_id !== task.id ||
      requests.at(-1)?.expected_version !== task.version
    ) {
      throw new Error("Ready Run next lost the exact task and version");
    }
    unmount();
    mount(
      <WorkflowControls
        task={task}
        decision={{
          ...readiness,
          reason_code: "workflow.awaiting_human_plan_approval",
          control_policy: {
            allowed_controls: ["pause_after_role", "pause_now", "cancel"],
          },
        }}
        onChanged={() => {}}
      />,
    );
    click("Pause now");
    await settle();
    if (
      requests.at(-1)?.kind !== "control" ||
      requests.at(-1)?.action !== "pause_now" ||
      requests.at(-1)?.task_id !== task.id ||
      requests.at(-1)?.expected_version !== task.version
    ) {
      throw new Error("active attempt pause lost the exact task and version");
    }
    unmount();
  } finally {
    globalThis.fetch = priorFetch;
  }
});

Deno.test("T22 restore claim and freeze records submit generic recovery without a session", async () => {
  const requests: Record<string, unknown>[] = [];
  let refreshes = 0;
  globalThis.fetch =
    (async (_input: string | URL | Request, init?: RequestInit) => {
      requests.push(JSON.parse(String(init?.body)));
      return new Response(
        JSON.stringify({ result: { state: "resolved_quiescent" } }),
      );
    }) as typeof fetch;
  try {
    for (
      const [kind, explanation] of [
        ["database_restore_claim", "prelaunch claim reservation"],
        ["database_restore_freeze", "interrupted local freeze"],
      ]
    ) {
      mount(
        <RecoveryPanel
          task={task}
          records={[
            {
              id: `historical-before-${kind}`,
              attempt_id: "a1",
              state: "attention_required",
              detail: { kind: "control_failure" },
            },
            {
              id: `restore-${kind}`,
              attempt_id: "a1",
              state: "attention_required",
              detail: {
                kind,
                operation_id: "restore-operation",
              },
            },
          ]}
          onChanged={() => {
            refreshes++;
          }}
        />,
      );
      if (
        !document.body.textContent?.includes(explanation) ||
        !document.body.textContent.includes(
          "Verify quiescence and reconcile",
        ) || document.body.textContent.includes("Verify and cancel")
      ) {
        throw new Error(`${kind} did not expose its generic recovery action`);
      }
      field("Recovery evidence", `operator annotation for ${kind}`);
      click("Verify quiescence and reconcile");
      await settle();
      const request = requests.at(-1);
      if (
        !request ||
        request.kind !== "resolve_recovery" ||
        request.task_id !== task.id ||
        request.attempt_id !== task.active_attempt?.id ||
        request.recovery_id !== `restore-${kind}` ||
        request.expected_version !== task.version ||
        request.decision !== "confirm_quiescent" ||
        request.evidence !== `operator annotation for ${kind}` ||
        "session_id" in request
      ) {
        throw new Error(
          `${kind} did not submit generic attempt recovery without a session`,
        );
      }
    }
    if (refreshes !== 2 || requests.length !== 2) {
      throw new Error("restore recovery actions did not refresh exactly once");
    }
  } finally {
    unmount();
    globalThis.fetch = nativeFetch;
  }
});

const check = (condition: unknown, message: string) => {
  if (!condition) throw new Error(message);
};
const liveBase: AppState = {
  schema: 8,
  generated_at: "2026-09-23T00:00:00Z",
  incarnation: "service-a",
  revision: "1",
  projects: [initialized],
  tasks: [task],
  production_role_restrictions: [],
  capabilities: [],
  active_sessions: [],
  controls: [],
  guidance: [],
  check_suites: [],
  checks: [],
  switches: [],
  recovery: [],
  history: [],
      profile_sets: [],
      task_recipes: [],
      recipe_schedules: [],
  instance_settings: {
    version: 1,
    auto_resume_eligible: false,
    updated_at: "",
  },
  restart_candidates: [],
  permission_requests: [],
  permission_rules: [],
  continuation_actions: [],
  decisions: [],
  attention: [],
  resources: {
    active_sessions: 0,
    active_controls: 0,
    queued_guidance: 0,
    running_checks: 0,
    observed_at: "",
    processes: [],
  },
};
const snapshotAt = (
  incarnation: string,
  revision: string,
  overrides: Partial<AppState> = {},
): AppState => ({ ...liveBase, ...overrides, incarnation, revision });
const changedTo = (state: AppState): StateWaitResult => ({
  outcome: "state_changed",
  incarnation: state.incarnation,
  revision: state.revision,
  state,
});

Deno.test("M8A concurrent negotiation and incarnation reset fence descriptor cache", async () => {
  const priorFetch = globalThis.fetch;
  let incarnation = "m8a-reset-a";
  let businessCalls = 0;
  globalThis.fetch = (async (input: string | URL | Request) => {
    if (String(input) === "/api/state") {
      return new Response(JSON.stringify(snapshotAt(incarnation, "1")));
    }
    businessCalls++;
    return new Response(JSON.stringify({ result: { state: "applied" } }));
  }) as typeof fetch;
  try {
    await getState();
    let resolveFirst: ((response: Response) => void) | undefined;
    let descriptorCalls = 0;
    protocolFixtureResponder = () => {
      descriptorCalls++;
      return new Promise<Response>((resolve) => { resolveFirst = resolve; });
    };
    const first = command({ kind: "retry", operation_id: "m8a-first" });
    const second = command({ kind: "retry", operation_id: "m8a-second" });
    await Promise.resolve();
    check(descriptorCalls === 1 && businessCalls === 0, "concurrent operations did not share protocol discovery");
    resolveFirst!(new Response(JSON.stringify({
      generation: 1, server_version: "fixture", instance_id: incarnation,
      supported_features: ["http_operational_v1"],
    })));
    await Promise.all([first, second]);
    check(businessCalls === 2, "negotiated operations did not dispatch once each");
    incarnation = "m8a-reset-b";
    await getState();
    protocolFixtureResponder = () => {
      descriptorCalls++;
      return Promise.resolve(new Response(JSON.stringify({
        generation: 1, server_version: "fixture", instance_id: incarnation,
        supported_features: ["http_operational_v1"],
      })));
    };
    await command({ kind: "retry", operation_id: "m8a-third" });
    check(descriptorCalls === 2 && businessCalls === 3,
      "incarnation reset kept a stale descriptor or replayed a mutation");
  } finally {
    protocolFixtureResponder = () => Promise.resolve(protocolFixture());
    globalThis.fetch = priorFetch;
  }
});

Deno.test("M8A a newer observed incarnation fences unresolved descriptor discovery", async () => {
  const priorFetch = globalThis.fetch;
  let stateCalls = 0;
  let businessCalls = 0;
  let resolveNewState: ((response: Response) => void) | undefined;
  let resolveOldDescriptor: ((response: Response) => void) | undefined;
  const descriptor = (instanceId: string) => new Response(JSON.stringify({
    generation: 1, server_version: "fixture", instance_id: instanceId,
    supported_features: ["http_operational_v1"],
  }));
  globalThis.fetch = (async (input: string | URL | Request) => {
    if (String(input) === "/api/state") {
      stateCalls++;
      if (stateCalls === 2) {
        return new Promise<Response>((resolve) => { resolveNewState = resolve; });
      }
      return new Response(JSON.stringify(snapshotAt(
        stateCalls === 1 ? "m8a-order-a" : "m8a-order-b", String(stateCalls),
      )));
    }
    businessCalls++;
    return new Response(JSON.stringify({ result: { state: "applied" } }));
  }) as typeof fetch;
  try {
    await getState();
    protocolFixtureResponder = () => Promise.resolve(descriptor("m8a-order-a"));
    await command({ kind: "retry", operation_id: "m8a-order-seed" });
    const newState = getState();
    await Promise.resolve();
    check(!!resolveNewState, "newer state read was not dispatched before renegotiation");
    await getState();
    let descriptorCalls = 0;
    protocolFixtureResponder = () => {
      descriptorCalls++;
      return descriptorCalls === 1
        ? new Promise<Response>((resolve) => { resolveOldDescriptor = resolve; })
        : Promise.resolve(descriptor("m8a-order-c"));
    };
    const mutation = command({ kind: "retry", operation_id: "m8a-order-mutation" });
    await Promise.resolve();
    check(descriptorCalls === 1 && !!resolveOldDescriptor && businessCalls === 1,
      "older descriptor discovery did not remain pending before newer state");
    resolveNewState!(new Response(JSON.stringify(snapshotAt("m8a-order-c", "4"))));
    await newState;
    resolveOldDescriptor!(descriptor("m8a-order-b"));
    await mutation;
    check(descriptorCalls === 2 && businessCalls === 2,
      "late older descriptor was cached or mutation replayed");
    await command({ kind: "retry", operation_id: "m8a-order-next" });
    check(descriptorCalls === 2 && businessCalls === 3,
      "current descriptor was not cached after fenced discovery");
  } finally {
    protocolFixtureResponder = () => Promise.resolve(protocolFixture());
    globalThis.fetch = priorFetch;
  }
});

Deno.test("M8A an older state observation allows bounded replacement discovery", async () => {
  const priorFetch = globalThis.fetch;
  let descriptorCalls = 0;
  let businessCalls = 0;
  globalThis.fetch = (async (input: string | URL | Request) => {
    if (String(input) === "/api/state") {
      return new Response(JSON.stringify(snapshotAt("m8a-replaced-a", "1")));
    }
    businessCalls++;
    return new Response(JSON.stringify({ result: { state: "applied" } }));
  }) as typeof fetch;
  protocolFixtureResponder = () => {
    descriptorCalls++;
    return Promise.resolve(new Response(JSON.stringify({
      generation: 1, server_version: "fixture", instance_id: "m8a-replaced-b",
      supported_features: ["http_operational_v1"],
    })));
  };
  try {
    await getState();
    descriptorCalls = 0;
    await command({ kind: "retry", operation_id: "m8a-replaced" });
    check(descriptorCalls === 2 && businessCalls === 1,
      "service replacement retried without a bound or dispatched a duplicate mutation");
  } finally {
    protocolFixtureResponder = () => Promise.resolve(protocolFixture());
    globalThis.fetch = priorFetch;
  }
});

interface PendingRequest<T> {
  resolve: (value: T) => void;
  reject: (cause: unknown) => void;
  signal: AbortSignal;
  at: number;
}
/** Scripted transport and manual clock for the live-state lifecycle. */
function liveHarness() {
  let now = 0;
  let nextHandle = 1;
  let replyToRead: ((read: PendingRequest<AppState>) => void) | undefined;
  let replyToWait:
    | ((wait: PendingRequest<StateWaitResult>) => void)
    | undefined;
  const timers = new Map<number, { due: number; callback: () => void }>();
  const reads: PendingRequest<AppState>[] = [];
  const waits: Array<
    PendingRequest<StateWaitResult> & { cursor: StateCursor }
  > = [];
  const pending = <T,>(signal: AbortSignal) => {
    const entry: PendingRequest<T> = {
      resolve: () => {},
      reject: () => {},
      signal,
      at: now,
    };
    const promise = new Promise<T>((resolve, reject) => {
      entry.resolve = resolve;
      entry.reject = reject;
      signal.addEventListener(
        "abort",
        () => reject(new DOMException("aborted", "AbortError")),
        { once: true },
      );
    });
    return { entry, promise };
  };
  const environment: LiveEnvironment = {
    transport: {
      read: (signal) => {
        const { entry, promise } = pending<AppState>(signal);
        reads.push(entry);
        replyToRead?.(entry);
        return promise;
      },
      wait: (cursor, _timing, signal) => {
        const { entry, promise } = pending<StateWaitResult>(signal);
        waits.push({ ...entry, cursor });
        replyToWait?.(entry);
        return promise;
      },
    },
    scheduler: {
      setTimeout: (callback, delayMs) => {
        const handle = nextHandle++;
        timers.set(handle, { due: now + delayMs, callback });
        return handle;
      },
      clearTimeout: (handle) => {
        timers.delete(handle);
      },
    },
  };
  return {
    environment,
    reads,
    waits,
    /** Settles each later request immediately, or queues it when undefined. */
    replyToReads: (reply: typeof replyToRead) => {
      replyToRead = reply;
    },
    replyToWaits: (reply: typeof replyToWait) => {
      replyToWait = reply;
    },
    now: () => now,
    timerCount: () => timers.size,
    async advance(milliseconds: number) {
      const until = now + milliseconds;
      for (;;) {
        // Pending continuations must schedule their timers at the current time.
        await settle();
        const [due] = [...timers].filter(([, timer]) => timer.due <= until)
          .sort(([, left], [, right]) => left.due - right.due);
        if (!due) break;
        timers.delete(due[0]);
        now = due[1].due;
        due[1].callback();
      }
      now = until;
    },
  };
}

Deno.test("M6 live state keeps only newer snapshots, queues post-mutation reads, and fences resets", async () => {
  const live = liveHarness();
  const states: AppState[] = [];
  const controller = startLiveState({
    onState: (state) => states.push(state),
    onStatus: () => {},
  }, live.environment);
  const shown = () =>
    `${states.at(-1)?.incarnation}/${states.at(-1)?.revision}`;
  try {
    live.reads[0].resolve(snapshotAt("service-a", "9"));
    await settle();
    check(
      live.waits.length === 1 && live.waits[0].cursor.revision === "9",
      "the long poll did not start from the accepted cursor",
    );
    void controller.refresh();
    let reconciled = false;
    const afterMutation = controller.refresh().then(() => {
      reconciled = true;
    });
    void controller.refresh();
    check(
      live.reads.length === 2,
      "invalidations overlapped the in-flight read",
    );
    live.waits[0].resolve(changedTo(snapshotAt("service-a", "10")));
    await settle();
    live.reads[1].resolve(snapshotAt("service-a", "9"));
    await settle();
    check(
      shown() === "service-a/10",
      "a delayed older read replaced a newer snapshot",
    );
    check(
      live.reads.length === 3 && !reconciled,
      "the post-mutation refresh reused the read that started before it",
    );
    const rendered = states.length;
    live.waits[1].resolve(changedTo(snapshotAt("service-a", "10")));
    await settle();
    check(
      states.length === rendered,
      "a duplicated wake re-rendered its revision",
    );
    live.reads[2].resolve(snapshotAt("service-a", "10", {
      resources: { ...liveBase.resources, observed_at: "volatile" },
    }));
    await afterMutation;
    check(
      reconciled && states.at(-1)?.resources.observed_at === "volatile",
      "a same-revision read did not refresh volatile observations",
    );
    live.waits[2].resolve(
      changedTo(snapshotAt("service-a", "9007199254740993")),
    );
    await settle();
    void controller.refresh();
    live.reads[3].resolve(snapshotAt("service-a", "9007199254740992"));
    await settle();
    check(
      shown() === "service-a/9007199254740993",
      "revision order lost precision beyond Number.MAX_SAFE_INTEGER",
    );
    void controller.refresh();
    live.waits[3].resolve({
      outcome: "reset",
      incarnation: "service-b",
      revision: "2",
      state: snapshotAt("service-b", "2"),
    });
    await settle();
    live.reads[4].resolve(snapshotAt("service-a", "9007199254740999"));
    await settle();
    check(
      shown() === "service-b/2",
      "a response issued before the reset restored the retired incarnation",
    );
    check(
      live.waits.at(-1)?.cursor.incarnation === "service-b" &&
        live.waits.at(-1)?.cursor.revision === "2",
      "the long poll did not re-arm on the reset cursor",
    );
    void controller.refresh();
    live.reads[5].resolve(snapshotAt("service-a", "9223372036854775807"));
    await settle();
    check(
      shown() === "service-b/2",
      "a later answer from the retired incarnation flipped the dashboard back",
    );
    void controller.refresh();
    live.reads[6].resolve(snapshotAt("service-b", "3"));
    await settle();
    check(shown() === "service-b/3", "the retired incarnation was not fenced");
    void controller.refresh();
    live.waits[live.waits.length - 1].resolve({
      outcome: "reset",
      incarnation: "service-b",
      revision: "1",
      state: snapshotAt("service-b", "1"),
    });
    await settle();
    live.reads[7].resolve(snapshotAt("service-b", "3"));
    await settle();
    check(
      shown() === "service-b/1",
      "a read issued before a same-incarnation reset was applied",
    );
    const accepted = states.length;
    const parked = live.waits[live.waits.length - 1];
    controller.dispose();
    await controller.refresh();
    await settle();
    check(
      parked.signal.aborted && live.timerCount() === 0 &&
        live.reads.length === 8 && states.length === accepted,
      "dispose left a request, timer, or listener active",
    );
  } finally {
    controller.dispose();
  }
});

Deno.test("M6 live state backs off failures, keeps an independent watchdog, and converges without a wake", async () => {
  const live = liveHarness();
  const states: AppState[] = [];
  const statuses: LiveStatus[] = [];
  const configuredWatchdog = liveTiming.watchdogMs;
  liveTiming.watchdogMs = 300_001;
  let unboundedRejected = false;
  try {
    startLiveState({ onState: noop, onStatus: noop }, live.environment)
      .dispose();
  } catch (cause) {
    unboundedRejected = cause instanceof RangeError;
  } finally {
    liveTiming.watchdogMs = configuredWatchdog;
  }
  check(
    unboundedRejected && live.reads.length === 0,
    "an out-of-range watchdog interval was accepted",
  );
  live.replyToReads((read) =>
    read.reject(new ApiError("browser session required", 401))
  );
  const controller = startLiveState({
    onState: (state) => states.push(state),
    onStatus: (status) => statuses.push(status),
  }, live.environment);
  const latestStatus = () => statuses.at(-1);
  try {
    await live.advance(60_000);
    const readTimes = live.reads.map((read) => read.at).join(",");
    check(
      readTimes === "0,1000,3000,7000,15000,30000,31000,60000",
      `authentication failures were not bounded backoff plus watchdog: ${readTimes}`,
    );
    check(
      live.waits.length === 0 && !latestStatus()?.online &&
        latestStatus()?.error === "browser session required",
      "an unauthenticated dashboard waited or hid the failure",
    );
    live.replyToReads(undefined);
    await live.advance(1_000);
    live.reads[8].resolve(snapshotAt("service-a", "5"));
    await settle();
    check(
      latestStatus()?.online && live.waits.length === 1,
      "recovery did not resume the long poll",
    );
    live.waits[0].resolve({
      outcome: "unchanged",
      incarnation: "service-a",
      revision: "5",
    });
    await settle();
    check(
      live.waits.length === 2 && live.waits[1].cursor.revision === "5" &&
        states.length === 1,
      "a timed-out wait re-rendered or did not re-arm",
    );
    for (const revision of ["6", "7", "8"]) {
      live.waits[live.waits.length - 1].resolve(
        changedTo(snapshotAt("service-a", revision)),
      );
      await settle();
    }
    const beforeWatchdog = live.reads.length;
    await live.advance(90_000 - live.now());
    check(
      live.reads.length === beforeWatchdog + 1,
      "continuous wakes starved the watchdog",
    );
    live.reads[live.reads.length - 1].resolve(snapshotAt("service-a", "9"));
    await settle();
    check(
      states.at(-1)?.revision === "9",
      "the watchdog did not converge on an external change that sent no wake",
    );
    const offline = statuses.filter((status) => !status.online).length;
    const waits = live.waits.length;
    live.waits[waits - 1].reject(
      new ApiError("too many dashboard state waits are active", 503),
    );
    await settle();
    await live.advance(1_000);
    check(
      live.waits.length === waits + 1 &&
        live.reads.length === beforeWatchdog + 1 &&
        statuses.filter((status) => !status.online).length === offline,
      "waiter back-pressure flashed offline or triggered a read storm",
    );
    live.waits[live.waits.length - 1].reject(new TypeError("Failed to fetch"));
    await settle();
    check(
      !latestStatus()?.online && latestStatus()?.error === "Failed to fetch",
      "a lost connection was not reported",
    );
    await live.advance(2_000);
    live.reads[live.reads.length - 1].resolve(snapshotAt("service-a", "9"));
    await settle();
    check(
      latestStatus()?.online && live.waits.at(-1)?.cursor.revision === "9" &&
        live.waits.length === waits + 2,
      "reconnect did not read authoritatively before re-arming the wait",
    );
    const requests = live.reads.length + live.waits.length;
    live.replyToReads((read) => read.resolve(snapshotAt("service-a", "9")));
    live.replyToWaits((wait) => wait.reject(new ApiError("Not Found", 404)));
    live.waits[live.waits.length - 1].reject(new ApiError("Not Found", 404));
    await live.advance(60_000);
    const issued = live.reads.length + live.waits.length - requests;
    check(
      issued <= 12,
      `a service that reads but cannot wait caused a request storm: ${issued} in 60s`,
    );
  } finally {
    controller.dispose();
  }
});

const secondProject: Project = {
  ...project,
  id: "p2",
  display_name: "Second",
  trip: {
    readiness: "needs_upgrade_review",
    reason: "Upgrade review required",
    detected_installation: "compatible",
    setup_operation_id: null,
  },
};
const managerSession: Session = {
  id: "s1",
  role_generation_id: "g1",
  provider: "codex",
  status: "running",
  readiness: "idle",
  capture_state: "capturing",
  updated_at: "",
  task_id: "AJ-1",
  attempt_id: "a1",
  role: "manager",
  generation: 1,
  config_revision: 1,
  launch: {
    model: "gpt-5.6-sol",
    effort: "high",
    permission_policy: "read-only",
    security_policy: {},
  },
};
const pendingPermission: PermissionRequest = {
  id: "pr1",
  project_id: "p1",
  task_id: "AJ-1",
  attempt_id: "a1",
  session_id: "s1",
  role_generation_id: "g1",
  role: "manager",
  provider: "codex",
  native_session_id: "native-1",
  tool_name: "shell",
  input: {},
  requested_access: null,
  created_at: "2026-09-23T00:00:00Z",
  deadline_at: "2999-01-01T00:00:00Z",
  state: "pending",
  revision: 3,
  delivery_state: "not_reserved",
};
const railItem = (
  id: string,
  category: AttentionItem["category"],
  title: string,
  target: AttentionTarget | null,
  held_tasks: AttentionItem["held_tasks"] = [],
): AttentionItem => ({
  id,
  category,
  title,
  reason: `${title} reason`,
  target,
  held_tasks,
});
const railState = snapshotAt("service-a", "40", {
  projects: [initialized, secondProject],
  tasks: [
    {
      ...task,
      active_attempt: {
        id: "a1",
        phase: "awaiting_plan_approval",
        status: "running",
        base_revision: "abc",
        plan_hash: "plan-1",
        plan: "Reviewed plan text",
      },
    },
    {
      ...task,
      id: "AJ-2",
      title: "Recovering",
      attention: "needs_recovery",
      active_attempt: {
        id: "a2",
        phase: "implementation",
        status: "needs_recovery",
        base_revision: "abc",
      },
    },
    {
      ...task,
      id: "AJ-3",
      project_id: "p2",
      title: "Paused",
      attention: "paused",
      active_attempt: undefined,
    },
  ],
  active_sessions: [managerSession],
  recovery: ["r-first", "r-target"].map((id) => ({
    id,
    session_id: "s-other",
    attempt_id: "a2",
    state: "attention_required",
    detail: {},
  })),
  permission_requests: [pendingPermission],
  continuation_actions: [{
    kind: "wait_for_exit",
    enabled: false,
    reason: "A verified exit is required.",
    owner: "service",
    waiting_for: "verified process-group exit",
    operation: "wait_for_exit",
    binding: { task_id: "AJ-1", attempt_id: "a1", session_id: "s1" },
  }],
  attention: [
    railItem("permission_request:pr1", "permission", "AJ-1 · shell", {
      kind: "permission_request",
      project_id: "p1",
      task_id: "AJ-1",
      attempt_id: "a1",
      session_id: "s1",
      request_id: "pr1",
      request_revision: 3,
    }),
    railItem("attempt:a1:awaiting_plan_approval", "decision", "AJ-1 plan", {
      kind: "attempt",
      project_id: "p1",
      task_id: "AJ-1",
      attempt_id: "a1",
      phase: "awaiting_plan_approval",
      plan_hash: "plan-1",
      candidate_hash: null,
    }),
    railItem("restore_hold", "recovery", "Instance restore hold", null, [{
      project_id: "p1",
      task_id: "AJ-1",
      task_version: 7,
    }]),
    railItem("recovery_record:r-target", "recovery", "AJ-2 recovery", {
      kind: "recovery_record",
      project_id: "p1",
      task_id: "AJ-2",
      attempt_id: "a2",
      recovery_id: "r-target",
    }),
    railItem("continuation:wait_for_exit:s1", "recovery", "Waiting for exit", {
      kind: "session",
      project_id: "p1",
      task_id: "AJ-1",
      attempt_id: "a1",
      session_id: "s1",
      role_generation_id: "g1",
    }),
    railItem("project_setup:p2", "compatibility", "Second setup", {
      kind: "project_setup",
      project_id: "p2",
      setup_operation_id: null,
    }),
    railItem("task:AJ-3:paused", "blocked", "AJ-3 · Paused", {
      kind: "task",
      project_id: "p2",
      task_id: "AJ-3",
      task_version: 7,
    }),
    railItem(
      "attempt:a-old:awaiting_human_review",
      "awaiting_acceptance",
      "AJ-2 acceptance",
      {
        kind: "attempt",
        project_id: "p1",
        task_id: "AJ-2",
        attempt_id: "a-old",
        phase: "awaiting_human_review",
        plan_hash: null,
        candidate_hash: "candidate-old",
      },
    ),
  ],
});

Deno.test("M6 attention rail groups exact items, preserves forms across snapshots, and opens only current targets", async () => {
  const live = liveHarness();
  const priorEnvironment = { ...liveEnvironment };
  const methods: string[] = [];
  globalThis.fetch =
    (async (_input: string | URL | Request, init?: RequestInit) => {
      methods.push(init?.method ?? "GET");
      return new Response("[]");
    }) as typeof fetch;
  liveEnvironment.transport = live.environment.transport;
  liveEnvironment.scheduler = live.environment.scheduler;
  localStorage.clear();
  localStorage.setItem("agenticjira.page", "workspace");
  const focused = () =>
    document.activeElement?.getAttribute("data-attention-target");
  const open = (selector: string) => {
    const button = document.querySelector<HTMLButtonElement>(selector);
    if (!button) throw new Error(`attention control not found: ${selector}`);
    act(() => button.click());
  };
  try {
    mount(<App />);
    await settle();
    live.reads[0].resolve(railState);
    await settle();
    const groups = [...document.querySelectorAll('.inbox [role="group"]')]
      .map((group) => group.getAttribute("aria-label")).join("|");
    check(
      groups ===
        "Permissions (1)|Decisions (1)|Recovery (3)|Compatibility (1)|Blocked (1)|Completed · awaiting acceptance (1)",
      `attention groups drifted: ${groups}`,
    );
    check(
      document.querySelector(".inbox header span")?.textContent === "8" &&
        document.querySelector('[data-attention-id="task:AJ-3:paused"]')
          ?.textContent?.includes("Second"),
      "the rail miscounted items or hid another project's item",
    );

    open('[data-attention-id="attempt:a1:awaiting_plan_approval"]');
    await settle();
    check(
      focused() === "attempt:a1" &&
        document.activeElement?.classList.contains("review-panel"),
      `the plan decision did not focus its review panel: ${focused()}`,
    );
    const scroller = document.querySelector(".detail-scroll");
    change(
      document.querySelector<HTMLTextAreaElement>(
        '[aria-label="Guidance message"]',
      )!,
      "Keep this draft",
    );
    change(
      document.querySelector<HTMLSelectElement>(
        '[aria-label="Guidance target"]',
      )!,
      "g1",
    );
    check(
      live.waits[0]?.cursor.incarnation === "service-a" &&
        live.waits[0].cursor.revision === "40",
      "the dashboard did not long-poll from its snapshot cursor",
    );
    live.waits[0].resolve(changedTo({ ...railState, revision: "41" }));
    await settle();
    live.waits[1].resolve({
      outcome: "unchanged",
      incarnation: "service-a",
      revision: "41",
    });
    await settle();
    check(
      document.querySelector<HTMLTextAreaElement>(
            '[aria-label="Guidance message"]',
          )?.value === "Keep this draft" &&
        document.querySelector<HTMLSelectElement>(
            '[aria-label="Guidance target"]',
          )?.value === "g1" &&
        document.querySelector(".detail-scroll") === scroller &&
        document.querySelector(".detail header .eyebrow")?.textContent ===
          "AJ-1",
      "a new snapshot reset the guidance draft, target, or open task",
    );
    check(
      document.body.textContent?.includes("Connected") &&
        !document.body.textContent.includes("Service offline") &&
        !document.querySelector(".boot"),
      "live waits flashed offline or loading state",
    );

    open('[data-attention-id="recovery_record:r-target"]');
    await settle();
    check(
      focused() === "recovery_record:r-target",
      `the exact recovery record was not selected: ${focused()}`,
    );
    open('[data-attention-id="continuation:wait_for_exit:s1"]');
    await settle();
    check(
      focused() === "session:s1",
      `the session action was not focused: ${focused()}`,
    );
    open('[data-attention-id="restore_hold"] button');
    await settle();
    check(
      focused() === "task:AJ-1",
      `the held task did not open: ${focused()}`,
    );
    open('[data-attention-id="permission_request:pr1"]');
    await settle();
    check(
      focused() === "permission_request:pr1" &&
        !document.querySelector("aside.detail"),
      `the permission request was not focused in the approval inbox: ${focused()}`,
    );

    const reads = live.reads.length;
    open('[data-attention-id="attempt:a-old:awaiting_human_review"]');
    await settle();
    check(
      live.reads.length === reads + 1 &&
        focused() === "permission_request:pr1" &&
        !document.querySelector("aside.detail") &&
        document.querySelector(".attention-notice")?.textContent?.includes(
          "a newer attempt or decision replaced it",
        ),
      "a superseded attempt was opened, not explained, or not refreshed once",
    );
    live.reads[reads].resolve({ ...railState, revision: "41" });
    await settle();

    open('[data-attention-id="task:AJ-3:paused"]');
    await settle();
    check(
      focused() === "task:AJ-3" &&
        document.querySelector(".topbar")?.textContent?.includes("Second"),
      "the cross-project task did not switch project and open exactly",
    );
    open('[data-attention-id="project_setup:p2"]');
    await settle();
    check(
      focused() === "project_setup:p2",
      `the project setup was not focused: ${focused()}`,
    );
    check(
      methods.every((method) => method === "GET"),
      `navigation or refresh dispatched a mutation: ${methods}`,
    );
  } finally {
    unmount();
    liveEnvironment.transport = priorEnvironment.transport;
    liveEnvironment.scheduler = priorEnvironment.scheduler;
    globalThis.fetch = nativeFetch;
    localStorage.clear();
  }
});

Deno.test("M6 attention reconciliation refuses superseded bindings instead of retargeting", () => {
  const item = (id: string) => {
    const found = railState.attention.find((candidate) => candidate.id === id);
    if (!found) throw new Error(`fixture attention item missing: ${id}`);
    return found;
  };
  const problem = (id: string, latest: AppState, target = item(id).target) => {
    if (!target) throw new Error(`fixture attention item has no target: ${id}`);
    return attentionTargetProblem(latest, item(id), target);
  };
  const [plan, recovering, paused] = railState.tasks;
  const withTasks = (...tasks: Task[]) => ({ ...railState, tasks });
  const cases: Array<[string, AppState, string | undefined]> = [
    ["attempt:a1:awaiting_plan_approval", railState, undefined],
    ["recovery_record:r-target", railState, undefined],
    ["continuation:wait_for_exit:s1", railState, undefined],
    ["permission_request:pr1", railState, undefined],
    ["project_setup:p2", railState, undefined],
    ["task:AJ-3:paused", railState, undefined],
    [
      "attempt:a1:awaiting_plan_approval",
      withTasks(
        {
          ...plan,
          active_attempt: {
            id: "a9",
            phase: "awaiting_plan_approval",
            status: "running",
            base_revision: "abc",
            plan_hash: "plan-1",
          },
        },
        recovering,
        paused,
      ),
      "a newer attempt or decision replaced it.",
    ],
    [
      "continuation:wait_for_exit:s1",
      {
        ...railState,
        active_sessions: [{ ...managerSession, role_generation_id: "g2" }],
      },
      "the session was replaced.",
    ],
    [
      "permission_request:pr1",
      {
        ...railState,
        permission_requests: [{ ...pendingPermission, revision: 4 }],
      },
      "the permission request was already decided or changed.",
    ],
    [
      "recovery_record:r-target",
      {
        ...railState,
        recovery: railState.recovery.map((record) =>
          record.id === "r-target" ? { ...record, state: "resolved" } : record
        ),
      },
      "the recovery record was resolved or its attempt was superseded.",
    ],
    [
      "project_setup:p2",
      {
        ...railState,
        projects: [initialized, {
          ...secondProject,
          trip: { ...secondProject.trip!, setup_operation_id: "setup-new" },
        }],
      },
      "a different setup operation is now current.",
    ],
    [
      "task:AJ-3:paused",
      withTasks(plan, recovering, { ...paused, version: 8 }),
      "the task changed.",
    ],
    [
      "task:AJ-3:paused",
      { ...railState, attention: [] },
      "it is no longer in the attention list.",
    ],
  ];
  for (const [id, latest, expected] of cases) {
    const actual = problem(id, latest);
    check(actual === expected, `${id}: expected ${expected}, got ${actual}`);
  }
  const held: AttentionTarget = {
    kind: "task",
    project_id: "p1",
    task_id: "AJ-1",
    task_version: 7,
  };
  check(
    problem("restore_hold", railState, held) === undefined &&
      problem("restore_hold", railState, { ...held, task_id: "AJ-2" }) ===
        "its target was replaced." &&
      problem("attempt:a1:awaiting_plan_approval", railState, {
          kind: "attempt",
          project_id: "p1",
          task_id: "AJ-1",
          attempt_id: "a0",
          phase: "awaiting_plan_approval",
          plan_hash: "plan-1",
          candidate_hash: null,
        }) === "its target was replaced.",
    "a held-task or rebound target was not checked against the offered item",
  );
});

Deno.test("M6 selected recovery record keeps its own draft and fails closed when it vanishes", async () => {
  const requests: Record<string, unknown>[] = [];
  globalThis.fetch =
    (async (_input: string | URL | Request, init?: RequestInit) => {
      requests.push(JSON.parse(String(init?.body)));
      return new Response(JSON.stringify({ error: "fixture outage" }), {
        status: 503,
      });
    }) as typeof fetch;
  const panel = (selected: string, ...ids: string[]) => (
    <RecoveryPanel
      task={task}
      records={ids.map((id) => ({
        id,
        session_id: `session-${id}`,
        attempt_id: "a1",
        state: "attention_required",
        detail: {},
      }))}
      selectedRecordId={selected}
      onChanged={noop}
    />
  );
  const shown = () => ({
    marker: document.querySelector("[data-attention-target]")
      ?.getAttribute("data-attention-target"),
    evidence: document.querySelector<HTMLTextAreaElement>(
      '[aria-label="Recovery evidence"]',
    )?.value,
    error: document.querySelector(".recovery .error")?.textContent,
  });
  const submit = async () => {
    click("Verify quiescence and reconcile");
    await settle();
    return requests.at(-1);
  };
  try {
    mount(panel("A", "A", "B"));
    field("Recovery evidence", "evidence for A");
    const first = await submit();
    rerender(panel("A", "A", "B"));
    check(
      shown().evidence === "evidence for A" &&
        shown().error === "fixture outage",
      "a same-record snapshot reset the draft or its error",
    );
    const retried = await submit();
    check(
      first?.recovery_id === "A" &&
        retried?.operation_id === first.operation_id,
      "a same-record snapshot dropped the retry identity",
    );
    rerender(panel("B", "A", "B"));
    check(
      shown().marker === "recovery_record:B" && shown().evidence === "" &&
        shown().error === undefined,
      "another record inherited the draft or its error",
    );
    rerender(panel("A", "A", "B"));
    check(shown().evidence === "", "the draft survived a change of record");
    field("Recovery evidence", "evidence for A");
    const reopened = await submit();
    check(
      requests.length === 3 && reopened?.recovery_id === "A" &&
        reopened.operation_id !== first?.operation_id,
      "a retry identity survived a change of record",
    );
    const sent = requests.length;
    rerender(panel("A", "B"));
    check(
      document.body.textContent?.includes("was resolved or changed") &&
        !document.querySelector("button, textarea, [data-attention-target]") &&
        requests.length === sent,
      "a vanished selection retargeted the remaining record",
    );
  } finally {
    unmount();
    globalThis.fetch = nativeFetch;
  }
});

Deno.test("M6 attention routes fail closed when the exact record or marker is gone", async () => {
  const live = liveHarness();
  const priorEnvironment = { ...liveEnvironment };
  const methods: string[] = [];
  globalThis.fetch =
    (async (_input: string | URL | Request, init?: RequestInit) => {
      methods.push(init?.method ?? "GET");
      return new Response("[]");
    }) as typeof fetch;
  liveEnvironment.transport = live.environment.transport;
  liveEnvironment.scheduler = live.environment.scheduler;
  localStorage.clear();
  localStorage.setItem("agenticjira.page", "workspace");
  const open = (id: string) => {
    const button = document.querySelector<HTMLButtonElement>(
      `[data-attention-id="${id}"]`,
    );
    if (!button) throw new Error(`attention control not found: ${id}`);
    act(() => button.click());
  };
  try {
    mount(<App />);
    await settle();
    live.reads[0].resolve(railState);
    await settle();

    open("recovery_record:r-target");
    await settle();
    field("Recovery evidence", "evidence for r-target");
    live.waits[0].resolve(changedTo({
      ...railState,
      revision: "41",
      recovery: railState.recovery.map((record) =>
        record.id === "r-target" ? { ...record, state: "resolved" } : record
      ),
    }));
    await settle();
    const recovery = document.querySelector(".detail .recovery");
    check(
      recovery?.textContent?.includes("was resolved or changed") &&
        !recovery.querySelector("button, textarea") &&
        !document.querySelector('[data-attention-target^="recovery_record:"]'),
      "a resolved exact record retargeted its sibling or kept its draft",
    );

    // Schema-valid, but no rendered panel carries the session's exact marker.
    const unmarked = { ...railState, revision: "42", continuation_actions: [] };
    live.waits[1].resolve(changedTo(unmarked));
    await settle();
    const reads = live.reads.length;
    open("continuation:wait_for_exit:s1");
    await settle();
    const explained = () =>
      document.querySelector("main.content > .attention-notice")?.textContent
        ?.includes("Waiting for exit could not be opened");
    check(
      live.reads.length === reads + 1 && explained() &&
        !document.querySelector("aside.detail") &&
        document.activeElement?.getAttribute("data-attention-target") !==
          "task:AJ-1",
      "a missing exact marker was accepted, unexplained, or not refreshed once",
    );
    live.reads[reads].resolve(unmarked);
    await settle();
    check(
      live.reads.length === reads + 1 && explained() &&
        methods.every((method) => method === "GET"),
      `a failed route refreshed again, lost its explanation, or mutated: ${methods}`,
    );
  } finally {
    unmount();
    liveEnvironment.transport = priorEnvironment.transport;
    liveEnvironment.scheduler = priorEnvironment.scheduler;
    globalThis.fetch = nativeFetch;
    localStorage.clear();
  }
});

Deno.test("M6 direct task opens drop a stale attention selection and route notice", async () => {
  const live = liveHarness();
  const priorEnvironment = { ...liveEnvironment };
  const mutations: Record<string, unknown>[] = [];
  globalThis.fetch =
    (async (_input: string | URL | Request, init?: RequestInit) => {
      if (init?.method !== "POST") return new Response("[]");
      mutations.push(JSON.parse(String(init.body)));
      return new Response(JSON.stringify({ error: "fixture outage" }), {
        status: 503,
      });
    }) as typeof fetch;
  liveEnvironment.transport = live.environment.transport;
  liveEnvironment.scheduler = live.environment.scheduler;
  localStorage.clear();
  localStorage.setItem("agenticjira.page", "workspace");
  const initial: AppState = {
    ...railState,
    tasks: [...railState.tasks, {
      ...task,
      id: "AJ-4",
      title: "Finished",
      lifecycle: "done",
      active_attempt: undefined,
    }],
    restart_candidates: [{
      session_id: "s-other",
      attempt_id: "a2",
      task_id: "AJ-2",
      source: "planned_shutdown",
      state: "parked",
      reason: "fixture parked session",
      result: {},
      updated_at: "now",
    }],
  };
  const resolved: AppState = {
    ...initial,
    revision: "41",
    recovery: initial.recovery.map((record) =>
      record.id === "r-target" ? { ...record, state: "resolved" } : record
    ),
  };
  const open = (id: string) => {
    const button = document.querySelector<HTMLButtonElement>(
      `[data-attention-id="${id}"]`,
    );
    if (!button) throw new Error(`attention control not found: ${id}`);
    act(() => button.click());
  };
  const recovery = () => {
    const panel = document.querySelector(".detail .recovery");
    return {
      record: panel?.getAttribute("data-attention-target"),
      stale: panel?.textContent?.includes("was resolved or changed"),
      evidence: panel?.querySelector("textarea")?.value,
      error: panel?.querySelector(".error")?.textContent,
    };
  };
  const notice = () =>
    document.querySelector("main.content > .attention-notice")?.textContent;
  try {
    mount(<App />);
    await settle();
    live.reads[0].resolve(initial);
    await settle();

    open("recovery_record:r-target");
    await settle();
    const routed = recovery().record;
    click("Board");
    click("AJ-2 · Normal");
    check(
      routed === "recovery_record:r-target" &&
        recovery().record === "recovery_record:r-first",
      `a board open kept the attention-selected record: ${routed} -> ${recovery().record}`,
    );

    click("Workspace");
    open("recovery_record:r-target");
    await settle();
    field("Recovery evidence", "evidence for r-target");
    click("Verify quiescence and reconcile");
    await settle();
    const failed = recovery().error;
    live.waits[0].resolve(changedTo(resolved));
    await settle();
    check(
      failed === "fixture outage" && recovery().stale &&
        mutations[0]?.recovery_id === "r-target",
      "the selected record did not fail closed after its failed submission",
    );
    click("Open task recovery");
    await settle();
    const reopened = recovery();
    check(
      !reopened.stale && reopened.record === "recovery_record:r-first" &&
        reopened.evidence === "" && reopened.error === undefined &&
        mutations.length === 1,
      `a direct open kept the stale selection, its draft, or mutated: ${
        JSON.stringify(reopened)
      }`,
    );
    field("Recovery evidence", "evidence for r-target");
    click("Verify quiescence and reconcile");
    await settle();
    check(
      mutations[1]?.recovery_id === "r-first" &&
        mutations[1].operation_id !== mutations[0].operation_id,
      "the remaining record reused the resolved record's retry identity",
    );

    // Without its continuation row the session has no exact marker.
    live.waits[1].resolve(
      changedTo({ ...resolved, revision: "42", continuation_actions: [] }),
    );
    await settle();
    open("continuation:wait_for_exit:s1");
    await settle();
    check(
      notice()?.includes("Waiting for exit could not be opened"),
      "the unmarked route was not explained",
    );
    click("History");
    click("AJ-4 · Finished");
    check(
      notice() === undefined &&
        document.querySelector(".detail header .eyebrow")?.textContent ===
          "AJ-4" &&
        mutations.length === 2,
      `a history open kept the obsolete route notice or mutated: ${notice()}`,
    );
  } finally {
    unmount();
    liveEnvironment.transport = priorEnvironment.transport;
    liveEnvironment.scheduler = priorEnvironment.scheduler;
    globalThis.fetch = nativeFetch;
    localStorage.clear();
  }
});
