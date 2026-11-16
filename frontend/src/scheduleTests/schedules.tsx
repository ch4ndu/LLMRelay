import type { ReactNode } from "react";
import type { AppState, KnowledgePreview, KnowledgePreviewRequest, Project } from "../types";

type ProtocolFixtureResponder = () => Promise<Response>;
interface ScheduleTestContext {
  Recipes: typeof import("../components/Recipes").Recipes;
  liveBase: AppState;
  initialized: Project;
  noop: () => void;
  knowledgePreviewFixture: (request: KnowledgePreviewRequest, omitSecond?: boolean) => KnowledgePreview;
  knowledgeJson: (value: unknown, status?: number) => Response;
  mount: (node: ReactNode) => void;
  unmount: () => void;
  rerender: (node: ReactNode) => void;
  check: (condition: unknown, message: string) => void;
  click: (text: string, within?: ParentNode) => void;
  findButton: (text: string) => HTMLButtonElement;
  settleKnowledge: () => Promise<void>;
  toggleKnowledge: (label: string) => void;
  nativeFetch: typeof fetch;
  command: typeof import("../api").command;
  getState: typeof import("../api").getState;
  recipeScheduleExecutionPreview: typeof import("../api").recipeScheduleExecutionPreview;
  protocolFixture: (features?: string[], instanceId?: string) => Response;
  allFeatures: string[];
  ProtocolError: typeof import("../api").ProtocolError;
  ApiError: typeof import("../api").ApiError;
  snapshotAt: (incarnation: string, revision: string, overrides?: Partial<AppState>) => AppState;
  getProtocolFixtureResponder: () => ProtocolFixtureResponder;
  setProtocolFixtureResponder: (responder: ProtocolFixtureResponder) => void;
}

export function registerScheduleTests({
  Recipes, liveBase, initialized, noop, knowledgePreviewFixture, knowledgeJson,
  mount, unmount, rerender, check, click, findButton, settleKnowledge, toggleKnowledge,
  nativeFetch, command, getState, recipeScheduleExecutionPreview, protocolFixture,
  allFeatures, ProtocolError, ApiError, snapshotAt,
  getProtocolFixtureResponder, setProtocolFixtureResponder,
}: ScheduleTestContext): void {
  function researchScheduleFixture(): import("../types").RecipeSchedule {
    return { id: "schedule-research", project_id: "p1", name: "Daily research", version: 7, archived: false, paused: true,
      recipe_revision_id: "recipe-reader-rev", recipe_name: "Reader", recipe_config_revision_id: "reader-config", recipe_archived: false,
      recipe_family: "single_agent", recipe_preset: "repository_research", cadence: "daily", anchor_utc: "2026-11-10T09:00:00Z", next_fire_utc: null,
      last_fire: null, mode: "create_draft", active_authorization_id: null, authorization: null, execution_intake_enabled: true, occurrences: [] };
  }
  function researchPreviewFixture(request: import("../types").RecipeSchedulePreviewRequest): import("../types").RecipeScheduleExecutionPreview {
    return { digest: "server-exact-schedule-digest", expected_version: request.expected_version, scope: {
      schema: 1, project_id: request.project_id, schedule_id: request.schedule_id, recipe_revision_id: "recipe-reader-rev",
      recipe: { id: "recipe-reader-rev", revision: 1, title: "Research repository", description: "Read only", acceptance_criteria: ["Evidence"], priority: 0, definition_id: "repository_research", workflow_version: "1", workflow_hash: "workflow-hash" },
      admission: { project_id: request.project_id, repository_identity: "repository-id", repository_path: "/fixture/repository", base_revision: "base-commit",
        execution_config_revision_id: "reader-config", execution_config_hash: "config-hash", profile_revision_id: "reader-profile", profile_hash: "profile-hash",
        provider: "codex", policy_hash: "policy-hash", adapter_hash: "adapter-hash", admission_id: "qualified-reader", prepared_scope_hash: "prepared-hash",
        capability_key: "capability-key", capability_id: "capability-id", capability_proof_hash: "proof-hash", native_identity_hash: "native-hash", effective_policy_hash: "policy-hash", guidance: [] },
      knowledge: knowledgePreviewFixture({ project_id: request.project_id, consumer: { kind: "schedule", schedule_id: request.schedule_id }, expected_consumer_version: request.expected_version, user_text: "", selection: request.knowledge_selection }),
      cadence: "daily", anchor_utc: "2026-11-10T09:00:00Z", limits: request.limits, deadline_stop_policy: "interrupt_then_identity_verified_force_stop_within_runtime",
    } };
  }
  function scheduleView(schedule = researchScheduleFixture(), onChanged = noop, onOpenTask: (id: string) => void = noop) {
    return <Recipes state={{ ...liveBase, recipe_schedules: [schedule] }} project={initialized} onChanged={onChanged} onOpenTask={onOpenTask} />;
  }
  function scheduleReceiptFixture(body: Record<string, unknown>): Response {
    const request = body.request as import("../types").RecipeSchedulePreviewRequest;
    const binding = body.binding as import("../types").ScheduleStopBinding;
    if (body.kind === "request_recipe_schedule_occurrence_stop") {
      const intent = body.intent as import("../types").ScheduleOccurrenceStopIntent;
      return knowledgeJson({ result: { operation_id: body.operation_id, entity_kind: "recipe_schedule_occurrence", entity_id: intent.occurrence_id, version: intent.expected_version, state: "stop_requested", detail: { intent } } });
    }
    return knowledgeJson({ result: body.kind === "authorize_recipe_schedule_execution"
      ? { operation_id: body.operation_id, entity_kind: "recipe_schedule", entity_id: request.schedule_id, version: request.expected_version + 1, state: "authorized", detail: { authorization_id: "grant", scope_hash: body.preview_digest, scheduled_execution_intake_enabled: true } }
      : { operation_id: body.operation_id, entity_kind: "recipe_schedule_occurrence", entity_id: binding.occurrence_id, version: body.expected_version, state: "stop_requested", detail: { binding } } });
  }
  Deno.test("schedule dashboard retains draft-only controls and refuses retired authorization without transport", async () => {
    sessionStorage.clear(); let posts = 0;
    globalThis.fetch = async () => { posts++; return knowledgeJson({ result: {} }); };
    try {
      mount(scheduleView());
      check(!document.body.textContent?.includes("Preview authorized research") && !document.querySelector('[aria-label="Schedule execution authorization"]'), "retired research authorization was offered");
      const request: import("../types").RecipeSchedulePreviewRequest = { project_id: "p1", schedule_id: "schedule-research", expected_version: 7, limits: { max_occurrences: 1, max_invocations_per_occurrence: 1, max_runtime_seconds: 60, start_window_seconds: 60 }, knowledge_selection: [] };
      let refused: unknown;
      try { await recipeScheduleExecutionPreview(request); } catch (cause) { refused = cause; }
      check(refused instanceof Error && refused.message.includes("retired") && posts === 0, "retired preview contacted the service");
      try { await command({ kind: "authorize_recipe_schedule_execution", operation_id: "stale-authorization", request, preview_digest: "old" }); } catch (cause) { refused = cause; }
      check(refused instanceof Error && refused.message.includes("retired") && posts === 0, "stale authorization contacted the service");
    } finally { unmount(); globalThis.fetch = nativeFetch; sessionStorage.clear(); }
  });

  Deno.test("schedule dashboard separates future pause from exact current stop and retains uncertain stop identity", async () => {
    sessionStorage.clear(); const commands: Record<string, unknown>[] = []; const opened: string[] = [];
    const binding: import("../types").ScheduleStopBinding = { occurrence_id: "occurrence", authorization_id: "grant", attempt_id: "attempt-exact", session_id: "session-exact", resume_invocation_id: "resume-exact", role_generation_id: "generation-exact", transcript_epoch: "epoch-exact", process: { pid: 120, process_group_id: 120, native_start_marker: "native-marker", observed_started_at: "2026-11-10T10:00:00Z" } };
    const occurrence: import("../types").RecipeScheduleOccurrence = { id: "occurrence", authorization_id: "grant", scheduled_for_utc: "2026-11-10T09:00:00Z", task_id: "task-exact", attempt_id: "attempt-exact", outcome: "running", reason: null, allowance_reserved: true, invocations_spent: 2, runtime_deadline_utc: "2026-11-10T10:10:00Z", stop_binding: binding };
    const schedule = { ...researchScheduleFixture(), paused: false, mode: "run_authorized_research" as const, occurrences: [occurrence] };
    let stopLost = true;
    globalThis.fetch = async (_url, init) => {
      const body = JSON.parse(String(init?.body)); commands.push(body);
      if (body.kind === "stop_recipe_schedule_occurrence" && stopLost) { stopLost = false; throw new TypeError("Stop receipt lost"); }
      return body.kind === "pause_recipe_schedule" ? knowledgeJson({ result: { entity_id: schedule.id, version: 7, state: "paused" } }) : scheduleReceiptFixture(body);
    };
    try {
      mount(scheduleView(schedule, noop, (id) => opened.push(id)));
      click("Pause future runs"); await settleKnowledge();
      check(commands.length === 1 && commands[0].kind === "pause_recipe_schedule" && !commands[0].binding && document.body.textContent?.includes("does not stop this current run"), "pause implied or sent a current-process stop");
      click("Open occurrence task"); check(opened.join() === "task-exact" && document.body.textContent?.includes("attempt-exact"), "history lost task navigation or exact recorded attempt");
      click("Stop current run"); await settleKnowledge();
      check(commands.length === 2 && commands[1].expected_version === 7 && JSON.stringify(commands[1].binding) === JSON.stringify(binding), "stop reconstructed or dropped exact process/resume binding");
      check(findButton("Retry exact pending request").disabled, "uncertain stop allowed blind replay");
      toggleKnowledge("I checked this schedule’s outcome"); click("Retry exact pending request"); await settleKnowledge();
      check(commands.length === 3 && JSON.stringify(commands[1]) === JSON.stringify(commands[2]), "stop receipt recovery changed immutable identity");
      rerender(scheduleView({ ...schedule, occurrences: [{ ...occurrence, outcome: "recovery_required", stop_binding: null, reason: "Current invocation identity is unavailable" }] }));
      check(findButton("Stop current run").disabled && document.body.textContent?.includes("No exact current process target") && document.body.textContent?.includes("Recovery required"), "unknown target permitted signalling or claimed stopped");
    } finally { unmount(); globalThis.fetch = nativeFetch; sessionStorage.clear(); }
  });

  Deno.test("schedule dashboard presents grant history, server qualification refusal and unsupported execution honestly", async () => {
    sessionStorage.clear(); let previews = 0;
    globalThis.fetch = async () => { previews++; return knowledgeJson({ error: "Activate the exact qualified reader admission before authorizing this schedule" }, 409); };
    const schedule = researchScheduleFixture();
    const request: import("../types").RecipeSchedulePreviewRequest = { project_id: "p1", schedule_id: schedule.id, expected_version: 7, limits: { max_occurrences: 3, max_invocations_per_occurrence: 1, max_runtime_seconds: 600, start_window_seconds: 300 }, knowledge_selection: [] };
    const grant = { id: "grant", scope_hash: "scope-hash", scope: researchPreviewFixture(request).scope, created_at: "2026-11-10", revoked_at: null, restore_invalidated_at: null, ...request.limits, reserved_occurrences: 1, remaining_occurrences: 2 };
    try {
      mount(scheduleView({ ...schedule, mode: "run_authorized_research", active_authorization_id: "grant", authorization: grant, occurrences: [{ id: "held", authorization_id: "grant", scheduled_for_utc: "2026-11-10T09:00:00Z", task_id: null, attempt_id: null, outcome: "held", reason: "Admission changed; review Project settings", allowance_reserved: false, invocations_spent: 0, runtime_deadline_utc: null }] }));
      check(document.body.textContent?.includes("2 occurrences remaining") && document.body.textContent.includes("1 reserved of 3") && document.body.textContent.includes("held") && !document.body.textContent.includes("Recorded attempt"), "grant count or no-execution history was misrepresented");
      check(document.body.textContent?.includes("Automatic execution is retired") && previews === 0, "historical automatic mode was relabelled or acquired authority");
      check(findButton("Retired").disabled, "historical schedule could resume");
      check(!document.body.textContent?.includes("Preview authorized research") && !document.body.textContent?.includes("Confirm research authorization"), "retired grant controls remained reachable");
    } finally { unmount(); globalThis.fetch = nativeFetch; sessionStorage.clear(); }
  });

  function writerScheduleFixture(preset: "single_agent" | "build_and_review" = "build_and_review"): import("../types").RecipeSchedule {
    return { ...researchScheduleFixture(), id: "schedule-writer", name: "Daily writer", recipe_revision_id: "writer-rev", recipe_name: "Writer", recipe_config_revision_id: "writer-config", recipe_preset: preset,
      execution_authorization_supported: true, execution_authorization_reason: "Review an authorization preview to verify current profile qualification, Knowledge, checks and limits", execution_qualification_state: "preview_required" };
  }
  function writerRecipeFixture(preset: "single_agent" | "build_and_review"): import("../types").TaskRecipe {
    return { id: "writer", project_id: "p1", name: "Writer", version: 1, archived: false, revision: 1, revision_id: "writer-rev",
      family: "single_agent", preset, definition_id: preset, execution_config_revision_id: "writer-config", primary_profile_revision_id: "writer-profile",
      reviewer_profile_revision_id: preset === "build_and_review" ? "reviewer-profile" : null, title: "Implement change", description: "Bounded writer", acceptance_criteria: ["Reviewed output"], priority: 0,
      profile_revision_id: null, required_check_ids: ["candidate-check"], config_revision_id: null, configuration_hash: "config-hash", workflow_version: "1", workflow_hash: "workflow-hash" };
  }
  function writerPreviewFixture(request: import("../types").RecipeSchedulePreviewRequest, preset: "single_agent" | "build_and_review" = "build_and_review"): import("../types").RecipeScheduleExecutionPreview {
    const research = researchPreviewFixture(request);
    const admission: import("../types").RecipeScheduleAdmission & { session_contract: "retained" } = {
      ...research.scope.admission, execution_config_revision_id: "writer-config", profile_revision_id: "writer-profile", admission_id: "qualified-writer", session_contract: "retained",
    };
    const base = { ...research.scope, schema: 2 as const, recipe_revision_id: "writer-rev",
      recipe: { ...research.scope.recipe, id: "writer-rev", title: "Implement change", definition_id: preset }, admission,
      workspace_mode: "isolated_worktree" as const, check_ids: ["candidate-check"], check_execution_policy: "per_candidate_human_authorization" as const,
      writer_resume_policy: "explicit_retained_only_within_allowance" as const, human_request_changes_policy: "separate_draft_and_new_grant_required" as const };
    return { ...research, scope: preset === "single_agent"
      ? { ...base, preset, reviewer_admission: null, max_repair_attempts: 0 }
      : { ...base, preset, reviewer_admission: { ...admission, profile_revision_id: "reviewer-profile", admission_id: "qualified-reviewer", session_contract: "fresh" }, max_repair_attempts: 1 } };
  }
  function writerScheduleView(schedule = writerScheduleFixture(), project = initialized) {
    return <Recipes state={{ ...liveBase, task_recipes: [writerRecipeFixture(schedule.recipe_preset === "single_agent" ? "single_agent" : "build_and_review")], recipe_schedules: [schedule] }} project={project} onChanged={noop} onOpenTask={noop} />;
  }
  Deno.test("schedule dashboard retains original writer preset history without automatic execution controls", async () => {
    sessionStorage.clear(); let posts = 0;
    globalThis.fetch = async () => { posts++; return knowledgeJson({ result: {} }); };
    try {
      for (const preset of ["single_agent", "build_and_review"] as const) {
        mount(writerScheduleView({ ...writerScheduleFixture(preset), mode: "run_authorized_recipe", paused: true }));
        check(document.body.textContent?.includes("Automatic execution is retired") && findButton("Retired").disabled, "automatic writer history could resume");
        check(!document.body.textContent?.includes("Preview authorized recipe") && !document.querySelector('[aria-label="Schedule execution authorization"]'), "automatic writer controls remained");
      }
      check(posts === 0, "historical writer inspection sent a request");
    } finally { unmount(); globalThis.fetch = nativeFetch; sessionStorage.clear(); }
  });

  Deno.test("schedule dashboard typed Stop covers waiting and preparation and recovers only its original receipt", async () => {
    const previousProtocol = getProtocolFixtureResponder();
    sessionStorage.clear();
    setProtocolFixtureResponder(() => Promise.resolve(protocolFixture([...allFeatures, "schedule_occurrence_stop_intent_v1"], "typed-stop-dashboard")));
    globalThis.fetch = async () => knowledgeJson(snapshotAt("typed-stop-dashboard", "1"));
    await getState();
    const schedule = writerScheduleFixture();
    const intent: import("../types").ScheduleOccurrenceStopIntent = { project_id: "p1", schedule_id: schedule.id, expected_version: 7, occurrence_id: "waiting-occurrence", authorization_id: "original-grant" };
    const occurrence: import("../types").RecipeScheduleOccurrence = { id: intent.occurrence_id, authorization_id: intent.authorization_id, scheduled_for_utc: schedule.anchor_utc, task_id: "waiting-task", attempt_id: null, outcome: "waiting", reason: null, allowance_reserved: true, invocations_spent: 0, runtime_deadline_utc: null,
      stop_binding: null, stop_intent: intent, stop_intent_available: true, stop_intent_reason: null, stop_requested_at: null, stop_reason: null };
    const commands: Record<string, unknown>[] = [];
    globalThis.fetch = async (_url, init) => {
      const body = JSON.parse(String(init?.body)); commands.push(body);
      const protocol = JSON.parse(new Headers(init?.headers).get("x-llmrelay-protocol") || "{}");
      check(protocol.required_features.includes("schedule_occurrence_stop_intent_v1"), "typed Stop omitted its negotiated feature header");
      if (commands.length === 1) throw new TypeError("Typed Stop receipt lost");
      if (commands.length === 2) return scheduleReceiptFixture({ ...body, intent: { ...intent, authorization_id: "unrelated-grant" } });
      return scheduleReceiptFixture(body);
    };
    try {
      mount(writerScheduleView({ ...schedule, occurrences: [occurrence] }));
      check(!findButton("Stop current run").disabled, "waiting occurrence without a process could not request Stop");
      rerender(writerScheduleView({ ...schedule, occurrences: [{ ...occurrence, outcome: "reserved", attempt_id: "preparing-root" }] }));
      click("Stop current run"); await settleKnowledge();
      check(commands.length === 1 && commands[0].kind === "request_recipe_schedule_occurrence_stop" && Object.keys(commands[0]).sort().join() === "intent,kind,operation_id" && JSON.stringify(commands[0].intent) === JSON.stringify(intent), "preparation Stop changed the typed wire body or required a fabricated process");
      const saved = sessionStorage.getItem("llmrelay.m9.pending-commands.v1");
      check(saved?.includes('"project_id":"p1"') && saved.includes('p1:schedule:schedule-writer') && findButton("Retry exact pending request").disabled, "unknown Stop lost its journal context or allowed blind retry");
      mount(writerScheduleView({ ...schedule, occurrences: [{ ...occurrence, outcome: "recovery_required", stop_intent: null, stop_intent_available: false, stop_requested_at: "2026-11-10T10:00:00Z", stop_intent_reason: "Stop requested; recovery required" }] }));
      check(commands.length === 1 && findButton("Stop current run").disabled && document.body.textContent?.includes("waiting for verified preparation and process quiescence"), "remount replayed authority or persisted intent claimed immediate completion");
      click("Refresh schedule outcome"); toggleKnowledge("I checked this schedule’s outcome"); click("Retry exact pending request"); await settleKnowledge();
      check(commands.length === 2 && sessionStorage.getItem("llmrelay.m9.pending-commands.v1") === saved && findButton("Retry exact pending request").disabled, "a receipt for another grant settled Stop or enabled automatic retry");
      click("Refresh schedule outcome"); toggleKnowledge("I checked this schedule’s outcome"); click("Retry exact pending request"); await settleKnowledge();
      check(commands.length === 3 && commands.every((body) => JSON.stringify(body) === JSON.stringify(commands[0])) && sessionStorage.getItem("llmrelay.m9.pending-commands.v1") === "[]", "Stop recovery changed the original operation or exact intent");
      rerender(writerScheduleView({ ...schedule, occurrences: [occurrence] }));
      check(findButton("Stop current run").disabled && document.body.textContent?.includes("waiting for verified preparation and process quiescence"), "receipt recovery renewed Stop authority while awaiting the refreshed projection");
    } finally { unmount(); globalThis.fetch = nativeFetch; setProtocolFixtureResponder(previousProtocol); sessionStorage.clear(); }
  });

  Deno.test("schedule API refuses missing typed Stop feature and stale exact receipt identities", async () => {
    const previousProtocol = getProtocolFixtureResponder();
    const intent: import("../types").ScheduleOccurrenceStopIntent = { project_id: "p1", schedule_id: "schedule-writer", expected_version: 7, occurrence_id: "occurrence", authorization_id: "grant" };
    const body: Extract<import("../types").ScheduleExecutionCommand, { kind: "request_recipe_schedule_occurrence_stop" }> = { kind: "request_recipe_schedule_occurrence_stop", operation_id: "stop-original-operation", intent };
    let posts = 0;
    try {
      setProtocolFixtureResponder(() => Promise.resolve(protocolFixture(allFeatures, "stop-feature-absent")));
      globalThis.fetch = async () => knowledgeJson(snapshotAt("stop-feature-absent", "1"));
      await getState();
      globalThis.fetch = async () => { posts++; return scheduleReceiptFixture(body); };
      let refusal: unknown;
      try { await command(body); } catch (cause) { refusal = cause; }
      check(refusal instanceof ProtocolError && posts === 0, "missing Stop feature dispatched a mutation");
      setProtocolFixtureResponder(() => Promise.resolve(protocolFixture([...allFeatures, "schedule_occurrence_stop_intent_v1"], "stop-feature-present")));
      globalThis.fetch = async () => knowledgeJson(snapshotAt("stop-feature-present", "1"));
      await getState();
      for (const stale of [{ ...intent, project_id: "p2" }, { ...intent, schedule_id: "another-schedule" }, { ...intent, expected_version: 6 }, { ...intent, occurrence_id: "old-occurrence" }]) {
        globalThis.fetch = async () => { posts++; return scheduleReceiptFixture({ ...body, intent: stale }); };
        let result: unknown;
        try { await command(body); } catch (cause) { result = cause; }
        check(result instanceof ApiError && result.ambiguous && result.operationId === body.operation_id, "stale receipt identity was accepted or lost its original operation");
      }
      globalThis.fetch = async () => scheduleReceiptFixture({ ...body, operation_id: "another-operation" });
      let unrelated: unknown;
      try { await command(body); } catch (cause) { unrelated = cause; }
      check(unrelated instanceof ApiError && unrelated.ambiguous && unrelated.operationId === body.operation_id, "unrelated operation receipt was accepted");
      globalThis.fetch = async () => { posts++; return scheduleReceiptFixture(body); };
      const before = posts;
      let invalid: unknown;
      try { await command({ ...body, project_id: "p2" }); } catch (cause) { invalid = cause; }
      check(invalid instanceof Error && posts === before, "mismatched journal project context was sent");
    } finally { globalThis.fetch = nativeFetch; setProtocolFixtureResponder(previousProtocol); }
  });

  Deno.test("schedule API accepts retired schedule projections and writer child stops but rejects stale context", async () => {
    const schedule: import("../types").RecipeSchedule = {
      ...writerScheduleFixture(),
      execution_intake_enabled: false,
      execution_authorization_supported: false,
      execution_authorization_reason: "Automatic recipe execution has been retired. Schedules create drafts only.",
      execution_qualification_state: "unsupported",
    };
    const draft: import("../types").RecipeSchedule = {
      ...schedule, id: "schedule-draft", recipe_preset: "single_agent",
    };
    const request: import("../types").RecipeSchedulePreviewRequest = { project_id: schedule.project_id, schedule_id: schedule.id, expected_version: schedule.version,
      limits: { max_occurrences: 2, max_invocations_per_occurrence: 4, max_runtime_seconds: 600, start_window_seconds: 300 }, knowledge_selection: [] };
    const preview = writerPreviewFixture(request);
    const intent: import("../types").ScheduleOccurrenceStopIntent = { project_id: schedule.project_id, schedule_id: schedule.id, expected_version: schedule.version, occurrence_id: "root-occurrence", authorization_id: "root-grant" };
    const binding: import("../types").ScheduleStopBinding = { occurrence_id: intent.occurrence_id, authorization_id: intent.authorization_id, attempt_id: "repair-child", session_id: "child-session", resume_invocation_id: null, role_generation_id: "child-generation", transcript_epoch: "child-epoch",
      process: { pid: 123, process_group_id: 123, native_start_marker: "child-start", observed_started_at: "2026-11-10T10:00:00Z" } };
    const occurrence: import("../types").RecipeScheduleOccurrence = { id: intent.occurrence_id, authorization_id: intent.authorization_id, scheduled_for_utc: schedule.anchor_utc, task_id: "root-task", attempt_id: "immutable-root", outcome: "running", reason: null, allowance_reserved: true, invocations_spent: 3, runtime_deadline_utc: "2026-11-10T10:10:00Z",
      stop_binding: binding, stop_intent: intent, stop_intent_available: true, stop_intent_reason: null, stop_requested_at: null, stop_reason: null };
    const active: import("../types").RecipeSchedule = { ...schedule, mode: "run_authorized_recipe", active_authorization_id: intent.authorization_id,
      authorization: { ...request.limits, id: intent.authorization_id, scope_hash: preview.digest, scope: preview.scope, created_at: schedule.anchor_utc, revoked_at: "2026-11-15T00:00:00Z", restore_invalidated_at: null, reserved_occurrences: 1, remaining_occurrences: 1 }, occurrences: [occurrence] };
    try {
      globalThis.fetch = async () => knowledgeJson(snapshotAt("writer-projection", "1", { recipe_schedules: [draft, active] }));
      const state = await getState();
      check(state.recipe_schedules[0].mode === "create_draft" && state.recipe_schedules[0].execution_authorization_supported === false && state.recipe_schedules[0].execution_qualification_state === "unsupported", "draft-only backend projection was rejected or changed");
      check(state.recipe_schedules[1].mode === "run_authorized_recipe" && state.recipe_schedules[1].paused && state.recipe_schedules[1].active_authorization_id === intent.authorization_id && state.recipe_schedules[1].authorization?.revoked_at === "2026-11-15T00:00:00Z" && state.recipe_schedules[1].execution_authorization_reason === schedule.execution_authorization_reason, "retired automatic projection lost its original mode, grant history or retirement reason");
      const recorded = state.recipe_schedules[1].occurrences?.[0];
      check(recorded?.attempt_id === "immutable-root" && recorded.stop_binding?.attempt_id === "repair-child", "writer child binding replaced its immutable root or failed projection validation");
      globalThis.fetch = async () => knowledgeJson(snapshotAt("writer-projection", "2", { recipe_schedules: [{ ...active, occurrences: [{ ...occurrence, stop_intent: { ...intent, expected_version: 6 } }] }] }));
      let stale: unknown;
      try { await getState(); } catch (cause) { stale = cause; }
      check(stale instanceof Error, "stale Stop context was accepted as current state");
    } finally { globalThis.fetch = nativeFetch; }
  });
}
