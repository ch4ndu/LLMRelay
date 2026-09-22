# Sessions, recovery, and diagnostics

[Back to README](../README.md)

Examples below use `llmrelay` on PATH. If you have not added it to PATH, substitute the full path to your packaged `bin/llmrelay`. Building a development executable is covered in [Building LLMRelay](BUILDING.md).

## Startup and browser authentication

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

LLMRelay restores the browser workspace independently from execution. The selected project, task detail, page, and details-column width persist locally. Live output belongs to cmux; old embedded-terminal layout and scrollback settings are no longer used. Reopening an ended session shows recorded history and never launches a provider. LLMRelay does not register its short-lived attachment command as cmux resume metadata: the command contains an exact process binding and route token which must not be restored after that binding changes. After a service restart, recover or resume eligible work in LLMRelay and open its current attachment; LLMRelay creates terminal access from the new current binding.

Automatic work resumption is off by default and remains off across service restarts until a human enables **Auto-resume eligible work** in the Workspace page’s **Restart restoration** group. During a deliberate restart, stop with drain so the service records the exact running native bindings before it interrupts them:

```sh
llmrelay stop --drain \
  --data-dir "$PWD/.local/my-agenticjira"
llmrelay serve \
  --data-dir "$PWD/.local/my-agenticjira" --port 0
```

Use the authenticated dashboard automatically opened by `serve`, or the current one-use login link when running with `--no-open`. The service holds all captured previously running work, including blocked or skipped native-resume candidates. Missing native history or matching current Supported capability evidence cannot silently trigger a fresh session. Legacy Codex Implementer Supported rows remain demoted because they predate the service-recorded native-policy identity checks. Migration 017 also demotes old Codex Manager/Reviewer Supported rows that lack the denied-read-floor marker to Unverified while preserving their proofs, evidence references, timestamps, and prior gaps. Those historical rows cannot authorize a launch or resume under the new key. An explicit Continue or recovery action can release the hold for normal workflow dispatch only after positive process-quiescence checks; it does not itself perform a native resume. In the Workspace page, choose **Open task recovery** on the relevant restart candidate. Its task exposes **Resume retained restart session** only when the exact binding is eligible. Use **Continue with fresh dispatch** only when its separately checked recovery prerequisites are satisfied. The equivalent non-browser commands are:

```sh
llmrelay resume-work \
  --data-dir "$PWD/.local/my-agenticjira" --session <SESSION_ID>
llmrelay resume-work \
  --data-dir "$PWD/.local/my-agenticjira" --eligible
```

Every selected-session response lists that session’s durable `resumed`, `queued_capacity`, `blocked`, or `failed` result and reason; the restoration list separately retains skipped candidates and their reasons. Submitting a request is not reported as a successful resume. Resume always uses the recorded provider’s exact native session, worktree, generation, candidate/review authorization, configuration, credentials, budgets, and normal capacity locks. A Claude executable changed by an external upgrade or another user-managed session legitimately fails the frozen executable check; LLMRelay does not pin, downgrade, or copy the old binary, so recovery requires a fresh accounted handoff. Paused, Needs input, Awaiting review, completed, cancelled, archived, isolated-validation, stale/replaced, missing-history, incompatible-policy, or process-uncertain sessions remain stopped. There is no fresh-session fallback labeled as restoration. Auto mode retries only a capacity wait; a real preflight or proven non-delivery failure remains visible for human recovery instead of spending another provider turn automatically.

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

The Attention inbox and owning task/setup screen explain who can advance a
blocked operation, what it is waiting for, and the available action. A visible
action is guidance: the service rechecks its current task version, session,
generation, policy and process ownership when submitted. A stale screen cannot
authorize a replacement or bypass an approval.

| Situation | Next action and boundary |
| --- | --- |
| Exact native resume rejected after runtime identity changes | Review the reason. **Start fresh accounted session** is available only if the same role profile and current authority still qualify. It consumes a fresh call; it is not native resume. |
| Role, candidate, configuration, hook trust, native history or invocation provenance changed | Review the current role authority or prepare corrected verification. Old approval evidence cannot be reused as replacement authority. |
| Resume or review authority already spent | New explicit authorization is required; the dashboard cannot reset the allowance. Unknown permanent rejections remain terminal with their reason. |
| Interrupted installation or uncertain workspace creation | Open project/task recovery and follow its journal or reservation action; see [project recovery](PROJECT_SETUP.md#interrupted-setup-and-workspace-recovery). |
| Service cannot prove process exit | Resolve exact process ownership before resuming or replacing work. A quiet terminal is not proof of exit. |
| Graceful stop exceeds 30 seconds | The service records recovery for the exact managed process. Choose **Retry graceful stop** or explicitly **Force stop exact managed process** when offered. |
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
