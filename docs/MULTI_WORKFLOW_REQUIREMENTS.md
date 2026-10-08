# MW-01 requirement and decision ledger

**Task:** AJ-4FCAAAE2, replacement MW-01.
**Base:** `8c6f1e72cb3282db4dd4e381ef8fd83b6b37ea9e`.
**Attempt:** `2412323b-91f2-48a9-a42e-d896873c92d0`; default retained implementer.
**App-owned approved plan hash:** `5247a194c717b0fe098f31ba58b1a8c4b52988affef05ea7c63bf58988c57925`.
**Execution configuration:** `b4147f19-6849-4fb6-a3aa-ec9c1176ab1e`; moderate testing coverage.
**Candidate scope:** five Markdown files; no runtime implementation.

This ledger records the user's exact five acceptance rows in order. “Documented” means present in this MW-01 candidate. It does not mean MW-02 through MW-04 behavior shipped, checks passed, the candidate was accepted or changes were integrated. The [plan](MULTI_WORKFLOW_PLAN.md) resolves the design; the [baseline](TRIP_COMPATIBILITY_BASELINE.md) identifies evidence and limitations.

## Review and authority record

The authenticated role context delivered a nonempty approved plan, plan hash above, implementation phase, and no rework feedback. Plan-review accounting is initial 2, spent 1, extension 0. Code review is initial 2, spent 0; final review is initial 1, spent 0. The implementer view does not include the exact independent plan verdict text or human approval/implementation receipts. This record preserves that boundary instead of inventing review IDs or approval evidence. Those records remain authoritative in LLMRelay.

The implementer may write only the five paths listed below and submit a documentation candidate through the authenticated role channel. The app-host manager owns integration, request verification, conformance and advancement; independent reviewers remain read-only. Selected checks and their execution are service-owned. No role may grant human approval or claim authoritative completion.

## Exact acceptance rows

### 1. A reviewed plan resolves policy/profile/run schemas and migration compatibility.

**Decision and artifact:** [Plan](MULTI_WORKFLOW_PLAN.md), sections Proposed schemas and Additive migration and historical authority. Define ExecutionPolicy and a separate SessionContract; immutable AgentProfileRevision and ProfileAdmission; independent ProjectExecutionConfiguration; WorkflowDefinition, TaskWorkflowSelection, WorkflowRunBinding, RunProfileBinding and ExecutionBinding; result and check binding schemas. Keep legacy TRIP wire serialization/hash inputs and tables as its authority branch. Use additive schema 39 after the inspected schema 38, with side bindings for historical TRIP and no old identity rewrites.

**Owner / paths:** manager for MW-01 specification; retained implementer for docs/MULTI_WORKFLOW_PLAN.md and this ledger. Future MW-02 owns exact schema/policy/profile/store/resource paths declared in the plan and must refresh them at its own gate.

**Source evidence:** src/domain.rs RoleKind, LaunchConfig, CapabilityIdentity and RoleContext; src/trip.rs task_profile_descriptor; src/provider_compatibility.rs BundleSet; src/store.rs CURRENT_SCHEMA_VERSION=38 and service-admitted schemas 31–37; existing migrations 018/020/021/031/032/037/038. [Baseline E1–E4](TRIP_COMPATIBILITY_BASELINE.md) identifies observed source and raw content anchors.

**Review disposition:** resolved in the app-supplied approved revision 1; documented in this candidate. Durable code/final review remains outstanding.

**Causal future gate:** fresh store, genuine schema-38 upgrade, supported older upgrade paths, rollback/interruption, repeated open, foreign keys and immutable identity conflict. Historical identities, review counts and approval lineage remain byte-for-byte. Missing old definition bytes stay readable/unavailable and cannot launch; unchanged available TRIP remains eligible.

**Explicit uncertainty:** no migration or new schema code exists in MW-01. If MW-02 starts at another schema, it must revise its exact migration number/path through planning before editing.

### 2. TRIP launch settings, permissions, credentials, session contracts and historical authority have comparison criteria.

**Decision and artifact:** [TRIP baseline](TRIP_COMPATIBILITY_BASELINE.md), Twelve role/provider comparison cells and Comparison criteria and failure consequences. Compare all six TRIP roles on both providers using identity bytes/keys plus effective argv, generated settings, native sandbox/network/MCP/tool/hook contracts, actionable permission/delivery fences, credential/channel operations and retained/fresh behavior. Preserve exact legacy workflow/upstream/overlay identities and provenance. New Single agent authority receives its own actor, policy identity and qualification.

**Owner / paths:** manager for criteria and evidence disposition; retained implementer for docs/TRIP_COMPATIBILITY_BASELINE.md. MW-02 owns causal preservation tests; MW-04 owns authorized native observations.

**Source evidence:** src/providers/mod.rs prepare_role_launch_with_policies, capability_identity and existing normalization; src/providers/codex.rs and claude.rs; src/provider_compatibility.rs resolve/compiled six-role validation; src/permissions.rs actionability and response fences; src/domain.rs RoleContext; src/roles.rs safe-switch/history/credential revocation; src/recovery.rs RestartSessionFacts. Activated profile hashes, keys, proofs and adapter hashes are copied exactly from authenticated context.

**Review disposition:** comparison criteria resolved in approved revision 1; documented with source evidence, twelve cells and triage items D1–D3. No compatibility approval or new native proof is implied.

**Causal future gate:** vary one binding dimension at a time; old proof must fail on policy/session/model/effort/instructions/configuration drift. Keep unchanged TRIP identity bytes and native behavior equal. Refuse cross-task/lane/session, revoked/stale credentials, grant delivery after boot/native/policy drift, interrupted fresh-final resume and uncertain ownership. Native permission and denial observations remain Gate C.

**Explicit uncertainty:** provider preparation, generated settings and runtime denial were not executed here. Historical RT runtime proofs are task-supplied evidence, not rerun observations. Explorer findings are unavailable in the current role projection; no inaccessible finding or approving verdict is claimed.

### 3. Single-agent completion distinguishes structured result, human acceptance and integrated changes.

**Decision and artifact:** [Plan](MULTI_WORKFLOW_PLAN.md), SingleAgentResult, WorkflowResultBinding, checks and recovery. One admitted retained writer submits workflow-scoped result_ready with bounded summary/evidence, exact run/profile binding and criterion rows. Every row must be met. The service validates generation/session/epoch/invocation/scope, waits for verified process-group quiescence and captures the complete candidate. Exact human Accept result alone records Done; it binds current result, candidate, definition/profile/configuration/criteria and selected-check receipts. Integration is separately verified against a reachable repository commit. No automatic Git action.

**Owner / paths:** manager for specification; MW-03 for workflow/store/snapshot/check/recovery/client/UI implementation and causal tests under its separately approved exact ownership. MW-01 writes docs/MULTI_WORKFLOW_PLAN.md and this ledger.

**Source evidence:** src/domain.rs RoleResultReport; src/review.rs reserve_freeze/freeze_reserved; src/snapshot.rs SnapshotManifest/capture; src/workflow.rs current acceptance and RecordIntegration; frontend ReviewPanel/TaskBoard wording that future Single agent projections must distinguish.

**Review disposition:** explicit result/acceptance/integration semantics resolved in revision 1; documented. The current product still implements TRIP.

**Causal future gate:** report before quiescence, exit/Stop without report, malformed or unmet rows, stale result/task/run/hash, changed candidate, operation replay with different bytes and unresolved check must not mark Done. Exact human acceptance may record a result while integration remains unrecorded; dependent coding work still requires verified integration. Linked rework is idempotent and preserves parent facts.

**Explicit uncertainty:** result_ready, new human operations and workflow-scoped acceptance are proposed contracts, not commands available in this attempt. No Single agent runtime acceptance is demonstrated.

### 4. The plan declares ownership, shared boundaries and causal tests for the ordered follow-up tasks.

**Decision and artifact:** [Plan](MULTI_WORKFLOW_PLAN.md), Exact current-task ownership, Ordered follow-up proposals and Shared-file barriers. MW-01 → MW-02 → MW-03 → MW-04 is sequential. MW-02 freezes policy/schema/profile/run seams before MW-03 lifecycle/UI consumes them. MW-04 owns qualification and documentation, with no default production source writes. Each task refreshes exact source, checks, bindings and scope at its own gate.

**Owner / paths:** app-host manager for task boundaries; one retained writer per task unless a future separately reviewed lane plan establishes disjoint ownership. MW-01 has exactly the five documentation paths below. Proposed follow-up lists name Rust modules, new resources/migration, shared tests, client protocol and each dashboard component in the plan.

**Source evidence:** current module/resource inventory; scheduler TRIP readiness/attempt creation; coordinator shared controls; role/snapshot process authority; frontend hardcoded readiness and progress. The [baseline regression inventory](TRIP_COMPATIBILITY_BASELINE.md) preserves existing causal expectations.

**Review disposition:** current ownership and ordered proposals resolved in revision 1; documented. Future ownership is a proposal, not implementation authority.

**Causal future gate:** identity/policy/migration assertions in MW-02; admission, results/checks, rework/recovery, mixed claims/capacity and exact UI operations in MW-03; bounded native and browser observations in MW-04. Interface adaptation must preserve existing behavioral assertions.

**Explicit uncertainty:** future tasks must name their current exact check IDs and source binding hashes. No lanes, shared-file concurrent edits, fixture, test harness or unplanned path are authorized here. A later repair needs scope authority from the task that owns the boundary.

### 5. Architecture proof, host checks and native/dashboard qualification are separate delivery gates.

**Decision and artifact:** [Plan](MULTI_WORKFLOW_PLAN.md), Causal test plan and separate delivery gates; [baseline qualification limits](TRIP_COMPATIBILITY_BASELINE.md). Gate A is provider-free architecture proof; Gate B is configured host verification; Gate C is authorized native/dashboard qualification; Gate D is independent review, manager conformance and fresh final verification before human acceptance. Receipts for one dimension never close another.

**Owner / paths:** manager for criterion conformance; service for one build/check slot and exact receipts; independent reviewers for their verdicts; human for approvals/acceptance. MW-04 records qualification evidence in the approved documentation paths and service-owned isolated artifacts.

**Source evidence:** current selected check revision 1 and catalog commands in authenticated context; engineering gate; current Rust/contracts/runtime and React DOM harnesses. Source inspection and historical native proof do not establish new Single agent behavior or visual verification.

**Review disposition:** gates and evidence limits resolved in revision 1; documented. This lane's source/Markdown inspection is request-level verification only.

**Causal future gate:** a complete architecture proof cannot advertise native distribution; a successful build cannot mark result accepted; a fixture/DOM proof cannot close browser/native observations. Codex-first proof precedes a separate Claude qualification before any two-provider claim.

**Explicit uncertainty:** finite paid budget, exact disposable repository/candidate-host/profile/commands/cells and app authorization are mandatory MW-04 preparation. MW-01 grants none.

## Resolved product decisions

| Decision | Chosen contract | Future owner / verification |
| --- | --- | --- |
| Initial product increment | Bundled immutable TRIP and Single agent; one host/store/scheduler/coordinator/PTY/dashboard | MW-02/03; no generic engine, Jinn or plugin runtime |
| Default and task selection | Legacy defaults to TRIP; explicit new-draft project default; task selection before reservation; Ready pins and claim rechecks | MW-02/03; changed default does not mutate drafts/runs; Ready edit invalidates admission |
| Profile reuse | Project-scoped named immutable revisions; rename is metadata; effective edits create revision and admission need | MW-02; rename preserves authority, effective drift rejects old proof |
| Policy versus session | Compiled policy controls permissions; distinct retained/fresh session contract controls conversation reuse | MW-02; read-only cannot be elevated by human action grant |
| Single agent prerequisites | Registered repository, independent reviewed configuration and one admitted writing profile | MW-02/03; Ready without TRIP installation, TRIP still blocked there |
| Run identity | Exact immutable definition/config/profile/policy bindings and generation-bound execution actor | MW-02/03; unknown/missing definition holds launch/resume |
| Credential/report authority | Legacy TRIP wire branch unchanged; new versioned Single agent branch; authenticated binding resolves allowed operations | MW-02/03; no Implementer impersonation or human-only role operation |
| Checks | Human optional selection at draft/Ready; selected checks mandatory for that run; exact service authorization and one shared slot | MW-02/03; no agent selection/unselection or native-grant substitution |
| Result | Structured report plus service snapshot after verified writer quiescence; all criterion rows met | MW-03; exit/report alone cannot mark Done |
| Acceptance and integration | Exact human acceptance records Done; separately verified integration required for dependent coding | MW-03; accept-without-integration remains truthful |
| Session/rework | Retained inside one eligible run; linked rework starts fresh child conversation then retains inside child | MW-03; no latest-history/fresh fallback or hidden retry |
| TRIP preservation | Six roles, identity bytes, approvals, budgets, legacy migration and final-repair history unchanged | MW-02/03/04; identity equality plus effective native comparison |
| Client compatibility | Protocol generation 1 preserved; compiled multi_workflow_v1 feature required for new human operations | MW-03; old client TRIP remains valid, unsupported Single agent operations refused |
| Provider order | Codex first from exact current preparation/admission; Claude Single agent visibly unqualified until separate proof | MW-04; finite native budget requires explicit authorization |
| Recovery | Shared holds/claims/process accounting; pinned family supplies eligibility; uncertain effects retain ownership | MW-03/04; no duplicate dispatch after uncertain launch or restart |
| Migration | Additive next schema after 38; historical TRIP side bindings only; offline restore route for rollback | MW-02; no live database mutation or destructive downgrade |

## Current ownership and file delivery

| Path | Authorized change |
| --- | --- |
| README.md | Planning-status link; continue documenting current TRIP behavior |
| docs/MULTI_WORKFLOW_FEASIBILITY.md | Point to reviewed MW chain; remove superseded first-push prerequisite; preserve future-product status |
| docs/MULTI_WORKFLOW_PLAN.md | Durable resolved schemas/decisions, ownership, barriers, tests and delivery gates |
| docs/MULTI_WORKFLOW_REQUIREMENTS.md | Exact acceptance rows, evidence/review disposition, decisions and remaining qualification requirements |
| docs/TRIP_COMPATIBILITY_BASELINE.md | Redacted source/context comparisons, identity inputs, current regression inventory and D1–D3 triage |

Protected paths include src/, frontend/, migrations/, resources/, tests/, scripts/, build.rs, Cargo.toml, Cargo.lock, AGENTS.md, .agents/, .claude/, .codex/ and all host/data/native-state locations. No fixture or production test is added. Documentation writes are the whole implementation for MW-01.

## Ordered causal verification matrix

These are follow-up test responsibilities, not checks executed by this lane.

| Task / behavior | Controlled cause | Required observable result |
| --- | --- | --- |
| MW-02 TRIP preservation | New descriptive catalog side binding with unchanged legacy launch | Equal old identity bytes/key and equal native policy; approvals/budgets untouched |
| MW-02 new policy | Single agent writer versus read-only policy | Only admitted writer can request scoped human action; read-only write remains denied |
| MW-02 profile admission | Rename versus one effective policy/session/model/instruction/config change | Rename retains authority; effective drift rejects old proof and needs new admission |
| MW-02 migration | Existing schema-38 history, interrupted transaction, repeat open, missing provenance | Exact old records survive; atomic rollback; no fabricated definition or eligibility |
| MW-03 independent admission | Same registered repository without TRIP | Single agent eligible with its own configuration/profile; TRIP fails its own readiness |
| MW-03 result/acceptance | Exit/no report, malformed/unmet row, stale binding, report while writer active | No Done; only complete quiescent exact result and human acceptance progress |
| MW-03 optional checks | Zero selection versus mandatory selected check or stale receipt | Zero follows preset; selected checks require exact authorization/freshness before acceptance |
| MW-03 request changes | Replay exact request versus changed scope/profile/definition | One linked child with preserved parent; fresh initial conversation; changed authority explicitly renewed |
| MW-03 recovery/cancel | Uncertain launch, live descendants, lost history, changed policy | Retain claims/resources; no duplicate/fallback launch; quiescence before release |
| MW-03 mixed workflows | Both families contend for same repository, provider capacity and build slot | Shared reservations/claims apply to active, reserved, stopping and uncertain states |
| MW-03 dashboard | Exact workflow selection/default changes, refresh, stale/ambiguous mutation | Current bound operations; Ready means queued; Single agent has one profile and no TRIP migration/plan option |
| MW-03 clients | Legacy generation-1 TRIP client versus unsupported new feature | Preserve TRIP semantics; require multi_workflow_v1 for new operations |
| MW-04 native | Authorized exact Codex profile, writer permission/denial, protected/read-only attempts, restart | Observe actual enforcement and exact retained history; separate Claude proof |
| MW-04 dashboard | New task → Workflow → Single agent → profile → Ready → result → Accept/Request rework | Real actions wired; Result accepted separate from Integration not recorded; exit without result needs action |

The plan specifies the remaining exact click path for TRIP, permission choices, refresh/reload and native/dashboard qualification. DOM tests and copy changes do not establish those live observations.

## MW-01 checks and separate gates

| Selected check | Immutable revision/configuration | Exact command | Execution owner / current status |
| --- | --- | --- | --- |
| 3449578b-7d7c-4574-a040-5ff0c2942871 | Revision 1 / b4147f19-6849-4fb6-a3aa-ec9c1176ab1e | `cd frontend && deno task build && cd .. && cargo test --locked` | Service only; not executed by lane |
| 133320df-738c-4bdb-8f3a-3d7ea36fe8c0 | Revision 1 / b4147f19-6849-4fb6-a3aa-ec9c1176ab1e | `cd frontend && deno task test` | Service only; not executed by lane |

Both selected commands are exact_shell, cwd `.`, timeout 900 seconds. The broad `./scripts/verify.sh` catalog check is not selected for MW-01. The implementer must not run any selected command independently or reinterpret native action permission as check authorization.

| Gate | Evidence owner | Scope and limit |
| --- | --- | --- |
| A: architecture proof | Future task writer/tests and independent reviewers | Source plus isolated causal tests; no real inference required |
| B: host verification | Service-owned check runner | Exact frozen candidate/configuration/selection receipts; build/DOM/Rust are not native proof |
| C: native/dashboard qualification | App-authorized MW-04 qualification | Actual provider enforcement/history and browser operations; exact finite budget and isolated targets |
| D: review and delivery | Code reviewer, manager, fresh final verifier, human | Current conformance and fresh exact-candidate review before handoff/acceptance; acceptance remains separate from integration |

Current implementation evidence is source/Markdown inspection and scoped hashes. Code review, selected service checks, fresh final verification, human acceptance and integration are not claimed by this lane.

## Limitations, triage and follow-up authority

D1 is the current role-context evidence-access gap: an activated Explorer decision exposes evidence_submitted but no accepted outcome/summary/findings. D2 is the source prompt's findings_ready versus reporting schema's evidence_ready mismatch. D3 is transport wording “embedded upstream 0.11.0” versus current 0.12.0 source/manifest. The [baseline](TRIP_COMPATIBILITY_BASELINE.md) provides redacted reproduction and distinguishes historical activation, current source observations and unobserved native effects. No defect is repaired or control bypassed in MW-01; the plan is grounded independently in accessible source.

There are no unresolved architecture choices in the approved design. Later current-source/check refresh, probe budget/profile/target preparation, inaccessible historical verdict evidence and native/browser observations are explicit gate requirements. The supervising host is fixed and no Git integration/commit/push/publication authority is granted. A dependent task needs app-verified integration of the accepted manifest or an explicitly authorized future base change before Ready/claim; acceptance alone does not supply that evidence.

