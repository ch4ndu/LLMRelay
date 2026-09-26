# LLMRelay v1 code audit — 2026-09-26

Status: A01–A13 repairs are engineering-complete within the agreed scope. Independent code review, aggregate verification, and fresh final review approved the candidate. A final acceptance-state regression was repaired and independently rechecked. This report is not application acceptance or a live-provider qualification.

The audit covers the current working tree, including the existing uncommitted implementation, rather than only HEAD `f8c7e29c60e0f26cda8ca2e25e8fcf72a1c06a86`. A 185-file SHA-256 baseline was checked by reviewers and the manager. Prior changes were preserved. Audit artifacts are under `.local/trip-explorer/llmrelay_v1_audit_20260926/`.

The manager reviewed dashboard and cross-layer paths; two read-only GPT-5.6 Sol/high passes reviewed engine and platform code; a fresh Claude Fable/medium pass independently challenged findings and completed remaining dashboard reads. Its APPROVED verdict applies to the adequacy of the audit evidence, not to the application. No production repairs, service operations, or data changes were performed.

## Findings

The findings below preserve the original audit evidence and severity. The repair
candidate adds atomic workspace publication and coordinator isolation, current
manager-authority checks, blocker invalidation, safe transcript lookup, bounded
conformance and role-connection handling, and dashboard retry/form/storage fixes.
Lane output hashes are explicitly agent claims. Blocking execution is bounded for
browser operations and stateless control requests; stateful attachment operations
and the human-command handler retain their existing execution paths. Portable
source provenance changes workflow identity; see
[the recovery implications](OPERATIONS.md#bundled-workflow-identity-changes).

Repair review and verification evidence is retained in
`.local/trip-explorer/llmrelay_audit_fixes_20260926/`. Existing audit line anchors
below refer to the pre-repair tree.

The aggregate check passed 33 browser flows, 75 Rust library tests, 109 contract
tests, and two runtime tests, plus formatting, type checking and builds. After
the narrow final repair, the affected acceptance regression passed again:
unresolved blockers remain rejected without mutation, while a legitimate held
single-step attempt reaches the unchanged snapshot gate. The full successful
acceptance path was not newly exercised by that focused test. Unaffected aggregate
evidence was retained; the suite was not repeated solely for a review round.

### Separate non-blocking observations

The final reviewer recorded these limitations for future assessment; they are not
additional completed repairs:

- The validation caller discards the promotion helper's boolean outcome. A stale
  tuple may remain reserved while that caller reports success; explicit handling
  of that outcome merits a focused follow-up.
- Setup-fixture readiness publication remains separate from the five
  `materialize_project_policy` callers covered by A01.
- Transcript validation retains a check-then-open window under the existing
  same-user boundary; it is not an OS-level isolation guarantee.
- Stateless control offload has source/classification coverage, while the causal
  responsiveness test exercises the browser operation route.
- Accept remains enabled by review phase even when a blocked attempt will be
  rejected by the server. The server now gives an accurate error; proactive UI
  eligibility was not part of this repair.

No service restart, real-data operation, live-provider qualification, packaging,
commit or publication was performed as part of the audit repairs.

Severity: P1 high-priority workflow failure; P2 substantive correctness/boundary failure; P3 lower-impact robustness or evidence issue; P4 low-priority hardening/conformance issue. Source anchors refer to the audited tree.

### A01 · P1 · Interrupted workspace promotion can stall the whole coordinator

Sources: `src/trip.rs:10537`, `src/scheduler.rs:969`, `src/coordinator.rs:74`, `src/coordinator.rs:134`, `src/store.rs:3793`.

Materialization marks the workspace ready separately from the scheduler's claim and attempt updates. A crash between these writes leaves a ready workspace with a `workspace_reserved` attempt. Reconciliation excludes ready workspaces. The coordinator repeatedly selects the stranded attempt, attempts manager dispatch, and gets `attempt is not dispatchable`. That error exits the tick before later attempts advance or new tasks are claimed. The attempt remains first because it never records a successful turn.

Correction to the initial review: this is not proven to require manual database repair. Generic cancellation runs before attempt dispatch and can cancel the task and release its reserved/running claim (`src/coordinator.rs:4099`, `:5597`). That sacrifices the current task's continuation: the user must recreate it, and the ordinary cancellation path leaves its workspace behind. No task-record deletion is claimed.

Remedy: atomically commit the related workspace/claim/attempt updates; recognize mixed states during recovery; isolate a non-dispatchable attempt so it cannot starve unrelated tasks. Regression evidence should interrupt between final materialization and scheduler promotion and prove both recovery and progress of a second task.

### A02 · P2 · Revoked manager authority can survive into a mutation transaction

Sources: `src/task_cli.rs:215`, `src/roles.rs:764`, `src/trip.rs:7633`, `src/trip.rs:1120`.

Role dispatch authenticates a credential and validates process ancestry before a TRIP operation acquires its own transaction. A concurrent role switch can revoke that credential between those steps. Explorer decisions, lane configuration, integration requests, check selection, and conformance submission check project/workspace readiness but do not revalidate the current credential and generation inside the transaction.

This concerns an in-flight request from the previous manager, not an arbitrary unauthenticated actor. Recheck exact credential/session/generation authority atomically with each mutation. Test a barrier-controlled revocation between authentication and commit and assert zero downstream changes.

### A03 · P2 · A blocked result does not invalidate an older transition proposal

Sources: `src/coordinator.rs:5604`, `src/workflow.rs:2391`, `src/workflow.rs:2217`, `frontend/src/components/TaskDetail.tsx:225`.

Consuming a blocked/needs-input result changes attempt status and task attention without incrementing task version or invalidating pending proposals. A previously offered transition can still match version, phase, and hashes after the manager exits. The human can apply it; the later acceptance path checks lifecycle and evidence but not the unresolved attempt status. The visible needs-input indicator does not enforce the gate.

Remedy: invalidate stale transition authority when consuming a blocker and reject transitions/acceptance incompatible with an unresolved blocker. Test proposal → blocked result → attempted transition/acceptance.

### A04 · P2 · Setup retries can replay a definitively rejected stale version indefinitely

Sources: `frontend/src/components/ProjectSetup.tsx:633`, `frontend/src/api.ts:682`, `src/trip.rs:1606`.

`runTrip` retains its request identity after every error, including definite stale-version rejection. The reuse helper deliberately ignores `expected_*` fields when comparing intent and returns the old complete body. Refreshing from project version 1 to 2 therefore does not update the next identical action: it resends version 1. Persistence survives reload.

A read-only invocation of the actual helper confirmed requested version 2 became retry version 1. The full UI/server sequence was traced, not exercised live. Clear retained identities on definitive failure while preserving exact ambiguous retries; test stale rejection → refresh → same action.

### A05 · P2 · A late recipe Save response can attach to a different New draft

Sources: `frontend/src/components/Recipes.tsx:344`, `:566`, `:718`, `:861`.

Profile, recipe, and schedule save handlers attach the returned ID/version to whichever draft is current when the response arrives. Their New buttons remain enabled during the request. Save A → New B → A's response can turn B into an edit of A; the next save overwrites A instead of creating B.

Disable changing form identity while pending or bind responses to a captured draft generation. Test all three ordinary save paths with deferred responses. The existing exact-retry protection does not cover these handlers.

### A06 · P2 · Transcript reads can rewrite files outside the transcript directory

Sources: `src/transcript.rs:554`, `:569`, `src/server.rs:1162`, `src/control.rs:953`.

Public transcript operations pass an unvalidated session string to `root.join(session_id + ".jsonl")`. An absolute or parent-traversing value can address an unrelated file. Reading a malformed or incomplete file invokes recovery that rewrites it; oversized files can be compacted. Structurally valid transcript records can be returned.

This requires authenticated browser or OS-admitted human control access and is not OS privilege escalation. It remains an unintended destructive file operation outside the chosen session. Validate closed session identifiers, verify session existence, and enforce transcript-root/regular-file ownership before I/O. Test browser and control routes against outside sentinel files without changing them.

### A07 · P3 · Conformance permits null evidence

Sources: `src/trip.rs:8556`, `:8627`, `src/operations.rs:209`.

Required conformance sections are checked for presence rather than meaningful typed values; acceptance evidence merely needs a nonempty array, so null sections and `[null]` can satisfy these checks. This weakens a manager's self-attestation rather than proving an independent permission bypass. Require bounded structured evidence and reject placeholders. Test null, empty, and valid minimal submissions through the real validator.

### A08 · P3 · Lane output hashes are syntactic claims, not verified content identities

Sources: `src/trip.rs:8440`, `:8478`.

A 64-character hexadecimal output hash is accepted and stored without recomputation from the lane output. Actual path/ownership checks still apply. Compute a canonical output identity or label the field explicitly as agent-claimed; test a mismatched hash and real output.

### A09 · P3 · Editing a task-create retry after an ambiguous response can duplicate it

Sources: `frontend/src/components/TaskForm.tsx:354`, `src/workflow.rs:1024`.

An identical retry safely reuses its operation ID. Editing a field or switching create mode generates a new ID, however, even when the first create committed but its response was lost. The still-open modal can create a second task. The live board may expose the first task, reducing but not preventing this risk.

Retain an explicit unresolved-create state and reconcile or retry the original request before accepting a new create intent. Test commit plus lost response, followed by an edited retry.

### A10 · P3 · Idle role connections have no deadline or connection ceiling

Sources: `src/task_cli.rs:85`, `:108`; contrast `src/control.rs:822`.

Each connection spawns a task that can wait indefinitely for a newline, retaining up to roughly 1 MiB before authentication. The mode-0600 socket is limited to the same user and admitted managed providers. A faulty or adversarial admitted client can consume descriptors/tasks/memory. Practical exhaustion was not measured.

Bound first-frame connections and apply an idle deadline, separately accounting for authenticated permission waits. Test timeout/disconnect capacity release and shutdown with parked readers in the existing harness.

### A11 · P4 · Slow synchronous operations occupy async executor workers

Sources: `src/server.rs:1022`, `src/control.rs:839`, `src/supervisor.rs:334`, `src/cmux.rs:1841`.

Several authenticated operations synchronously perform filesystem/process work or sleeps on Tokio workers, contrary to project standards. The runtime is multithreaded and the coordinator tick already uses a blocking pool. Whole-runtime starvation under realistic usage was not demonstrated; this is a hardening/conformance finding, not a confirmed explanation for observed hangs.

Move expensive synchronous work to bounded blocking execution while preserving operation receipts and ordering. Test responsiveness with a controlled slow-operation barrier before claiming measured runtime improvement.

### A12 · P4 · Production manifest embeds a local developer path

Sources: `resources/trip-explorer/0.9.0/source-manifest.json:3`, `src/trip.rs:24`.

The embedded source manifest contains a private absolute checkout path. No credential was identified. Replace it with stable repository provenance and retain the immutable revision; validate production resources for accidental home-directory paths.

### A13 · P4 · Browser storage failure can latch setup controls

Source: `frontend/src/components/ProjectSetup.tsx:634`.

The setup command guard is set before `localStorage.setItem`, which is outside the try/finally block. A quota or storage-access exception leaves the guard true, and later actions silently return until remount/reload. This is conditional on browser storage failure, not ordinary setup behavior. Move persistence inside protected error handling; test throwing storage with visible error and released guard.

## Disposition and repair order

All twelve original candidates survive with narrowed impacts/severities; one storage-exception finding was added. The final review's additional coordinator-stall entry is merged into A01, not double-counted. Totals: **1 P1, 5 P2, 4 P3, 3 P4**.

Suggested bounded repair groups:

1. A01: atomic workspace promotion, reconciliation, and coordinator fault isolation.
2. A02/A03/A06: transaction authority, blocker invalidation, and transcript ownership.
3. A04/A05/A09/A13: dashboard operation identity and form/persistence recovery.
4. A07/A08/A10/A11/A12: evidence validation and lower-priority hardening.

These are recommendations, not an approved implementation plan. Each group needs causal checks in existing harnesses and independent review of the changed boundaries.

## Coverage and verification

Engine census: 45,507 lines / 743 declared functions across coordinator, workflow, scheduler, TRIP, recovery, roles, reviews, checks, recipes, store, and domain. Platform census: 30,810 lines / 723 functions across process supervision, IPC/HTTP, cmux, permissions/authentication, database, providers/compatibility, filesystem/import/export/transcripts, diagnostics/models/protocol/CLI, and build code. Additional inventories covered 30 migrations, 25 resource files, packaging scripts, and binary entrypoints. Dashboard mutation/recovery/navigation and recorded-output code was read by the manager and final reviewer.

These are lexical inventories and risk-driven source traces, not exhaustive branch verification. CSS was only searched for hiding rules; visual layout was not audited. Test corpora were searched selectively for causal gaps, not re-audited assertion by assertion. External cmux, provider CLIs/services, macOS sandbox/PTY behavior, and Linux branches were not exercised. No finite source audit proves absence of all errors.

Request verification: source-level findings, triggers, consequences, remedies, dispositions, and coverage limits delivered. Production fixes are intentionally absent because the request was an audit.

Build/test verification: no suite was rerun for this source-only audit. The earlier implementation checks (29 DOM, 67 library, 103 contract, 2 runtime tests, plus formatting/check/type/build) are historical passing evidence, not proof against these newly identified sequences. The stale-version helper reproduction is the only new executed behavioral evidence.

Deferred M2/M3/M8B, native notifications, historical migrations, and live qualification remain separate; they are not counted as defects here.

## Evidence and timing

Local reports: `engine_g1_result.md`, `manager_ui_findings.md`, `manager_followup.md`, `platform_g2_result.md`, `final_g1_result.md` in the audit runtime directory. Exact role receipts and queued notification records are retained there.

Measured reviewer wall times: engine 1,013.110472 s; platform 802.774551 s; fresh consolidation 453.992132 s. These are the three measured review units, totaling about 37.8 minutes, not total task duration. Manager activity overlapped parts of these intervals. Overall/exclusive phase timing and unattributed time were not fully recorded, so timing coverage percentage is unknown. No repair cycles or repeated broad test runs occurred. The independent challenge materially improved accuracy by removing the unsupported manual-database-repair claim and reducing several severity ratings.
