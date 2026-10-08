# Shared workflow architecture: TRIP plus Single agent

**Status:** MW-01 documentation candidate; Single agent is a future implementation.

**Task:** AJ-4FCAAAE2, replacement MW-01.

**Source checkpoint:** `8c6f1e72cb3282db4dd4e381ef8fd83b6b37ea9e`.

**Approved plan:** revision 1, app-owned hash `5247a194c717b0fe098f31ba58b1a8c4b52988affef05ea7c63bf58988c57925`.

**Configuration:** `b4147f19-6849-4fb6-a3aa-ec9c1176ab1e`.

This specification materializes the bounded approved plan delivered through the authenticated role context for attempt `2412323b-91f2-48a9-a42e-d896873c92d0`, whose phase is `implementation`. The independent plan-review budget records one spent call. The implementer context exposes the approved plan and phase, not the review verdict text or a human approval receipt; those remain app-owned evidence. This document does not replace any approval, authorize MW-02 through MW-04, or establish runtime qualification.

MW-01 delivers this decision specification, the [requirement ledger](MULTI_WORKFLOW_REQUIREMENTS.md), and the [redacted TRIP baseline](TRIP_COMPATIBILITY_BASELINE.md), with planning-status links in README and the [feasibility study](MULTI_WORKFLOW_FEASIBILITY.md). It changes exactly five Markdown files. No production code, fixture, provider configuration, test, migration or workflow resource is changed.

The replacement MW-01 through MW-04 chain follows the externally delivered RT-01 through RT-03 checkpoint, whose setup activation and six ordinary runtime role proofs were verified through LLMRelay according to the task description. That is historical input, not a rerun or a claim of app-owned bootstrap completion. Original drafts and failed probe histories remain preserved.

The ordered delivery is MW-01 specification → MW-02 policy/profile/run contracts → MW-03 lifecycle and dashboard → MW-04 native/dashboard qualification. Build and review, Research, workflow editing, plugins and recipe generalization remain deferred. Existing TRIP behavior remains the product contract.

## Resolved product decisions

1. Ship only the existing TRIP preset and an immutable Single agent preset. Keep one service, SQLite store, scheduler, coordinator, repository claim system, PTY owner and dashboard. Preserve the specialized TRIP evaluator; add only a small Single agent evaluator and explicit dispatch boundaries in existing modules. Do not introduce a generic graph engine, scripting, dynamic workflow editor, Jinn dependency, plugin runtime, provider API, new manager or standalone workflow launcher.
2. Existing projects/tasks default to TRIP. A project can explicitly choose a default workflow for new drafts; a task can choose another bundled workflow in Backlog or Ready before any attempt is reserved. Resolve and pin the selection at Ready and revalidate it transactionally at claim. A newer project default never changes an existing task selection or an active run. Editing an already-Ready selection explicitly creates a new selection/task revision, invalidates its Ready admission and requires successful revalidation before it can be picked up. Changing the workflow after an attempt exists requires an explicit new task/fresh lineage; no in-place TRIP-to-Single-agent conversion or approval carry.
3. Single agent requires one admitted workspace-writing profile, a retained session within its run, an authenticated structured result, a complete quiescent candidate snapshot, and an exact human Accept result decision. It has no mandatory manager, Explorer, independent review chain, plan approval or TRIP installation. Process exit, native Stop, terminal prose, passing checks and report submission cannot mark Done.
4. Human result acceptance and changes integrated into the registered repository are independent. Done means the exact run result was accepted. It does not mean committed, merged or deployed. Keep separate integration evidence and require it for dependent coding work. Never perform automatic Git integration.
5. First architectural proof targets Codex using a newly qualified exact Single agent writer profile, with model/effort selected through current capability preparation rather than guessed model aliases. Keep both providers in the design. Claude Single agent remains visibly unqualified until its own native proof succeeds; TRIP provider qualification is preserved only for semantically unchanged contracts. No new paid native qualification budget is authorized by MW-01. MW-04 must obtain an explicit app-owned finite probe/session authorization before live work.
6. Reusable profiles are project-scoped, named, immutable revisions. Editing a name changes display metadata only; editing model, effort, adapter, instructions, policy or session mode creates a new revision and new admission need. Saving is structural validation, not execution authority. No silent task/run adoption of new profile revisions.
7. Preserve the source checkpoint and supervising executable at /private/tmp/llmrelay-development-20261006/host-runtime-0.12.0/llmrelay. Future candidate hosts may be checked only in separately authorized isolated qualification; this supervisor is never rebuilt or replaced while it supervises this attempt.

## Proposed schemas for MW-02/MW-03

The following are approved design contracts for future MW-02/MW-03 implementation, not commands or fields available in this MW-01 attempt. Implement them within existing Rust modules and SQLite migrations; use explicit validated types and current transactional/receipt machinery.

### ExecutionPolicy schema 1 (compiled and versioned, not user-authored code)

policy_id:string; revision:positive integer; authority:read_only|workspace_write; filesystem_contract:{worktree_write:boolean,protected_read_floor_revision:string,temporary_root_rule:string}; native_permission_contract:string; tool_contract_revision:string; network_contract_revision:string; hook_contract_revision:string; account_contract_revision:string; channel_operations:ordered unique string list; compatibility_contract_id:string; content_hash:sha256.

SessionContract is a distinct value retained|fresh, never inferred from authority or display role. New compiled Single agent writer policy permits worktree writes and human-native action requests while preserving sandbox and channel limits. A read-only compiled policy has no grants that can elevate it to writing; it is used for negative comparisons, not offered as an alternate Single agent writer. No arbitrary shell/interpreter access is added.

Keep RoleKind and its current serialization for TRIP. Introduce an execution actor discriminant TripRole(RoleKind)|SingleAgent in internal runtime bindings, stored as exact existing TRIP role keys or the new single_agent slot. SingleAgent never impersonates implementer to obtain authority. Existing LaunchConfig/CapabilityIdentity/RoleContext are the legacy TRIP wire/identity branch. A new versioned policy execution branch carries actor, policy and session contract explicitly; its role-context envelope is distinguished by schema and workflow binding. Authentication first resolves the generation-bound execution binding, then dispatches its allowed operations. Existing TRIP context/report serialization and capability hash inputs remain unchanged. New Single agent proofs use a new policy identity, never old Implementer proof relabeling.

### AgentProfileRevision

id:string; profile_id:string; revision:integer; provider:codex|claude; adapter_id:string; adapter_revision/hash:string; model:string; effort:string; instructions_text:string; instructions_hash:sha256; policy_ref:{policy_id,revision,content_hash}; session_contract:retained|fresh; profile_hash:sha256; created_at:string. AgentProfiles hold project_id, stable id, display_name and current_revision_id with optimistic versioning. Instructions are bounded plain text plus explicitly selected contained regular guidance files; no executable recipes or directory traversal. Revisions are immutable and contain no account material.

### ProfileAdmission

id; project_id; profile_revision_id; execution_config_revision_id; policy_hash; adapter_hash; session_contract; capability_id/key/proof_hash; prepared_scope_hash; state:pending|activated|invalidated; activated_at. Exact native capability evidence and explicit activation are both required. Reuse is allowed only in the already proved exact scope. New profile/definition references do not manufacture qualification or import authority from another project.

### ProjectExecutionConfiguration

immutable revision id, project_id, revision, content hash, selected contained guidance paths with per-file identities, validated testing coverage, documentation policy, exact verification catalog revision, and ordinary repository registration boundary. TRIP binds its existing trip_config_revisions record as the configuration authority; do not rewrite or rehash it into the new schema. Single agent has an independent human-reviewed execution configuration and one profile admission; no TRIP files, discovery manager or six-role prerequisite. Shared source freshness still compares the registered commit against the exact approved guidance that would be materialized. Do not silently overwrite uncommitted guidance.

### WorkflowDefinition schema 1

family:trip|single_agent; immutable definition id/version; exact definition_text and content_hash; evaluator_id/revision; ordered consumed steps; slot requirements with policy/session refs; result_schema_revision; human_acceptance:required; check_requirement:none|selected; definition provenance.

For new Single agent, executable consumed steps are agent_execution -> structured_result -> optional selected_checks -> human_decision. No reviews or general graph edges. TRIP retains its existing full workflow identity calculation over workflow text, overlay and upstream source manifest. Catalog family membership is a separate annotation; content_hash never substitutes for or overwrites legacy workflow_hash.

### TaskWorkflowSelection

task_id, family, definition_id/hash, configuration revision, explicit slot-to-profile-revision references and optimistic task version. An omitted selection on legacy/old-client TRIP commands resolves to TRIP under the existing behavior. Supplying Single agent must use the new validated typed selection path; unknown presets are refused. Ready freezes the resolved draft selection, while claim rechecks exact definition availability, configuration, profile admission and repository freshness before recording run authority.

### WorkflowRunBinding

attempt_id primary key; family; definition_id optional only for incomplete historical provenance; exact stored definition identity; definition availability state:available|unavailable; evaluator_revision; execution_config revision/hash; selection/profile binding hash; scope_hash; parent_attempt_id; source_snapshot_id; created_at. Preserve existing attempts.id, context_id, workflow_version/hash, upstream/overlay hashes, phases, plan approvals, review budgets, final-repair receipts and acceptance history. Mutable phase/status remain in existing attempts, under the family evaluator. An absent/mismatched definition or policy is explicit recovery, never fallback to the current global TRIP identity.

### RunProfileBinding

attempt_id, slot_key, immutable profile revision, admission id, policy identity, session contract, capability binding and provenance. This is separate from current named-profile defaults. A safe profile switch is explicit, creates an accounted generation with frozen checkpoint/handoff and appended binding history; it never mutates a historical execution binding or silently changes a current run. TRIP uses existing trip_attempt_profiles and settings-revision rules as the compatibility authority.

### ExecutionBinding

role_generation_id primary key; attempt_id/slot; run/profile/admission refs; actor; policy_hash; session_contract; permitted channel operations; compatibility identity revision. Existing session and hook/process/epoch/native identity records remain the shared process owner. Channel authentication binds all these dimensions before returning context or accepting reports.

### SingleAgentResult schema 1

Submitted through existing role report transport with the new workflow-scoped outcome result_ready (blocked/needs_input remain non-completing), nonblank summary, bounded evidence and metadata containing run_binding_id, profile_binding_id, and acceptance_rows in exact task-criterion order. Each row has criterion, status:met|unmet, and evidence items distinguishing observed checks, source/manual inspection and not_run. Claimed changed paths or hashes are diagnostics; only the service snapshot establishes authoritative candidate identity. Every criterion must be met to enter human review. The service validates the current actor, generation, session/epoch/invocation, definition and scope, records an operation receipt, waits for verified writer/process-group quiescence, then binds the result to a complete candidate snapshot. Exit without a result is incomplete. Invalid, stale, duplicate-changed or malformed reports cannot progress.

### WorkflowResultBinding

id, attempt_id, report_id, run_binding_id, snapshot_id, candidate_hash, criteria_hash, scope_hash, profile_binding_hash, selected_check_revision, evidence_hash, state:pending_quiescence|awaiting_human|accepted|superseded. Frozen candidate/result data are immutable. A separate human acceptance receipt binds operation id, expected task version, current attempt, result binding, candidate hash, definition/profile/configuration/criteria hashes and selected-check receipts. Record existing human_acceptance_at and Done only after that exact transaction and quiescence. Retain the original report/check/acceptance facts separately.

Optional checks for Single agent are selected by the human at draft/Ready from its independent execution configuration; zero is permitted by this preset. ExecutionVerificationCheck uses the exact validated command, cwd, timeout, relevant-input, acceptance-row and invalidation shape of the current catalog with an execution_config_revision_id authority instead of the TRIP-only foreign key. WorkflowCheckSelection binds task/run, immutable revision, exact catalog IDs and selecting human; WorkflowCheckAuthorization binds run/selection revision, check ID, exact command hash, scope hash, decision/lifetime and consumption. Extend the existing CheckRunner with a typed TRIP-or-Single-agent authority resolver while retaining shared check_runs, process tracking and one build slot; do not reuse a TRIP-only selection row for another family or duplicate a runner. If selected, each becomes mandatory for that run and uses the same exact authorization, freshness and build-slot service contract. A native command permission cannot substitute for selected-check authorization. The Single agent cannot choose/unselect checks to accept its own result. Engineering verification for MW-01 through MW-04 remains governed by the activated moderate coverage policy and each task-specific matrix, independently of Single agent task defaults.

## Additive migration and historical authority

Propose migrations/039_multi_workflow_contracts.sql as the next schema after inspected 38, with store registration and schema assertions updated only in the later approved MW-02 task. If that task starts from a different schema, its own plan must resolve the change rather than silently use this filename/number.

Add tables agent_profiles, agent_profile_revisions, profile_admissions, execution_config_revisions, workflow_definitions, task_workflow_selections, workflow_run_bindings, workflow_run_profiles, execution_bindings, workflow_result_bindings and workflow_result_acceptances matching the record shapes above. Add execution_verification_checks, workflow_check_selections and workflow_check_authorizations for the independent optional Single agent check catalog/selection/grant bindings; retain existing role_results, sessions, snapshots, claims, operation receipts and TRIP tables. Do not duplicate process ownership or check execution. Add foreign keys, uniqueness for one run binding per attempt/current slot/result, current-result identity, and idempotent operation receipts. Immutable definitions/profile/result bindings reject conflicting bytes under the same identity.

Backfill side bindings only: all preexisting tasks/attempts are TRIP, irrespective of current project default. Preserve historical version/hash and provenance exactly. Where exact historical definition bytes are available from stored approved resources, store and validate them against the old identity. Where bytes or provenance are missing, record unavailable/incomplete provenance without reconstructing from a version name; history remains readable, launch/resume is held. Never fill missing old authority using present bundled bytes.

Retain final_reviewer parsing compatibility and current stored final_verifier names. Preserve legacy_migration_required and existing migration/approval/recheck meanings for TRIP. Do not grant old attempts new eligibility, invalidate unchanged valid TRIP proofs indiscriminately, or demote rows merely because descriptive catalog metadata was added. Effective-contract drift instead requires targeted new qualification.

Use existing offline pre-upgrade restore-point and supported-startup-upgrade contracts; preserve held-restoration and authority revocation semantics. Test fresh creation, genuine schema-38 upgrade, supported older paths through 38 then 39, transaction interruption/rollback, foreign keys, immutable identity conflicts and repeated open. No live instance database, migration, direct SQL or bootstrap replacement is authorized. Rollback uses the existing operator-controlled restore route; no destructive downgrade migration.

## Admission and evaluator boundaries

Shared admission first checks repository identity/base, approved guidance freshness, queue/dependencies, holds/controls, active/uncertain process inventory, worktree reservation, claims, global/provider capacity, permission/input delivery state and relevant evidence. Reserved, idle, stopping and unknown processes continue occupying their real resources. Workflow-specific admission then checks the pinned preset: TRIP still requires activated installation, six exact profiles and existing approvals/accounting; Single agent requires repository registration plus its independent execution configuration, one current writer admission and explicit human Ready decision. Recheck both layers in claim/dispatch transactions; no optimistic dashboard label grants launch.

Attempt construction obtains phase, required slots and budgets from the selected evaluator. TRIP remains planning and 2/2/1 ordinary budgets plus the current dedicated final-repair lane. Single agent begins agent_execution with one writer and no review budgets or fake manager session. Dispatch/report progression, resume, rework, result acceptance and dashboard projections resolve the same pinned family/definition, never a scattered collection of label checks. The shared coordinator owns controls and actual launch scheduling.

Shared writer detection for snapshots/review/checks/cancel must derive from effective workspace_write policy, not only role='implementer'. Frozen candidate capture requires all affected writers yielded/quiescent. One build/check slot is shared across both families, with exact selected-check authorization and no release while a check/launch is pending or uncertain. Mixed workflows hold the same repository claim across all active states.

## Resume, request changes and recovery

Single agent retains the exact native conversation within one eligible run. Resume requires saved native history, same run/definition/profile/policy/account compatibility, current settings/admission, current generation, worktree/candidate identity, hook/session/epoch provenance, allowed phase, resolved holds/permissions/input, positive process quiescence and capacity. The same eligibility must govern restart preview, manual resume and auto-resume. No latest-session lookup, missing-history fresh fallback, cross-workflow resume or label-based exception. Account/provider/adapter drift leaves a visible typed recovery hold.

TRIP final verification remains fresh for every frozen candidate. An interrupted fresh verifier cannot be resumed or relabeled fresh. Retained reviewer accounting and dedicated final-repair rechecks remain exactly as recorded.

Human Request rework for Single agent requires nonblank feedback, expected task version and exact current result/snapshot. It creates one idempotent linked attempt under the same pinned workflow, new run identity and fresh initial native conversation with subsequent retained turns inside that child. Preserve parent output and native history; do not resume its ended result conversation as a new run. Changed scope/profile/definition requires explicitly selected/admitted child bindings and renewed applicable authority. TRIP carry-plan-approval remains its existing separately checked human option; Single agent exposes no such option.

Cancellation/drain stop effects before releasing repository/process/build ownership. Unknown launch effects remain recovery and cannot trigger duplicate dispatch. Restart does not automatically retry uncertain effects, guidance, result acceptance, integration or paid probes. Queued/written guidance is acknowledged only after correlated native submission and context acknowledgement_eligible=true. Human-only operations remain inaccessible to every policy branch.

## TRIP comparison criteria and causal evidence

For each of six roles on both providers compare before/after: exact model/effort/adapter and executable fingerprint/pin; prepared argv and sanitized environment-key contract; generated native permission/sandbox/network/MCP/tool configuration; hooks/report/context allowlists; human permission actionability and delivery/resolution fences; channel operations and scope; current capability identity serialization/key and proof lookup; retained or fresh behavior; denial results; launch/Stop/quiescence/history; profile safe-switch/review budget/final-repair behavior. Normalize only existing bounded transient-path/native-ID fields; do not remove policy-bearing differences to make equality pass.

Unchanged TRIP branches must yield equal old identity bytes and keys AND equal effective native behavior. A semantic difference fails preservation even if hashes agree. Intentional new Single agent authority cannot reuse old TRIP identity/proof. No baseline stores account contents, auth material, raw transcripts or unrestricted local logs.

Current existing regression anchors (read as source inventory, not executed here): trip_uninitialized_admission_rejects_scheduler_and_direct_store_launch; trip_materialization_and_profile_drift_fail_closed; trip_runtime_scope_exactly_fences_project_and_task_authority; trip_runtime_native_hook_commands_bind_report_acceptance_and_preserve_sibling; trip_review_budgets_and_final_sessions_preserve_fresh_only_compatibility; trip_legacy_migration_preserves_review_accounting_and_lineage; trip_disjoint_lanes_keep_independent_credentials_and_require_quiescence; m7_same_version_contract_change_rejects_actual_resume_with_typed_category; m7_retained_launch_and_resume_share_exact_bound_identity; t08_review_resume_replacement_and_extension_account_exactly; t09_rework_is_idempotent_materializes_full_candidate_and_fences_carry; t18_permission_decision_is_idempotent_one_shot_and_restart_fenced; t18_permission_rules_match_only_current_verified_scope_and_reservation; exited_sessions_offer_resume_only_while_current_and_unfinished; second_final_review_requires_the_approved_dedicated_recheck_of_the_candidate. Existing frontend T14 task inheritance, T15 role/human review authority, T19 permissions, T21 ambiguous mutations and Active/Completed/Ready flow cases remain authoritative. Adapt legitimate interfaces without weakening these assertions.

## Exact current-task ownership and delivery

The approved implementation scope assigns one default retained implementer these exact paths:

- README.md
- docs/MULTI_WORKFLOW_FEASIBILITY.md
- docs/MULTI_WORKFLOW_PLAN.md
- docs/MULTI_WORKFLOW_REQUIREMENTS.md
- docs/TRIP_COMPATIBILITY_BASELINE.md

The three new documentation files are materialized by this MW-01 candidate. README gains a planning-status link; it must continue describing currently implemented TRIP behavior. Feasibility points to the reviewed MW chain and removes the superseded first-push prerequisite without claiming Single agent shipped. The plan document records resolved contracts and ordered ownership. The requirement ledger records all five exact acceptance rows, decisions, owner/path, source evidence, review disposition, future gates and explicit uncertainty. The baseline records focused source/identity/behavior comparisons and app defect reproductions.

The manager owns README/ledger/documentation decisions and independent request verification; the retained implementer executes those exact authorized writes from the manager handoff. Reviewers remain read-only. Shared read boundaries are current source/configuration/guides. Protect src/, frontend/, migrations/, resources/, tests/, scripts/, build.rs, Cargo.toml, Cargo.lock, AGENTS.md, .agents/, .claude/, .codex/ and host/data/native-state locations. No parallel lanes or lane configuration for this bounded task. No fixture, framework, source module, provider configuration or production tests added in MW-01.

## Ordered follow-up proposals (not authority to execute them)

### MW-02

Execution policy/session/profile/run identities and additive migration; depends on reviewed, human-accepted MW-01 and explicitly verified integration of its documentation if needed by the registered task base. One retained writer sequentially owns src/domain.rs, src/providers/mod.rs, src/providers/codex.rs, src/providers/claude.rs, src/provider_compatibility.rs, src/permissions.rs, src/trip.rs, src/store.rs, src/roles.rs, src/workflow_resources.rs, src/lib.rs, migrations/039_multi_workflow_contracts.sql, resources/workflows/single-agent-1.json, resources/prompts/single-agent.md, resources/provider-compatibility/single-agent-codex.json, resources/provider-compatibility/single-agent-claude.json, tests/contracts.rs and in-module regression cases. These proposed new resources hold the immutable consumed Single agent definition, prompt and separately compiled policy compatibility contracts; they do not change the six-role schema-1 TRIP packs. Existing TRIP workflow/prompt/source-package files are protected. Manager owns README/security/setup/workflow/plan/ledger documentation content, executed through the retained writer at the documentation barrier. Deliver causal legacy-policy equivalence, new-policy denial/actionability, immutable revision/run selection, independent admission, hash compatibility and migration tests using the existing harness. No production Single agent dispatch until MW-03.

### MW-03

Workflow-aware admission, Single agent progression, snapshot/result acceptance, rework/recovery and minimum task/dashboard selection; depends on MW-02 frozen interfaces plus verified integration. One retained writer owns src/domain.rs, src/store.rs, src/scheduler.rs, src/coordinator.rs, src/workflow.rs, src/operations.rs, src/roles.rs, src/recovery.rs, src/review.rs, src/checks.rs, src/snapshot.rs, src/workspace.rs, src/server.rs, src/control.rs, src/cli.rs, src/task_cli.rs, frontend/src/types.ts, frontend/src/api.ts, frontend/src/components/ProjectSettings.tsx, frontend/src/components/TaskForm.tsx, frontend/src/components/RoleSettings.tsx, frontend/src/components/TaskBoard.tsx, frontend/src/components/TaskDetail.tsx, frontend/src/components/ReviewPanel.tsx, frontend/src/components/AttentionInbox.tsx, frontend/src/components/RecoveryPanel.tsx, frontend/src/components/WorkflowControls.tsx, frontend/src/components/SessionTree.tsx, frontend/src/flows.test.tsx, tests/contracts.rs and tests/runtime.rs. Provider policy renderers and TRIP bundled bytes are frozen shared seams and protected from redesign. Manager owns README/operations/workflows/building/ledger update decisions. Deliver provider-free causal proof of Single agent admission without TRIP, no Done from exit, exact report/snapshot/acceptance, optional-check gating, stale/replay refusal, linked rework, uncertain launch/cancel/restart, typed read-only denials and mixed-workflow claims/capacity.

Client schemas: preserve protocol generation 1 and existing authentication/feature semantics for legacy TRIP. Advertise and require a new compiled multi_workflow_v1 feature for new Single agent human operations; unsupported older clients cannot send them. Additive existing TRIP DTO fields must remain optional with legacy semantics. Do not use an unqualified blanket protocol bump or relax generation validation. Exact feature/name/field additions are proposed here and must be implemented and tested against existing protocol.rs, control.rs, cli.rs, frontend/api/types. Include src/protocol.rs in MW-03 ownership. Role-agent envelopes remain independently authenticated and versioned; browser features do not authorize role operations.

### MW-04

Bounded native/dashboard qualification and compatibility sign-off after MW-03 frozen candidate and verified integration. One build/check owner; no active writers during checks/inspection. Manager owns README and docs/WORKFLOWS.md, docs/PROJECT_SETUP.md, docs/SECURITY.md, docs/OPERATIONS.md, docs/BUILDING.md, docs/MULTI_WORKFLOW_REQUIREMENTS.md and docs/TRIP_COMPATIBILITY_BASELINE.md via exact retained-implementer documentation instructions. No production source ownership by default; any discovered app defect is recorded first and returns to the task/plan owning that boundary for approved repair. Use existing tests/runtime.rs/flows and existing scripts/browser_smoke.py only; no new permanent fixture/server/browser harness. Qualification artifacts are service-owned and isolated under approved paths. Exact disposable repository, candidate-host location, native profiles, probe cells, commands and finite paid budget must be prepared and authorized by the app/human at that task; none is supplied by this plan as existing authority.

## Shared-file barriers and integration

The sequence is MW-01 -> MW-02 -> MW-03 -> MW-04, not simultaneous changes to src/domain.rs/store/trip/roles or shared tests. Every later task must name exact current owned/shared/protected paths, source bindings, selected checks and retained writer at its own planning gate. Finish shared schema/policy seams first and freeze them before lifecycle/UI work. If a later task elects lanes, its separately reviewed structured ownership must have disjoint mutable paths and service-computed admission hashes; all required lanes must yield before manager-directed integration by the retained implementer. Do not configure lanes or consume extra provider capacity in this task.

Human-accepted documentation or code is not automatically integrated. Before dependent task Ready/claim, the app must verify the accepted manifest against the registered repository commit through Record integration, or an explicitly authorized future base change. There is no commit/merge authority here to satisfy that boundary automatically.

## Causal test plan and separate delivery gates

Moderate coverage: target primary causal behavior, negative authority boundaries, compatibility and interruption/replay; use current Rust unit/contracts/runtime and React DOM harnesses, no new testing dependency or numerical coverage quota.

MW-02 tests must change one binding dimension at a time and show invalidation of authority, not merely compare generated snapshots. Rename-only profile metadata must preserve effective authority; changed policy/session/model/instructions/configuration must reject old proof. Historical rows and review counts survive migration byte-for-byte. Unknown/missing definition is visible history plus blocked resume; exact available unchanged TRIP remains eligible.

MW-03 tests use the same repository registered without TRIP and with TRIP; Single agent can become Ready in the former while TRIP is blocked, and no six-role or migration UI is projected onto Single agent. Inject report-before-quiescence, exit-without-report, stale task/result/run hash, malformed/unmet criteria, drifted candidate, duplicate operation and unresolved check; verify only exact human acceptance marks Done. Request rework preserves one linked parent snapshot and no repeated launch. Inject uncertain launch, cancellation with live descendants, lost history and changed policy; verify locks stay held and no fallback dispatch. Mixed families compete for the same claim/build/capacity. Existing TRIP behavior continues to pass unchanged causal expectations.

Dashboard flow proof traces: Add project -> workflow configuration/default; New task -> Workflow -> Single agent -> one profile -> Save draft/Create Ready task/Make Ready; ready queue -> agent execution -> structured result -> Awaiting your review -> Accept result/Request rework. Hide Keep approved plan for Single agent. Show separate Result accepted and Integration not recorded facts. Native exit without result shows Needs your action, not accepted. TRIP clicks remain Review plan -> Approve plan -> Allow implementation -> reviews/checks/final -> Accept result; permissions retain Approve once/Always approve matching actions/Deny. Every click submits current bound native/app operations; navigation or copy never approves. Refresh/reload, changed project default, ambiguous saves and stale evidence must preserve the exact draft/operation identity and fail closed.

| Gate | Required evidence |
| --- | --- |
| A: architecture proof | Source tracing plus isolated causal tests demonstrate schemas/evaluators/boundaries with no real model inference. |
| B: host verification | Configured Rust/DOM/type/build matrix with service-owned receipts for the frozen candidate. |
| C: native/dashboard qualification | Actual exact provider policy, actionable writer permission, explicit denial, protected/read-only enforcement, native history/retained resume, uncertain ownership, cancellation quiescence, restart and human result/rework controls in an approved disposable instance. |
| D: review and delivery | Independent code review, current manager conformance and fresh final verification followed by human acceptance. |

Source/fixture/DOM results do not close native or browser-observation gaps. Codex qualifies first; Claude is separate before any two-provider delivery claim. Gates are separate evidence dimensions; none substitutes for another.

Current verification selection is immutable revision 1 under configuration `b4147f19-6849-4fb6-a3aa-ec9c1176ab1e`. Both checks are exact_shell, cwd `.`, timeout 900 seconds.

| Check ID | Exact selected command |
| --- | --- |
| 3449578b-7d7c-4574-a040-5ff0c2942871 | `cd frontend && deno task build && cd .. && cargo test --locked` |
| 133320df-738c-4bdb-8f3a-3d7ea36fe8c0 | `cd frontend && deno task test` |

These existing focused checks supply host/build/test baseline evidence after MW-01 documentation is written, reviewed and frozen. They do not prove the architecture decisions, native compatibility, runtime migration, visuals or new Single agent behavior. Only the service may execute them under exact selected-check authorization and its one slot. No selected checks were run during planning or by the MW-01 documentation implementer. Execution and authoritative receipts belong to the service.

The available broad check 16db56cf-d2f9-47e8-8da7-18a7cb84dd79, ./scripts/verify.sh, is not selected for MW-01. Wide runtime follow-ups should select their applicable broad matrix from then-current role context, with fresh exact IDs/revision; do not copy this selection as authority. Reuse receipts only for identical relevant candidate/configuration/inputs; rerun only missing/invalidated dimensions, and run one stable post-review final matrix.

## Current MW-01 verification and authority

The retained default implementer owns only README.md, docs/MULTI_WORKFLOW_FEASIBILITY.md, docs/MULTI_WORKFLOW_PLAN.md, docs/MULTI_WORKFLOW_REQUIREMENTS.md and docs/TRIP_COMPATIBILITY_BASELINE.md. There are no parallel lanes, integration capsule, new fixtures or additional provider launches. Source and guidance inspection is read-only. Scoped file hashes accompany the implementation result; only the service's frozen snapshot establishes the authoritative candidate hash.

Request verification maps each of the five exact acceptance criteria to these documents and their future causal gates. Build verification remains separate: the two selected exact checks above must run through the service's authorized one-slot runner. A clean Markdown diff or complete specification cannot substitute for their receipts; passing those checks cannot qualify the proposed Single agent design.

Before handoff, the app-host manager rereads the request, approved plan and complete scoped diff; checks each ledger row, exact ownership, compatibility decisions, documentation accuracy, moderate coverage and source stability; and records current conformance. Required service receipts bind the frozen candidate, configuration, selected-check revision, current conformance and exact fresh final-review request. Independent code review and fresh final verification remain required. A documentation candidate report is not authoritative completion, human acceptance or repository integration.

No live-instance database work, supervisory-host rebuild/replacement, service restart, Git integration, commit, push, publication, paid probe, runtime/browser qualification or delegation is authorized by MW-01. The supervising executable stays fixed at `/private/tmp/llmrelay-development-20261006/host-runtime-0.12.0/llmrelay`. Human-only actions are inaccessible to role policies. Native/app grants and selected-check authorization remain separate; no denied action may be rewritten or rerouted to evade a decision.

The [baseline](TRIP_COMPATIBILITY_BASELINE.md) records the non-approving Explorer evidence-access limitation and D1–D3 app triage items. None grants repair authority. Only the applicable local engineering/coding guidance and current source were inspected for this documentation implementation; no upstream coding guidance download, production change or live qualification is claimed.

**Unresolved architecture decisions:** none in this approved design. Exact future source/check bindings, finite native probe budget, profiles, disposable repository and candidate-host paths are later task preparation requirements. Each future task must refresh and approve its own exact scope before implementation.
