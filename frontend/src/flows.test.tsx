/// <reference lib="deno.ns" />
import { Window } from "happy-dom";
import type { ReactNode } from "react";
import type { Root } from "react-dom/client";
import { ROLES } from "./types";
import type {
  AppState,
  CmuxSessionSurface,
  CmuxViewOutcome,
  Project,
  Session,
  Task,
  TripSetupState,
  TripTaskVerification,
} from "./types";

const window = new Window({ url: "http://127.0.0.1/" });
for (
  const [name, value] of Object.entries({
    window,
    document: window.document,
    localStorage: window.localStorage,
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
const { RecoveryPanel } = await import("./components/RecoveryPanel");
const { ProjectSetup } = await import("./components/ProjectSetup");
const { ProjectPicker } = await import("./components/ProjectPicker");
const { TaskForm } = await import("./components/TaskForm");
const { TaskDetail } = await import("./components/TaskDetail");
const { WorkflowControls } = await import("./components/WorkflowControls");
const { ModelSelector } = await import("./components/ModelSelector");
const { Workspace } = await import("./components/Workspace");
const { ApiError, command, transportTimeouts } = await import("./api");
const nativeFetch = globalThis.fetch;
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
      JSON.stringify(requests.at(-1)?.role_overrides) !==
        JSON.stringify({ implementer: roleOverride })
    ) throw new Error("pending override was not retained in the draft");

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
      continuation_actions: [],
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
      continuation_actions: [],
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
      <AttentionInbox state={state} onSelect={() => {}} onChanged={() => {}} />,
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
  let changed = 0;
  globalThis.fetch =
    (async (_input: string | URL | Request, init?: RequestInit) => {
      requests.push(JSON.parse(String(init?.body)));
      return conflict
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
    continuation_actions: [],
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
    unmount();
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
