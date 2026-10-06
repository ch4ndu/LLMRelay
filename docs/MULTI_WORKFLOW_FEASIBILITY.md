# Feasibility of multiple workflows in LLMRelay

**Date:** October 6, 2026

**Status:** Future product direction. Implementation follows repository cleanup and the initial GitHub push. The current TRIP product contract remains in effect until an implementation plan is approved.

LLMRelay can offer several workflows in one distributed application. TRIP would become one option alongside simpler ways to run development work. The existing session runtime provides a substantial foundation, but workflow selection requires changes to execution authority, profile activation, admission, recovery, and result handling.

## Product direction

Keep one installation, dashboard, task store, scheduler, coordinator, and process owner. A project selects a default workflow, and each task can choose another supported workflow. The same repository could use Single agent for a small fix, Build and review for independent oversight, and TRIP for a substantial feature.

Non-TRIP work should require normal repository registration, provider authentication, compatible agent profiles, and relevant permissions. TRIP installation and its six-role setup belong to the TRIP workflow. Agent completion, passing checks, an approved review, and human acceptance remain separate facts.

Jinn provides useful architectural patterns: reusable named agent profiles, durable work, versioned workflow definitions, explicit outputs, and a boundary between workflow execution and session supervision. An organization hierarchy, Jinn runtime dependency, plugin system, or workflow canvas is unnecessary for the first product increment.

## Runtime and workflow ownership

| Concept | Responsibility |
| --- | --- |
| Shared runtime | Provider execution, permissions, native process identity, terminal control, repository ownership, evidence capture, uncertain launches, and recovery mechanisms. |
| Workflow | Required steps, handoffs, gates, completion requirements, rework, and eligibility to continue or resume. |
| Agent profile | Provider, model, effort, instructions, and an admitted execution policy. |
| Workflow run | Exact definition, effective profile configuration, executions, evidence, and decisions for a task attempt. |

An agent's display name must not determine its permissions. Execution policy and the retained or fresh session contract are also separate: read-only access does not imply that every invocation starts a fresh conversation.

Use one coordinator with shared checks for controls, holds, permission waits, process quiescence, uncertain effects, claims, and evidence. Workflow-specific evaluation decides the next authorized operation. Keep workflow selection at defined boundaries for admission, attempt creation and rework, advancement, resume authority, and projections rather than scattering workflow identifiers throughout the application.

TRIP can retain its specialized evaluator and its rules for lanes, integration, approval lineage, and final verification. Share primitives where the second workflow demonstrates a common contract; there is no requirement to express all TRIP behavior in a generic graph.

## Versioned workflow catalog

Ship bundled presets first. New simple workflows use small validated sequence definitions whose steps their evaluator actually consumes. Begin with agent execution, structured result submission, and a human decision; add checks and review when required by another workflow.

Retain the exact definition content and hash, together with effective profile configuration, for each run. A version string alone is insufficient to explain historical execution. Presets may initially be immutable. User editing can follow without requiring arbitrary graphs, conditions, nesting, scripting, or a general template language in the first increment.

Existing TRIP attempts retain their exact stored workflow version, hash, and provenance. A catalog can identify their workflow family without rewriting historical identities or reinterpreting approvals. Changes to definitions or profiles do not silently alter active runs; incompatible changes require an explicit transition or a fresh attempt.

Pinning a definition does not itself authorize resume. Native history, compatible provider and security contracts, current workflow authority, evidence freshness, session rules, and accounting must also remain valid. A missing or ineligible definition produces explicit recovery.

## Initial workflows

| Workflow | Proposed flow | Sequence |
| --- | --- | --- |
| Single agent | Task and acceptance criteria, one writing agent, structured result, human acceptance or request changes. No mandatory manager or review chain. | First proof alongside existing TRIP. |
| TRIP | Existing planning, approvals, implementation, integration, reviews, checks, and handoff. | Preserve throughout the refactor. |
| Build and review | Implementation, independent review, configured checks, bounded repair, and acceptance. | After Single agent establishes the shared boundaries. |
| Research | Investigation, findings, and optional critique. | Optional later workflow. |

Human acceptance before Done is the recommended Single-agent default, subject to the product decision. An agent ending its turn or a process exiting is not a successful result. Request changes creates explicitly authorized linked work with retained history, rather than a hidden automatic retry.

Research remains exclusive initially. Concurrent inspection of a worktree being modified can produce unstable findings. Later concurrency would need an isolated checkout or immutable snapshot with a clear evidence identity. Recipes can remain visibly TRIP-specific during the first proof; generalizing their configuration and provenance is separate work.

## Required architectural changes

The current workflow JSON records contract identity; Rust code implements progression. Adding a second JSON file alone does not add an executable workflow.

| Boundary | Current coupling | Required change |
| --- | --- | --- |
| Execution authority and compatibility | Role identities affect native launch policy, permission actionability, credentials, and capability identity. Compatibility packs require six role contracts. | Provide an explicit policy contract for new workflows while preserving effective TRIP behavior. |
| Profile activation | Task profile authority joins active TRIP configuration tables. | Support profile activation without TRIP installation or configuration. |
| Resume and recovery | Global TRIP workflow and prompt hashes, phase lists, and final-verifier special cases determine eligibility. | Resolve authority against the run's workflow and session contract. |
| Attempt creation and rework | Creation records TRIP identity, planning, and review budgets. Rework and migration actions also assume TRIP. | Create attempts under the selected workflow and scope legacy migration behavior correctly. |
| Admission | Ready requires TRIP project readiness and six current role settings. | Evaluate requirements for the selected workflow after shared admission checks. |
| Dashboard projections | Progress, labels, readiness explanations, and completion text assume TRIP stages. | Supply workflow-specific steps and truthful evidence states. |

Execution authority, profile activation, and resume are the highest-risk changes. Preserve capability identities only when effective launch behavior is also unchanged. Before refactoring, capture comparisons of prepared launch configuration, generated native settings, denials, tool lists, hooks, and credential scopes for each TRIP role and provider. Hash equality alone does not establish semantic compatibility. New or changed contracts need relevant provider qualification.

## Smallest credible proof

Run Single agent and TRIP through the same application and shared runtime. The proof must cover:

1. Single-agent admission and execution on a registered repository without TRIP installation.
2. A structured result followed by human acceptance, with no Done transition from process exit alone.
3. An actionable writer permission request, native enforcement of denial, and denial of actions outside a read-only policy.
4. An uncertain launch retaining unresolved ownership without a duplicate dispatch.
5. Cancellation waiting for verified quiescence and preserving history.
6. Restart resuming the exact eligible native conversation under the correct workflow authority.
7. Request changes creating linked rework under the same workflow, with correctly bound acceptance.
8. Mixed workflows respecting the same repository ownership rules.
9. Existing TRIP launch policies, history, gates, review accounting, and recovery behavior retaining their meaning.
10. Non-TRIP tasks receiving neither six-role prerequisites nor TRIP migration actions.

An architectural proof can start with one provider, selected from current capability evidence. Distribution claims covering both Codex and Claude require qualification on both. Existing behavioral expectations remain authoritative; tests can receive legitimate interface adaptations and focused additional coverage.

## Decisions before implementation

- Initial catalog and whether Research is required.
- Default human acceptance policy.
- Degree of user customization: preset parameters first, or eventual sequence editing.
- Provider qualification order and authorized native-session budget.
- Exact policy, profile, definition, and migration schemas.

Technical feasibility is well supported by the current architecture. Effort remains uncertain until the second workflow exercises the shared boundaries. Source inspection and architectural consensus do not establish live provider behavior or release readiness.

## Implementation references

- [Domain identities](../src/domain.rs): `RoleKind`, `CapabilityIdentity`, and `RoleContext`.
- [Provider preparation](../src/providers/mod.rs) and [compatibility packs](../src/provider_compatibility.rs).
- [Permission handling](../src/permissions.rs) and [TRIP profile authority](../src/trip.rs): `task_profile_descriptor`.
- [Scheduler](../src/scheduler.rs), [coordinator](../src/coordinator.rs), and [workflow operations](../src/workflow.rs).
- [Session store](../src/store.rs), [recovery](../src/recovery.rs), and [role switching](../src/roles.rs).
- [Workflow resources](../src/workflow_resources.rs) and [recipe provenance](../src/recipes.rs).
- [Task readiness](../frontend/src/components/TaskForm.tsx) and [board progress](../frontend/src/components/TaskBoard.tsx).
- [Current product contract](../RESEARCH_AND_PLAN.md) and [project milestones](MILESTONES.md).
- [Jinn workflow model](https://github.com/hristo2612/jinn/blob/3ae6465715b6195db057d4c23156b696c71dc179/packages/jinn/src/workflows/model.ts) and [session executor](https://github.com/hristo2612/jinn/blob/3ae6465715b6195db057d4c23156b696c71dc179/packages/jinn/src/workflows/session-executor.ts) at the inspected revision.
