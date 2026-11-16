# Interactive agent workspace, comparisons, and project knowledge

Status: proposed implementation plan; no implementation or native execution authorized by this document.

Date: November 7, 2026.

## Outcome and scope

Make LLMRelay useful for everyday conversations with coding agents, optional promotion into established workflows, evidence-based workflow/provider comparisons, and reusable project knowledge with inspectable sources. Preserve TRIP and Single agent as supported choices.

This plan follows the user's selected three stages: prove the interactive foundation; deliver the everyday workspace; add repeatable work. Comparisons and project knowledge are first-class deliverables, not optional follow-ups.

Included:

- A structured Codex integration alongside the existing terminal adapters.
- Project conversations, streamed activity, follow-up input, interruption, and recovery.
- Explicit conversion of a conversation into Single agent or TRIP work.
- Completion of the native qualification needed for the existing Repository research and Build and review presets.
- Workflow/provider comparisons with outcome, duration, rework, and coverage information.
- Project-scoped, human-approved knowledge with immutable revisions and source references.
- Opt-in, bounded execution of existing recipes on a schedule.

Not prioritized in this plan: advanced inline code-review tooling, a review-to-integration pipeline, Git commit/merge/PR controls, and issue/CI intake. Existing Changes, Checks, and acceptance flows remain available.

Excluded: a full editor, general workflow builder, broad computer automation, remote workers, expanded same-repository concurrency, a plugin marketplace, and general browser automation. No cloud service or cross-device synchronization is introduced.

## Verified starting points

| Existing seam | Implementation | Planning implication |
| --- | --- | --- |
| Native launches and supervision | `src/providers/{mod,codex,claude}.rs`, `src/supervisor.rs` | Retain current adapters; a structured adapter needs distinct compatibility and qualification identities. |
| Workflow contracts | `src/workflow.rs`, `src/workflow_resources.rs`, `src/domain.rs` | Reuse existing task workflows; do not represent a conversation as a TRIP role. |
| Workspace ownership | `src/workspace.rs`, `src/scheduler.rs` | Preserve one active coding task per repository and the current global capacity. |
| Authenticated dashboard API | `src/server.rs`, `frontend/src/api.ts`, `frontend/src/types.ts` | Provider events reach the browser through LLMRelay, not a browser-to-provider connection. |
| Existing task interaction | `frontend/src/components/TaskDetail.tsx`, `WorkflowControls.tsx`, `SessionHistory.tsx` | Keep existing task controls functional while introducing conversations. |
| Usage comparisons | `frontend/src/components/UsageComparisons.tsx`, history API/store paths | Existing values are partial Codex observations; unavailable Claude values must not become zero. |
| Historical evidence | `frontend/src/components/HistorySearch.tsx`, history source API/store paths | Reuse source identities and exact-source navigation for knowledge. |
| Recipes and schedules | `src/recipes.rs`, `frontend/src/components/Recipes.tsx` | Draft creation already exists; execution is a distinct opt-in policy. |
| Durable storage/recovery | `src/database.rs`, `src/store.rs`, `src/recovery.rs` | Extend existing migrations, transactional operations, backup, and recovery conventions. |

The current Codex adapter disables several app tools, including memory and plugins. Project knowledge in this plan is owned by LLMRelay; it does not require enabling provider memory. Research/reviewer policies remain unavailable pending native qualification (`docs/WORKFLOWS.md`). Roadmap documents contain historical checkpoints; source and current qualification evidence determine readiness.

## Proposed architecture contracts

These are recommended defaults for implementation review, not newly approved runtime permissions.

1. **Ownership:** LLMRelay owns project/task identity, execution authority, user decisions, persisted UI events, comparisons, and knowledge. The provider owns its native thread and turn history. Store references and UI projections rather than attempting to recreate the provider's private conversation store.
2. **Adapter boundary:** add a versioned Codex app-server adapter using local stdio. Pin support to an inspected installed CLI/schema. Do not replace PTY supervision assumptions globally or silently move existing sessions to the new adapter. Unsupported capabilities are visible.
3. **Conversation boundary:** a conversation is a project-scoped entity with provider/thread binding and explicit execution policy. First release uses a qualified read-only exploration policy; file edits require explicit promotion into an admitted task. Read-only access still requires a defined tool/network scope.
4. **Single owner:** LLMRelay owns threads it creates. Do not attach to an arbitrary existing Codex app session or allow two runtime controllers to submit turns concurrently. One active turn per conversation; preserve existing task/repository claims and global resource limits.
5. **Durable commands:** record a user operation before delivery and correlate it with provider thread/turn identity. A lost acknowledgement is an unknown outcome, not permission to send again. Reconcile using supported provider history/state; expose recovery when reconciliation cannot establish the outcome.
6. **Events:** project a small set of typed events: user message, agent message, tool activity, permission request/resolution, usage observation, turn status, and error. Persist stable correlation and local ordering; handle duplicate, late, and out-of-order notifications. Rebuild the UI projection where possible without repeating execution.
7. **Permissions:** translate provider requests into exact app-owned decisions. Stale requests cannot authorize a replacement turn. Preserve native sandbox/policy enforcement and do not turn a chat message or knowledge entry into authority.
8. **Storage:** use additive, versioned SQLite records within the existing instance data directory. Preserve old tasks and history; no automatic import of external provider history. Include new records in backup/restore. Retained sensitive content follows existing authenticated access and export exclusions.
9. **Scope continuity:** no silent provider/model substitution, no automatic cross-provider conversation migration, no automatic knowledge approval, and no scheduled acceptance or integration.

## Stage 1 — Prove the interactive foundation

### 1A. Define and implement the bounded adapter

Deliver one app-owned Codex conversation in one project behind an explicit experimental capability. Inspect the installed app-server protocol and generate or capture its exact schema during development. Establish initialization, thread creation/resume, turn start, streaming, permission handling, interruption, and supported reconciliation behavior.

Likely owners: new focused module under `src/providers/`; `src/providers/mod.rs`, `src/provider_compatibility.rs`, `src/domain.rs`, `src/supervisor.rs`, `src/database.rs`, `src/store.rs`, `src/recovery.rs`, and `src/server.rs` at their relevant seams. Add small typed frontend API contracts, not the complete workspace UI yet.

Define the comparison event contract here: provider, exact model/effort, adapter revision, workflow/definition revision, profile revision, task/run/turn identity, timestamps, terminal outcome, and usage coverage. Unknown values remain explicit. Capture new data prospectively; historical backfill uses only facts actually recorded.

### 1B. Verify lifecycle and authorization behavior

Focused protocol/contract cases:

- Start, stream, complete, follow up, and interrupt a turn.
- Deny a permission request; ensure the denied action does not proceed.
- Disconnect before acknowledgement and after delivery; do not duplicate the turn.
- Restart LLMRelay or lose the provider process; distinguish resumable, failed, and unknown states.
- Reject stale approvals, duplicate submissions, unexpected response identities, and unsupported versions.
- Bound event payloads and transcript growth; preserve existing credential redaction.
- Keep old PTY sessions and both existing workflows functional.

Use existing small fixtures/test helpers. Native qualification is a separately authorized finite run in a disposable repository and instance. No blanket paid-session budget or broad fixture framework is assumed.

**Exit gate:** recorded evidence for a real qualified Codex conversation covering follow-up, denial, interruption, and restart/reconnect. Host tests alone cannot satisfy this gate. If the installed interface cannot provide reliable reconciliation, narrow the supported recovery flow before Stage 2 rather than masking the gap.

## Stage 2 — Deliver the everyday workspace and insight tools

Stage 2 has three deliverables. Comparison and knowledge work can progress independently of adapter qualification, but their integration with live conversations depends on Stage 1. This is an implementation dependency statement, not authorization to spawn agents.

### 2A. Conversations and workflow promotion

User flow: **Project → New conversation → select admitted profile → ask → read messages/tool activity → follow up or interrupt → resume later**.

Show permission requests and questions in the existing attention surfaces, with a link to their exact conversation. Keep provider terminal access available where the active adapter supports it; do not present terminal controls that a structured session cannot fulfill.

Promotion flow: **Create task from conversation → choose Single agent or TRIP → review proposed task text and acceptance criteria → choose source messages/approved knowledge → save draft or explicitly queue**. Promotion creates a linked task with a frozen context bundle; it does not mutate the conversation into a writer or bypass admission. Do not duplicate a task after an ambiguous response.

Preserve existing Changes/Checks/result acceptance. Qualify existing Repository research and Build and review policies separately for each offered provider; clearly label unsupported combinations. This activates an existing preset, not an advanced inline review or Git integration feature.

Likely owners: `frontend/src/App.tsx`, new focused conversation components, existing attention/inbox components, `TaskForm.tsx`, API/types, `src/server.rs`, `src/workflow.rs`, and relevant persistence/recovery modules.

**Acceptance:** the complete flow works without requiring terminal interaction for supported conversation operations; reloading preserves state; an unavailable provider fails with a useful remedy; denied writes remain denied; task promotion preserves provenance and requires the selected workflow's setup.

### 2B. Workflow and provider comparisons

Extend the existing comparisons screen rather than create a competing analytics system.

Filters/grouping: project, workflow/preset, provider, exact model/effort, profile revision, adapter revision, and explicit time cohort. Show sample counts and drill down to contributing tasks/runs. Separate observational comparisons across unlike tasks from any future controlled experiment; do not claim causation or a universal winner.

Initial metrics:

- Accepted tasks, rejected/request-changes outcomes, cancelled and failed runs, and incomplete work.
- Rework count using persisted linked attempts/feedback; distinguish user rework from infrastructure recovery.
- End-to-end duration from first execution to user acceptance; separately show execution duration and known waits where event boundaries support them. Ongoing work is marked ongoing, not counted as completed.
- Selected-check results, with not-run/unavailable separate from pass/fail.
- Observed token usage and coverage by provider/model; unavailable, invalid, observed zero, and partial are distinct. Do not add cached/reasoning tokens twice.

Define denominators and cohort semantics in the API/UI. Attribute per-run execution to its frozen profile; label tasks using multiple providers as mixed instead of crediting one provider with the entire outcome. Keep execution activity and human acceptance separate. Do not label wall-clock agent execution as CPU time.

No dollar-cost comparison or automated provider recommendation in the first release. Add pricing only later with dated rate provenance and sufficient usage coverage. No automatic benchmark jobs are included.

Likely owners: `UsageComparisons.tsx`, `frontend/src/types.ts`, `frontend/src/api.ts`, history/usage query paths in `src/store.rs` and `src/server.rs`, event writers and additive migrations where needed.

**Acceptance:** known small datasets produce exact counts/durations; missing data never improves a provider's apparent result; filters and pagination remain coherent under updates; every aggregate links to its evidence. Existing partial Codex totals remain consistent.

### 2C. Project knowledge with explicit provenance

User flow: **Open report/message/finding → Save as knowledge draft → edit statement and sources → approve → search/open later → attach to a conversation or task**.

Each stable entry has immutable revisions containing statement/title, project, category/tags, author/origin, source references, available source content hashes and repository commit/path anchors, approval actor/time, and revision relationships. Use existing historical source identities for task/report/review references; add conversation message references without changing their meaning. Direct user-authored notes are allowed and visibly labeled as user assertions.

Lifecycle: draft, approved, superseded, archived. Track freshness separately: verified for a stated scope, needs revalidation, or source unavailable. A changed referenced file/commit can trigger a deterministic freshness warning; it cannot prove that a statement became false. Do not claim automatic semantic freshness detection.

Editing an approved statement creates an unapproved revision; the existing approved revision stays identifiable until superseded. Archived/superseded entries are excluded from normal context selection. Source loss is visible; never replace a missing source with an unrelated current file. Preserve approved text and immutable source identity rather than copying entire raw transcripts.

First release uses project-scoped text search and explicit selection. No embeddings service, cross-project retrieval, provider-memory toggle, or automatic memory injection is required. Before launch, show selected entries and freshness warnings. Freeze exact entry revisions into a per-run context manifest, enforce a size budget, and record what was actually included. Stale entries require explicit selection/acknowledgement.

User-created knowledge does not modify `AGENTS.md`, change permissions, or become higher-priority instructions. Removing an entry from future use does not rewrite a past run's manifest. Archive is the initial removal operation; irreversible purge/retention policy requires a separate design decision.

Likely owners: new focused Rust knowledge module; existing SQLite/store/API boundaries; new Knowledge view; `HistorySearch.tsx` exact-source links; conversation/task context preparation and export/backup policies.

**Acceptance:** approve, revise, supersede, archive, search, inspect sources, detect source unavailability, and attach an exact revision. Demonstrate project isolation, bounded context, stale-entry acknowledgement, backup/restore, and preservation of historical manifests.

**Stage 2 exit:** demonstrate conversations, promotion, comparisons with traceable evidence, and approved knowledge reuse as four separate user journeys. Ship comparisons/knowledge incrementally if adapter qualification is still pending; do not label the entire stage complete until its offered native paths pass.

## Stage 3 — Add bounded repeatable work

Extend existing recipes/schedules with explicit modes: **create draft** (current default) and **run an authorized recipe**. Existing schedules retain draft behavior after migration.

First execution slice: qualified Repository research recipes producing a report for review. Then enable Single agent and Build and review recipes only after their own schedule/permission/recovery paths pass. Retain TRIP as draft-only in this plan; unattended execution must not bypass its plan/implementation decisions.

A run-enabled schedule pins project, recipe/configuration/profile/policy revisions, workflow definition, selected knowledge revisions, cadence/timezone semantics, maximum runtime, and a finite occurrence/run allowance. Stale admission, archived knowledge, changed pins, or unsupported limits hold the run and explain the remedy. Usage-based budgets are enforced only when reliable provider observations support them; otherwise label them unavailable and use enforceable run/time limits.

Preserve repository claims and current global capacity. Never overlap occurrences of the same schedule. Persist a unique occurrence identity before launch; restart cannot launch it twice. Missed occurrences are recorded and skipped without a catch-up burst. A capacity-blocked occurrence can wait only within its explicit bounded start window, then becomes missed. Reuse existing cadence semantics and document timezone/DST behavior before adding new calendar choices.

Permission requests pause progress for a human. Results go to the existing review/attention flow; no auto-accept, commit, merge, push, or publication. A run may propose knowledge drafts but cannot approve them. Attribute run outcomes to their schedule and pinned profile in comparisons.

Controls distinguish **Pause future runs** from **Stop current run**. An interruption request is not confirmation of stopped processes. Unknown outcomes require reconciliation; no automatic re-execution after ambiguous delivery.

Likely owners: `src/recipes.rs`, `src/scheduler.rs`, `src/service.rs`, `src/server.rs`, recovery/control paths, `Recipes.tsx`, attention surfaces, and comparisons/knowledge integration.

**Acceptance:** default schedules still create drafts; explicit authorization enables only the pinned scope; duplicate delivery/restart cannot execute twice; denied permission, stale evidence, exhausted allowance, overlap, missed time, pause, cancellation, and provider loss produce correct durable outcomes. One authorized native research schedule must finish with a reviewable report and comparison evidence. Writer scheduling stays unavailable until separately verified.

## Delivery sequence and verification

Recommended slices: 1A adapter/event contracts → 1B qualification → 2A conversation/promotion → 2B comparison screen → 2C knowledge lifecycle/context → 3 read-only schedules → qualified writer schedules. Define comparison events in 1A to avoid losing useful evidence while the UI is built. Knowledge can be implemented before comparison UI if implementation capacity or user feedback favors it; both remain required Stage 2 scope.

For each slice:

1. Recheck current source, applicable repository guidance, schema compatibility, and exact requested user flow.
2. Implement a bounded change with focused causal Rust/contract and existing frontend DOM tests. Exercise migrations against existing and new records; include backup/restore where storage changes.
3. Run relevant focused checks during development, then `./scripts/verify.sh` for the integrated milestone. This includes dashboard type checking/tests/build and documented host/package verification; it does not prove native provider behavior.
4. Trace exact UI actions and failure paths. Real browser/device/provider checks need their applicable explicit authorization; record host, browser, native, and human acceptance evidence separately.
5. Update user guides for delivered behavior and limitations. Record stage status against acceptance criteria; do not infer completion from compilation.

No time estimate is committed before the Stage 1 protocol/qualification result. Relative effort: Stage 1 high uncertainty; conversation UX medium/high; comparisons medium; knowledge medium/high; scheduled execution high because it changes execution authority and recovery.

## Decisions to confirm before dependent implementation

The plan recommends: Codex structured integration first, existing Claude support retained; project-scoped read-only conversations with explicit promotion for edits; local instance-owned knowledge with human approval and explicit context selection; archive rather than irreversible purge; observations rather than automatic model rankings; read-only schedules before writer schedules.

Review these concrete defaults at the start of the relevant implementation slice. A different storage scope, permission model, retention policy, or automatic knowledge-injection behavior needs a revised design before dependent code changes. Planning does not authorize live model runs, device checks, publication, or changing provider settings.

## External interface references

- [Codex app-server](https://developers.openai.com/codex/app-server): structured conversation integration; use the installed version's schema, not an assumption of feature parity.
- [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/overview): possible later structured Claude adapter. Adoption is not a dependency of these stages; verify authentication/runtime implications separately.

These references were consulted during the preceding feasibility assessment. Revalidate relevant interfaces during Stage 1 against the installed CLI before implementation.
