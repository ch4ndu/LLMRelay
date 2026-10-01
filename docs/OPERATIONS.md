# Sessions, recovery, and diagnostics

[Back to README](../README.md)

Examples below use `llmrelay` on PATH. If you have not added it to PATH, substitute the full path to your packaged `bin/llmrelay`. Building a development executable is covered in [Building LLMRelay](BUILDING.md).

## Startup and browser authentication

The dashboard waits for committed state changes instead of refreshing every two
seconds. A separate 30-second reconciliation read refreshes process observations,
lease expiry and other time-dependent data, and recovers missed notifications.
Transient failures use bounded retry delays while retaining the last snapshot.
The existing refresh/reconcile control requests a fresh authoritative read.
After a service restart, the dashboard resets its cursor to the new service
identity; if its browser session is no longer valid, use the current startup
authentication flow below.

**Needs your attention** and **Approvals** include items across projects; their
counts stay visible in the top bar. Selecting an item opens its exact task,
attempt, session, permission, recovery record or project setup when that
binding is still current, on the task tab that holds it (live output opens the
**Activity** tab). If it changed or its panel is absent, the dashboard explains
the failure and refreshes once; opening an item never executes its action. A
selected recovery record that resolves stays non-actionable instead of silently
selecting another record. Open the task normally from the board, history or the
restart row's **Open task** button to return to its current recovery view.
Recovery drafts stay with their record and are not transferred to another record.

Recovery offers resume only when it can still apply. A stopped session of a
current, unfinished task offers **Resume the stopped session** only while its
exact binding is eligible. Sessions that reported their result and were stopped
after their turn, sessions replaced by a newer generation, and sessions of
completed or cancelled tasks are history: they appear under **Earlier sessions**
and in the task's **Activity** tab, never as urgent recovery.

Native OS notifications are deferred; the dashboard attention rail is the
implemented notification surface for this milestone.

The default macOS data directory is `~/Library/Application Support/AgenticJira`. An explicit data directory must be absolute and must be reused for every command targeting that instance.

```sh
llmrelay serve --data-dir "$PWD/.local/my-agenticjira" --port 0
```

Run `serve` from a terminal inside cmux to enable automatic attachment creation and focus. The host must verify its process ancestry for the current service boot; a reachable socket or a copied cmux environment variable is not sufficient. An outside-cmux or inconclusive host keeps workflow control available and shows the manual attachment command instead.

Automatic cmux commands use the exact absolute executable captured from that still-live ancestor. They never resolve `cmux` again through `PATH`.

Keep that terminal open. The selected startup behavior is for `serve` to open and authenticate the dashboard in your default browser automatically. After the one-use login exchange, the browser shows the clean loopback address; there is no task/session ID to add manually. The service still reports its actual address, instance and boot IDs, data and log paths, and role socket.

Use `serve --no-open` for a terminal-only or headless instance. In that mode, or if the operating-system browser opener fails, use the one-use dashboard login link printed in the service terminal. The fragment credential is exchanged for an in-memory, HttpOnly, same-site browser session and removed from the address bar and current history entry. A different browser without that session must still authenticate; the clean URL does not grant public control access.

In another terminal:

```sh
llmrelay status --data-dir "$PWD/.local/my-agenticjira"
llmrelay state --data-dir "$PWD/.local/my-agenticjira"
llmrelay diagnostics --data-dir "$PWD/.local/my-agenticjira"
```

LLMRelay stores human control authority separately from role credentials. Role processes cannot use same-user Unix access as human authority. Service hooks, human authentication, session state, and other repositories remain outside an implementer's write root.

## Attach from a terminal

For automatic dashboard presentation, LLMRelay owns one cmux workspace per task in the current service boot and one active, mode-neutral terminal for each exact live role-session binding. **View output** creates or focuses that terminal in view-only mode. It does not acquire, release, renew, resize, or take over a keyboard lease. **Take keyboard control** requests a revision-bound acquire on the same authenticated attachment connection; **Release keyboard control** or **Ctrl-]** returns that connection to view-only without stopping the provider. Keep the terminal hosting `serve` open: cmux presentation is separate from the service and does not install a background service.

If a bounded create, focus, health, or global-tree observation does not yield the exact validated identity required by the protocol, LLMRelay records the reservation as unknown and does not retry or reuse it. **Discard unknown reservation** is an authenticated human action available only while the exact provider binding is still live and no child attachment is live. It retires the durable reservation without addressing, closing, or selecting any possible cmux surface; opening again remains a separate explicit action. If the exact attachment client is live while the surface identity is unknown, LLMRelay reports `unknown_live` and keeps that authenticated output connection alive in a distinct Hold state. Hold preserves the durable control record but does not treat it as local authority: the client sends no renew, input, resize, acquire, release, or takeover request until a later exact sync resolves the presentation. Detach or any rejected late input operation clears the connection's in-memory secret and reconciles the durable state. A timeout, denial, malformed response, invalid UTF-8, oversized output, or error text never proves terminal loss.

Only a successful, schema-valid global `system.tree` inventory can prove that an owned workspace or surface disappeared. A validated loss never creates a replacement in the same operation while its exact attachment is live or its matching bounded human lease remains active. The loss first returns `Retire`; the connection clears its in-memory secret/lease and then detaches to record the exact binding ended. A positively observed expiry of that exact bounded lease is the only alternate retirement proof. Only after that durable retirement may a later explicit View reserve one fresh view-only terminal with retained output replayed for the exact transcript epoch. It does not restore a prior lease, focus or inject into the old pane, restart the provider, resume workflow work, or resume the native conversation. A live attachment that ends or fails while its pane still exists follows the same historical-pane rule: a later View creates a fresh view-only surface in the still-owned task workspace.

To watch an existing live session manually, use the same instance data directory and the session ID shown by LLMRelay:

```sh
llmrelay attach --data-dir "$PWD/.local/my-agenticjira" --session <SESSION_ID> --view-only
```

A `--view-only` attachment reads captured output without requesting or renewing an input lease. Omit that flag to request exclusive keyboard control. Its opening message says whether it owns input or is view-only. If another viewer owns input, use `--takeover` explicitly to transfer control; the former owner's lease no longer authorizes input. Only the input owner changes the provider's terminal dimensions. Use **Ctrl-]** to detach and return to your shell without stopping the provider. **Ctrl-C** is sent to the provider while you own input. This manual diagnostic path does not adopt, focus, close, or mutate a dashboard-owned persistent surface.

Attaching does not launch another agent or resume an ended conversation. It stays bound to the exact session generation and transcript epoch; a service restart or replacement requires a new attachment after the appropriate recovery action. A control client renews its input lease while connected and releases it on normal detach. A connection that stops sending requests for 90 seconds is closed and its attachment ownership is released; normal output polling keeps a healthy attachment connected even when the agent is quiet. If ownership is lost, it becomes view-only and does not silently take control again. Output retained after a normal provider exit can drain to an already connected client, but a new live attachment requires a running session. A capture-gap message means older output was removed by bounded retention.

## Restart and restore

LLMRelay restores the browser workspace independently from execution. The selected project, open task, page, board and Workspace Active/Completed section persist locally, and an open task keeps its tab while the dashboard stays open. Live output belongs to cmux; old embedded-terminal layout and scrollback settings are no longer used. Reopening an ended session shows recorded history and never launches a provider. LLMRelay does not register its short-lived attachment command as cmux resume metadata: the command contains an exact process binding and route token which must not be restored after that binding changes. After a service restart, recover or resume eligible work in LLMRelay and open its current attachment; LLMRelay creates terminal access from the new current binding.

Guidance that was being delivered when the service stopped is marked as uncertain at the next startup and is never sent again automatically. It becomes submitted only if the agent's own prompt-submit event for the same session and invocation later matches the recorded submitted form; otherwise it stays uncertain and is shown as such. Queued guidance that had not started delivery stays queued for the next safe boundary. A session whose launch never began is recorded as proven nondelivery and may be replaced; a session that had started but whose process ownership is unknown needs recovery before anything is delivered to it.

Automatic work resumption is off by default and remains off across service restarts until you enable **Resume eligible work automatically** under the Workspace page's **Restart and recovery tools**. During a deliberate restart, stop with drain so the service records the exact running native bindings before it interrupts them:

```sh
llmrelay stop --drain \
  --data-dir "$PWD/.local/my-agenticjira"
llmrelay serve \
  --data-dir "$PWD/.local/my-agenticjira" --port 0
```

Use the authenticated dashboard automatically opened by `serve`, or the current one-use login link when running with `--no-open`. The service holds all captured previously running work, including blocked or skipped native-resume candidates. Missing native history or matching current Supported capability evidence cannot silently trigger a fresh session. Legacy Codex Implementer Supported rows remain demoted because they predate the service-recorded native-policy identity checks. Migration 017 also demotes old Codex Manager/Reviewer Supported rows that lack the denied-read-floor marker to Unverified while preserving their proofs, evidence references, timestamps, and prior gaps. Those historical rows cannot authorize a launch or resume under the new key. An explicit Continue or recovery action can release the hold for normal workflow dispatch only after positive process-quiescence checks; it does not itself perform a native resume. In the Workspace page, choose **Open task** on the relevant row under **Waiting after restart**. Its task exposes **Resume after restart** only when the exact binding is eligible. Use **Continue with a new session** only when its separately checked recovery prerequisites are satisfied. The equivalent non-browser commands are:

```sh
llmrelay resume-work \
  --data-dir "$PWD/.local/my-agenticjira" --session <SESSION_ID>
llmrelay resume-work \
  --data-dir "$PWD/.local/my-agenticjira" --eligible
```

Every selected-session response lists that session’s durable `resumed`, `queued_capacity`, `blocked`, or `failed` result and reason; the restoration list separately retains skipped candidates and their reasons. Submitting a request is not reported as a successful resume. Resume always uses the recorded provider’s exact native session, worktree, generation, candidate/review authorization, configuration, credentials, budgets, and normal capacity locks. A Claude executable changed by an external upgrade or another user-managed session legitimately fails the frozen executable check; LLMRelay does not pin, downgrade, or copy the old binary, so recovery requires a fresh accounted handoff. Paused, Needs input, Awaiting review, completed, cancelled, archived, isolated-validation, stale/replaced, missing-history, incompatible-policy, or process-uncertain sessions remain stopped. There is no fresh-session fallback labeled as restoration. Auto mode retries only a capacity wait; a real preflight or proven non-delivery failure remains visible for human recovery instead of spending another provider turn automatically.

A host-restart resume includes a fixed notice identifying the host interruption
and telling the agent to verify any in-flight command before repeating it. The
service retains the original prompt and its hash unchanged and records the
actual delivered resume text separately. It rechecks the exact admission,
attempt, generation and task authority when reserving delivery. If that
admission becomes stale before any delivery, its own candidate becomes blocked
with a recovery explanation; a newer control or replacement admission is left
intact. Uncertain delivery is never relabeled as proven nondelivery.

## Optional legacy task import

The optional legacy task import reads supported task Markdown records. It does not register a Git repository or initialize TRIP. Preview the source first, then authorize importing its unchanged content into the selected project. Original fields and historical run/bug records are preserved.

The command-line equivalent binds import to the preview hash and current project version:

```sh
llmrelay import --data-dir <INSTANCE> preview --source <TASK.md>
llmrelay import --data-dir <INSTANCE> apply --project <PROJECT_ID> \
  --operation-id <UUID> --expected-project-version <CURRENT_PROJECT_VERSION> \
  --source <TASK.md> --expected-source-hash <PREVIEW_SOURCE_HASH>
```

## Stop and recover

### Preview and bounded restoration

Use **Preview restart** under **Restart and recovery tools** in Workspace, or run
`llmrelay restart-preview --data-dir <INSTANCE>` against the running service, to
inspect recorded sessions before stopping work. Preview runs only when requested.
It does not drain, signal, reconcile, reserve a launch, or change saved state.
It distinguishes resumable, fresh-only, blocked, uncertain, awaiting-approval and
completed work. Running-session eligibility is conditional on verified quiescence;
a preview never authorizes a resume. The CLI control channel retains its 1 MiB
response limit and returns an explicit error when a complete preview exceeds it.
It does not silently truncate the result.

One distinct selected session uses the direct resume path. **Resume eligible**
and selections with two or more distinct sessions create a durable batch of at
most four, ordered by manager role, candidate creation time and session ID.
Responses distinguish queued work from resumed sessions and list every omitted
session. Replaying an operation returns its original result without adding work.
Selected input is limited to 200 distinct IDs.

The coordinator admits at most one due restoration per tick. Human batches take
priority even when automatic restoration is off. Capacity delays persist at 2,
4, 8, 16, 30 and then 60 seconds; a delayed member does not block other due work.
Explicit single-session retry can bypass the delay but never ownership, approvals,
stale identity or uncertain delivery. Capacity does not spend a replacement
attempt. Three proven preflight or nondelivery failures exhaust the candidate's
allowance; failed or uncertain deliveries are not automatically retried. Counters
and consumed batch membership survive restart.

### Exact recovery records

Recovery commands carry the displayed `recovery_id` with the current task,
attempt, optional session and task version. Refresh after a stale-record refusal.
The service resolves that exact supported record; it does not select a different
record because it is newer. Human evidence annotates the decision and does not
replace process-ownership verification.

### Drain

Request a safe drain with:

```sh
llmrelay stop --drain --data-dir "$PWD/.local/my-agenticjira"
```

Drain disables new dispatch, interrupts owned PTY and configured-check processes, and waits until recorded processes are absent. Closing the browser does not stop the host. Nondelivery requires evidence that the provider never started, such as an explicit pre-provider failure acknowledgement from the launch wrapper. Proven provider nondelivery keeps the request unspent, but uncertain wrapper cleanup still fences another launch until positive reconciliation. Once the provider starts, cleanup can prove that its processes stopped, but cannot prove that it never received the prompt; an uncertain delivery keeps its review round spent and its workflow authority fenced until recovery. Recovery records the process-group leader's generation separately from the provider child and observed descendants. After an unexpected restart, complete process inventory may prove those generations absent or positively identify an unrelated replacement group without signaling it. A changed stable operating-system boot UUID can prove prior-boot absence; restarting LLMRelay or migrating an older identity format cannot. Human recovery text is an annotation and cannot declare an unknown process quiescent.

Offline diagnostics and sanitized export use the same selected data directory:

```sh
llmrelay logs --data-dir "$PWD/.local/my-agenticjira"
llmrelay diagnostics export --data-dir "$PWD/.local/my-agenticjira" --output "$PWD/.local/agenticjira-export.zip"
```

The export is a standards-compatible ZIP containing `manifest.json`, sanitized application state, and bounded sanitized diagnostics. It excludes the raw database, credentials, native provider history, transcript payloads, repository source, hook payload bodies, and runtime sockets.

Setup previews, runtime proof challenges, local workspace and service paths, and freeform failure details are omitted. Correlation IDs, states, timestamps, hashes, capability keys, and normalized failure categories remain available for debugging.

If the dashboard reports **browser session required**, use the authenticated browser opened by the current `serve` startup, or the current one-use login link when running with `--no-open`. A previously consumed link is not reusable after its session is lost. If a Unix socket path is rejected, choose a normal absolute data directory; LLMRelay uses a deterministic short private socket directory and fails closed on symlinks, wrong ownership, or permissive existing directories. If a task is blocked on setup or capability, open its project setup and review the exact missing profile or installation evidence. Existing files and native provider policy must be preserved; editing global hooks or adding bypass flags is not a recovery action.

## Existing installation compatibility

`llmrelay` is the primary executable and package name. Packages also include `bin/agenticjira` as a legacy executable alias. Both run the same implementation and use the existing default data directory, `~/Library/Application Support/AgenticJira`, plus the existing database, logs, environment variables, socket identities, browser-storage keys, hooks, and persisted policy identifiers. Existing users do not move or reset data. The repository itself also remains at `/Users/murali/Private/GitHub/AgenticJira`.

## Recovery actions and bounded waits

Needs your attention and the owning task/setup screen explain who can advance a
blocked operation, what it is waiting for, and the available action. A visible
action is guidance: the service rechecks its current task version, session,
generation, policy and process ownership when submitted. A stale screen cannot
authorize a replacement or bypass an approval.

| Situation | Next action and boundary |
| --- | --- |
| Exact native resume rejected after runtime identity changes | Review the reason. **Start a fresh session** is available only if the same role profile and current authority still qualify. It consumes a fresh call; it is not native resume. |
| Role, candidate, configuration, hook trust, native history or invocation provenance changed | Review the current role authority or prepare corrected verification. Old approval evidence cannot be reused as replacement authority. |
| Resume or review authority already spent | New explicit authorization is required; the dashboard cannot reset the allowance. Unknown permanent rejections remain terminal with their reason. |
| Interrupted installation or uncertain workspace creation | Open project/task recovery and follow its journal or reservation action; see [project recovery](PROJECT_SETUP.md#interrupted-setup-and-workspace-recovery). |
| Workspace preparation refused to overwrite changed workflow or guidance files | The task explains which case applies. A guidance file that differs from the commit can be restored and the reservation retried. Workflow files changed again after setup cannot be fixed by retrying, because the workspace always starts from the recorded commit: choose **Verify and cancel reservation** (this cancels the task), commit the changes, choose **Validate and relink**, then create the task again. The failed reservation stays recorded. |
| Service cannot prove process exit | Resolve exact process ownership before resuming or replacing work. A quiet terminal is not proof of exit. |
| Graceful stop exceeds 30 seconds | The service records recovery for the exact managed process. Choose **Ask it to stop again** or explicitly **Force stop this process** when offered. |
| Browser request times out | Refresh and reconcile. The operation may have succeeded; retain its operation ID until a definitive result rather than submit a new operation blindly. |

Only frozen-runtime identity drift becomes stale when the original frozen
runtime is current again; an unchanged capability key does not erase a permanent
hook-trust, history, provenance or authority rejection.

The stop deadline is measured from the first durable interrupt request and
survives service restart. Startup adjudicates interrupted stops before preparing
restart candidates, preserving the original clock and exact recovery authority. Retrying from recovery does not restart that clock.
Force stop rechecks the exact process identity immediately before escalation;
replacement work still waits for proven quiescence. The deadline never triggers
automatic force-kill, paid retry or shutdown completion. Drain remains visibly
held when ownership is unresolved. Failed controls and switches receive a
recorded disposition so they do not repeatedly take the coordinator's next slot.
Failures without an exact recoverable ownership tuple are rejected and refreshed
as corrected versioned controls, rather than presented as generic process recovery;
unrelated work can proceed only within its own capacity and ownership limits.

Proven process-group quiescence resolves the exact graceful-stop recovery record,
so its Retry and Force stop buttons disappear. Pending pause, cancel, manager
stop, switch, drain and other unresolved ownership still govern the next action.
Otherwise the task retains a fresh-dispatch hold: **Continue with fresh dispatch**
explicitly releases that hold through the normal control path. Settling the stop
never automatically resumes the old native session or launches a provider.

Interrupted restart admission is classified separately from the validity of the
original request. Proven nondelivery with stale task or configuration authority
shows `restart.admission_authority_stale`; exhausting the replacement limit keeps
its existing limit reason. Both retain a hold rather than inventing evidence that
a process is still running. Pending pause, cancel and manager controls still apply.

Uncertain delivery has an exact recovery record. Confirming quiescence verifies
the current invocation, not merely the absence of an earlier process from the same
session. A same-boot spawn with only a prior-generation anchor remains held; the
operator needs applicable current-generation evidence or verified reboot evidence.
After successful resolution, the attempt retains an explicit fresh-Continue hold,
including when another recovery record is resolved last. Confirmation does not
launch work or reset retry counts. An already active lane retains its ownership.
`restart.fresh_dispatch_hold_releasable` identifies a hold that can be explicitly
released after the existing checks pass; other unresolved records remain blockers.
`restart.ownership_reconciliation_required` remains a defensive explanation for
unbound evidence, not permission to bypass it.

Continue requests are idempotent by operation ID. While a Continue is pending, a
different operation ID is refused. A queued Continue that has become ineligible
is rejected with a recorded reason instead of advancing the task again.

Guidance whose delivery was never confirmed (`delivery_reserved`,
`written_awaiting_submit` or `delivery_unknown`) fences guidance
reauthorization. When its delivery session has exited and the guidance is
obsolete, abandon that exact message with an authenticated command:

```sh
llmrelay apply --data-dir "$PWD/.local/my-agenticjira" --file abandon.json
```

```json
{"kind":"abandon_unconfirmed_guidance","operation_id":"<new id>",
 "task_id":"<task>","expected_version":7,"attempt_id":"<attempt>",
 "guidance_id":"<guidance>","role_generation_id":"<generation>",
 "delivery_session_id":"<session>","delivery_transcript_epoch":"<epoch>",
 "delivery_resume_invocation_id":"<invocation or null>",
 "expected_state":"written_awaiting_submit","reason":"<why it is obsolete>"}
```

`delivery_transcript_epoch` and `delivery_resume_invocation_id` identify the
guidance row's historical delivery. That epoch may differ from the session's
latest transcript epoch; use the stored message binding, not the latest session
values. Every field must equal the recorded binding, the attempt must be the task's
current unfinished attempt, and the session must have exited with its recorded
processes proven absent. An unfinished launch or resume outcome needs a later
verified-quiescent recovery first. Open recoveries, keyboard control, starting or
recovering sessions and uncertain claims refuse. The message becomes `abandoned`
with reason `human_abandoned_unconfirmed_delivery`; its body and delivery
identity stay, and the audit event keeps the previous state, reason and your
reason. The delivery outcome remains unknown: nothing is marked submitted,
replayed or resumed, and task holds, attention and review counts are unchanged.
Repeating the same operation ID with identical input returns the stored result.

A `skipped` restart candidate can never be restored, but until it reaches a
terminal disposition it still counts as an open restart hold, for example in
guidance reauthorization. When its session has exited, cancel that exact
candidate with the same `llmrelay apply` route:

```json
{"kind":"cancel_stale_restart_candidate","operation_id":"<new id>",
 "task_id":"<task>","expected_version":7,"attempt_id":"<attempt>",
 "session_id":"<session>","role_generation_id":"<session generation>",
 "expected_state":"skipped","expected_source":"<source>",
 "expected_requested_by":"<requested_by or null>",
 "expected_updated_at":"<updated_at>",
 "expected_result_sha256":"<SHA-256 of the stored result_json text>",
 "reason":"<why it is stale>"}
```

Only `skipped` is accepted; parked, queued, admitting, failed, blocked and
pending-reconciliation candidates keep their own recovery routes. Every field
must equal the stored candidate, the attempt must be the task's current
unfinished attempt, and the result may carry no admission, active batch or
scheduled resume. The session must have exited with its recorded processes
proven absent and must not be marked for restoration at startup. The same
session and attempt fences as guidance abandonment apply, and a pending admission
or reconciliation anywhere in the attempt refuses. The candidate becomes
`cancelled` with an explicit human-cancellation reason; `result_json`, `source`
and `requested_by` stay, and the audit event keeps the previous row and your
reason. Nothing is resumed, released or dispatched, and task holds, attention
and review counts are unchanged. Identical replay returns the stored result.

Browser reads have an eight-second response deadline and mutations have a
15-second response deadline. These bound browser waiting, not the lifetime of an
accepted operation. Selected checks return after reservation and spawn are
recorded; a service-owned worker continues output capture and the check's own
timeout. Closing the browser or aborting its request does not cancel the check.
After service restart, check recovery uses the recorded check and process identity.

After a CLI upgrade, an old native session may fail exact resume because its
frozen executable or policy identity changed. The service records that mismatch
without spending a resume or silently starting a replacement. Complete current
role verification first; only a matching Supported proof and unchanged role
authority can enable **Start fresh accounted session**. Genuine hook-trust
failures and missing invocation provenance retain their separate recovery routes.

Upgrade-rejection evidence records the actual attempted executable and capability
identity separately from the old session. Fresh recovery requires the latest
applicable evidence itself to be Supported for that identity and role profile.
A newer unverified or unsupported result blocks a stale dashboard action, and the
service rechecks the evidence at final dispatch before reserving provider work.

### Replan after a terminal review

An attempt left incomplete by a terminal review can continue in a new planning
attempt. The review is either an ordinary code or final `needs_rework`, or the
nonapproving verdict that closed a final-repair recheck. Request it explicitly
with the same `llmrelay apply` route:

```json
{"kind":"replan_after_terminal_review","operation_id":"<new id>",
 "task_id":"<task>","attempt_id":"<attempt>","expected_version":7,
 "review_request_id":"<terminal review request>",
 "role_result_id":"<its consumed structured result>",
 "candidate_hash":"<rejected candidate hash>",
 "snapshot_id":"<complete candidate snapshot with that hash>",
 "reason":"<why a new plan is needed>"}
```

The attempt must be the task's current attempt in `needs_input` without a
frozen candidate, at the given version, and not already replanned. The review
must be its latest review request, finished with that verdict and exactly one
consumed result, and the snapshot must be that candidate's complete snapshot. A
recheck receipt must be closed on that review. Active, uncertain or unconsumed
reviews, a working implementer, open recovery, restart holds, pending controls,
manager stops or switches refuse the request.

The result state is `replan_created_pending_quiescence`. The new attempt waits
in `materialization_pending` while the old attempt's roles are stopped. Its
workspace is created only after every old role has exited with recorded
process-group quiescence. Checks, captures, permission requests, keyboard
control, submitted or unconfirmed guidance, recovery, restart, switch and
control state must also be settled, and the repository claim must be one
running claim. Until then nothing is copied and the claim stays with the old
attempt. The workspace must match the rejected snapshot exactly, including
untracked files, deletions and modes. If the old attempt's state changes during
copying, the new attempt needs recovery and keeps its reserved lineage. The
old attempt's workspace is left in place. Identical replay returns the same new
attempt; another operation for the same old attempt is refused.

Configured guidance normally comes from the project repository, and a
different file already in a new workspace is refused. A rework or replan
workspace may keep a guidance file whose content differs from the repository
only if the old attempt's activated policy pinned exactly that content through
its latest guidance approval. That policy must also still match the current
workflow, manifest and active configuration, and the bytes must be the source
snapshot's. Protected workflow files (`.agents/`, `.claude/`, `.codex/`,
`AGENTS.md`) and package or configuration files are never kept this way. Each
kept file is listed with its provenance in the new workspace policy under
`rework_guidance_sources` and in a `rework.guidance.preserved` audit event; no
plan approval, accepted snapshot or review outcome comes with it. If
materialization stopped with a recovery, fix the cause and retry the same
lineage with `resolve_recovery`, using the rework intent id as `recovery_id`,
no `session_id`, and `decision` `retry_materialization`. The retry re-verifies
the workspace against the exact source snapshot before continuing.

Service startup leaves an unfinished rework or replan workspace alone. Until its
lineage completes or is cancelled, only that lineage's own materialization and
the recovery retry above can promote the workspace or move the claim; ordinary
startup reconciliation neither promotes nor recovers it, and never retries it.

## Bundled workflow identity changes

A new application build can change the bundled workflow identity even when its
upstream revision stays the same. For example, replacing a machine-specific source
path with portable provenance changes the manifest hash. Existing project setup,
attempt workspaces and retained sessions remain bound to their recorded identity;
the new build does not silently rewrite those bindings.

When the dashboard reports stale project readiness, use the project setup flow to
review and activate the current bundled workflow and complete the required fresh
role qualification. An attempt pinned to the previous workflow or workspace policy
still needs fresh attempt lineage through the available task recovery controls.
Project reactivation alone does not make an old attempt or native session current.
An exact-resume refusal requires the appropriate fresh accounted session flow and
current capability evidence; repeatedly retrying the old binding cannot repair it.
If no applicable recovery action is available, preserve the old task as history and
create a new task under the current project setup. Do not edit stored hashes or
reuse old qualification evidence to bypass these checks.

## Database restore points

The database commands operate on the instance selected by `--data-dir`. Current
schema only is supported: they do not upgrade an earlier installation. Fresh
service initialization still creates the current schema. Service start, under
the instance lock, upgrades supported existing schemas 31, 32 and 33 to schema
34 through the existing migration transactions. Unsupported existing schemas,
including schema 30 and newer unknown schemas, are refused. Each failed
migration rolls back its own transaction. Schema 32 records final-repair receipt
provenance; schema 33 records native-resolution provenance; schema 34 adds the
exact submitted guidance text and digest while preserving the original body.
Existing receipts retain their recorded values. Automatic pre-migration restore
points are outside the selected scope; database observation and backup commands
do not grant upgrade authority.

```sh
llmrelay database --data-dir <ABSOLUTE_INSTANCE_PATH> inspect
llmrelay database --data-dir <ABSOLUTE_INSTANCE_PATH> check
llmrelay database --data-dir <ABSOLUTE_INSTANCE_PATH> backup
llmrelay database --data-dir <ABSOLUTE_INSTANCE_PATH> verify <BACKUP_DIRECTORY>
llmrelay database --data-dir <ABSOLUTE_INSTANCE_PATH> restore <BACKUP_DIRECTORY>
llmrelay database --data-dir <ABSOLUTE_INSTANCE_PATH> release-hold
```

Inspection, integrity checks and sanitized export do not create or migrate a
missing database. Backup, restore and hold release require exclusive instance
ownership: stop the service first and wait for drain to finish. A command refuses
if the instance lock is held; it does not stop the service for you.

Backup defaults to a private sibling directory named `<instance-directory>.backups`.
`backup --backup-dir <ABSOLUTE_DIRECTORY>` selects another private location outside
the instance and registered repositories. A restore point includes SQLite state
and a versioned manifest with length, schema and SHA-256. SQLite's backup API
includes committed WAL data. Only successfully published restore points are usable.
Retention targets ten backups, thirty days and five GiB, always preserving the
newest verified restore point. Incomplete evidence and displaced-state quarantine
are not automatically removed.

Restore replaces database state, not repository files, native provider history,
worktrees, transcripts or external effects. The displaced database and sidecars
are preserved under `state/database-quarantine/`. A durable journal tracks file
replacement and the restore hold. Preserve both when investigating interruptions;
do not manually delete the journal or move SQLite sidecars to bypass a refusal.

If interruption occurred before displacement, startup and backup refuse the
unfinished operation. Explicitly repeat `database restore` with the same backup;
the command continues only if the backup, staging file and original live file
bindings still match the journal. Changed evidence is a refusal, not permission
to overwrite it. Later replacement phases are reconciled from the journal.

A restored instance is held for reconciliation. Old credentials, reusable
permission rules and automatic resume authority are invalidated. The dashboard
may be available for inspection and safe recovery, but execution remains fenced.
Resolve the reported process, claim, check and external-reference prerequisites,
stop the host, and run `release-hold` offline. Success leaves projects and work
paused: resuming normal work is a separate explicit action and requires current
authority. Release does not restore old approvals or launch agents.

The dashboard shows one instance restore-hold explanation with exact recorded
session, claim, check, freeze and recovery blockers, plus affected-task links.
Current boot, displaced-process, journal and external-path checks remain marked
unobserved until offline verification. Offline release uses the same recorded
prerequisite enumeration and still performs its current environment checks.
A dashboard explanation cannot release the hold.

If the displaced database was unreadable, LLMRelay cannot inventory all old
processes. It records a valid OS boot identity before replacement and requires a
verified machine reboot after that point before release, in addition to the
other prerequisites. A reboot performed before restore does not satisfy this
condition. The command never reboots the machine, and acknowledgment alone cannot
release the hold. Unavailable boot evidence refuses unsafe replacement/release.

## Provider compatibility explanations

Role settings, project setup and diagnostics show the reviewed contract candidate,
missing evidence and next step for a provider profile. Expand details for the pack,
contract revision and observed version. A candidate match is not a Supported profile:
the existing exact-profile qualification requirements still apply.

Unknown provider versions cannot use a matching contract's launch or resume paths.
Follow the displayed next step rather than repeatedly retrying the old session.
The current bundle names exact Codex CLI 0.157.1 as a candidate. Older 0.155.1
proofs remain historical and cannot authorize the new executable. The Claude
bundle has no production version selector yet, so it directs operators to an
updated LLMRelay release. Native role qualification remains required for each
exact profile; engineering fixtures do not supply that evidence. Manifests are
shipped with the application, not downloaded or edited
through dashboard settings.

## Local client protocol compatibility

The dashboard, human CLI, and cmux attachment client use protocol generation 1.
Use the CLI packaged with the running service. The browser discovers the
authenticated `/api/protocol` descriptor before operational requests and refreshes
that discovery after observing a different service incarnation. Every operational
HTTP request must declare a compatible generation and required transport features.
The human control socket requires a compatible Hello before any business request;
attachment connections identify themselves separately from ordinary CLI clients.

A protocol refusal occurs before business dispatch. Reload the dashboard or use
a matching packaged CLI; do not automatically restart the service or replay a
mutation. An old or closed control connection may prevent discovery entirely.
That does not establish whether the server is old or merely unavailable. Ordinary
revision conflicts and uncertain mutation outcomes retain their existing refresh
and operation-ID reconciliation behavior.

These descriptors describe local transport support only. They do not qualify a
provider, replace native capability evidence, or grant workflow or input authority.

## Foreground recipe schedules

In a project's Recipes page, create a daily or weekly schedule for an exact recipe
revision. New schedules start paused. **Enable** or **Resume** arms the next strictly
future occurrence; **Pause** stops intake. Times are canonical UTC, with a local-time
preview; daily means 24 hours and weekly means 7 days, not a DST-aware local calendar.
Edits reanchor to a future occurrence without changing already-created tasks.

Scheduling runs only while the foreground service runs and execution is enabled,
with drain and restore holds respected. It creates backlog drafts only, including
when a project queue is paused. It never requests run-next, starts an attempt, grants
approval or accepts work. Due events before this service start are recorded as missed,
not backfilled. After a delay, at most the newest eligible occurrence creates a draft;
earlier missed events are summarized. Schedule/fire identity prevents duplicate tasks.

The page shows next fire and last outcome, including missed or skipped-ineligible
events. Ineligible pinned configuration/checks are reported and that occurrence is
not retried; re-save the recipe and explicitly edit the schedule to its new revision.
Archiving a recipe pauses its schedules; archiving schedules preserves fire history.
A profile set cannot be archived while active recipe/schedule references depend on it.
Created tasks retain their provenance and ordinary authority checks.

### Sessions waiting at startup

A running provider process is not proof that work has begun. The session card shows **Waiting for startup** while its current readiness is unknown, including after a retained resume. Choose **View output** to inspect a possible native startup prompt, then **Take keyboard control** if it needs an answer. If Codex displays **Hooks need review**, inspect the listed hooks before deciding whether to trust them. Choose **Release keyboard control** afterward so automatic work can continue. The notice clears when current readiness is reported or the process exits; it does not infer a particular prompt from terminal text or authorize hook trust. Process, launch, readiness, and capture diagnostics remain under **Technical details**.

## Permission notices and appearance

If an automatic step fails for an identified attempt, **An automatic step failed**
opens that attempt's exact recovery record. Other eligible tasks can progress on
later coordinator ticks. Inspect Activity, enter a note, and choose **Retry failed
step** when available. It releases only that record for reevaluation; it does
not repeat an action with an unknown outcome or clear another hold. **Review the
failed step** in Controls opens the same exact record. A stale or disappeared
record requests fresh state instead of substituting another recovery item.
Ordinary Pause and Cancel remain available subject to their existing process
and task-state checks. Some completed or disposed subjects can settle their
exact hold automatically; unrelated activity, time and task-version changes
cannot do so. Failures that cannot be attributed to an attempt retain the global
coordinator deferral.

An unavailable process table is reported as unknown, not as an empty or stopped
set of processes. Recorded status and database-only permission expiry remain
available, while process-dependent actions wait. A drain cannot report verified
quiescence or finish shutdown solely because its lists are empty when the process
inventory is unknown.

The dashboard lists current waits in **Waiting for you now**. A managed approval opens the exact request in Approvals; a provider-native prompt or turn failure opens the exact agent output. An independent task hold remains visible alongside an approval. A failed or uncertain approval response requests fresh state and is never automatically resent.

Guidance written to a current running session, or a current resumed invocation,
appears in attention when trusted input acceptance has not been observed for
30 seconds. The notice opens that agent's output. It is an observation, not a
claim that the agent is stuck or permission to resend. An older invocation's
hook, ordinary tool activity or a running process cannot establish acceptance.
The dashboard's existing 30-second state refresh updates time-dependent notices
even without a committed state change, so the notice can appear on the next
refresh after its threshold; background-browser throttling or disconnection may
delay presentation. Initial-launch acceptance is not covered by this notice.

If dashboard rendering fails, the page displays a recovery screen with an
explicit **Reload page** button and collapsed, sanitized technical details.
Reloading does not replay a mutation. The boundary covers rendering failures;
ordinary request failures continue to use the existing error messages.

Choose **Enable browser notifications** to enable alerts for this dashboard visit and request browser permission if needed. Previously granted site permission does not enable app alerts by itself. New waits may notify while the dashboard is unfocused; denied or unavailable browser support leaves the in-app notice available. Initial and reset snapshots list current waits without replaying historical alerts. An alert is navigation, never permission or proof of provider execution. External Codex completion watchers are separate local engineering tooling.

Session Access keeps **Session open**, native accepted-turn evidence and report submission separate. An implementation report awaits processing or verification; neither label means the task is accepted. A later accepted turn can make that report historical. For native dialogs without a supported signal, use **View output** and explicit keyboard control; the app does not guess from terminal text. See the [signal matrix](PERMISSION_SIGNAL_MATRIX.md).

The sidebar **Appearance** selector offers System, Light and Dark. An explicit choice overrides the operating-system preference and is stored for this browser origin. System follows the operating-system preference; unavailable or invalid preference storage falls back to System.
