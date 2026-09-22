# Tasks, agents, and verification

[Back to README](../README.md)

## Required TRIP workflow

LLMRelay hosts the selected standalone TRIP Explorer v0.9.0 package. Its versioned skills, references, and optional helper scripts are copied verbatim from the selected source snapshot. Content hashes pin that snapshot, including its local changes. A separately versioned LLMRelay overlay maps role launch, context records, build execution, and integration writes onto the local engine. It does not remove the workflow's independent reviews, human approvals, test policy, ownership boundaries, or manager completion gate.

Registering a Git repository creates a project record. Running ordinary tasks additionally requires a compatible, initialized project. The app's host manager remains the engineering owner; the five delegated roles are Explorer, plan reviewer, implementer/fixer, code reviewer, and fresh final verifier. Explorer runs only when the workflow's recorded conditions require it. Existing `final_reviewer` identifiers remain a storage compatibility detail.

## Create and run tasks

Choose **New task** and provide:

1. Project, title, description, priority, and one acceptance criterion per line.
2. Review the inherited host-manager and delegated-role profiles. Make an explicit task override only when needed; changed profiles require matching preflight before dispatch.
3. **Save draft** to keep it in Backlog, or **Create Ready task** to make it eligible for dispatch. A valid saved draft can later be opened and moved with **Make Ready**.

Failed saves keep the form and its role selections in local browser storage. New tasks use the selected project when available. A restored draft with a missing or unavailable project shows an explicit project choice, rather than displaying a different repository while retaining a stale ID. An uninitialized registered project can hold draft tasks; making them Ready requires initialization and valid effective profiles. Disabled actions explain what is missing and link to setup where applicable.

Each physical repository can have only one active coding task. Different repositories may progress concurrently within the displayed process limits. Within one task, the approved structured plan must declare the exact ownership of parallel implementer lanes. The service waits for the manager to admit those reviewed lanes with current source and shared-file hashes; it does not fall back to a default writer while admission is pending. The service captures missing-file, file, and directory identities at admission and rechecks them before initial dispatch; managers do not need hash utilities or broader shell permissions. Admitted lanes run through the same profile, permission, process-capacity, dependency, and ownership gates as other work. All writers must yield before integration, review, or checks, and there is one build owner. A lane's role generation remains independent of another lane's generation. Coupled work uses one writer.

Same-provider concurrency is limited to two occupied invocations, and no more than one of them may be a persistent manager so a worker or reviewer slot remains available. An unused launch permit is released and audited when preflight fails before session reservation; startup also releases abandoned unused permits. Pending launch reservations and idle, stopping, or unknown sessions still occupy capacity; a task whose manager cannot be admitted remains visibly queued for capacity while eligible work on another provider can continue.

The normal lifecycle is:

```text
Project initialization → Backlog → Ready → Planning / conditional Explorer
→ Plan review → Human plan approval and implementation authorization
→ Implementation → Integration → Code review → TRIP verification → Fresh final review
→ Manager handoff → Human accept or fresh rework lineage → Done
```

Agent terminal prose is visible output. Only authenticated structured role results, stable review requests, verified check records, and explicit human decisions change workflow state. Plan and code review begin with two calls each; explicit extensions can raise either total to at most five. Final verification starts with one fresh invocation. A second fresh invocation requires the dedicated repair cycle, retained code-review approval, and refreshed manager conformance. Retries, role switches, and recovery do not reset consumed allowances. A structural NEEDS_REWORK result leaves the task incomplete.

The first turn of a retained setup discovery or profile probe completes at a strict provider `Stop` boundary. The coordinator accepts that boundary only for the exact current generation, credential, transcript epoch, native session, process identity, and invocation, with no pending permission, input lease, delivery, control, or recovery, and only after the supervisor verifies the managed process inventory is idle. The service then requests a one-shot completion interrupt while preserving the retained session for one later human-triggered resume. This action neither spends the resume nor creates the replacement control used by **Stop discovery manager**; no second turn is started automatically. At the durable deadline, the exact stop adjudicator observes the attached child exit before deciding and runs before the global multi-session reconciliation sweep. An attached exit or restart-time PID/start/process-group/boot proof of quiescence is persisted as the winning outcome; a proven-live or unverifiable exact generation enters explicit recovery without another signal. After the retained reporting turn is spent, the same session cannot be resumed a third time.

Ordinary reviewers receive evidence tied to their current review request: the frozen manifest, structured plan and approval receipts, lane yields, integration records, and applicable verification evidence. Missing or stale evidence remains explicitly unavailable. Reviewers inspect workspace content read-only against the manifest; a recorded hash alone is not proof of the file contents. Selected checks that await code approval remain pending. The standalone formatting restrictions for submitting a role report do not prohibit separately authorized ordinary review inspection, and do not expand setup or runtime-probe authority.

## Stop or change the task manager

Open the task's workflow controls to see its requested and effective manager
profile, current session, and pending stop or change. **Stop manager** holds new
dispatch before requesting an interrupt of that manager. Running workers and
their history are preserved. **Pause now** and **Cancel** remain separate
actions that stop all task work. A requested interrupt is not a completed stop;
the service waits for a verified process-group exit and shows failures that need
attention.

To replace the manager, save the exact provider, model, and reasoning effort in
Role Settings. Complete the explicit capability verification and activation
steps if that revision lacks current evidence. Then choose **Change manager at
safe boundary**, or **Interrupt and change manager** when you want to interrupt
the current turn. The safe option waits for a verified native idle boundary or
an already verified exit. It does not infer idle from a quiet terminal. Both
paths retire the old authority and require verified exit before launching the
captured replacement; later settings edits remain future requests.

A stopped manager's hold survives service restart. **Continue manager** releases
that manager-specific hold when its prerequisites are met; it does not clear
unrelated holds or itself prove that a session resumed. Missing or stale
replacement evidence keeps the change from proceeding and is shown with the
next required action. Failed changes do not trigger automatic paid retries.

Manager replacement is supported before the first completed plan using the
actual task inputs and current workspace. It does not invent a completed plan.
When the task awaits your final acceptance, role edits apply only to future
configuration: they cannot restart the manager or rerun final review. Completed
tasks remain read-only. Setup discovery has its own
[manager correction flow](PROJECT_SETUP.md), separate from task controls.

## Dashboard workspace

The Workspace page lists service-owned sessions and opens live output in cmux. In one service boot, each task owns one cmux workspace and each current role-session binding owns one active, mode-neutral surface in that workspace. **View output** creates or focuses the exact surface in view-only mode and never changes an existing input lease. **Take keyboard control** sends a revision-bound acquire to that same authenticated attachment connection; **Release keyboard control** or **Ctrl-]** releases it without stopping the agent. There are no separate dashboard watch and control routes. Reopening a valid route focuses its existing cmux surface. An unknown result requires authenticated discard after any live child ends. A validated loss with a live attachment first waits for `Retire`, connection-secret/lease cleanup, and durable `ended` recording (or the positive expiry of the matching bounded lease); only a later explicit View can reserve a fresh view-only surface with retained output replayed. An ended or failed attachment leaves its old pane historical and the next View creates a fresh view-only surface in the same task workspace. Workspace, Project Setup, and runtime-admission cards derive their refreshed surface state from the durable projection, showing a control-revision gap as pending before historical actual state. Ended sessions expose bounded recorded output in the dashboard without launching an agent. No cmux action restarts a provider, resumes workflow work, or resumes a native conversation. Each application transcript is a bounded recent-output record: its steady file is capped at 10 MiB and is compacted to approximately 6 MiB before more output is appended. The atomic compaction can temporarily use one additional approximately 6 MiB file, bounding per-session application transcript storage at approximately 16 MiB during compaction and 10 MiB otherwise. Compaction retains complete recent frames and inserts an explicit gap; an evicted replay cursor receives a recoverable gap followed by the retained tail. This application scrollback is separate from provider-native CLI history, which remains the source for exact native conversation resumption.

The existing attachment connection holds a short, renewed human input lease only after its exact revision-bound acquire succeeds, and serializes bytes through the exact session and process generation. Another viewer or automatic guidance delivery cannot write through the same lease concurrently. A blocked acquire, ownership loss, or stale acknowledgement remains view-only and never silently reacquires or takes over control. Detach when finished answering a prompt so automatic guidance can proceed.

Manager guidance is submitted once. It remains queued while a viewer owns input or while native work is active, and is delivered automatically after matching native submit/stop evidence and known helper descendants have exited. Claude must explicitly report empty background-task and scheduled-wakeup registries; missing or nonempty registries keep guidance queued, including after denied commands. Outstanding permission requests also prevent automatic delivery. For an exact pending service-owned Manager obligation, an otherwise current native turn whose latest real Stop has incomplete tool-hook bookkeeping may receive one service-initiated SIGINT stop request, distinct from verified native idle; the service still requires current generation, credential, invocation, candidate, permission, input, guidance, control, recovery, and whole-process-quiescence fences before it can retain-resume or transition. The atomic claim and the signal path are one-shot: another interrupt entry point does not send another signal, and drain reports the request separately as already pending. Completed checks/final-Explorer evidence and a completed final handoff are also held until the exact current Manager is either at a verified idle boundary or has exited with positively recorded whole-process quiescence; evidence recorded during a live busy turn is not consumed early. The stop has no timeout-based escalation and grants no blanket permission. If the process does not exit within the durable graceful-stop deadline, it enters explicit recovery with revoked role credentials and exact process controls; the service never fabricates idle, delivery, acknowledgement, or completion. Retained resume replays the saved native invocation unchanged, and the Manager loads current obligations and queued notices from role context. Guidance uses a framed terminal paste and a separate Enter under the same input lease. Only matching native submission evidence advances it to submitted; only the role acknowledgement advances it to acknowledged. The dashboard shows queued, written, submitted, and acknowledged states separately.

Use **Continue**, **Run next**, **Pause after role**, **Pause now**, **Retry**, or **Cancel** from task details. These are versioned requests. A draining control remains visible until owned process state is reconciled. **Run next** arms one coordinator action and then pauses again.

Role settings retain requested revisions separately from the effective generation and running session. Codex Implementer shows its backend-owned current `Unverified` validation requirement on both the requested row and edit selection before a capability row exists; historical capability rows keep their recorded status. Each active-session record identifies its task, attempt, role, generation, configuration revision, and immutable launch settings. A task-specific profile requires explicit activation against the current reviewed project configuration and exact capability evidence. Missing replacement evidence leaves the current role active and exposes the runtime-check path. A provider/model switch also requires a complete checkpoint and typed handoff. Once replacement eligibility is established, LLMRelay revokes the old credential before interrupting the old process, verifies process-group quiescence, and only then makes the captured settings revision dispatchable. A later settings edit remains requested for a future invocation and does not alter the captured replacement.

The plan and final acceptance panels are human-only. Requesting rework records feedback in a new attempt lineage. Plan approval carries only when task scope and the complete role configuration are unchanged.

## TRIP verification

Project verification commands belong to the initialized TRIP configuration. A task's reviewed plan selects the applicable checks, their acceptance coverage, relevant inputs, working directories, timeouts, and invalidation rules. There is no second mandatory checklist to configure in LLMRelay. An empty project matrix is allowed during setup, but missing applicable evidence prevents a task from being declared verified.

The verification view shows inherited commands, the task's selected matrix, approval state, and current or stale evidence. Editing project commands creates a configuration revision. Historical configured suites and receipts remain available as migration inputs; their presence never grants permission to run them under a new plan.

A check executes only after its actual command and scope are approved. Its permission controls offer **Approve once**, **Always approve matching actions**, and **Deny**. A reusable rule is available only when the structured executable and arguments can be represented safely; its preview explains the executable family, argument coverage, and scope. Shell syntax and unsupported wrapper forms require exact approval. Rules can be revoked, and a matching rule never selects a new check or bypasses the reviewed command, current candidate, working directory, or input-freshness requirements.

Service-run check grants are separate from native agent grants. An approval made for a sandboxed Codex or Claude action does not authorize LLMRelay to execute that command itself. The service holds the one build slot, checks writer quiescence and the frozen candidate, and records command, inputs, configuration, elapsed time, and result. Unaffected evidence can be reused; changed inputs invalidate the relevant checks. A passing build or review alone cannot satisfy the manager's request-verification gate.

## Human gates and continuation authority

Plan approval and implementation authorization are separate decisions. After
approving a plan, use **Authorize implementation of this exact plan** when
shown; generic Continue cannot grant that authorization. The request binds the
current task version and approved plan identity.

An active attempt requiring workflow migration exposes **Migrate this active
attempt to the current workflow** after its current configuration and quiescence
checks. A second rescue-Explorer call exposes its own bounded authorization
with a required justification. Neither action can be replaced by generic Retry.

Resume preserves the exact native conversation. **Start fresh accounted
session** creates new work only under the current role-specific authority:
manager/implementer dispatch, an activated Explorer decision, or a current review
request with remaining allowance. A final verifier always starts fresh.
Ambiguous or observed review delivery remains spent; a replacement cannot reset
the budget. Missing authorization is shown as a prerequisite rather than an
unusable resume button. See [recovery actions](OPERATIONS.md#recovery-actions-and-bounded-waits).

A permanent resume rejection gates only its current effective role or lane
generation. Preparing a provisional replacement does not clear that gate. Once
replacement authority becomes effective, the old rejection remains historical
evidence and no longer blocks current controls; direct requests against the old
authority still reject.

Imported `in_progress` or `implemented` tasks without managed attempt history
retain their original metadata but enter backlog with input required. **Start a
fresh managed attempt** normalizes eligible older imports before normal Ready
admission. It does not fabricate a completed plan, workspace, review or acceptance.
