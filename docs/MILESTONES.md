# LLMRelay milestones

Decision recorded: September 18, 2026.

Initial-v1 boundary updated September 23, 2026: the user explicitly excludes
deferred milestones from v1 completion. M2, M3, M8B and native notifications are
post-v1. M7 and M8A engineering verification is complete; M8A local packaging is
complete. M9 engineering verification is complete; its local packaging is complete.
The bounded M5 path-normalization repair and consolidated initial-v1 automated
verification passed September 25. All agreed initial-v1 engineering milestones
are complete. M4A/M4B, M5 and the M6 dashboard slice are verified.
Hands-on acceptance and deferred native qualification remain explicit evidence
boundaries, not completed checks. See [current scope](NEXT_MILESTONE_PLAN.md#initial-v1-scope-decision--september-23-2026).

September 22 planning update: the user selected the Herdr/Jinn-derived work as
the next product scope and deferred personal hands-on acceptance until the
selected feature set is incorporated. See [the proposed delivery sequence and
milestone 4A implementation plan](NEXT_MILESTONE_PLAN.md). Engineering verification
continues at each checkpoint. Historical status and gates below describe the
earlier decisions; the new plan proposes their ordering and acceptance changes
without claiming implementation approval or completed user validation.

The selected direction combines a per-user macOS LaunchAgent (option A) with
a later menu-bar application backed by that same service (option D). Deliver
A first, then D. Both are deferred while the user validates the current service
flow. This document records the roadmap; it does not authorize starting either
implementation now or claim that the user has accepted the everyday flow.

## Milestone 1 — Validate the current service flow

**Status: integration delivered; user acceptance of the everyday service flow pending.**

### Refinements selected during validation

On September 18, the user selected **cmux terminals with removal of the embedded
browser terminal**. LLMRelay retains workflow control, approvals, session status,
and diagnostics. The delivered integration provides:

- Opening and reopening the correct session in cmux, with safe reuse of existing
  routes.
- Removal of the embedded terminal and its split, search, scroll, and
  keyboard-input controls.
- Keep transcript capture and useful error/history access in LLMRelay even
  though interactive output is presented in cmux.
- Explain exact provider model identifiers through readable model selection,
  with explicit manual entry and no implied account-availability guarantee.
- Stop and Change manager controls for both setup discovery and task managers,
  including recovery from an invalid model selected before discovery succeeds.
- Preserve process ownership, input arbitration, permission gates, native-session
  history, and generation-safe switching. Opening or restoring a terminal must
  not independently start or resume an agent.

The independently reviewed approach is a cmux terminal running an LLMRelay
attachment client while the existing engine owns the agents. The embedded
browser terminal has been replaced by this external terminal path. Dashboard
routing, setup and task manager controls, recorded-output access, and integrated
validation are delivered. The disposable attachment checks cover input
takeover, detach and reconnect, terminal restoration, output latency, and idle
CPU. Live six-role Claude requalification under the changed socket policy has
also completed for this integration; older capability proofs remain
insufficient for changed policy. Direct tmux ownership remains an evaluated
alternative rather than a selected requirement.

These refinements do not advance the deferred LaunchAgent or menu-bar work.

### Selected terminal-lifecycle refinement

On September 19, the user selected a tighter cmux integration after live
validation exposed repeated surface creation, cmux resume-command prompts,
blank late-approved surfaces, and attachment routing races. The current
implementation candidate records one LLMRelay-owned cmux workspace per task in
each service boot and one active, mode-neutral surface for each current role
session. **View output** creates or focuses that surface in view-only mode;
**Take keyboard control** advances a revision-bound input intent on the same
authenticated attachment instead of creating a second terminal.

LLMRelay remains authoritative for provider processes, workflow state,
permissions, approvals, transcripts, and recovery. cmux remains the terminal
presentation layer. An ephemeral attachment command is never registered as a
cmux resume command. Unknown create or observation results require explicit
authenticated cleanup and never trigger automatic reuse. Validated presentation
loss creates a fresh view-only surface only after a later explicit View, with
retained transcript replay and no restored lease. Stale surfaces, tokens, or
native bindings are never reused after a process or role-generation change.

The immediate compatibility repair removes attachment resume registration,
silences terminal-generated input in read-only watches, and recognizes the
real Codex helper when Codex was launched through the cmux shim. The persistent
task-workspace candidate is awaiting manager integration and independent review;
it does not mark the conversion accepted, change the deferred LaunchAgent or
menu-bar milestones, or replace their separate packaging and human-acceptance
gates.

### Validation flow

Use the existing terminal-started `llmrelay serve` and browser dashboard to
validate the everyday flow before changing how the service is hosted:

- Start the service and open the authenticated dashboard.
- Register a project, inspect an existing TRIP installation, and follow the
  reviewed initialization/adoption flow with understandable approvals.
- Create and run a task, follow agent output, and handle permission requests
  and workflow review gates.
- Stop with `stop --drain`, restart against the same data directory, and inspect
  restoration of tasks, session context, and pending approvals.
- Record confusing UI, failures, and recovery gaps for repair and revalidation.

Existing automated and disposable-runtime evidence is separate from this user
validation. Project installation and live agent work retain their applicable
approvals; this checklist does not authorize changes to a user repository.

**Exit gate:** the user accepts the current service flow and chooses to start
the background-service milestone. No automatic transition to implementation.

## Milestone 2 — Per-user macOS background service (A)

**Status: selected direction; explicitly deferred from the unattended run by the
user on 2026-09-23. Revisit after the user returns; service-flow acceptance remains
pending.**

Run the engine as a LaunchAgent under the signed-in user's account, independent
of the terminal that starts it. Keep the browser dashboard and native CLI agent
architecture.

Planned scope:

- Provide service installation, start, status, controlled stop, and uninstall
  operations using a stable installed executable location.
- Support manual start and optional start at login; keep login startup an
  explicit user setting.
- Open and authenticate the dashboard on demand with a clean browser URL.
  Service restarts must not repeatedly open browser windows or expose login
  tokens in routine service logs.
- Handle service-manager termination with bounded graceful shutdown and
  durable recovery information for interrupted work.
- Recover from service crashes without undoing an intentional stop. Preserve
  single-instance ownership and prevent duplicate agent dispatch.
- Retain the existing data directory, diagnostics, provider account setup,
  sandbox restrictions, and approval gates. Resolve provider executables
  reliably outside an interactive shell environment.
- Keep host restart distinct from task/session resumption; honor the existing
  explicit resume policy and outstanding approvals.

**Acceptance:** closing Terminal leaves the service usable; dashboard reopening,
intentional stop, crash recovery, optional login startup, and active-work shutdown
are verified. Installation/uninstall behavior is clear and uninstall preserves
project and application data. A user LaunchAgent runs while the user is logged
in; this milestone does not promise execution through logout or machine sleep.

Exact CLI names, installation paths, restart policy, and shutdown timeout will
be settled in the implementation plan. No proposed command is current usage
documentation.

## Milestone 3 — Menu-bar companion (D)

**Status: selected follow-on; explicitly deferred from the unattended run by the
user on 2026-09-23. Milestone 2 validation remains a dependency.**

Add a native menu-bar interface that controls the same LaunchAgent and opens
the existing browser dashboard.

Planned scope:

- Show service status and provide Open dashboard, Start, and controlled Stop.
- Expose the Start at login setting and access to diagnostics.
- Make service failures and recovery actions understandable without Terminal.
- Keep quitting the menu-bar interface distinct from stopping the service,
  with clearly labeled actions.
- Reuse the service lifecycle, authentication, and ownership controls from
  milestone 2; do not create a second engine or a separate permission system.

**Acceptance:** routine service control works from the menu bar without a
terminal, the dashboard opens authenticated, and closing the companion does
not unexpectedly interrupt running work. Existing task and permission behavior
remains consistent with milestone 2.

## Candidate future milestones — stability and product improvements

**Status: research-backed candidates; not selected, scheduled, or authorized for implementation.**

The candidates below record ideas identified while comparing LLMRelay with
[Herdr](https://github.com/herdrdev/herdr) and
[Jinn](https://github.com/hristo2612/jinn). They are product ideas, not proposals
to embed either project or replace LLMRelay's engine. Milestones 1–3 retain their
current decision state and gates. The ordering below puts state safety and
explainability ahead of convenience features; a later planning decision may
split, combine, reorder, or decline any candidate.

Research snapshots used for this comparison:

- Herdr commit `f10df75c8a5f1b5e5f90689c3a8ceb6758366e05`, inspected September 20, 2026.
- Jinn commit `62f026aa6cb740c807ac74c350a356538ba6eea8`, CLI version `0.33.3`, inspected
  September 20, 2026. This is the same revision used by the earlier Jinn
  architecture comparison, now rechecked against the current LLMRelay tree.

### Milestone 4 — Durable state protection and recovery rehearsal

**Candidate priority: highest.**

**Engineering status (2026-09-23): approved current-schema M4A and crash-boundary
M4B scope complete.** M4B passed independent code and fresh final review, 44 Rust
unit tests, 78 contract tests and two runtime tests; unchanged frontend checks
remain valid. Coverage uses durable-state reconstruction and four bounded test
hooks, not physical power-loss or live-provider crash claims. Historical migration
rehearsal remains outside the user-selected fresh-install scope. Hands-on user
acceptance and installation remain deferred. See [the M4B plan](MILESTONE_4B_PLAN.md)
for the precise accepted scope and [operations](OPERATIONS.md) for recovery actions.

LLMRelay already opens SQLite with foreign keys, WAL, `synchronous=FULL`, and a
busy timeout. Its startup path then runs migrations directly. Add a recovery
layer around that durable state before expanding background-service automation.

Planned scope:

- Confine migration authority. Only `serve` while holding the existing exclusive
  instance lock, or an explicit offline database command holding that same lock,
  may migrate. Diagnostics export and every other read-only command must open the
  database read-only and refuse a schema it cannot read; inspecting state must
  never upgrade it underneath a running older service.
- Run a read-only integrity preflight before any migration that can change an
  existing database. Distinguish corruption, unsupported schema/data, lock
  contention, and low disk space instead of reporting all of them as migration
  failures. Size the disk-space requirement from the database, WAL, backup, and
  expected migration headroom rather than using a fixed amount alone.
- Create a consistent, version-labelled restore point before a schema migration.
  Use SQLite's online backup API or an equivalent checkpoint-safe mechanism;
  copying only the main file while WAL data may be pending is not sufficient. A
  migration must fail closed if its required restore point cannot be created and
  verified.
- Write snapshots into a temporary location, include a versioned manifest with
  the SQLite `user_version`, application version, byte sizes, and SHA-256 digests,
  reread and verify the result, and expose it as usable only after an atomic
  rename. Keep backups outside the live data tree and apply explicit count, age,
  and total-size retention limits that never prune the newest verified backup.
- Provide offline inspect, verify, database-check, and restore commands. Reuse the
  instance lock to refuse a running instance. A restore must verify the manifest
  before touching the target, refuse data from a schema newer than the running
  build, avoid merging old and restored state, and preserve the failed live state
  for diagnosis. Restore stays CLI-only; it is not a browser action.
- Add a migration rehearsal that backs up a disposable prior-version database,
  migrates it, checks relational and workflow invariants, restores the backup,
  and proves that the restored instance can open. The existing per-step
  `IMMEDIATE` migration transactions and newer-schema refusal are strengths to
  retain; test restart from every supported intermediate `user_version`.
- Build a crash-point matrix around each external-side-effect boundary: database
  commit, backup creation, restore replacement, worktree creation, provider
  launch, permission delivery, role report, review result, cmux binding, drain,
  restart admission, and restart restoration. For each injected interruption,
  prove the next boot reaches one explicit outcome without a duplicate dispatch
  or false completion. Record the boundary, injected failure, expected next-boot
  outcome, stable reason code, diagnostics event, and owning reconciler.

Herdr reinforces the value of treating restart and update behavior as a product
contract. Jinn contains two different backup paths. Its operator backup command
uses an online SQLite copy, manifests and hashes, temporary staging followed by
rename, bounded retention, and verification before restore. Its startup
pre-migration path instead copies the database plus WAL and SHM sidecars and only
warns when that copy fails. LLMRelay should implement the stronger guarantees in
its own Rust storage and service model and fail closed when a required
pre-migration restore point cannot be verified.

**Acceptance:** a killed migration, truncated backup, low-disk condition, corrupt
database, and crash at every named side-effect boundary each produce a tested,
operator-readable recovery path. No failed backup is advertised as restorable;
no restore runs against a live service; no recovery path silently relaunches an
agent, repeats an uncertain side effect, grants authority, or marks work complete.
After restoring older state, every restart candidate is blocked from automatic
resume until its process and exact native-session identity are reconciled again.

**Ordering decision still required:** the migration-authority, verified
pre-migration backup, restore, and rehearsal subset is a strong candidate to
become a prerequisite for milestone 2. A LaunchAgent that automatically relaunches
the service increases exposure to migration and crash-loop failures. This document
records that recommendation without changing the already selected milestone 2
decision or authorizing milestone 4 work.

### Milestone 5 — Explainable holds, guarded retries, and restart readiness

**Implementation verified on September 22, 2026.** The approved scope in
[Milestone 5 plan](MILESTONE_5_PLAN.md) passed independent ordinary and fresh final
reviews, the Rust/DOM verification matrix, and focused repair checks. Personal
hands-on acceptance remains deferred. Next is milestone 4B crash-boundary planning;
this result does not establish recovery at every crash boundary.

Capability path-boundary follow-up, September 25: the bounded repair now replaces
only complete working-directory/denial roots and descendants, preserving lexical
siblings such as `/fixture/control.sock` beside `/fixture/c`. Focused public
identity/key tests preserve Claude rule syntax, probe-policy equivalence and raw
reservation drift checks. The earlier repo-name fixture explanation was inaccurate;
those fixtures do not cover capability identity. Ordinary and fresh final reviews approved; integrated verification passed
29 DOM, 67 library, 103 contract and 2 runtime tests. See the
[repair plan](CAPABILITY_PATH_BOUNDARY_PLAN.md).

**Candidate priority: highest, after the durable-state foundation.**

Turn LLMRelay's existing transition guards, capability checks, claims, restart
candidates, permission state, and recovery facts into one explainable decision
surface. The same predicates that authorize a transition must produce the user
explanation; a second UI-only rules engine would drift.

Planned scope:

- Add **Why is this waiting?** to tasks, attempts, and sessions. Return the
  current blockers, the evidence that satisfied each prerequisite, evidence that
  became stale, current ownership, and the exact next safe action. Include a
  stable machine-readable reason code beside the human explanation. Derive those
  codes from the coordinator's existing held/waiting decisions and restart
  candidate reasons rather than inventing parallel UI predicates.
- Record each automatic hold as durable audit evidence. Relevant guard classes
  include active or uncertain ownership, pending permission or approval, stale
  capability proof, provider authentication, rate-limit reset, capacity,
  dependency state, recent successful work awaiting review, and an already
  produced candidate or external deliverable.
- Add retry guards before any automated redispatch. A retry must prove that it
  can help, acquire the current transition/claim atomically, and retain the
  refusal reason when it cannot. Manual user actions remain distinct and must
  not be inferred from a timer.
- Treat claims according to their authority. Provider launch permits and
  `reserved`, `launching`, `running`, `unknown`, or `stopping` claims never expire
  by wall clock. They are released only by proven nondelivery or the existing PID,
  process-group, boot, hook, and session reconciliation. Renewable leases remain
  appropriate only for human keyboard input, where LLMRelay already has an
  explicit expiring lease and revocation contract.
- Add a restart-readiness preview before drain or restart. It should classify
  every active session as resumable, fresh-only, blocked, uncertain, awaiting an
  approval, or already complete; show which in-flight side effects need
  re-verification; and explain what the selected restart policy will do. The
  preview must be read-only and must not call the current drain preparation that
  mutates `desired_running`.
- Close the remaining restart bounds. Automatic resumption is already off by
  default, admits one candidate per coordinator tick, and does not retry a failed
  candidate. Add an explicit cap and stagger to the human bulk **Resume eligible**
  path, back off repeated `queued_capacity` attempts, and cap replacement attempts
  per restart candidate. Preserve the exact native-session identity and re-verify
  any command that may have been in flight.
- Add classify-only recovery as the default for any new recovery classifier.
  Classification may surface a proposed action, but it must not impersonate the
  user, close review, or start backlog work. Any later automatic recovery mode
  requires its own explicit decision and narrower acceptance criteria.

Jinn's useful implementation patterns here are a compare-and-swap claim with a
status precondition, owner-checked release, pre-respawn guards, a classify-only
recovery default, an exact-once restart marker, bounded restart replacement,
staggered resume, and an instruction to re-verify commands that were in flight.
Its renewable, expiring Todo lease is useful for understanding the pattern but is
not suitable for LLMRelay launch authority. LLMRelay already has stronger native
process ownership and human acceptance rules, so those existing rules remain the
authority.

**Acceptance:** every non-progressing task has a deterministic explanation that
matches the actual transition decision. Repeating a scheduler or restart tick
cannot create a second attempt. Proven-undelivered reservations and expired input
leases are recoverable, uncertain processes are not treated as dead, automatic
recovery cannot cross a human gate, and all retry and restart limits end in an
actionable recorded state. Tests must compare the explanation payload with the
actual coordinator hold or wait result
for the same tick, and prove that merely opening the restart preview writes no
state.

### Milestone 6 — Revisioned live state and an actionable attention rail

**Candidate priority: high after milestones 4 and 5.**

Engineering status: live-state/attention slice complete, with ordinary and fresh
final approval and integrated verification (15 DOM, 51 Rust library, 84 contract,
and 2 runtime tests, plus formatting, type checking and builds). Native OS
notifications, delivery/deduplication and OS acceptance remain deferred under the
unattended-work decision. This is not completion of the entire original M6 scope.

Before this milestone, the dashboard refreshed full state every two seconds while the
coordinator reconciles every second. Keep periodic reconciliation as a safety
net, but add a revisioned event path so the UI can react quickly without treating
delivery as durable truth. Start with a monotonic state revision and the existing
`Notify` pattern powering revision-aware long polling; consider SSE or WebSocket
only after that smaller contract proves the ordering and reconnect rules.

Planned scope:

- Publish typed local events only after the transaction that created the state
  has committed. Rolled-back writes must emit nothing, and a subscriber failure
  must not fail the authoritative write.
- Give each projection a monotonic revision or cursor. On connection, subscribe
  first, read a snapshot, apply newer events, and reconcile gaps. On reconnect or
  cursor loss, refresh the relevant projection. Keep low-frequency polling as a
  watchdog until the event path has proven reliable.
- Patch UI caches only when an event is newer than the cached revision, then run
  a debounced authoritative refresh for membership and filter changes. Coalesce
  noisy invalidations without reordering task, approval, or completion events.
- Upgrade the existing cross-project **Attention inbox**. It already includes
  task, setup, and continuation attention from all projects; add grouping for
  pending user decisions, permissions, failed recovery, stale compatibility
  evidence, blocked work, and completed work awaiting acceptance. Every row must
  deep-link to the exact task, attempt, session, revision, or approval it
  describes.
- Add optional native notifications derived only from committed workflow
  transitions. Notify for decisions the user can act on and for requested
  completion signals; do not infer authority from terminal text or screen state.
  Deduplicate by durable event identity and let the user mute categories.
- If production evidence shows that several delegated completions routinely pile
  up while a manager is busy, retain each immutable receipt and its order but
  permit a bounded combined wake-up. An intervening user action, approval, or
  workflow turn is an ordering fence and prevents batching across it. Do not add
  this batching from Jinn without a demonstrated LLMRelay problem.

Herdr demonstrates a useful typed socket/event protocol and attention-oriented
agent presentation. Jinn demonstrates post-commit event emission, version-aware
cache patches, reconnect reconciliation, and bounded batching of durable
completion receipts. LLMRelay should use authoritative workflow events rather
than Herdr's terminal-screen heuristics or Jinn's company activity vocabulary.

**Acceptance:** the UI converges after dropped, duplicated, delayed, and
out-of-order events; refresh remains a correct fallback; no rollback is announced
as success; attention rows cannot point at a superseded revision; and a native
notification can always be traced to one committed durable event. Events contain
no credentials or unrestricted provider payloads and can never trigger a mutation
by themselves. The fallback poll interval is bounded and remains configurable for
fault injection.

### Milestone 7 — Versioned provider compatibility packs

**Engineering status (2026-09-25): ordinary and fresh final review plus integrated
verification passed; the engineering slice is locally packaged and complete.** Native six-role
qualification remains deferred. The initial Claude pack has no production selector;
synthetic fixtures do not establish usable native Claude support. The approved
[M7 plan](MILESTONE_7_PLAN.md) narrows the candidate description below.

**Candidate priority: medium.**

LLMRelay already records the adapter name and hash and requires exact local
capability evidence. Package that compatibility knowledge so a provider change
can be reviewed and tested without scattering version assumptions through the
runtime.

Planned scope:

- Define a bundled, versioned compatibility manifest for each
  supported provider. It may describe executable identity and version range,
  launch and resume syntax, model-discovery behavior, supported hooks/events,
  credential type, sandbox and permission requirements, and fixture expectations.
  Bind the effective selected contract hash in the existing capability key; keep
  descriptive pack revision and full bundle hash separate as provenance. The pack
  ships in the executable and needs no independent update or signing pipeline.
- Bind each active role profile and capability proof to the effective contract in
  addition to the executable and adapter hashes. Changed compatibility policy
  invalidates only the evidence whose effective contract changed.
- Add a compatibility explanation showing the matched pack, failed predicate,
  required evidence, and safe corrective action. Unknown versions remain
  unsupported until explicitly validated.
- Keep manifests shipped with an LLMRelay release. Do not remotely update launch
  flags, sandbox policy, approval behavior, hooks, or credentials independently
  of a reviewed application release.
- Use data-driven detection only for descriptive presentation where false
  recognition has no authority. Herdr's agent detection manifests are a useful
  maintainability pattern, but their remote runtime refresh is specifically
  rejected. Detection data must not become proof that a provider is safe to
  launch or resume.

This deliberately differs from Jinn's dynamic, unpinned engine discovery. Jinn
asks installed CLIs what they support and exposes new models quickly; LLMRelay's
workflow authority and native-session reuse require a narrower, proven
compatibility envelope.

**Acceptance:** a supported exact provider configuration selects one immutable
pack and passes the existing profile capability probes plus the applicable
six-role requalification; an unknown or changed configuration fails with a
specific reason; stale evidence cannot satisfy the new pack; and no compatibility
update can silently weaken sandbox or approval policy.

### Milestone 8 — Stable local client protocol and verified distribution

M8A source approved September 25: generation-1 local browser/CLI/attachment
negotiation, authenticated compatibility refusals, and existing authority checks.
The stable matrix passed 22 DOM, 65 library, 91 contract, and 2 runtime tests.
Local packaging is complete. M8B signing/update activation remains post-v1.

**Candidate priority: medium, naturally aligned with the background service and
menu-bar work.**

Once the engine can outlive its current browser or terminal, formalize the local
client boundary before adding more clients.

Planned scope:

- Publish a versioned local protocol for the browser, CLI, terminal attachment,
  and future menu-bar client. Negotiate protocol generation and capabilities at
  connection time; reject an incompatible mutating client with upgrade guidance
  instead of attempting a best-effort command.
- Keep authentication, operation IDs, optimistic revisions, idempotency, and
  authority checks on every mutating route. Capability discovery describes what
  the service supports; it does not grant permission to use it.
- Add signed release metadata with version, platform, protocol generation, asset
  size, and a mandatory cryptographic digest for every channel. Verify the
  signature against a key embedded in the running application. Download into a
  temporary file, verify before activation, preserve the current executable as a
  rollback target, and switch atomically where the platform allows it.
- Before updating a running service, show whether the update requires a restart,
  reuse the restart-readiness preview, drain under the existing ownership rules,
  and verify the new service's version and protocol before considering the update
  complete. An ambiguous handoff stops with recovery guidance rather than killing
  or retrying blindly.
- Keep package-manager installs and self-managed installs distinct. Never rewrite
  a package-manager-owned executable behind the package manager. Coordinate with
  the LaunchAgent so it cannot relaunch the old binary during the activation
  window.

Herdr's protocol-generation checks, capability negotiation, checksummed release
manifests, temporary downloads, restart planning, and ambiguous-handoff recovery
are useful references. Its stable channel requires a digest, but preview can omit
one; its manifest is not signature-verified; and its atomic rename keeps no
rollback copy. Mandatory all-channel digests, signed metadata, and rollback are
LLMRelay requirements rather than behavior inherited from Herdr. Herdr's live
terminal-server handoff is not an immediate LLMRelay requirement and must not
bypass LLMRelay's provider-process ownership.

**Acceptance:** old and new clients receive deterministic compatibility results;
corrupt or incomplete artifacts never activate; a failed update leaves a known
runnable version or an explicit recovery state; and update completion proves the
running service version and protocol rather than assuming that a file rename was
enough.

### Milestone 9 — Reusable task recipes without an organization model

**Status: engineering implementation verified September 25; local packaging complete.**

Delivered immutable project profile sets and task recipes, draft-only creation, and
explicitly enabled daily/weekly UTC foreground intake. Ordinary and fresh final
reviews approved after one final repair. Verification: 29 DOM, 67 library, 100
contract and 2 runtime tests; typecheck, frontend build, Rust formatting/check pass.
Live-service, provider and hands-on acceptance remain separate. The original
research scope below is historical; [the approved plan](MILESTONE_9_PLAN.md) defines
the delivered boundary.

Jinn's company metaphor is not LLMRelay's paradigm. The reusable parts can be
expressed in project and task terms without employees, departments, ranks, a COO,
or an editable reporting hierarchy.

Possible scope:

- Save a **task recipe** containing a task description template, acceptance
  criteria template, selected TRIP workflow version, role-profile set, required
  checks, and default priority. Creating from a recipe produces an ordinary draft
  task that the user can inspect before making it Ready.
- Add small reusable role-profile sets at project scope. These are validated
  execution configurations, not personas or a reporting structure, and they do
  not supersede per-task revisions or capability proofs.
- Add optional local scheduled intake only for recipes the user explicitly
  enables. A schedule should create a visible draft or Ready candidate according
  to its saved policy; it must never imply implementation authorization, bypass
  project queue pause, set the current `run_next_requested` override, or accept
  completed work. Persist an idempotency key derived from the schedule identity
  and exact scheduled fire time so a repeated fire cannot create a second task.
- Preserve structured phase completion contracts and evidence. A role finishing
  a turn is not the same as completing its phase; a phase report must satisfy the
  existing schema and review gates. Expose the evidence in the task activity
  history rather than adding a general company chat layer.
- Consider conditional or parallel recipe steps only when a concrete TRIP use
  case cannot be represented by the current reviewed workflow. Do not build a
  general workflow-canvas product speculatively.

Jinn shows the value of durable task ownership, structured completion contracts,
reusable procedures, schedules, and restart-safe callbacks. LLMRelay already has
task ownership, role reports, a fixed reviewed TRIP workflow, and human final
acceptance; the candidate feature is reuse and intake, not a second orchestration
engine.

**Acceptance:** recipes create reviewable ordinary tasks; profile sets retain all
capability and revision checks; schedules are locally visible, pausable, and
idempotent; and no recipe, schedule, role report, or delegated callback can grant
approval or final acceptance.

### Ideas intentionally not adopted

- Do not embed Herdr or Jinn as LLMRelay's host, scheduler, database, workflow
  authority, provider supervisor, or terminal-input owner. Herdr remains a
  presentation/runtime reference; Jinn remains an orchestration reference.
- Do not add Jinn's company, organization, department, rank, manager, COO, or
  connector paradigm. LLMRelay is organized around projects, tasks, attempts,
  explicit TRIP roles, and the user as the final authority.
- Do not infer workflow state from terminal contents, pane titles, or screen
  heuristics. Such signals may help presentation but cannot satisfy a gate.
- Do not use Jinn's current Codex launch behavior: the inspected runner passes
  `--dangerously-bypass-approvals-and-sandbox`. LLMRelay retains its validated
  provider policy, native approvals, scoped permissions, and exact capability
  evidence.
- Do not copy Jinn's current plugin security boundary. Its v1 host gate exposes a
  typed verb list but grants every listed verb, and plugins execute with the
  application's authority. A future LLMRelay extension system would need explicit
  per-extension grants, isolation, provenance, revocation, and audit before it
  could be considered.
- Do not add a general plugin marketplace, remote multi-host orchestration,
  messaging connectors, arbitrary pane input, remote policy updates, a broad
  workflow editor, or live executable handoff until a concrete LLMRelay use case
  and authority model justify them.

### Independent Fable-medium review disposition

The September 20 independent read-only review found the overall milestone order
sound and produced source-specific corrections. The document now incorporates
its confirmed recommendations to:

- confine migrations to the locked service or a locked offline command;
- distinguish Jinn's best-effort pre-migration copies from its verified operator
  backups;
- preserve the current no-expiry contract for launch permits and process claims;
- describe the existing auto-resume and cross-project Attention behavior before
  naming the remaining gaps;
- treat Herdr's remote detection refresh, optional preview digest, unsigned
  manifest, and lack of rollback as boundaries rather than guarantees;
- make restart preview read-only, define stable recovery evidence, protect the
  newest verified backup, and bind compatibility packs to existing capability
  proofs; and
- defer completion batching and scheduled intake until there is measured demand
  and the preceding safety milestones are in place.

One recommendation remains an explicit product decision: whether to require the
milestone 4 migration-safety subset before starting the selected LaunchAgent
milestone. The review does not grant that decision or authorize implementation.

### September 30 Jinn runtime reference inventory

This follow-up records the September 30 inventory walkthrough alongside the
September 20 research. It uses the same pinned Jinn revision
`62f026aa6cb740c807ac74c350a356538ba6eea8`; it does not claim to describe the
latest release. The findings below come from source inspection, without running
Jinn or establishing end-to-end parity. Recording them does not select, schedule,
or authorize additional implementation.

The inventory focused on Claude and Codex interactive execution, permission
waits, turn completion, input acknowledgement, and causal regression coverage.
LLMRelay retains its own task ledger, scheduler, provider supervision, review
gates, and human acceptance.

| Area | Observed Jinn behavior at the pinned revision | LLMRelay disposition |
| --- | --- | --- |
| Claude permission waits | Recognizes the structured `Notification` event with `notification_type: permission_prompt`. | Useful detection reference. Validate the exact admitted provider version and event contract; a generic notification may support an attention notice and **Open agent output**, but cannot establish a reviewable operation, approval, resolution, or execution. |
| Native safety-dialog answers | Parses terminal questions, numbered options, and cursor position, then chooses an affirmative option and sends arrow keys and Enter. Claude safety-prompt auto-answering defaults to enabled unless explicitly configured off. | Intentional non-adoption. Preserve explicit human decisions through supported routes. Jinn's unattended responsiveness must be assessed with its default auto-answering behavior in mind. |
| Codex approvals | Adds `--dangerously-bypass-approvals-and-sandbox` to fresh and resumed execution. | Intentional non-adoption. This is not evidence that Jinn implements an equivalent human permission inbox. Retain LLMRelay's admitted native policy, isolation, and exact capability evidence. |
| Turn failures and process lifetime | Its turn resolver handles structured `StopFailure`, later completion evidence, and activity/grace behavior independently of a surviving PTY process. | Useful semantic distinction. Preserve exact native identity, invocation, ordering, audit, and acceptance fences. Do not transplant its timer/fallback machinery or infer completed work from an open or idle process. |
| Input submission | Tracks submission acknowledgement and may resend Enter when acknowledgement remains unconfirmed, with a busy-work guard. | Preserve the distinction between transport delivery and native acceptance. Do not adopt automatic retries for ambiguous input or treat unrelated in-turn activity as acceptance of the exact submitted turn. |
| Permission regression coverage | Exercises permission hook → terminal viewport interpretation → keystrokes with a fake PTY and a headless terminal. | Useful causal-test reference for the guarantee Jinn tests. It does not prove LLMRelay's live behavior or authorize a terminal parser; exercise LLMRelay's actual hook, store, projection, decision, and native-resolution path. |

Pinned primary-source references:

- [Claude interactive engine: notification detection, turn resolver, default
  safety auto-answer, and input acknowledgement](https://github.com/hristo2612/jinn/blob/62f026aa6cb740c807ac74c350a356538ba6eea8/packages/jinn/src/engines/claude-interactive.ts).
- [Claude permission-dialog parser and response helper](https://github.com/hristo2612/jinn/blob/62f026aa6cb740c807ac74c350a356538ba6eea8/packages/jinn/src/engines/claude-permission-prompt.ts).
- [Codex fresh and retained-session launcher](https://github.com/hristo2612/jinn/blob/62f026aa6cb740c807ac74c350a356538ba6eea8/packages/jinn/src/engines/codex.ts).
- [Codex interactive runner](https://github.com/hristo2612/jinn/blob/62f026aa6cb740c807ac74c350a356538ba6eea8/packages/jinn/src/engines/codex-interactive.ts)
  and [rollout reader](https://github.com/hristo2612/jinn/blob/62f026aa6cb740c807ac74c350a356538ba6eea8/packages/jinn/src/engines/codex-rollout.ts).
- [Claude interactive permission regression tests](https://github.com/hristo2612/jinn/blob/62f026aa6cb740c807ac74c350a356538ba6eea8/packages/jinn/src/engines/__tests__/claude-interactive-permission-prompt.test.ts).

The [official Claude hook reference](https://code.claude.com/docs/en/hooks#notification),
inspected during the September 30 walkthrough, supplied a narrower correction
for network prompts: ordinary `PermissionRequest` hooks do not cover sandbox
network approval, while terminal `Notification/permission_prompt` support for
those requests is documented from Claude Code **2.1.246** onward. The notification
is delayed by roughly six seconds and can be deferred by typing. This is a
version-qualified detection candidate; it does not establish support in the
older admitted **2.1.220** tuple, nor does a newer engineering CLI automatically
change the application's admitted provider contract.

Remaining evidence boundaries from this walkthrough:

- **Plugin recommendations and onboarding:** the inspected subset did not
  establish a general supported structured signal. Keep those cases separate
  from a generic permission notification.
- **Reconnect and duplicate notifications:** Jinn parity was not established.
  Verify LLMRelay's accepted snapshots, stable request/incarnation identity,
  actionability, and duplicate handling on its own running application.
- **Native resolution:** generic later activity cannot resolve a particular
  pending approval. Require an exact correlated request/tool/native event;
  keep the app decision, response reservation, response delivery, native
  resolution, and observed execution separate.
- **Resume recovery and completion cleanup:** reference behavior cannot replace
  LLMRelay's transactional authority, retained audit history, review budgets,
  exact accepted-turn evidence, or independent pause/recovery/rework holds.

The broader engine-path inventory also identifies startup, background activity,
late recovery, races, compaction, transcript recovery, process exit, PTY
lifecycle, and pooled/retry tests as possible future reading. Finding those paths
is not a review of their behavior or confirmation of a current LLMRelay defect.
The delivered comparison and supported/unsupported signal dispositions belong
in the [permission and provider signal matrix](PERMISSION_SIGNAL_MATRIX.md);
that matrix must continue to distinguish source/test evidence from actual live
provider and dashboard verification.

### September 30 hardening-first Jinn comparison (latest release)

**Status: research-backed candidates agreed by two independent reviewers; not
selected, scheduled, or authorized for implementation.**

This pass compared the latest Jinn release, `main` at
`3ae6465715b6195db057d4c23156b696c71dc179` (v0.33.4, September 27, 2026), with
the current LLMRelay tree. It covered Jinn's engines, sessions, work items,
gateway, workflows and web dashboard, and its changelog's fix entries as a
catalogue of real bug classes. It was then checked against the open rows in the
AJ-1701EE4D issue reconciliation. The user asked for hardening before new
features because the application is currently unstable.

Two reviewers worked read-only: Claude Opus 5.5 and an independent Claude Fable
5.1 (high effort) session. Fable explored first without seeing the Opus list,
then critiqued it. They reached consensus after one review round. Every LLMRelay
claim below was verified in source; line numbers refer to the tree at the time
of review.

Between the pinned `62f026aa` revision used above and `3ae6465`, Jinn's runtime
source changed in one line: `shared/engine-failure.ts` now classifies Codex's
"selected model is at capacity" error as a provider outage. That is the open
"Codex model capacity error remains busy" row. The pinned analyses therefore
describe current Jinn source.

#### Corrections to the September 30 runtime inventory

- The admitted Claude Code version is **2.1.283**
  (`resources/provider-compatibility/claude.json`), not 2.1.220; 2.1.220 is the
  version Jinn characterized. Because 2.1.283 is above 2.1.246, the documented
  network-prompt `Notification` is in scope for the admitted version, subject
  to live verification.
- The inventory rejected input resubmission but missed its root cause. Claude
  Code's terminal treats a leading `/`, `@` or `!` in pasted text as a command,
  mention or shell mode. It also auto-attaches a bare image path and discards
  keystrokes, including the submitting Enter, while it encodes the image.
- It did not check the current tree for the `StopFailure` readiness wedge (H2).

#### Agreed hardening list

**P0**

| # | Item | LLMRelay evidence | Jinn reference |
| --- | --- | --- | --- |
| H1 | Contain coordinator tick failures to the attempt or session that caused them and record a durable hold for that subject. Replace substring matching on formatted errors with typed error classification. Preserve one committed action per tick. | `src/coordinator.rs:15-27, 148-180` return out of `tick` on any unclassified attempt error, skipping later attempts and the scheduler claim every second. `src/supervisor.rs:970-976` fails reconciliation for every handle when process inventory fails. | `work-items/recovery-controller.ts:127-150`, `gateway/status-reconciler.ts:96-108` |
| H2 | After a trusted `StopFailure` for the current accepted turn, apply the existing safe-idle-boundary predicate and record a distinct idle-after-failure readiness. Guidance delivery, manager proposals and review eligibility accept it; any later trusted hook supersedes it. No timer, fallback or fabricated result. | `src/store.rs:1984-1992` maps `StopFailure` to `busy`. Guidance (`src/roles.rs:888-890`), manager transition proposals (`src/coordinator.rs:3897-3925`) and review eligibility (`src/review.rs:169`) require `idle_candidate`. Claude emits no `Stop` after non-retrying failures, so the only exit is kill and resume. | `engines/claude-interactive.ts:455-617, 1460-1464` |

**P1**

| # | Item | LLMRelay evidence | Jinn reference |
| --- | --- | --- | --- |
| H3 | Classify-only liveness and anomaly sweep that writes one durable attention item per entity and kind and never settles, stops or retries. Kinds: process live without an accepted turn; stalled turn (elapsed and quiet, confirmed on two sweeps; a pending permission prompt is not work); attempt without a live session; guidance `written_awaiting_submit` beyond a bound; permission pending on an exited session; `resume_failed` despite a later accepted turn; `busy` readiness with an exited process. | Session `running` means process alive (`src/store.rs:3138-3152`); `TaskProgress.activity` is presentation only (`src/workflow.rs:6567-6571`). | `work-items/anomaly-detect.ts:10-114`, `gateway/status-reconciler.ts:7-108`, `engines/claude-interactive.ts:641-676` |
| H4 | Failure-kind holds. Rate limit or overload: clock hold until the provider-stated reset, or a bounded default. Authentication, billing or account state: human hold naming the required action. No model fallback; a human resume overrides. | `NativeTurnFailureKind` (`src/domain.rs:1686-1719`) has no consumer in dispatch, auto-resume (`src/operations.rs:707-775`) or guidance/rework retry (`src/coordinator.rs:3927, 4299`). | `work-items/respawn-guards.ts:93-120`, `work-items/availability-resume.ts`, `shared/engine-failure.ts`, `work-items/stop-cause.ts` |
| H5 | Neutralize paste triggers (leading `/`, `@`, `!` and bare image paths) in the guidance body **before it is stored**, so the submit match still compares like with like. | Raw body pasted, then one Enter after 100 ms (`src/roles.rs:1036-1045`); stored at `src/workflow.rs:2906`; submit match `src/store.rs:1951-1955`. | `shared/skill-commands.ts:30-37`, `engines/claude-interactive.ts:789-835` |
| H6 | Durable "input written, not accepted" fact per resume invocation, surfaced after a bound. Report only; never resend input. | Acceptance is the trusted `UserPromptSubmit` (`src/domain.rs:1620-1629`); the keyboard lease is released immediately after the Enter (`src/roles.rs:1047-1049`). | `engines/claude-interactive.ts:679-683, 767-787` (report-only half) |
| H7 | Per-instance session cookie name. | Fixed `agenticjira_session` (`src/server.rs:743, 1411`). Cookies ignore port, so isolated verification services on 127.0.0.1 sign the main dashboard out. | `gateway/auth.ts:285-312` |
| H8 | React error boundary around the dashboard shell. | None in `frontend/src`; one render error unmounts the only operator surface. | Route-failure recovery, `CHANGELOG.md` 0.23.2 and 0.33.2 |
| H9 | Fake-provider PTY regression harness: drops the first Enter, emits `StopFailure` without `Stop`, never stops, emits `permission_prompt`. Also confirm whether Claude 2.1.283 still drops the Enter on an image path. | Current fixtures are synthetic compatibility bundles and hook CLI calls (`tests/contracts.rs`, `tests/runtime.rs`). | `engines/__tests__/` permission-prompt, grace and late-recovery tests |
| H10 | Restart resume states that the host, not the user, interrupted the session and instructs the agent to re-verify any in-flight command before repeating it. | Role restart resume re-sends the persisted original invocation prompt (`src/operations.rs` role resume path) with no restart wording. | `sessions/restart-resume.ts:118-124` |

**P2**

- **H11.** Delay the `StopFailure` attention item for kinds the CLI normally
  retries (`server_error`, `invalid_request`, `unknown`), so it does not flap
  before a superseding `Stop` (`src/workflow.rs:5345-5350, 7240-7280`; Jinn
  `engines/claude-interactive.ts:455-459, 509-513`).
- **H12.** Retire a `permission_prompt` notice only on the exact correlated
  native resolution from migration 033 (`src/workflow.rs:5448-5451`).
- **H13.** Set `busy_timeout` before WAL and DDL on the fresh-database open path
  (`src/store.rs:900-908`).
- **H14.** Audit `consume_blocked_result`, `eligible_progress_result` and
  `eligible_review_result` (`src/coordinator.rs`) for ordering against the
  latest human control. Add a causal floor only where missing; most
  human-decision precedence already exists.
- **H15.** Bounded counter for recurring same-kind blocked or needs-input
  reports from the same role, routing to attention (Jinn `work-items/blocks.ts`).
- **H16.** Classify the Codex capacity error only if a live check shows Codex
  session history records it as a structured event; attention text only. Codex
  registers no failure hook (`src/providers/codex.rs:163-173`).

#### User decision required

- **Fail-closed restore point and free-space preflight before the serve-path
  schema upgrade.** `upgrade_supported_service_schema` (`src/store.rs:8955-8972`)
  migrates the live database from schema 31 or 32 without a restore point;
  `require_capacity` is used only by backup and restore (`src/database.rs`).
  [Next milestone plan](NEXT_MILESTONE_PLAN.md) records that automatic
  pre-migration backup orchestration was removed from scope. Both reviewers
  recommend approving it: it reuses the existing verified online backup
  (`src/database.rs:203-270`) and, unlike Jinn's warn-only copy
  (`shared/db.ts:29-96`), must fail closed.

#### Documentation corrections

- Sept 30 runtime inventory: admitted Claude Code version 2.1.220 → 2.1.283.
- [Signal matrix](PERMISSION_SIGNAL_MATRIX.md): the Jinn link points at
  `jinn-network/jinn` (404); use `hristo2612/jinn`.
- [Operations](OPERATIONS.md) documents a 31 → 32 upgrade; the code upgrades
  31 or 32 to 33.
- [README](../README.md) names Codex 0.155.1; the matrix names 0.157.1.
- Milestone 4 "complete" wording should state that automatic pre-migration
  restore points were removed by the scope correction.

#### Features after hardening

- Opt-in, audited reminder (at most two, never for the final verifier) when a
  role's turn ends without a role report (Jinn `sessions/stop-nudge.ts`).
- Scheduled in-service verified backup; restore remains offline CLI-only.
- Usage and limit display plus per-turn token accounting, presentation only.
- Per-attempt handoff fields in role reports (changed files, verification,
  retry notes, residual risk) fed into rework prompts (Jinn `work-items/runs.ts`).
- Edit or cancel queued guidance.
- Task comments and a unified activity stream.
- Drafts that survive a reload without erasing newer edits.
- Command-palette search over tasks with FTS5, treating user input as literal
  phrases.

#### Already present; do not re-propose

Child environment allowlist (`src/providers/mod.rs:48-87`); diagnostic log
rotation and retention (`src/diagnostics.rs:118-155`); transcript bounds;
owner-only file modes; exact Host/Origin checks and constant-time token
comparison (`src/server.rs:1368-1402`, `src/auth.rs:20`); loopback-only bind;
embedded dashboard assets; control-message and input-write size caps;
verified backup and restore; post-commit revision notification; operation
idempotency receipts; atomic scheduler claims; exactly-once restart admission;
interrupt deadlines with exact-process verification; refusal of exact resume
when native history is missing; long-poll reconnect backoff.

#### Additional ideas not adopted

These extend [Ideas intentionally not adopted](#ideas-intentionally-not-adopted):
resending Enter until acknowledged; auto-settling stalled turns or recovering
completion text from transcripts; model or engine fallback chains; wall-clock
expiring claim leases; automatic recovery mode; deriving task status from
session liveness; warn-only pre-migration copies; a detached restart helper;
inferring Codex completion from a quiet session-history tail; approval gates
that any agent holding the service credential can pass.

## Related documentation

- [Current usage](../README.md)
- [Operations, shutdown, and recovery](OPERATIONS.md)
- [Project setup](PROJECT_SETUP.md)
- [Research and implementation history](../RESEARCH_AND_PLAN.md)
