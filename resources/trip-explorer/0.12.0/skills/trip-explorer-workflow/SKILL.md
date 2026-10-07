---
name: trip-explorer-workflow
description: Run an opt-in engineering workflow with deterministic conditional Explorer assistance, repository-grounded planning, exact independent review roles, explicit approval before edits, proportional exact-ownership implementers, verification pacing, per-task timing, and a manager-owned completion gate. Use only when the user invokes $trip-explorer-workflow or explicitly requests this workflow.
---

# TRIP Explorer Workflow

For an explicitly requested guidance audit, follow [the read-only audit entry](references/guidance-quality.md#guidance-drift-audit)
and return its report; do not enter the engineering or role-preflight flow below.

Apply [correction guidance](references/guidance-quality.md#corrections-and-durable-lessons) whenever user input corrects the active task.
Run one continuous planning-to-verified-implementation workflow. Keep the
manager as the only user-facing orchestrator and completion owner. Read the
installed `.agents/trip-explorer/config.json`, `adapters.json`,
`preflight.json`, the repository's configured guidance, and only task-relevant
documents. A missing or stale required path or
verification matrix is a project-adaptation gap; do not invent a substitute.
Read `testing.coverage` before planning every task. If it is not exactly
`minimal`, `moderate`, or `extensive`, stop and ask the user to select one.

Resolve every delegated role through `roles.<role>.profile`, then its profile
and adapter. The active host agent remains manager; never launch or substitute
another manager. Require the configured adapter, provider, model, effort,
authority, and session mode to match successful installed preflight evidence.
Recheck adapter availability at task start. A changed profile requires a new
live preflight. Never store or expose credentials.

When cmux is active and the selected profile uses a CLI adapter, run roles through the provider-aware
`.agents/trip-explorer/bin/cmux_observer.py launch`; do not also spawn hidden
duplicates. Invoke native-agent profiles through the host's configured native
agent mechanism. Without cmux, invoke CLI profiles through the installed runner.
Never launch a workflow role through `TRIP-*`, `codex-*`, or another orchestration
skill/script: that hides its console and can recursively create another worker.
Every role prompt must say it is already the assigned role and must work directly
without delegating or launching another Codex/agent process.

## Entry and authority

1. Re-read the latest request and constraints. Inspect repository guidance,
   status, relevant source, current docs, and an approved plan when present.
2. Choose new-task planning or approved-plan resume. A plan artifact or role
   report is never approval evidence.
3. Keep planning read-only until the user approves the exact plan and separately
   authorizes implementation. Editing never implies commit, push, merge, tag,
   version, publish, release, device, destructive, or external authorization.
4. Preserve unrelated/user changes. Record `HEAD` and the complete changed-path
   inventory once at entry. For each implementation or inspection unit, declare
   exact owned, reviewed, shared, and protected paths and compare scoped hashes
   plus changed-path deltas at handoff. Outside-scope drift is preserved and
   reported but does not invalidate the unit; owned/shared/protected drift or
   unsafe attribution pauses it. Freeze the complete candidate diff identity
   once at integration and final review. Never repeatedly hash the whole tree
   merely because control passes between roles.

## Configured roles

Fail closed rather than substituting an unavailable adapter, provider, model,
effort, authority, or session mode.

| Role | Required authority | Required session | Purpose |
| --- | --- | --- | --- |
| Explorer | `read-only` | `retained` | Conditional repository evidence specialist, retained only while an activated uncertainty remains open. |
| Plan reviewer | `read-only` | `retained` | Independent reviewer; five calls maximum. |
| Implementer/fixer | `workspace-write` | `retained` | One writer per independent exact-ownership lane; never integration or completion owner. |
| Code reviewer | `read-only` | `retained` | Independent reviewer; five ordinary calls. |
| Final verifier | `read-only` | `fresh` | Fresh verifier of the frozen final candidate. |

Use the exact user-selected profile for each role. The same provider/model may
serve several roles only through distinct logical sessions. A final verifier
never resumes another role. Read-only prompts prohibit edits, write-producing
commands, builds, tests, generators, Git/config mutation, workflow invocation,
and delegation; the adapter must enforce read-only authority rather than rely
on prose alone. Missing or mismatched capability is a blocking gate.

Never inspect while a writer is active.

## Identity and accounting

Native-agent delivery applies only to native profiles. Preserve logical roles,
generations, counters, findings, and full handoffs in cmux. Process start means
delivery; receipt and final response determine success or ambiguity. After exit,
resume built-in CLIs with `--resume-session <UUID>` from the same role/profile/lane's
receipt through the runner or `cmux_observer.py launch`. Never select the latest
session or resume fresh verifiers. Use distinct result/completion paths per call
and record native IDs separately from logical identity.

Normalize one workflow base, role labels, and optional lane labels to lowercase
`[a-z0-9_]+`. Name roles `<base>_<role>[_<lane>]_g<generation>`, omitting the
lane suffix for the default single-writer path. Begin at generation one and
never reuse a generation. Reuse retained roles while provenance is available;
rehydrate a lost role at its next generation with exact tuple, authority,
counters, findings, ownership, and authoritative context.

Every delivered or ambiguous reviewer attempt consumes one round. A creation
failure proven before delivery consumes none. Never retry uncounted or reset a
counter. Stop ordinary plan/code review before a sixth call.

The automatic review path is one initial review, one consolidated repair, and
one retained focused recheck. A third plan- or code-review call never begins
automatically; it requires an explicit maintainer decision and remains subject
to the five-call absolute safety cap.

Initialize `final_repair_round=0` with `code_round`. After ordinary review
convergence, a final-verifier change request may consume exactly one dedicated
consolidated configured-implementer repair and retained-reviewer recheck, including when
`code_round==5`. It never changes the ordinary counter and cannot repair
ordinary-review nonconvergence.

## Durable context, timing, and acceptance

Use one ignored ledger at `.local/trip-explorer/<context_id>/context.md` after
write authorization. Validate that it is a strict non-symlink descendant.
Before writing, require `git check-ignore` to confirm `.local/trip-explorer/`
is ignored; stop with an adaptation error if it is not.
During read-only planning keep the full handoff in chat. Record request,
constraints, plan/hash/authority, classifications, roles/generations/tuples,
counters/verdicts/findings/dispositions, branch/snapshots/ownership,
verification freshness, conformance, health, docs, risks, and next action.
Never store secrets, private data, hidden reasoning, or raw logs.

Start one overall monotonic timer when the workflow begins or resumes. Maintain
exactly one exclusive top-level phase at all times: discovery/planning, approval
wait, implementation/repair, review, verification, recovery, or release when
separately authorized. Time nested roles, waves, commands, and checks within
that phase. Store UTC timestamps only for auditability and prefer the execution
tool's measured wall time for commands. No phase has a time budget; timing is
diagnostic evidence, never a deadline or reason to truncate work.

Record each completed or interrupted work unit in the existing ledger with a
stable label, category, owner/role/generation, round or retry, start, finish,
elapsed wall time, active work/tool wait/agent wait/user wait classification,
outcome, blocker, produced evidence, discarded work, and verification
invalidation. Close a role timer on delivery, failure, ambiguity, or
interruption; never leave it running across a user wait. Do not create a timer
daemon, polling loop, timing database, or separate timing artifact. Nested and
parallel units may overlap and must not be summed as total elapsed time.

Maintain a living acceptance matrix in the ledger from request through final
gate. For every requested outcome record owner, paths/platforms, implementation
state, focused evidence, review disposition, final evidence, and any manual or
device boundary. Scope discoveries update this matrix without widening authority.
For optional machine checks, read [evidence-format.md](references/evidence-format.md).
For diagnostics, activity/usage reports, or upgrade previews, read
[maintenance.md](references/maintenance.md). Helpers never declare completion.

At every terminal complete or incomplete handoff, reconcile overall wall time
against the exclusive phases. Report coverage percentage and unattributed time
prominently; the total must approximately equal exclusive phases plus
unattributed time. Include nested active/tool/agent/user waits, useful overlap,
retries, repeated checks, discarded work, and outcomes delivered. Rank the
three slowest units and state evidence-backed inconsistencies or efficiency
improvements. Mark unavailable historical timing as `unknown`; never invent
precision or present a partial measured interval as the whole task.

## Optional cmux role consoles

The ledger and normal workflow always operate without cmux. Read
`observability.cmux`; `TRIP_CMUX=auto|on|off` may override it. Invoke
`.agents/trip-explorer/bin/cmux_observer.py` best-effort at workflow start,
role launches, phase changes, and terminal handoff. Record its returned
workspace and disabled reason in the ledger. Any missing executable,
unreachable socket, timeout, malformed response, or command failure falls back
to direct collaboration and never blocks, retries, changes, or completes work.

When invoked from a cmux terminal, operate in-place in that workspace. Use
`CMUX_WORKSPACE_ID` normally. When `CMUX_CODEX_HOOKS_DISABLED=1` removes the
normal hook environment, resolve `cmux current-workspace` and report
`hook-disabled-current-workspace`. Create an unfocused TRIP workspace only when
neither invocation signal is present.

Use one cmux workspace with the manager pane plus one unfocused plain terminal
pane per active role. Launch each role through `cmux_observer.py launch`, which
runs `cmux_role_runner.py`. Codex retains the selected provider's ordinary console.
Claude uses `--output-format stream-json --verbose` with `claude_console.py` to
display public text/tool events as they arrive; save only its final result text.
Never display/store private reasoning or replace events with generated summaries.
Do not inject extra Git status/diff commands or launch a second display agent.

Keep user interaction and authority decisions in the manager pane. Role panes
are passive observation surfaces by convention; do not add input locks, key
interception, or custom terminal security. Never open Markdown, diff, browser,
or simulator previews, and never call `cmux markdown` or `cmux diff`. Leave
completed panes and their scrollback open for inspection.

Store each role prompt, final response, and completion receipt under the active
ignored `.local/trip-explorer/<context_id>/` runtime. The receipt is only an
exit/result/session pointer, not a second console stream. Nonzero exit, missing
result, or session mismatch is failure/ambiguity, never approval or automatic retry.
Prefer completion notifications or supported blocking waits. Reuse a role pane only after its prior
process exits, and always reuse it through `cmux_observer.py launch --surface`.
Never send prompt, correction, stop, or follow-up prose directly to the pane:
after the Codex process exits, the shell would execute that prose as commands.
Never run a writer and inspector concurrently.

Require the writer prompt to stop and return `scope-expansion-request` before
adding an unplanned abstraction, dependency, fixture, state machine, timer/job,
module, platform path, or file outside assigned ownership. Pause the affected
wave until the manager reconciles it with the approved plan; obtain user
authority for a material expansion. Treat a closed role pane as a failed role
invocation and recover under the normal generation/accounting rules. cmux never
owns workflow state.

Report role starts, material progress, phase changes, failures, and recovery
with elapsed time and last useful evidence. Follow host communication rules
during active work; do not wake merely to repeat unchanged status or inspect
logs. Continue independent authorized work, then yield to a verified completion
notification or supported blocking wait. Use the host's documented receipt
watcher when available; follow [completion-driven waits](references/maintenance.md#completion-driven-waits).
Report unavailable or ambiguous delivery explicitly, without promising a wake-up
or automatically retrying. Waiting for the user is never a stall; elapsed time
alone never proves one. Inspect public activity, process state, and receipts
when a failure, deadline, or other evidence warrants investigation. Recover
only under the exact identity/accounting rules. After two materially identical
stalled attempts, narrow the check or report the infrastructure boundary;
never make an unchanged third attempt.

## Deterministic Explorer activation

Explorer is conditional, never a routine relay. Read [the activation reference](references/explorer-activation.md), complete its census, and launch only under its planning, rescue, or final conditions.
Otherwise record `Explorer not invoked`. Never request routine capsules,
post-wave deltas, or repair checks; apply the reference's output, measurement,
retention, and second-rescue approval rules to every invocation.

## Planning and size

1. Follow [discovery recommendations](references/guidance-quality.md#discovery-recommendations), then conduct the bounded census. Apply the deterministic gate; ask
   Explorer only its activated evidence question or plan directly from current
   repository evidence.
2. Interpret evidence and draft the file-level plan yourself with scope,
   architecture, edge cases, docs, verification, the installed test-coverage
   level and resulting test matrix, platforms, completeness criteria, and exclusions.
   Present it using [explanation guidance](references/guidance-quality.md#explanations-the-user-can-assess).
   Apply the complexity expansion decision gate below to every proposed
   mechanism before plan review. If it triggers, obtain the user's decision
   before selecting that mechanism in the plan.
3. Before plan review, classify exactly once:
   - **bounded:** one tightly coupled outcome; ordinary multi-file/layer
     traversal is not automatically broad;
   - **broad:** one coherent outcome with materially wide ownership/blast
     radius across platform, shared/public, build, release, or verification
     contracts;
   - **program-sized:** multiple independently reviewable outcomes or major
     subsystem/platform slices with materially long work.
4. For every task, create a concise ownership/dependency map. Bounded work uses
   one owner without lane ceremony. When two or more independent outcomes can
   materially shorten the work, identify exact owned paths, shared/protected
   seams, dependency edges, focused checks, integration order, and one manager
   integration/documentation owner. Complete mutable shared seams first, then
   run only disjoint lanes concurrently. If safe ownership or separable inputs
   cannot be proven, use one writer.
5. For program-sized work, expose coherent slices, ownership, verification,
   rough elapsed range, uncertainty, and cost. Obtain an explicit sliced versus
   consolidated choice. Slices use bounded outcome matrices. Consolidated work
   uses the same dependency-aware lanes: finish blocking seams, run independent
   exact-ownership lanes concurrently, integrate once, and use one aggregate
   plan matrix. A one-go choice means one approved program and integration
   cycle, not one monolithic writer. Never silently create branches, releases,
   or workflows or override that choice. Reclassify after substantive scope
   change.
6. Have the retained plan reviewer return exactly `APPROVED`,
   `REQUEST_CHANGES`, or `NEEDS_REWORK`. Preserve every finding/disposition and
   use one initial review and one retained recheck after consolidated revisions.
   If the recheck remains open, stop for a maintainer decision before any third
   call; the five-call absolute cap still applies. Then obtain exact-plan
   approval and separate implementation authorization.

## Complexity expansion decision gate

Implementation authorization covers the approved outcome and described
solution shape. It does not authorize an unplanned permanent framework,
analyzer, linter, parser or compiler surrogate, code generator, background
subsystem, module, dependency, integration harness, test framework, duplicated
bootstrap, or other support system. Apply this gate while planning and again
before implementation or review-driven repair. Treat support or test
infrastructure with a maintenance surface materially larger than the behavior
it protects as the same kind of expansion. Profile-appropriate causal tests
using existing seams do not trigger this complexity gate, though the separate
test-expansion rules below still apply.

Before editing an expansion, stop the affected writer or repair and present the
user with:

- the required outcome and evidence for the gap;
- the smallest adequate solution and its focused verification;
- the proposed expansion and why the smaller solution is insufficient;
- the expected permanent footprint in files, approximate code and test size,
  dependencies, build or CI runtime, and ongoing maintenance; and
- a recommendation.

Wait for an explicit choice. When planning proposes the expansion, obtain that
choice before plan review or approval and record it in the plan. When the need
emerges later, pause the affected edits. Plan approval, general implementation
authority, assigned ownership, or reviewer severity does not approve an
expansion that was not presented this way. A reviewer may identify a gap but
cannot authorize new scope. Require every implementation and accepted-finding
repair prompt to return `scope-expansion-request` with this comparison before
editing the expansion. Continue only non-overlapping approved work that cannot
prejudice the decision. When classification is uncertain, pause and ask.

## Installed test coverage policy

Honor `testing.coverage` for every task and accepted review repair:

- `minimal`: New automated tests default to zero. Name the shortest exact
  manual validation path and add at most one cheap causal test only for a
  meaningful deterministic app-owned risk that existing evidence does not
  protect and that reuses existing seams.
- `moderate`: require focused causal coverage for meaningful deterministic
  changed behavior or accepted defects using existing seams. Reuse or repair
  sufficient tests; add tests only for genuine behavior-coverage gaps.
  Cover the primary success path plus the most relevant failure or boundary;
  document why any meaningful behavior remains manual-only.
- `extensive`: plan comprehensive automated coverage for relevant success,
  failure, boundary, regression, and interaction paths across affected
  app-owned layers and platforms wherever deterministic existing seams permit.
  Explicitly identify any remaining manual, device, or external-system boundary.

The selected profile governs the plan matrix, every writer and repair prompt,
manager integration, independent code review, verification receipts, and final
conformance. It is an expectation level, not a line/branch percentage promise
or a mechanical test-count quota. Apply [meaningful behavior testing](references/behavior-testing.md)
for test selection, TDD, bug fixes, existing-test changes, and causal evidence.

Under `minimal`, pause before a second new test for one bounded outcome, more than
three across a program-sized task, or roughly 100 test/support lines for one
outcome. Under every profile, pause before adding a test-only production seam,
new fake/helper framework, scheduler model, dependency, integration/server/
device harness, duplicated bootstrap, exhaustive low-value permutations, or
test growth materially disproportionate to the selected profile and risk.
Report the unmet coverage expectation, smallest adequate approach, projected
footprint/runtime/maintenance, and recommendation. The coverage selection never
authorizes a device run or permanent support system; obtain separate approval
or report the accepted gap. Reviewer severity never bypasses this gate.

## Essential code readability contract

Prefer fewer comments. Names and structure explain the normal path;
ordinary control flow, assignments, delegation, and behavior already clear from the code
receive no explanatory comment or KDoc. Before adding one, improve naming or
structure when that can carry the meaning without creating unjustified
abstraction. Do not add KDoc merely because a declaration is public.

Retained comments and KDoc are brief and reserved for a non-obvious contract,
invariant, rationale, lifecycle or concurrency constraint, security boundary,
external-system behavior, or platform limitation. They never narrate code,
restate the next line, compensate for unclear naming, or preserve superseded
behavior. Judge comments by meaning rather than count; do not add a comment
quota or remove a necessary boundary explanation merely to reduce volume.
Project guidance may strengthen this contract but must not relax it.

Include this contract in every implementation and accepted-finding repair prompt.
Before handoff and during integration, inspect every added or changed comment and KDoc
together with the code it describes; remove ordinary-path
narration, redundancy, and stale explanation, tighten justified wording, and
improve names or structure where those can carry the meaning.
Require the independent code reviewer to report narrative, stale, or
unjustified comments as concrete maintainability findings. Final manager
conformance must confirm that every retained changed comment belongs to one of
the allowed categories before verified completion.

## Authorized implementation and review

1. Build the implementation capsule from outcome, classification, constraints,
   plan/snapshot, contracts, acceptance checks, docs, artifacts, risks, and
   ownership. Reconcile Explorer evidence against source; it is not authorization.
2. Launch the single retained configured writer for
   bounded work or one retained configured writer per proven disjoint lane. Give
   every writer exact owned and protected paths, shared-seam state,
   dependencies, the essential code readability contract, selected test
   coverage profile and planned test matrix, manual boundaries, any approved
   test support, [behavior-testing rules](references/behavior-testing.md), and integration order.
   Writers preserve concurrent
   changes, never edit another lane or shared integration/docs surface, and
   return `scope-expansion-request` instead of redesigning the approved plan.
   Multiple lanes may write concurrently only while their mutable inputs and
   output paths remain disjoint; otherwise use one writer or a shared-seam
   barrier. Writers never own integration or completion.
3. Name exactly one build owner and one build slot for the checkout. Concurrent
   writers return requested checks and yield at a coherent source checkpoint;
   only the build owner runs write-producing builds, tests, compiles,
   formatters, generators, or shared-daemon cleanup. Batch compatible checks.
   A check may overlap another writer only when that writer cannot mutate any
   input to it; otherwise pause writers for the check.
4. After all relevant writers yield, the manager integrates once, inspects
   effective behavior and the complete candidate diff, applies documentation
   policy and the essential code readability contract, and runs diff hygiene
   plus focused checks through the build owner.
   Consolidate routine failures using [repair decisions](references/guidance-quality.md#repair-or-revisit-a-decision)
   and [proportionate execution](references/guidance-quality.md#proportionate-execution).
   For difficult investigations, apply [explanation guidance](references/guidance-quality.md#explanations-the-user-can-assess).
5. Start one independent integrated code review with `code_round=0`. Require
   exactly one terminal `APPROVED`, `REQUEST_CHANGES`, or `NEEDS_REWORK`.
   Apply [completion criteria and review scope](references/guidance-quality.md#completion-criteria-and-review-scope) across handoffs and rechecks.
   The reviewer applies the essential code readability contract to every
   changed comment and KDoc and checks the selected test coverage profile
   against every acceptance row. The manager decides whether each finding is a
   current defect separately from whether its proposed remedy is proportionate.
   Consolidate accepted findings
   into one repair batch, parallelizing only still-disjoint ownership, then run
   focused invalidated checks through the build owner and give the same reviewer
   one retained recheck. If that recheck does not approve, stop for a maintainer
   decision before any third call; the five-call absolute cap still applies.

Do not add permanent integration/server/bootstrap/UI/screenshot/device harnesses
without explicit user authorization. Prefer source tracing, existing coverage,
affected compilation, and reported manual boundaries. Do not run a device,
emulator, simulator, or live runtime path unless the current request explicitly
authorizes it.

## Verification pacing and conformance

During implementation and ordinary repair, the named build owner runs focused
behavior checks and affected module/source-set/platform compiles from project
configuration. Reuse evidence only while relevant files, contracts, build
inputs, and configuration are unchanged; record every invalidation. Never
repeat a broad matrix solely because a review round is new.

Write a verification receipt for every completed check: candidate Git/diff
identity, command or inspection, covered acceptance rows/platforms, relevant
inputs, result, elapsed time, and invalidation conditions. Before any later
gate—including an authorized release—reuse every still-valid receipt and rerun
only missing or invalidated dimensions. A release-metadata-only change gets
lightweight consistency and diff hygiene; rebuild only artifacts whose embedded
metadata changed. Record a reason whenever a full gate is repeated, and count
unnecessary conservative duplication separately in the final timing report.

After ordinary review and its retained recheck converge, run exactly one
post-review final matrix for the stable integrated candidate entering final
verification. Bounded work uses the configured proportionate plan matrix.
Broad/project-wide work uses configured broad checks.
Sliced program outcomes use bounded matrices; a consolidated program uses one
aggregate plan-specific matrix across all affected outcomes/platforms. Run a
broad check earlier only when a broad contract itself changed and name why
focused evidence is insufficient. Empty/missing applicable configuration blocks
verified completion.

Then the manager checks every conformance dimension before final verification:
exact module/source-set/platform placement,
visibility, named docs/contracts, the essential code readability contract,
the selected test coverage profile, owned/protected paths, prohibited
dependencies or abstractions, preservation
constraints, one documentation owner/no-doc disposition, and
presence/location/boundary coverage for every named test. All must pass.
Candidate defects use ordinary repair only while budget remains;
material plan divergence returns to plan authority. At `code_round==5`, an
unresolved manager divergence is incomplete and cannot borrow final repair.

Workflow structure or verification speed alone does not justify a module split.
Use existing module/source-set boundaries first. Any actual split requires a
separately approved measured architecture plan proving cohesive ownership,
acyclic direction, measured current scope, and material reduction.

## Fresh final gate

1. Invoke no-verdict final Explorer traceability only for an explicitly
   repository-wide request, three or more production modules, or unresolved
   source-evidence conflict. Otherwise record `Final Explorer not invoked`;
   every final pass remains bounded and supplies no verdict.
2. Launch the configured fresh exact final verifier with authoritative
   paths/evidence only through its exact selected adapter. Require a fresh
   session, installed preflight match, enforced read-only authority, no
   workflows/delegation, and no silent model fallback. Give it the same frozen
   candidate and verdict schema; do not ask
   it to implement, test, or orchestrate. Require exactly one `APPROVED`,
   `REQUEST_CHANGES`, or `NEEDS_REWORK`.
   `APPROVED` proceeds to the manager gate. A malformed result fails the gate.
   `NEEDS_REWORK` is structural and ends incomplete without consuming the
   dedicated repair. Only `REQUEST_CHANGES` may enter step 3.
3. On `REQUEST_CHANGES`, consume the one dedicated repair before delivery,
   adjudicate accepted defects separately from proposed remedies, consolidate
   the smallest approved fixes, and run invalidated focused checks through the
   build owner. Reuse unaffected final-matrix evidence; repeat the aggregate
   matrix only when the repair changes a shared, build, dependency, packaging,
   or release input that invalidates it. Then require the retained code reviewer
   to return explicit `APPROVED`. `REQUEST_CHANGES`, `NEEDS_REWORK`, malformed
   output, or delivery failure from that recheck ends incomplete.
   Refresh every conformance dimension invalidated by repair; remaining
   divergence ends incomplete. Only then launch one second verifier. Under
   an external provider, make this a focused disposition pass over the prior findings,
   accepted fixes, and current candidate rather than another open-ended full
   audit. Its `REQUEST_CHANGES`, `NEEDS_REWORK`, or malformed output ends
   incomplete; never grant a second cycle.
4. The manager rereads request/plan/diff, traces every requested behavior and
   platform, consumes fresh evidence, and runs only missing/invalidated
   configured dimensions. Apply project documentation policy and separate
   request verification, builds, tests, manual/device evidence, docs, and risk.
   Use [explanation guidance](references/guidance-quality.md#explanations-the-user-can-assess); show accepted requirements,
   delivered behavior, evidence type, and uncertainty from existing records.
5. The manager alone declares completion. Stop at verified implementation by
   default. Require separate authority for Git integration or release.
6. Report an implementer-quality assessment from observed ownership, review rounds,
   finding severity/repetition, repair regressions, manager corrections,
   documentation compliance, and retained-context behavior. Classify corrections
   using [correction guidance](references/guidance-quality.md#corrections-and-durable-lessons) before treating them as failures.

Stop for unavailable exact roles, unresolved decisions, unsafe ownership,
exhausted review budgets, failed dedicated repair, unresolved verification, or
new destructive/external/device/fixture authority.
