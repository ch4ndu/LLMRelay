# M6 — Revisioned live state and actionable attention

Status: approved by independent plan review G2, 2026-09-23. Finalized in-scope plans
and proportionate verification are covered by the user's standing authorization.
M4B is verified and packaged; preserve all existing uncommitted work.

## Outcome and scope

Replace the dashboard's two-second full-state polling loop with authenticated,
revision-aware long polling and a slower reconciliation watchdog. Present a grouped
cross-project attention rail whose navigation resolves exact current entities.
Keep the existing Rust/React/SQLite host and all mutation/approval authority gates.

Native OS notifications and their durable delivery/deduplication subsystem are
deferred with intervention-dependent work. No notification permission or GUI test.
No SSE/WebSocket, remote push, provider API, new dependency, generic event framework,
completion batching, background service, menu-bar app or update mechanism. Full
snapshots are deliberate: no speculative entity patch/reducer protocol or audit
payload stream. A typed invalidation announces committed state; fetching its
snapshot remains the authority for presentation and never grants mutation rights.

Classification: broad, one coherent state-to-dashboard outcome. One writer because
cursor/schema/DTO/endpoint/frontend contracts share mutable inputs. Implement in
three bounded handoffs (storage/transport, attention/UI, integration); manager owns
integration, docs and final verification. No parallel writer or competing build.

## Evidence and smallest sufficient mechanism

Store::lock serializes supported service database access. workflow::state holds
that lock through its persisted projection. SQL writes are distributed across
store/workflow/coordinator/recovery/scheduler/permissions/TRIP/checks, including
autocommit writes. A manual bump at selected application operations is incomplete.
Existing permission Notify is not a global change signal. Audit UUIDs and created_at
ordering cannot serve as a durable, universally covered state cursor.

A bounded schema addition and a Store guard are needed to cover these writers
without rewriting their transactions. This is not a second event authority: SQL
triggers own the durable revision; Notify is a lossy optimization. Existing locks,
transactions and watchdog convergence remain. Estimated production footprint
700–1,200 lines plus explicit migration triggers, in existing modules; narrow shared
Rust/TS types only. No new runtime dependency or separately maintained framework.

## Unit 1: committed revision and safe transport

Owned: migrations/029_state_revision.sql, src/store.rs, src/domain.rs,
src/operations.rs, src/server.rs, src/workflow.rs; tests/contracts.rs and existing
server test module only when the build slot is held. Prior recovery semantics protected.

1. Add a singleton durable integer revision and explicit INSERT/UPDATE/DELETE
   triggers for every persisted table read directly or indirectly by AppStateDto.
   Enumerate the projection-to-table coverage during implementation; include
   decision/recovery/permission/setup dependencies, not only displayed tables.
   Use a dedicated singleton metadata table: instance_settings is projected as a
   whole row and its version participates in optimistic mutation checks.
   Exclude liveness-only updates: session_processes last_seen_at and input_leases
   expires_at/updated_at renewals must not advance the cursor. Use value-change
   predicates on semantic columns, retaining insert/delete, process identity,
   lease ownership and revocation changes. Watchdog refreshes timestamps/expiry.
   Other repeatedly written tables require the same semantic-change audit.
   Exclude metadata itself. Do not create dynamic trigger discovery or a generator.
   Each write increments within its transaction; rollback restores the revision.
   Jumps are legal, revision exhaustion must fail rather than wrap. Advance schema
   constant/registration together. Fresh-install scope only; keep read-only open and
   current-schema backup/restore authority intact, update current-schema fixtures.
2. Store clones share a state Notify. A narrow lock guard
   preserves Deref/DerefMut connection access and existing poison/error behavior.
   Capture revision on acquisition and compare the committed revision at guard
   release; only wake if it increased and connection is in autocommit state.
   total_changes may optimize but is never proof of commit. Rolled-back changes
   must not announce success. Guard Drop must not panic, acquire its own mutex or
   fail an already committed write; if observation fails, watchdog read exposes
   errors/reconciles. Support read-only Store openings without writes.
3. Snapshot includes opaque decimal-string revision (avoid JS number precision)
   and the existing WebState.instance_id as service incarnation; no new Store token.
   The token changes on service restart/open of restored state so a lower restored
   revision cannot be mistaken for an older response in the same history. Snapshot
   cursor and persisted fields are read under the same Store guard. Volatile process
   metrics retain observation timestamps and are refreshed by the watchdog.
4. Add GET /api/state/wait with the existing browser cookie/Host/Origin checks.
   Validate bounded cursor/timeout inputs; wait at most25seconds, client timeout
   longer (35seconds), watchdog default30seconds with bounded configuration.
   Register/enable Notify future before checking the revision. Release every DB
   guard before await. Snapshot construction runs in spawn_blocking; no process
   sampling on async executor. On newer revision return state_changed plus full
   snapshot; timeout returns bounded unchanged metadata without rebuilding the
   expensive snapshot. Unknown incarnation, future/invalidated cursor returns an
   explicit reset snapshot. Malformed input returns400. No unauthenticated waiter.
   No provider payload/credential stream; use existing redacted projection only.
   Bound wait concurrency per service, cleanly release permit on cancellation;
   shutdown closes waits. Don't allocate unbounded tasks or hold DB across await.
   Exact query: incarnation (nonempty, at most128bytes), revision (canonical
   unsigned decimal string within SQLite signed64bit range), timeout_ms (optional,
   default25000, accepted1000..25000). Response is a closed tagged union:
   {outcome:"state_changed"|"reset", incarnation, revision, state} or
   {outcome:"unchanged", incarnation, revision}, with no state on unchanged.
   Same incarnation and client revision below current yields state_changed; equal
   waits; greater yields reset. Different incarnation yields reset. A maximum of
   64 concurrent waits uses nonblocking permit acquisition: excess returns503
   with Retry-After:1. Shutdown returns503 and releases permits; client disconnect
   cancels its waiter. Create the shutdown watch channel before WebState and share
   its receiver with handlers. Apply spawn_blocking to existing /api/state too.
   Both endpoints return the same cursor-bearing snapshot shape.

## Unit 2: frontend convergence and exact attention navigation

Owned: frontend/src/api.ts, types.ts, App.tsx, new frontend/src/liveState.ts,
components/AttentionInbox.tsx,
Workspace.tsx and the existing task/setup/permission/recovery panels needed for
typed navigation; styles.css; src/domain.rs/workflow.rs projection. Exact panel
paths will be named in the writer handoff after source inspection.

1. One cancellable long-poll loop per mounted dashboard after bootstrap. Track
   the lifecycle in liveState.ts, exported for the existing DOM harness; App owns
   mounting and disposal. Export a mutable timing object using the existing api
   transportTimeouts pattern: wait25000ms, request35000ms, watchdog30000ms,
   reconnect backoff1000..30000ms. Validate watchdog configuration within
   1000..300000ms. Inject transport/timing into tests rather than real long waits.
   Compare validated canonical revision strings by length then lexicographically,
   never Number. Track
   accepted incarnation/revision and local request generation. Reject older
   same-incarnation snapshots, duplicated responses and callbacks from cancelled
   generations. A reset establishes a new generation; an old response cannot flip
   the UI back to a retired service incarnation. Reconnect/auth failure triggers
   bounded backoff and authoritative bootstrap/refresh; unmount aborts requests.
2. Post-mutation refresh must queue a new authoritative read if the current read
   started before the mutation. It must not simply return the stale in-flight
   promise. Coalesce multiple invalidations without losing a dirty refresh, prevent
   overlapping refresh storms, and preserve latest state on transient failure.
   Watchdog provides volatile metrics and event-loss recovery. Same revision may
   update observed metrics but cannot overwrite a newer durable snapshot. Avoid
   flashing offline/loading on every wait or resetting forms/scroll on refresh.
3. Add server-projected typed AttentionItem values with stable ID, category,
   readable reason, exact navigation target, and enclosing state cursor. Categories:
   decisions/permissions/recovery/compatibility/blocked/completed-awaiting-acceptance.
   Derive from existing authoritative task/decision/setup/permission/recovery data,
   never terminal text. Specify precedence/deduplication for overlapping task rows;
   preserve distinct actionable permission/recovery items and cross-project scope.
   Keep manager guidance behavior and empty state; avoid counting the same item twice.
4. Navigation carries exact task/attempt/session/project/approval/recovery identity
   through a closed discriminated target enum: task, attempt, session,
   permission_request, recovery_record, project_setup. Each variant requires its
   own project/entity identities; do not reuse untyped ContinuationAction.binding.
   and relevant entity version/hash when applicable. On click reconcile against
   latest snapshot, switch project and open/scroll/focus the exact existing panel.
   If superseded or missing, refresh once and explain that the item changed; do not
   silently route to a newer attempt or execute an action. Stable DOM keys, accessible
   group labels, keyboard buttons and bounded layout at narrow widths. Preserve
   existing per-action revision checks as final mutation authority.

## Verification and acceptance

Moderate coverage in existing Rust/DOM harness, proposed12causal areas and up to
2,000 support lines including inline test helpers. Standing authorization permits
proportionate increases with recorded cause. No new harness, paid product sessions,
computer use or live user-project changes. Existing temporary fixtures only.

1. Transaction/autocommit increment, rollback nochange/no wake, read nochange.
2. Projection table coverage, reopen persistence, read-only access/schema behavior.
   Freeze a hard-coded coverage list including helper reads from coordinator,
   scheduler, permissions, trip and database restore prerequisites; exercise each
   table's INSERT/semantic UPDATE/DELETE and assert exact trigger coverage. Include
   nonprojected check_processes/operation_receipts as negative cases. Source tracing
   during Unit1 showed hook_events affects readiness decisions and must be covered.
   Repeated process observations, transcript sequence-only updates, and
   pure lease renewals do not advance revision; identity/revocation changes do.
3. Subscribe/read race, multiple commits/coalescing, timeout/cancel without DB lock.
   Wake tests must write through Store: Fixture::connection opens a separate
   connection and proves durable revision only, not Store notification delivery.
4. Authentication/Host/Origin and invalid/future cursor bounds, waiter limit/release.
5. Consistent snapshot cursor with concurrent writes; restart/restore incarnation.
6. Frontend duplicate/delayed/out-of-order and retired-generation rejection.
7. Dropped wake/reconnect/watchdog recovery and auth failure without request storm.
8. Mutation during in-flight read queues later reconcile and preserves newest state.
9. Attention grouping/precedence, exact counts and all project scope.
10. Exact task/attempt/session/permission/recovery/setup navigation, stale item refusal.
11. Guidance and form/selection preservation with changing snapshots.
12. No credentials/provider raw payload in event envelopes; read-only refresh cannot
    mutate task authority or dispatch work.

Use focused Rust/DOM checks per handoff, affected compilation, formatting/deslop,
independent code review, one integrated ./scripts/verify.sh after convergence,
activated final traceability (cross-layer modules), fresh final review. Reuse valid
receipts; only rerun invalidated dimensions. Manager manually traces exact UI click
routes in source/DOM tests, no GUI acceptance claim. Package locally in taskdir;
no service installation/restart or publication. Docs/OPERATIONS, SECURITY, WORKFLOWS
and milestone status updated for actual final behavior, one manager owner.

## Risks and boundaries

Trigger coverage omission, connection-guard API drift, lost Notify race, cursor
reset across restore, stale frontend promise and wrong-entity navigation are primary
risks addressed above. SQLite transaction/schema authority must remain unchanged.
Permanent observer/outbox/daemon/framework or material architectural expansion
requires plan reconciliation, not an implementer improvisation. Native notification
delivery/dedup/mutes remain deferred, so do not claim the entire original M6 list
complete; mark the unattended live-state/attention slice and deferred portion.
Implementation uses requested Claude opus-5.5-max only after exact CLI identity/
effort preflight. No silent model replacement. Reviewer roles stay configured.
