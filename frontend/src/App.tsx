import { ErrorNotice } from "./components/ErrorNotice";
import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  bootstrap,
  command,
  discardUnknownCmuxSurface,
  operationId,
  setCmuxKeyboardControl,
  viewCmuxSurface,
} from "./api";
import {
  type LiveStateController,
  type LiveStatus,
  startLiveState,
} from "./liveState";
import {
  type AppState,
  type AttentionItem,
  type AttentionTarget,
  type CmuxKeyboardControlAction,
  type CmuxSessionSurface,
  type PermissionRequest,
  type Project,
  roleLabel,
  type Task,
  type TaskAction,
  type TripSetupState,
} from "./types";
import {
  nativePromptLabel,
  nativeTurnFailureLabel,
} from "./components/SessionTree";
import {
  attentionFocusElement,
  attentionTargetProblem,
} from "./components/AttentionInbox";
import { ProjectPicker } from "./components/ProjectPicker";
import { ProjectSettings } from "./components/ProjectSettings";
import { TaskBoard } from "./components/TaskBoard";
import { TaskForm } from "./components/TaskForm";
import { TaskDetail, type TaskTab } from "./components/TaskDetail";
import { pendingApprovalCount } from "./components/ApprovalInbox";
import { Workspace } from "./components/Workspace";
import { History } from "./components/History";
import { Recipes } from "./components/Recipes";
import { DiagnosticsPanel } from "./components/DiagnosticsPanel";
import { CompatibilityDetails, RoleSettings } from "./components/RoleSettings";
type Page = "workspace" | "board" | "recipes" | "roles" | "history" | "diagnostics";
const pages: Array<{ id: Page; label: string; icon: string }> = [
  { id: "workspace", label: "Workspace", icon: "◫" },
  { id: "board", label: "Board", icon: "▦" },
  { id: "recipes", label: "Recipes", icon: "▤" },
  { id: "roles", label: "Projects", icon: "▣" },
  { id: "history", label: "History", icon: "◴" },
  { id: "diagnostics", label: "Diagnostics", icon: "⚙" },
];
export type Appearance = "system" | "light" | "dark";
const appearanceKey = "llmrelay.appearance";
const isAppearance = (value: unknown): value is Appearance =>
  value === "system" || value === "light" || value === "dark";

export function storedAppearance(): Appearance {
  try {
    const value = localStorage.getItem(appearanceKey);
    return isAppearance(value) ? value : "system";
  } catch {
    return "system";
  }
}

const systemPrefersDark = () =>
  typeof window.matchMedia === "function" &&
  window.matchMedia("(prefers-color-scheme: dark)").matches;

export function applyAppearance(appearance: Appearance): void {
  document.documentElement.dataset.theme = appearance === "dark" ||
      (appearance === "system" && systemPrefersDark())
    ? "dark"
    : "light";
}

type BrowserAlerts = "unavailable" | "off" | "on" | "blocked" | "failed";
const browserAlertHints: Record<BrowserAlerts, string> = {
  unavailable:
    "This browser cannot show notifications here, so waiting items appear only at the top of the page.",
  off:
    "Waiting items always appear at the top of the page. Browser notifications are optional.",
  on:
    "Browser notifications are on for new waits while LLMRelay is in the background, until this page is reloaded. A notification never approves or runs anything.",
  blocked:
    "Browser notifications are blocked for this site, so waiting items appear only at the top of the page.",
  failed:
    "This browser could not show a notification, so waiting items appear only at the top of the page.",
};
const notificationApi = () =>
  "Notification" in window ? window.Notification : undefined;
const browserAlertsFor = (permission: NotificationPermission): BrowserAlerts =>
  permission === "granted" ? "on" : permission === "denied" ? "blocked" : "off";
const pageFocused = () =>
  document.visibilityState === "visible" && document.hasFocus();

/** Something that needs you now, keyed by its exact request or hook event. */
interface WaitNotice {
  key: string;
  title: string;
  detail: string;
  action: string;
  target: AttentionTarget;
}
const permissionWait = (
  state: AppState,
  request: PermissionRequest,
): WaitNotice => ({
  key: `permission:${request.id}`,
  title: `${roleLabel(request.role)} is waiting for your approval`,
  detail: `${
    state.tasks.find((task) => task.id === request.task_id)?.title || "A task"
  } · wants to use ${request.tool_name}`,
  action: "Review approval request",
  target: {
    kind: "permission_request",
    project_id: request.project_id,
    task_id: request.task_id,
    attempt_id: request.attempt_id,
    session_id: request.session_id,
    request_id: request.id,
    request_revision: request.revision,
  },
});
function currentWaits(state: AppState): WaitNotice[] {
  const permissions = state.permission_requests
    .filter((request) => request.actionable)
    .map((request) => permissionWait(state, request));
  const sessions = state.active_sessions
    .filter((session) => !["exited", "launch_failed"].includes(session.status))
    .flatMap((session) => {
      const task = state.tasks.find((item) => item.id === session.task_id);
      const owner = task?.title || "Project setup";
      const target: AttentionTarget = {
        kind: "session",
        project_id: task?.project_id ||
          state.trip_setups?.find((setup) =>
            setup.setup_operation_id === session.setup_operation_id
          )?.project_id || "",
        task_id: session.task_id,
        attempt_id: session.attempt_id,
        session_id: session.id,
        role_generation_id: session.role_generation_id,
      };
      const waits: WaitNotice[] = [];
      const failure = session.native_turn?.failure;
      if (failure) {
        waits.push({
          key: `turn_failure:${session.id}:${failure.hook_event_id}`,
          title: `${roleLabel(session.role)}'s latest turn stopped: ${
            nativeTurnFailureLabel[failure.kind]
          }`,
          detail:
            `${owner} · The session is still open. LLMRelay does not retry or switch models in response to this notice.`,
          action: "Open agent output",
          target,
        });
      }
      if (session.native_prompt && !session.native_prompt.dismissed) {
        waits.push({
          key: `native_prompt:${session.id}:${session.native_prompt.hook_event_id}`,
          title: `${roleLabel(session.role)} is ${
            nativePromptLabel[session.native_prompt.kind]
          }`,
          detail:
            `${owner} · LLMRelay cannot answer it; open the agent's output to answer it there.`,
          action: "Open agent output",
          target,
        });
      }
      return waits;
    });
  return [...permissions, ...sessions];
}

const stateLabel = (value: string) => {
  const words = value.replaceAll("_", " ");
  return words[0].toUpperCase() + words.slice(1);
};
const resolveProjectSetup = (
  project: Project,
  setups: TripSetupState[],
): { setup?: TripSetupState; warning?: string } => {
  const candidates = setups.filter((setup) => setup.project_id === project.id);
  const expectedId = project.trip?.setup_operation_id;
  if (expectedId) {
    const setup = candidates.find((item) =>
      item.setup_operation_id === expectedId
    );
    return setup ? { setup } : {
      warning:
        `Current setup operation ${expectedId} is missing from this state snapshot. Historical setup rows were not substituted.`,
    };
  }
  const eligibleFallbacks = candidates.filter((item) =>
    !["aborted", "superseded"].includes(item.state)
  );
  if (eligibleFallbacks.length === 1) return { setup: eligibleFallbacks[0] };
  return candidates.length > 0
    ? {
      warning:
        "The project state does not identify one current setup operation. Historical or ambiguous setup rows were left unselected.",
    }
    : {};
};
const empty: AppState = {
  schema: 8,
  generated_at: "",
  incarnation: "",
  revision: "",
  projects: [],
  tasks: [],
  profile_sets: [],
  task_recipes: [],
  recipe_schedules: [],
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
  instance_settings: {
    version: 1,
    auto_resume_eligible: false,
    updated_at: "",
  },
  restart_candidates: [],
  permission_requests: [],
  permission_rules: [],
  trip_setups: [],
  trip_explorer: [],
  trip_lanes: [],
  trip_checks: [],
  trip_task_verification: [],
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
export function App() {
  const [state, setState] = useState<AppState>(empty);
  const [page, setPage] = useState<Page>(() => {
    const saved = localStorage.getItem("agenticjira.page") as Page;
    return ["workspace", "board", "recipes", "roles", "history", "diagnostics"].includes(
        saved,
      )
      ? saved
      : "workspace";
  });
  const [project, setProject] = useState<string | undefined>(() =>
    localStorage.getItem("agenticjira.project") || undefined
  );
  const [selectedId, setSelectedId] = useState<string | undefined>(() =>
    localStorage.getItem("agenticjira.task") || undefined
  );
  const [form, setForm] = useState<"new" | "edit">();
  const [editing, setEditing] = useState<Task>();
  const [loading, setLoading] = useState(true);
  const [connection, setConnection] = useState<LiveStatus>({
    online: false,
    error: "",
    refreshing: false,
  });
  const [error, setError] = useState("");
  const [lastSuccessfulAt, setLastSuccessfulAt] = useState("");
  const [attentionFocus, setAttentionFocus] = useState<AttentionTarget>();
  const [navigationNotice, setNavigationNotice] = useState("");
  const [menuOpen, setMenuOpen] = useState(false);
  const [appearance, setAppearance] = useState<Appearance>(storedAppearance);
  const [browserAlerts, setBrowserAlerts] = useState<BrowserAlerts>(() => {
    const api = notificationApi();
    // Origin permission granted earlier is not this page's opt-in; only Enable turns alerts on.
    if (!api) return "unavailable";
    return api.permission === "denied" ? "blocked" : "off";
  });
  // Wait keys already listed per service incarnation; only later ones alert.
  const announcedWaits = useRef<
    { incarnation: string; keys: Set<string> } | undefined
  >(undefined);
  const openWaitRef = useRef<(wait: WaitNotice) => void>(() => {});
  // Each task keeps the tab it was last shown with while the app is open.
  const [taskTabs, setTaskTabs] = useState<Record<string, TaskTab>>({});
  const pendingSection = useRef<string | undefined>(undefined);
  const pendingFocus = useRef<
    { title: string; target: AttentionTarget } | undefined
  >(undefined);
  const live = useRef<LiveStateController | undefined>(undefined);
  const currentProject = useRef(project);
  currentProject.current = project;
  const currentPage = useRef(page);
  currentPage.current = page;
  const refresh = useCallback(
    () => live.current?.refresh() ?? Promise.resolve(),
    [],
  );
  const viewCmuxSession = useCallback(async (sessionId: string) => {
    const result = await viewCmuxSurface(sessionId, operationId());
    await refresh();
    return result;
  }, [refresh]);
  const setCmuxSessionKeyboardControl = useCallback(async (
    sessionId: string,
    surface: CmuxSessionSurface,
    action: CmuxKeyboardControlAction,
  ) => {
    const result = await setCmuxKeyboardControl(
      sessionId,
      surface.id,
      surface.binding_revision,
      surface.control_revision,
      action,
      operationId(),
    );
    await refresh();
    return result;
  }, [refresh]);
  const discardCmuxSessionSurface = useCallback(async (
    sessionId: string,
    surfaceRouteId: string,
    id: string,
  ) => {
    const result = await discardUnknownCmuxSurface(
      sessionId,
      surfaceRouteId,
      id,
    );
    await refresh();
    return result;
  }, [refresh]);
  useEffect(() => {
    let unmounted = false;
    let controller: LiveStateController | undefined;
    const report = (cause: unknown) =>
      setError(cause instanceof Error ? cause.message : String(cause));
    void bootstrap().catch(report).finally(() => {
      if (unmounted) return;
      try {
        controller = startLiveState({
          onState: (next) => {
            setState(next);
            setLastSuccessfulAt(next.generated_at || new Date().toISOString());
            setError("");
            setLoading(false);
          },
          onStatus: (status) => {
            setConnection(status);
            if (status.error) setLoading(false);
          },
        });
        live.current = controller;
      } catch (cause) {
        report(cause);
        setLoading(false);
      }
    });
    return () => {
      unmounted = true;
      controller?.dispose();
      live.current = undefined;
    };
  }, []);
  // A routed attention click settles in the render that routed it. Without its
  // exact marker nothing stands in: the route is withdrawn, explained and
  // refreshed once.
  useEffect(() => {
    const pending = pendingFocus.current;
    if (!pending) return;
    pendingFocus.current = undefined;
    const element = attentionFocusElement(pending.target);
    if (element) {
      element.scrollIntoView({ block: "nearest" });
      element.focus({ preventScroll: true });
      return;
    }
    setSelectedId(undefined);
    setAttentionFocus(undefined);
    setNavigationNotice(
      `${pending.title} could not be opened: its exact panel is no longer shown. The latest state was requested; nothing was executed.`,
    );
    void refresh();
  });
  useEffect(() => {
    const section = pendingSection.current;
    if (!section) return;
    pendingSection.current = undefined;
    const element = document.getElementById(section);
    element?.scrollIntoView({ block: "start" });
    element?.focus({ preventScroll: true });
  });
  useEffect(() => {
    if (appearance !== "system" || typeof window.matchMedia !== "function") {
      return;
    }
    const query = window.matchMedia("(prefers-color-scheme: dark)");
    const follow = () => applyAppearance("system");
    query.addEventListener("change", follow);
    return () => query.removeEventListener("change", follow);
  }, [appearance]);
  useEffect(() => {
    if (!state.incarnation) return;
    const waits = currentWaits(state);
    const announced = announcedWaits.current;
    if (announced?.incarnation !== state.incarnation) {
      // A first or reset snapshot lists current waits without replaying alerts.
      announcedWaits.current = {
        incarnation: state.incarnation,
        keys: new Set(waits.map((wait) => wait.key)),
      };
      return;
    }
    const fresh = waits.filter((wait) => !announced.keys.has(wait.key));
    for (const wait of fresh) announced.keys.add(wait.key);
    const api = notificationApi();
    if (!fresh.length || browserAlerts !== "on" || !api || pageFocused()) {
      return;
    }
    for (const wait of fresh) {
      try {
        const notification = new api(`LLMRelay: ${wait.title}`, {
          body:
            `${wait.detail}\nOpen LLMRelay to act; this notification does not approve or run anything.`,
          tag: `llmrelay:${state.incarnation}:${wait.key}`,
        });
        notification.onclick = () => {
          window.focus();
          notification.close();
          openWaitRef.current(wait);
        };
      } catch {
        setBrowserAlerts("failed");
        return;
      }
    }
  }, [state, browserAlerts]);
  useEffect(() => {
    localStorage.setItem("agenticjira.page", page);
  }, [page]);
  useEffect(() => {
    if (project) localStorage.setItem("agenticjira.project", project);
    else localStorage.removeItem("agenticjira.project");
  }, [project]);
  useEffect(() => {
    if (selectedId) localStorage.setItem("agenticjira.task", selectedId);
    else localStorage.removeItem("agenticjira.task");
  }, [selectedId]);
  useEffect(() => {
    if (!state.generated_at) return;
    if (project && !state.projects.some((item) => item.id === project)) {
      setProject(undefined);
    }
    if (selectedId && !state.tasks.some((item) => item.id === selectedId)) {
      setSelectedId(undefined);
    }
  }, [state.generated_at, state.projects, state.tasks, project, selectedId]);
  const tasks = useMemo(
    () =>
      project
        ? state.tasks.filter((task) => task.project_id === project)
        : state.tasks,
    [state.tasks, project],
  );
  const selected = state.tasks.find((task) => task.id === selectedId);
  const cmuxSurfaces = useMemo(
    () =>
      Object.fromEntries(
        state.active_sessions
          .filter((session) => !!session.cmux_surface)
          .map((session) => [session.id, session.cmux_surface!] as const),
      ),
    [state.active_sessions],
  );
  const nav = (value: Page) => {
    setPage(value);
    setMenuOpen(false);
    if (value === "recipes") {
      setSelectedId(undefined);
      setForm(undefined);
    }
  };
  // Approvals and attention are reachable from every page without scrolling.
  const openWorkspaceSection = (section: string) => {
    setPage("workspace");
    setMenuOpen(false);
    pendingSection.current = section;
    if (page === "workspace") {
      const element = document.getElementById(section);
      element?.scrollIntoView({ block: "start" });
      element?.focus({ preventScroll: true });
      pendingSection.current = undefined;
    }
  };
  const openSetup = (projectId: string) => {
    setProject(projectId);
    setPage("roles");
    setSelectedId(undefined);
    setForm(undefined);
  };
  // Opening a task directly ends any attention route, so the task shows its own
  // current records rather than a stale exact binding.
  const openTask = (task: Task) => {
    pendingFocus.current = undefined;
    setAttentionFocus(undefined);
    setNavigationNotice("");
    setSelectedId(task.id);
  };
  // Reconciles against the newest accepted snapshot, which can be ahead of
  // the rendered one. Navigation only opens and focuses; it never acts.
  const navigateAttention = (
    item: AttentionItem,
    target: AttentionTarget,
  ): string | undefined => {
    setNavigationNotice("");
    const latest = live.current?.latest() ?? state;
    const problem = attentionTargetProblem(latest, item, target);
    if (problem) {
      void refresh();
      return `${item.title} changed before it could open: ${problem} The latest state was requested; nothing was executed.`;
    }
    // Live output and Agent settings are on the Activity tab; every other
    // exact destination is on Overview.
    routeTo(
      latest,
      item.title,
      target,
      item.action?.kind === "open_agent_output" ||
        target.kind === "role_settings"
        ? "activity"
        : "overview",
    );
    return undefined;
  };
  const routeTo = (
    latest: AppState,
    title: string,
    target: AttentionTarget,
    taskTab: TaskTab,
  ) => {
    if (
      "project_id" in target &&
      latest.projects.some((candidate) => candidate.id === target.project_id)
    ) {
      setProject(target.project_id);
    }
    setForm(undefined);
    switch (target.kind) {
      case "project_setup":
        setPage("roles");
        setSelectedId(undefined);
        break;
      case "permission_request":
        setPage("workspace");
        setSelectedId(undefined);
        break;
      case "diagnostics":
        setPage("diagnostics");
        setSelectedId(undefined);
        break;
      default:
        // A setup session has no task; its output is on the Workspace.
        if (!latest.tasks.some((task) => task.id === target.task_id)) {
          setPage("workspace");
          setSelectedId(undefined);
          break;
        }
        setTaskTabs((current) => ({ ...current, [target.task_id]: taskTab }));
        setSelectedId(target.task_id);
    }
    // A fresh object re-renders, and so re-focuses, a repeated click.
    setAttentionFocus({ ...target });
    pendingFocus.current = { title, target };
  };
  // Rechecks the wait against the newest accepted snapshot; opening never acts.
  const openWait = (wait: WaitNotice) => {
    setNavigationNotice("");
    const latest = live.current?.latest() ?? state;
    const current = currentWaits(latest).find((item) => item.key === wait.key);
    if (!current) {
      void refresh();
      setNavigationNotice(
        `${wait.title}: this notice changed or was dismissed. The latest state was requested; nothing was executed.`,
      );
      return;
    }
    routeTo(latest, current.title, current.target, "activity");
  };
  openWaitRef.current = openWait;
  const openPermission = (request: PermissionRequest) =>
    openWait(permissionWait(state, request));
  const chooseAppearance = (next: Appearance) => {
    setAppearance(next);
    applyAppearance(next);
    try {
      localStorage.setItem(appearanceKey, next);
    } catch {
      // Unavailable storage keeps the choice for this page only.
    }
  };
  const enableBrowserAlerts = async () => {
    const api = notificationApi();
    if (!api) {
      setBrowserAlerts("unavailable");
      return;
    }
    try {
      setBrowserAlerts(browserAlertsFor(await api.requestPermission()));
    } catch {
      setBrowserAlerts("failed");
    }
  };
  // A card or header action opens the same exact item the inbox would.
  const openTaskAction = (action: TaskAction, itemId = action.item_id) => {
    const latest = live.current?.latest() ?? state;
    const item = action.item_ids.includes(itemId)
      ? latest.attention.find((candidate) => candidate.id === itemId)
      : undefined;
    if (!item?.target) {
      void refresh();
      setNavigationNotice(
        "That task's next step changed before it could open. The latest state was requested; nothing was executed.",
      );
      return;
    }
    const problem = navigateAttention(item, item.target);
    if (problem) setNavigationNotice(problem);
  };
  // Controls' failed-step row opens the exact attention item the inbox would;
  // a binding the newest snapshot no longer offers is refused, never replaced.
  const openRecoveryRecord = (
    taskId: string,
    attemptId: string,
    recoveryId: string,
  ): string | undefined => {
    const latest = live.current?.latest() ?? state;
    const item = latest.attention.find(({ target }) =>
      target?.kind === "recovery_record" && target.task_id === taskId &&
      target.attempt_id === attemptId && target.recovery_id === recoveryId
    );
    if (!item?.target) {
      void refresh();
      return "That failed step changed before it could open. The latest state was requested; nothing was executed.";
    }
    return navigateAttention(item, item.target);
  };
  const queue = async () => {
    const current = state.projects.find((v) => v.id === project);
    if (!current) return;
    try {
      await command({
        kind: "set_queue_paused",
        operation_id: operationId(),
        project_id: current.id,
        expected_version: current.version,
        paused: !current.queue_paused,
      });
      refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  if (loading) {
    return (
      <main className="boot">
        <div className="logo">LR</div>
        <p>Connecting to the local LLMRelay instance…</p>
      </main>
    );
  }
  const projectTasks = project
    ? state.tasks.filter((task) => task.project_id === project)
    : state.tasks;
  const attentionCount = state.attention.filter((item) =>
    !item.target || !project || !("project_id" in item.target) ||
    item.target.project_id === project
  ).length;
  const approvalCount = pendingApprovalCount(state);
  const waits = currentWaits(state);
  const dialogOpen = !!selected;
  return (
    <div className={`app-shell${menuOpen ? " menu-open" : ""}`}>
      <aside className="sidebar" inert={dialogOpen}>
        <div className="brand">
          <span className="logo" aria-hidden="true">LR</span>
          <div>
            <strong>LLMRelay</strong>
            <small>Local workspace</small>
          </div>
          <button
            className="menu-toggle"
            aria-expanded={menuOpen}
            aria-controls="sidebar-menu"
            onClick={() => setMenuOpen((open) => !open)}
          >
            {menuOpen ? "Close menu" : "Menu"}
          </button>
        </div>
        <div className="sidebar-menu" id="sidebar-menu">
          <nav aria-label="Main navigation">
            {pages.map((item) => (
              <button
                key={item.id}
                className={page === item.id ? "active" : ""}
                aria-current={page === item.id ? "page" : undefined}
                onClick={() => nav(item.id)}
              >
                <span className="nav-icon" aria-hidden="true">{item.icon}</span>
                <span className="nav-label">{item.label}</span>
                {item.id === "workspace" && attentionCount + approvalCount > 0 && (
                  <span className="nav-badge">
                    <span aria-hidden="true">{attentionCount + approvalCount}</span>
                    <span className="visually-hidden">
                      , {attentionCount + approvalCount} waiting for you
                    </span>
                  </span>
                )}
              </button>
            ))}
          </nav>
          <ProjectPicker
            projects={state.projects}
            selected={project}
            onSelect={setProject}
            onChanged={refresh}
            onAdded={openSetup}
          />
          <div className="sidebar-preferences">
            <label>
              Appearance
              <select
                value={appearance}
                onChange={(event) => {
                  if (isAppearance(event.target.value)) {
                    chooseAppearance(event.target.value);
                  }
                }}
              >
                <option value="system">System</option>
                <option value="light">Light</option>
                <option value="dark">Dark</option>
              </select>
            </label>
            {browserAlerts === "off" && (
              <button type="button" onClick={() => void enableBrowserAlerts()}>
                Enable browser notifications
              </button>
            )}
            <small className="browser-alerts-hint">
              {browserAlertHints[browserAlerts]}
            </small>
          </div>
          <footer
            title={lastSuccessfulAt || state.generated_at
              ? `Last successful update: ${
                new Date(lastSuccessfulAt || state.generated_at).toLocaleString()
              }`
              : undefined}
          >
            <span
              aria-hidden="true"
              className={`status ${connection.online ? "observed" : "offline"}`}
            />
            {connection.online ? "Local service" : "Service offline"}
            <br />
            <small>
              {connection.online
                ? "Connected"
                : lastSuccessfulAt || state.generated_at
                ? "Showing last successful state"
                : "State unavailable"}
            </small>
            {!connection.online && (
              <button
                className="link-button"
                disabled={connection.refreshing}
                onClick={() => void refresh()}
              >
                {connection.refreshing ? "Reconnecting…" : "Retry connection"}
              </button>
            )}
          </footer>
        </div>
      </aside>
      <main className="content" inert={dialogOpen}>
        <header className="topbar">
          <div className="topbar-title">
            {state.projects.find((v) => v.id === project)?.display_name ||
              "All projects"}
            <small>
              {projectTasks.filter((t) =>
                !t.archived && !["done", "cancelled"].includes(t.lifecycle)
              ).length} active tasks
            </small>
          </div>
          <div className="waiting-links" aria-label="Waiting for you">
            <button
              className={attentionCount ? "waiting-link has-items" : "waiting-link"}
              onClick={() => openWorkspaceSection("workspace-attention")}
            >
              Needs your attention
              <span className="count">{attentionCount}</span>
            </button>
            <button
              className={approvalCount ? "waiting-link has-items" : "waiting-link"}
              onClick={() => openWorkspaceSection("workspace-approvals")}
            >
              Approvals<span className="count">{approvalCount}</span>
            </button>
          </div>
          <div className="button-row topbar-actions">
            <button onClick={queue} disabled={!project}>
              {state.projects.find((v) => v.id === project)?.queue_paused
                ? "Resume pickup"
                : "Pause pickup"}
            </button>
            <button
              className="primary"
              onClick={() => {
                setEditing(undefined);
                setForm("new");
              }}
            >
              ＋ New task
            </button>
          </div>
        </header>
        {(error || connection.error) && (
          <div className="global-error" role="alert">
            <ErrorNotice error={error || connection.error || ""} />
            <button
              disabled={connection.refreshing}
              onClick={() =>
                void refresh()}
            >
              {connection.refreshing ? "Reconciling…" : "Refresh and reconcile"}
            </button>
          </div>
        )}
        {waits.length > 0 && (
          <section className="wait-notices" aria-labelledby="wait-notices-title">
            <h2 id="wait-notices-title">Waiting for you now</h2>
            <ul aria-live="polite">
              {waits.map((wait) => (
                <li key={wait.key}>
                  <span className="wait-notice-copy">
                    <strong>{wait.title}</strong>
                    <small>{wait.detail}</small>
                  </span>
                  <button type="button" onClick={() => openWait(wait)}>
                    {wait.action}
                    <span className="visually-hidden">: {wait.title}</span>
                  </button>
                </li>
              ))}
            </ul>
          </section>
        )}
        {navigationNotice && (
          <p className="attention-notice" role="status">
            {navigationNotice}
            <button
              type="button"
              onClick={() => setNavigationNotice("")}
            >
              Dismiss
            </button>
          </p>
        )}
        {page === "workspace" && (
          <Workspace
            state={state}
            onSelect={(task) => {
              setProject(task.project_id);
              openTask(task);
            }}
            onChanged={refresh}
            onOpenSetup={openSetup}
            onNavigateAttention={navigateAttention}
            onTaskAction={openTaskAction}
            onOpenPermission={openPermission}
            onViewCmuxSession={viewCmuxSession}
            onSetCmuxKeyboardControl={setCmuxSessionKeyboardControl}
            onDiscardCmuxSurface={discardCmuxSessionSurface}
          />
        )} {page === "board" && (
          <>
            <header className="page-heading">
              <div>
                <span className="eyebrow">Board</span>
                <h1>Tasks</h1>
                <p>
                  Drafts, queued and running work, and tasks waiting for your
                  review. Completed tasks are kept separately.
                </p>
              </div>
            </header>
            <TaskBoard
              tasks={tasks}
              projects={state.projects}
              sessions={state.active_sessions}
              taskActions={state.task_actions}
              permissionRequests={state.permission_requests}
              onTaskAction={openTaskAction}
              onOpenPermission={openPermission}
              onOpen={openTask}
              onEdit={(task) => {
                setEditing(task);
                setForm("edit");
              }}
              onChanged={refresh}
            />
          </>
        )}
        {page === "recipes" && (
          <Recipes
            key={project || "all"}
            state={state}
            project={state.projects.find((item) => item.id === project)}
            onChanged={refresh}
            onOpenTask={async (taskId) => {
              const sourceProject = project;
              await refresh();
              if (
                currentProject.current !== sourceProject ||
                currentPage.current !== "recipes"
              ) return;
              const task = live.current?.latest()?.tasks.find((item) =>
                item.id === taskId && item.project_id === sourceProject
              );
              if (task) {
                openTask(task);
              } else {
                setNavigationNotice(
                  "The draft was created, but its task detail is not in the latest state. Refresh and open it from the board.",
                );
              }
            }}
          />
        )}
        {page === "roles" && (
          <section>
            <header className="page-heading">
              <div>
                <span className="eyebrow">Projects and agents</span>
                <h1>Projects</h1>
                <p>
                  Project setup, verification checks and each task's agent
                  settings. A requested change takes effect only after it is
                  verified and applied.
                </p>
              </div>
            </header>
            <h2 className="settings-group-title">Project settings</h2>
            {state.projects.filter((item) => !project || item.id === project)
              .map((item) => {
                const setupResolution = resolveProjectSetup(
                  item,
                  state.trip_setups || [],
                );
                return (
                  <Fragment key={item.id}>
                    <ProjectSettings
                      project={item}
                      setup={setupResolution.setup}
                      tripChecks={(state.trip_checks || []).filter((check) =>
                        check.project_id === item.id &&
                        check.config_revision_id ===
                          item.trip?.active_config_revision_id
                      )}
                      cmuxSurfaces={cmuxSurfaces}
                      onChanged={refresh}
                      onViewSession={viewCmuxSession}
                    />
                    {setupResolution.warning && (
                      <ErrorNotice error={setupResolution.warning} />
                    )}
                  </Fragment>
                );
              })}
            {tasks.map((task) => (
              <RoleSettings
                key={task.id}
                task={task}
                project={state.projects.find((item) =>
                  item.id === task.project_id
                )}
                sessions={state.active_sessions}
                switches={state.switches}
                lanes={state.trip_lanes || []}
                productionRestrictions={state.production_role_restrictions}
                capabilities={state.capabilities}
                actions={state.continuation_actions}
                onChanged={refresh}
              />
            ))}
            {!tasks.length && (
              <p className="empty">
                Create a task to see and change its six agent roles.
              </p>
            )}
            <section className="panel capabilities">
              <h3>Agent verification records</h3>
              {state.capabilities.map((item, index) => (
                <div key={index}>
                  <strong>
                    {item.provider === "claude" ? "Claude" : "Codex"} ·{" "}
                    {stateLabel(String(item.role))}
                  </strong>
                  <span className={`badge ${item.status}`}>
                    {item.status === "supported"
                      ? "Verified"
                      : stateLabel(item.status)}
                  </span>
                  <small>
                    {stateLabel(item.mode)}
                    {item.version ? ` · ${item.version}` : ""} ·{" "}
                    {item.checked_at
                      ? `checked ${new Date(item.checked_at).toLocaleString()}`
                      : "never checked"}
                  </small>
                  {item.gaps.length > 0 && (
                    <small>Missing evidence: {item.gaps.join(" · ")}</small>
                  )}
                  <CompatibilityDetails compatibility={item.compatibility} />
                </div>
              ))}
            </section>
          </section>
        )}
        {page === "history" && (
          <History
            tasks={state.tasks}
            projects={state.projects}
            onOpen={openTask}
            onChanged={refresh}
          />
        )} {page === "diagnostics" && (
          <div data-attention-target="diagnostics" tabIndex={-1}>
            <DiagnosticsPanel
              capabilities={state.capabilities}
              setups={state.trip_setups || []}
              attention={state.attention}
            />
          </div>
        )}
      </main>
      {selected && (
        <TaskDetail
          key={selected.id}
          task={selected}
          state={state}
          selectedRecoveryId={attentionFocus?.kind === "recovery_record" &&
              attentionFocus.task_id === selected.id
            ? attentionFocus.recovery_id
            : undefined}
          settingsFocus={attentionFocus?.kind === "role_settings" &&
              attentionFocus.task_id === selected.id
            ? attentionFocus
            : undefined}
          tab={taskTabs[selected.id] || "overview"}
          onTabChange={(tab) =>
            setTaskTabs((current) => ({ ...current, [selected.id]: tab }))}
          onClose={() => {
            setSelectedId(undefined);
            setAttentionFocus(undefined);
          }}
          onChanged={refresh}
          onOpenSetup={(projectId) => {
            setSelectedId(undefined);
            openSetup(projectId);
          }}
          onTaskAction={openTaskAction}
          onOpenRecoveryRecord={(attemptId, recoveryId) =>
            openRecoveryRecord(selected.id, attemptId, recoveryId)}
          onOpenPermission={openPermission}
          onViewCmuxSession={viewCmuxSession}
          onSetCmuxKeyboardControl={setCmuxSessionKeyboardControl}
          onDiscardCmuxSurface={discardCmuxSessionSurface}
        />
      )} {form && (
        <TaskForm
          projects={state.projects}
          selectedProjectId={project}
          editing={editing}
          onClose={() => setForm(undefined)}
          onSaved={() => {
            setForm(undefined);
            void refresh();
          }}
          onOpenSetup={openSetup}
        />
      )}
    </div>
  );
}
