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
import type {
  AppState,
  AttentionItem,
  AttentionTarget,
  CmuxKeyboardControlAction,
  CmuxSessionSurface,
  Project,
  Task,
  TripSetupState,
} from "./types";
import {
  attentionFocusElement,
  attentionTargetProblem,
} from "./components/AttentionInbox";
import { ProjectPicker } from "./components/ProjectPicker";
import { ProjectSettings } from "./components/ProjectSettings";
import { TaskBoard } from "./components/TaskBoard";
import { TaskForm } from "./components/TaskForm";
import { TaskDetail } from "./components/TaskDetail";
import { Workspace } from "./components/Workspace";
import { History } from "./components/History";
import { Recipes } from "./components/Recipes";
import { DiagnosticsPanel } from "./components/DiagnosticsPanel";
import { CompatibilityDetails, RoleSettings } from "./components/RoleSettings";
type Page = "workspace" | "board" | "recipes" | "roles" | "history" | "diagnostics";
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
    if (value === "recipes") {
      setSelectedId(undefined);
      setForm(undefined);
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
    if (
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
      default:
        setSelectedId(target.task_id);
    }
    // A fresh object re-renders, and so re-focuses, a repeated click.
    setAttentionFocus({ ...target });
    pendingFocus.current = { title: item.title, target };
    return undefined;
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
  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="logo">LR</span>
          <div>
            <strong>LLMRelay</strong>
            <small>Local workspace</small>
          </div>
        </div>
        <nav aria-label="Main navigation">
          {(["workspace", "board", "recipes", "roles", "history", "diagnostics"] as Page[])
            .map((item) => (
              <button
                key={item}
                className={page === item ? "active" : ""}
                onClick={() => nav(item)}
              >
                <span>
                  {item === "workspace"
                    ? "◫"
                    : item === "board"
                    ? "▦"
                    : item === "recipes"
                    ? "▤"
                    : item === "roles"
                    ? "▣"
                    : item === "history"
                    ? "◴"
                    : "⚙"}
                </span>
                {item === "roles"
                  ? "Projects"
                  : item[0].toUpperCase() + item.slice(1)}
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
        <footer
          title={lastSuccessfulAt || state.generated_at
            ? `Last successful update: ${
              new Date(lastSuccessfulAt || state.generated_at).toLocaleString()
            }`
            : undefined}
        >
          <span
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
      </aside>
      <main className="content">
        <header className="topbar">
          <div>
            {state.projects.find((v) => v.id === project)?.display_name ||
              "All projects"}
            <small>
              {tasks.filter((t) => t.lifecycle === "in_progress").length}{" "}
              in progress · {state.attention.filter((item) =>
                !item.target || !project || item.target.project_id === project
              ).length} need attention
            </small>
          </div>
          <div className="button-row">
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
            onViewCmuxSession={viewCmuxSession}
            onSetCmuxKeyboardControl={setCmuxSessionKeyboardControl}
            onDiscardCmuxSurface={discardCmuxSessionSurface}
          />
        )} {page === "board" && (
          <>
            <header className="page-heading">
              <div>
                <span className="eyebrow">Task-only workflow</span>
                <h1>Task board</h1>
                <p>
                  Draft, queue, execute, review, accept, and retain each task’s
                  evidence.
                </p>
              </div>
            </header>
            <TaskBoard
              tasks={tasks}
              projects={state.projects}
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
                <h1>Project control center</h1>
                <p>
                  Requested revisions stay distinct from effective and running
                  generations.
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
                Create a task to inspect its host manager and five delegated
                roles.
              </p>
            )}
            <section className="panel capabilities">
              <h3>Capability evidence</h3>
              {state.capabilities.map((item, index) => (
                <div key={index}>
                  <strong>{String(item.provider)} · {String(item.role)}</strong>
                  <span className={`badge ${item.status}`}>
                    Production {stateLabel(item.status)}
                  </span>
                  <small>
                    Validation observation: {stateLabel(item.mode)}
                    {item.version ? ` · ${item.version}` : ""} ·{" "}
                    {item.checked_at || "never checked"}
                  </small>
                  {item.gaps.length > 0 && (
                    <small>Evidence gaps: {item.gaps.join(" · ")}</small>
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
        )} {page === "diagnostics" && <DiagnosticsPanel capabilities={state.capabilities} setups={state.trip_setups || []} />}
      </main>
      {selected && (
        <TaskDetail
          task={selected}
          state={state}
          selectedRecoveryId={attentionFocus?.kind === "recovery_record" &&
              attentionFocus.task_id === selected.id
            ? attentionFocus.recovery_id
            : undefined}
          onClose={() => {
            setSelectedId(undefined);
            setAttentionFocus(undefined);
          }}
          onChanged={refresh}
          onOpenSetup={openSetup}
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
