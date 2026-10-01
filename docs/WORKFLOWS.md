# Tasks, agents, and verification

[Back to README](../README.md)

## Required TRIP workflow

**Needs your attention** groups what is waiting for you: approvals, decisions,
manual actions, setup problems, work that cannot continue yet, and results
ready for your review. Each entry names the task and, where one is involved,
the affected role, explains the reason in plain words, and has one button that
opens the exact place to act (for example **Review plan**, **Review request**,
**Answer question**, **Open agent output** or **Resolve issue**). These entries
are derived from service-owned state, not terminal output. An explanation shared
by several entries is shown once while distinct permission requests and
recovery records remain separate. Setup and instance restore holds remain
visible alongside task work, including unresolved records of a replaced
attempt. Automatic waits such as a full queue or a busy agent slot are shown on
the task rather than here. Navigation does not approve or resume anything; use
the action in the opened panel.

Task details and Needs your attention show backend-owned decision explanations:
the reason work cannot progress, the responsible owner, prerequisite evidence,
and the current control policy. Missing, stale and unobserved evidence remain
distinct. An explanation is a snapshot; every action rechecks its exact binding
and current authority before changing work. The dashboard's restart preview is
an explicit read, separate from normal state refresh. See
[restoration and recovery](OPERATIONS.md#stop-and-recover) for batch limits,
capacity delays and exact recovery-record selection.

LLMRelay hosts the selected standalone TRIP Explorer v0.9.0 package. Its versioned skills, references, and optional helper scripts are copied verbatim from the selected source snapshot. Content hashes pin that snapshot, including its local changes. A separately versioned LLMRelay overlay maps role launch, context records, build execution, and integration writes onto the local engine. It does not remove the workflow's independent reviews, human approvals, test policy, ownership boundaries, or manager completion gate.

Registering a Git repository creates a project record. Running ordinary tasks additionally requires a compatible, initialized project. The app's host manager remains the engineering owner; the five delegated roles are Explorer, plan reviewer, implementer/fixer, code reviewer, and fresh final verifier. Explorer runs only when the workflow's recorded conditions require it. Existing `final_reviewer` identifiers remain a storage compatibility detail.

## Create and run tasks

Choose **New task** and provide:

1. Project, title, description, priority, and one acceptance criterion per line.
2. Review the inherited host-manager and delegated-role profiles. Make an explicit task override only when needed; changed profiles require matching preflight before dispatch.
3. **Save draft** to keep it in Backlog, or **Create Ready task** to make it eligible for dispatch. A valid saved draft can later be opened and moved with **Make Ready**.

**Ready** means queued and eligible to start; it never means ready for your review. A Ready task starts automatically when an agent slot is free and pickup is on for its project.

A new task's workspace always starts from the project's registered commit. Before a task becomes Ready, and again when it is picked up, LLMRelay checks that such a workspace would receive the activated workflow files and approved guidance files unchanged. If they were changed without being committed, for example by running setup again, Make Ready and pickup stop and list the affected files; **Create Ready task** keeps the task as a draft. Nothing is overwritten. Commit or restore the changes, then choose **Validate and relink** in Project settings so new tasks start from the current commit. The same check reports a project folder that has moved to a newer commit.

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

The final-repair recheck is a separate one-shot accounting lane. It is never an extension or reset of the ordinary five-call allowance. When the first final review requests changes, the transaction that applies that verdict also records the recheck receipt and opens the repair round. The receipt binds the final request and its authenticated structured result, the one finished code approval of the same candidate, the approved plan and configuration, and the approving code reviewer's generation, session, settings revision and profile. The coordinator and a manual verdict apply the same rule. A missing or ambiguous approval, a changed reviewer profile, an existing receipt or an earlier repair round rejects the verdict without changing anything. The repaired candidate then receives exactly one code recheck under that retained reviewer, whether the ordinary count is below or at five. The recheck never adopts a newer code-reviewer profile. A newer activation, a changed reviewer binding or stale capability evidence holds the task for input without reserving a request or spending either count. In the repair round a missing or mismatched receipt holds the same way; it never falls back to the ordinary allowance. Delivery or ambiguous delivery spends that recheck and leaves the ordinary count unchanged; a failure proven before delivery retries the same request. Only an explicit approval continues to verification, refreshed manager conformance, and the second final verification. Any other verdict, malformed output, a reviewer that exits without an accepted result, or failure after delivery closes the recheck without a replacement and leaves the task incomplete. For history recorded before receipts existed, where an ordinary code review was used on the repaired candidate instead, the TRIP human action `authorize_final_repair_recheck` opens this lane once. It requires the exact task version, attempt, prior code approval, final change request, rejected review, and retained reviewer that the service derives from history, and an ordinary code count of exactly six allowed and six spent, as the removed exception left it. It keeps every earlier verdict and the ordinary count as recorded and returns only that attempt to implementation.

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

The Workspace page lists what is waiting for you, Active and Completed tasks, and service-owned sessions, and opens live output in cmux. A task's details open as a dialog, full screen on narrow windows, with **Overview** (summary, why it is waiting, the next decision and controls), **Changes**, **Checks** and **Activity** (agent output, reports and history). The dialog remembers its tab, and the page behind it keeps its section, filters and scroll position. In one service boot, each task owns one cmux workspace and each current role-session binding owns one active, mode-neutral surface in that workspace. **View output** creates or focuses the exact surface in view-only mode and never changes an existing input lease. **Take control** sends a revision-bound acquire to that same authenticated attachment connection so you can answer an agent prompt; **Release control** or **Ctrl-]** releases it and lets automation continue without stopping the agent. Closing the output view never stops work. There are no separate dashboard watch and control routes. Reopening a valid route focuses its existing cmux surface. An unknown result requires authenticated discard after any live child ends. A validated loss with a live attachment first waits for `Retire`, connection-secret/lease cleanup, and durable `ended` recording (or the positive expiry of the matching bounded lease); only a later explicit View can reserve a fresh view-only surface with retained output replayed. An ended or failed attachment leaves its old pane historical and the next View creates a fresh view-only surface in the same task workspace. Workspace, Project Setup, and runtime-admission cards derive their refreshed surface state from the durable projection, showing a control-revision gap as pending before historical actual state. Ended sessions expose bounded recorded output in the dashboard without launching an agent. No cmux action restarts a provider, resumes workflow work, or resumes a native conversation. Each application transcript is a bounded recent-output record: its steady file is capped at 10 MiB and is compacted to approximately 6 MiB before more output is appended. The atomic compaction can temporarily use one additional approximately 6 MiB file, bounding per-session application transcript storage at approximately 16 MiB during compaction and 10 MiB otherwise. Compaction retains complete recent frames and inserts an explicit gap; an evicted replay cursor receives a recoverable gap followed by the retained tail. This application scrollback is separate from provider-native CLI history, which remains the source for exact native conversation resumption.

The existing attachment connection holds a short, renewed input lease for you only after its exact revision-bound acquire succeeds, and serializes bytes through the exact session and process generation. Another viewer or automatic guidance delivery cannot write through the same lease concurrently. A blocked acquire, ownership loss, or stale acknowledgement remains view-only and never silently reacquires or takes over control. Detach when finished answering a prompt so automatic guidance can proceed.

A current manager code-review transition proposal suppresses further frozen-candidate reminders while its native turn finishes. This checks the exact manager generation, source phase, task version, plan, and candidate; it does not authorize transition before verified idle or process quiescence. Stale proposals do not suppress the reminder.

Manager guidance is submitted once. It remains queued while a viewer owns input or while native work is active, and is delivered automatically after matching native submit/stop evidence and known helper descendants have exited. Claude must explicitly report empty background-task and scheduled-wakeup registries; missing or nonempty registries keep guidance queued, including after denied commands. Outstanding permission requests also prevent automatic delivery. For an exact pending service-owned Manager obligation, an otherwise current native turn whose latest real Stop has incomplete tool-hook bookkeeping may receive one service-initiated SIGINT stop request, distinct from verified native idle; the service still requires current generation, credential, invocation, candidate, permission, input, guidance, control, recovery, and whole-process-quiescence fences before it can retain-resume or transition. The atomic claim and the signal path are one-shot: another interrupt entry point does not send another signal, and drain reports the request separately as already pending. Completed checks/final-Explorer evidence and a completed final handoff are also held until the exact current Manager is either at a verified idle boundary or has exited with positively recorded whole-process quiescence; evidence recorded during a live busy turn is not consumed early. The stop has no timeout-based escalation and grants no blanket permission. If the process does not exit within the durable graceful-stop deadline, it enters explicit recovery with revoked role credentials and exact process controls; the service never fabricates idle, delivery, acknowledgement, or completion. Retained resume preserves the saved native invocation and its identity. The host-restart path adds a fixed interruption notice to its separately recorded delivered text so the agent verifies in-flight effects before repeating a command; ordinary resume does not add that notice. The Manager loads current obligations and queued notices from role context. Guidance uses a framed terminal paste and a separate Enter under the same input lease. Only matching native submission evidence advances it to submitted; only the role acknowledgement advances it to acknowledged. The dashboard shows queued, written, submitted, and acknowledged states separately.

Use **Continue**, **Run next**, **Pause after this step**, **Pause now**, **Retry**, or **Cancel** from task details. These are versioned requests. A draining control remains visible until owned process state is reconciled. **Run next** arms one coordinator action and then pauses again.


For the admitted Claude Code 2.1.283 contract, automatic guidance uses a
JSON-encoded text envelope so slash commands, mentions, shell prefixes and image
paths remain guidance text instead of native terminal actions. The original
body remains unchanged in the dashboard and audit. Before writing, the service
records the exact submitted form and its digest with the session, transcript
epoch and resume invocation; the encoded form must fit the existing input limit.
A matching native submission advances only that delivery, and acknowledgment
requires the same current invocation. A recorded submitted form is immutable.
Older deliveries without an encoded snapshot still match their original raw
body; uncertain deliveries are never transformed and resent. Unsupported Claude
bindings fail before writing; Codex retains its existing raw-paste behavior.

When an agent asks a question or reports that it is blocked, the task waits for you. **Answer question** appears only when the service has matched the question to the manager session that asked it, and your reply goes to exactly that session; a wait that is not a question has no reply form. If the same agent then sends a newer, usable report, for example a finished plan, LLMRelay releases the wait by itself exactly once and continues from that report; you do not also need to choose **Continue**. The wait stays in place while a permission request, keyboard control, an unconfirmed guidance delivery, a recovery, a pending control, or another unread blocked report still needs you. Only an agent's newest report can hold its task: older questions it has already moved past are retired once and recorded, so **Retry** or **Continue** never brings an answered question back, and a report made while the task waited stays available afterwards instead of being treated as stale.

A provider failure also places a durable hold on automatic work for the affected
role and lane. Rate-limit and overload failures use a 60-second app cooldown;
other failure kinds require you to fix the cause and choose **Release provider
hold** on the agent's session. The hold remains visible after the session exits.
It does not turn into a generic coordinator-failure recovery item, and unrelated
eligible work can continue.

**Continue** and **Run next** retain their pause behavior but do not clear this
hold. **Retry** and rework cannot create another attempt to escape it. Releasing
a hold does not start an agent, send input or establish that a session is idle;
automatic work that was already authorized may become eligible at a later step.
If the release response is uncertain, refresh and check the current state before
retrying. An accepted newer turn supersedes that session's older holds, and an
explicit human role replacement retires the replaced role's holds. Stop and tool
activity alone do not clear them. Failure attribution and identifiers appear in
**Technical details**. The cooldown is LLMRelay's own restriction, not a claimed
provider reset time.

A Codex manager turn sometimes ends without a completion hook for every tool it started. LLMRelay then treats the turn as finished only after the service has checked, for the manager's exact current session, credential, invocation and hooks, that nothing else is pending (permission, keyboard control, uncertain guidance, control, switch, restart, recovery, capture or check) and the process inventory shows the manager idle. A completion hook that arrives late for a tool from that turn counts as bookkeeping; any new work keeps the turn busy. Elapsed time or a quiet terminal is never used. Queued guidance, such as your answer, is delivered only after that boundary, and it counts as submitted only when the manager's own prompt-submit event carries exactly its text.

Each task card on the Board and in Workspace, and the task's header, shows the same next step as the Workspace inbox, for example **Review plan**, **Review request**, **Answer question**, **Open agent output**, **Open agent settings** or **Resolve issue**, and opens the same exact item. When a task has more than one waiting item, the card shows how many more there are and the task header lists each of them with its own button. If an item changed before it could open, nothing is opened or run and the latest state is requested. An item with no current page to open offers **Refresh**.

The task's Overview explains why it is waiting, who it is waiting on (you, LLMRelay, a named agent role or another task), when the wait began and when the last workflow step was recorded. Reports, decisions, deliveries, reviews, recoveries and phase changes count as steps; an agent that is only running, or is producing output without reporting anything, is shown as such and never counts as progress.

If one workflow step fails on every try, Workspace shows **Automatic progress is paused**. When the failure belongs to one task, the item names the task and opens it; otherwise **Open diagnostics** opens the Diagnostics page, which lists the recorded failure. The raw error is under **Technical details**. LLMRelay keeps retrying and records each distinct failure once, not on every retry; the item clears by itself when the step succeeds.

Role settings retain requested revisions separately from the effective generation and running session. Codex Implementer shows its backend-owned current `Unverified` validation requirement on both the requested row and edit selection before a capability row exists; historical capability rows keep their recorded status. Each active-session record identifies its task, attempt, role, generation, configuration revision, and immutable launch settings. A task-specific profile requires explicit activation against the current reviewed project configuration and exact capability evidence. Missing replacement evidence leaves the current role active and exposes the runtime-check path. A provider/model switch also requires a complete checkpoint and typed handoff. Once replacement eligibility is established, LLMRelay revokes the old credential before interrupting the old process, verifies process-group quiescence, and only then makes the captured settings revision dispatchable. A later settings edit remains requested for a future invocation and does not alter the captured replacement.

The Explorer and the reviewers never write, and each exploration or review starts a new session, so an activated settings change for one of them applies at the next session boundary without a switch. That boundary requires every earlier session of the role to have exited with a verified quiet process group, the task to be at that role's step, and nothing that could still use the old settings: no review of its kind in flight, and no pending switch, permission request, keyboard control, uncertain guidance, control, restart hold, recovery, capture, running check or other writer or reviewer. The service then rechecks the activation, settings revision, adapter, capability evidence and the ready workspace's reviewed policy files, updates the attempt's settings and workspace policy together, and records the change; the next review request and its launch are bound to the new settings revision. Until then the task shows that the new settings are waiting, and no review is launched with the replaced settings. If the new settings need their profile verified again, the task shows **Open agent settings**, which opens that role in the task's Agent settings. An earlier review that went to replaced settings stays spent and never counts as approval. An activation made for a different project configuration does not apply to an attempt, which keeps the configuration it was reviewed with.

The plan and final acceptance panels are human-only. Requesting rework records feedback in a new attempt lineage. Plan approval carries only when task scope and the complete role configuration are unchanged.

A terminal review ends the attempt incomplete: an ordinary code or final `needs_rework`, or a closed final-repair recheck. Such an attempt can continue only through an explicit replan (see [Operations](OPERATIONS.md#replan-after-a-terminal-review)). The replan stages a new planning attempt whose workspace starts from the exact rejected candidate. That candidate is only the new attempt's source; it never becomes an accepted snapshot. The new attempt carries no plan, plan approval, checks, conformance or verdicts, and starts with fresh default review allowances. The old attempt's requests, results, allowances, recheck receipt and snapshots stay as recorded. Its workspace is created only after every role generation of the old attempt is terminal, every launched session has exited with proven process-group quiescence, and nothing else still owns it. An exited session does not release a role generation that is still stopping or held for recovery.

## TRIP verification

Project verification commands belong to the initialized TRIP configuration. A task's reviewed plan selects the applicable checks, their acceptance coverage, relevant inputs, working directories, timeouts, and invalidation rules. There is no second mandatory checklist to configure in LLMRelay. An empty project matrix is allowed during setup, but missing applicable evidence prevents a task from being declared verified.

The verification view shows inherited commands, the task's selected matrix, approval state, and current or stale evidence. Editing project commands creates a configuration revision. Historical configured suites and receipts remain available as migration inputs; their presence never grants permission to run them under a new plan.

A check executes only after its actual command and scope are approved. Its permission controls offer **Approve once**, **Always approve matching actions**, and **Deny**. A reusable rule is available only when the structured executable and arguments can be represented safely; its preview explains the executable family, argument coverage, and scope. Shell syntax and unsupported wrapper forms require exact approval. Rules can be revoked, and a matching rule never selects a new check or bypasses the reviewed command, current candidate, working directory, or input-freshness requirements.

Service-run check grants are separate from native agent grants. An approval made for a sandboxed Codex or Claude action does not authorize LLMRelay to execute that command itself. The service holds the one build slot, checks writer quiescence and the frozen candidate, and records command, inputs, configuration, elapsed time, and result. Unaffected evidence can be reused; changed inputs invalidate the relevant checks. A passing build or review alone cannot satisfy the manager's request-verification gate.

During implementation, ordinary implementer role context includes the current
manager's latest `needs_input` repair feedback when it names the same approved
plan and candidate. This lets an exact retained resume read the handoff without
changing its frozen invocation. The feedback is guidance, not scope approval or
verification evidence; source candidate readiness can retain explicit pending
checks for the later service verification phase.

A default-lane candidate report bound to the current approved plan remains
eligible across a pause. Freezing still requires the current implementer
generation, an unconsumed report, and verified process quiescence. Reports with
a different plan are rejected; legacy reports without a plan binding retain
the timestamp freshness check. Publication consumes the exact eligible report.

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

A changed provider compatibility contract invalidates the affected evidence.
Matching replacement proof can authorize the existing fresh-session recovery route;
it does not make the old frozen session eligible for exact resume. Follow the
current compatibility explanation and existing review/approval gates. A reviewed
contract candidate alone is not a qualified role profile.

## Recipes and reusable project profiles

Select a project, then open **Recipes**. Save a profile set containing the six role
configurations, then a recipe with task content, acceptance criteria, priority, an
exact profile revision and required checks. Saved configurations are not capability
proofs; ordinary task-specific activation and Ready admission still apply. Templates
are literal editable text, with no executable substitution language.

**Create draft** creates an ordinary backlog task and opens its detail view. Inspect
and edit it before making it Ready. Its provenance retains the exact recipe/profile
revision. Required recipe checks constrain the Manager's later selection; they do
not impersonate a Manager-selected check set. Revisions of recipes and profile sets
do not silently change existing tasks or schedules.

If project reactivation supersedes a recipe's configuration pin, re-save its profile
set and recipe using the active configuration and explicitly select current checks.
Copy any needed draft edits, archive the stale draft, and create a new draft. An
archived recipe can be replaced by a new recipe. Never-started backlog drafts may be
archived from task detail and listed in History. Restore
returns them to backlog; it does not repair stale pins or make them Ready.

A plan/code reviewer stopped before a native session identity was recorded cannot be exact-resumed. A resume inspection can record current prepared runtime identity without reserving or launching a provider. Once the stopped session, unchanged profile, current frozen review, supported runtime proof, and remaining review allowance all match, the dashboard offers an explicit fresh accounted replacement. Missing proof or an existing native identity does not use this startup-replacement route. The original delivered call remains spent; final verification and other roles are excluded. A later inspection may append the missing runtime observation to rejection history without rewriting the original rejection.

## Accepted turns, superseded reports and permission waits

A trusted accepted resumed turn can supersede its own prior unconsumed blocked or needs-input report only under exact session, generation, configuration, epoch and invocation fences. Supersession preserves the report and correlated audit; it does not fabricate workflow consumption. Releasing a matching resume-failed hold, advancing the task version and retiring invalidated transition proposals occur in one transaction. Independent committed controls and still-current blockers remain binding. Merely starting a process or sending input cannot perform this reconciliation.

Permission decisions, response delivery, native resolution, role report submission, verification and human acceptance remain separate. A native notification or failed turn does not consume a review call or imply a role verdict. See [operations](OPERATIONS.md#permission-notices-and-appearance) and the [signal matrix](PERMISSION_SIGNAL_MATRIX.md).
