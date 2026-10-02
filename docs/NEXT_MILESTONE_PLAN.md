# Herdr/Jinn roadmap and next implementation task

Planning baseline: `f8c7e29` (initial LLMRelay implementation).
Status: M4A, M5, approved M4B and the M6 live-state/attention slice completed;
M7 engineering implementation passed ordinary and fresh final review plus integrated
verification; its engineering slice is locally packaged. M8A passed ordinary and
fresh final review and the integrated verification matrix; local packaging is complete. M9 passed ordinary and fresh final review; local packaging is complete.
M6 native notifications are being addressed in ledger task AJ-1701EE4D,
alongside exact resume/status reconciliation and review access/theme consistency.
This consolidated task is in engineering review; browser/provider verification
and the final completion gate remain pending. It does not change the historical
managed TaskFocus attempt's acceptance state.
Fresh-install scope supersedes historical migration work. On 2026-09-23 the user
authorized autonomous completion of finalized in-scope plans and necessary review
and test increases, then deferred milestones and checks requiring their intervention.

The user selected the Herdr/Jinn milestone work as the next product scope and
will defer personal hands-on acceptance until the selected features are present.
Engineering verification continues at each checkpoint. The earlier implementation
is a verified foundation, not acceptance of the complete desired product.

## Current engineering task — October 1 hardening

The user selected the H1–H16 hardening findings in
[the Jinn comparison](MILESTONES.md#september-30-hardening-first-jinn-comparison-latest-release)
and approved the reviewed implementation plan. The approved hardening scope is
now implemented and verified: independent code review and fresh final verification
approved it, and all 343 automated tests passed along with the build checks. H2
is the accepted follow-up below; evidence-gated dispositions and verification
limits remain recorded in MILESTONES. No release is included.
The older milestone proposals below retain their historical scope and do not
authorize additional work.

Existing LLMRelay data may be purged under the user's explicit instruction;
backup and restoration of that data are not prerequisites for this task. This
does not remove the existing offline backup/restore features or authorize
unrelated deletion. Automatic pre-migration restore-point orchestration remains
outside the approved hardening scope.

### Accepted follow-up: H2 failed-turn automatic readiness

On October 1, the user accepted completing the current hardening task with
conservative failed-turn recovery preserved. Automatic readiness remains
unimplemented and is a separate follow-up, not a completion blocker for this task.
Before enabling it, establish trustworthy evidence from the application's
admitted CLI version that the exact failed turn has ended and no background work
remains. Readiness must remain separate from authority to start another call;
authentication, billing, rate-limit and unknown failures must not trigger automatic
new work. Process liveness, elapsed time, normal completion observations and a
different CLI version do not establish that evidence. Further investigation and
implementation need their own scoped plan; no compatibility upgrade or new
provider probe is authorized by this disposition.

## Scope and delivery policy

### Initial v1 scope decision — September 23, 2026

The user explicitly removed deferred milestones from initial-v1 completion.
Initial v1 targets the delivered core, M4A/M4B, M5, the M6 dashboard live-state and
attention slice, M7 engineering compatibility contracts, M8A client protocol, and
M9 recipes/profile sets/scheduled intake while the foreground service is running.
M2 LaunchAgent, M3 menu-bar companion, M8B signed distribution/update activation,
and M6 native notifications are post-v1 work and do not block that engineering
finish. Historical migration support remains excluded. M7's engineering slice is complete.

This changes the feature boundary, not the truthfulness of verification: native
qualification remains separately pending where deferred, and no unsupported
provider may be advertised as usable. Final hands-on acceptance remains pending.
The capability path-normalization defect recorded under M5 is resolved and passed
independent ordinary and fresh final review on September 25. Consolidated automated
verification passed 29 DOM, 67 library, 103 contract and 2 runtime tests plus
formatting, typechecking and builds. All agreed initial-v1 engineering work is
complete; see [the delivery record](V1_ENGINEERING_DELIVERY.md) for package and
remaining acceptance boundaries.
No live installation, signing, publication or service restart is implied.

Cover the adopted features in milestones 4–9 and the already selected background
service/menu-bar direction in milestones 2–3. This does not adopt every feature
of either upstream product: the exclusions in MILESTONES.md remain in force.
LLMRelay retains its engine, SQLite state, TRIP gates, provider supervision,
approval authority, and cmux presentation model. No provider API dependency.

Use one product roadmap with small implementation checkpoints. Each checkpoint
has a reviewed file-level plan, implementation, focused regression checks,
independent review, and a checkpoint commit. Run aggregate verification at real
integration boundaries. Do not reopen closed areas without a concrete regression
or changed dependency. No user click-testing is required between checkpoints.
Final user acceptance remains explicitly pending; absence of testing is not approval.

## Recommended sequence

| Step | Milestone mapping | Deliverable and exit evidence |
| --- | --- | --- |
| 1 | 4A | Explicit database-open authority, verified current-schema restore points, offline recovery; backup/restore interruption tests pass. |
| 2 | 5 | One authoritative explanation for waits, exact next actions, read-only restart preview, bounded bulk resume and retry guards; explanation matches the actual coordinator decision. |
| 3 | 4B | Complete the crash-boundary matrix across worktrees, provider launch, permissions, reports, reviews, cmux, drain and restart; each interruption has a deterministic recovery outcome without duplicate effects. |
| 4 | 6 | Revisioned committed events using existing Notify plus long polling, reconnect/gap reconciliation and polling fallback; grouped attention rail with exact links and optional deduplicated native notifications. |
| 5 | 7 | Bundled immutable provider compatibility packs, proof binding and actionable failure explanations; changed effective contracts invalidate the correct evidence. |
| 6 | 8A | Versioned local client protocol and capability negotiation for browser, CLI and attachment clients; incompatible mutating clients are rejected explicitly. |
| 7 | 2 | Per-user LaunchAgent, stable installation, intentional-stop semantics, optional login startup, clean dashboard authentication and graceful recovery; closing Terminal leaves the service available. |
| 8 | 3 | Menu-bar companion for the same service; quitting the companion does not stop tasks. |
| 9 | 8B | Signed release metadata, mandatory digests, verified staging, rollback and restart-aware update activation; package-manager ownership preserved. |
| 10 | 9 | Reviewable task recipes, reusable project profile sets and opt-in idempotent scheduled intake; no schedule grants implementation approval or human acceptance. |

This ordering deliberately puts storage safety and restart explanation before
automatic service restarts, and a client protocol before another client.
The LaunchAgent must also resolve the current cmux-startup provenance requirement:
a background service cannot simply pretend it was launched inside cmux. Its plan
must specify authenticated local cmux discovery/connection without weakening
process or terminal ownership.

Completion batching remains a measured optimization: preserve immutable receipt
order and user-action fences; add batching only if actual backlog measurements
justify it. Conditional/parallel recipe extensions require a concrete TRIP use
case. Neither means adopting Jinn's organization model or a workflow canvas.
Track these conditional decisions explicitly rather than silently marking them done.

### Unattended sequence and deferred intervention (2026-09-23)

The current execution order is **finish M4B → M6 → M7 → M8A → M9**.
Finalize and independently review each in-scope plan under the user's standing
authorization; routine plan confirmation and review/test budget increases do not
require another user prompt. Preserve engineering gates and record any increases.

**M2 and M3 are deferred in full** until the user returns to them. M8B may receive
independent planning, implementation, and fixture verification where those do not
depend on M2, but real signing, publication, installed-service activation, and
LaunchAgent-dependent update acceptance are deferred. Do not claim M8B complete
without its dependency and acceptance evidence.

Defer other steps that need new user action: authentication/account interaction,
unavailable exact-role resolution, extra paid product qualification outside existing
authority, OS/UI permission decisions, live installation/restart, and hands-on
acceptance. Continue independent authorized work and list deferred checks precisely.
For M7, compatibility implementation and fixture coverage do not substitute for
deferred native requalification. For M6, native notification permission checks can
remain deferred while dashboard/event behavior is verified. M9 scheduled intake
can be verified while the foreground service runs; execution after Terminal closes
depends on deferred M2. Do not widen host access or silently substitute models.

## Next task: milestone 4A — protect durable state

### Why first

`Store::open` in `src/store.rs` enables WAL/FULL durability and runs all migrations.
`serve` in `src/server.rs` holds the instance lock before calling it, but
`src/export.rs` also calls it for diagnostics export. The code therefore does not
yet express the roadmap's rule that observation cannot grant migration authority.
There are 28 migration steps; their existing per-step transactions are preserved.
The existing rusqlite dependency has bundled SQLite but not its backup feature.

### Proposed user-visible behavior

Command names below are proposals, not current usage:

| Command | Behavior |
| --- | --- |
| `llmrelay database inspect` | Report schema/application compatibility and database metadata without migration. |
| `llmrelay database check` | Run read-only SQLite integrity and foreign-key checks; report stable failure categories. |
| `llmrelay database backup` | Offline, lock-protected consistent database snapshot with verified manifest. |
| `llmrelay database verify <backup>` | Verify manifest, hashes, SQLite integrity and schema compatibility without restoring. |
| `llmrelay database restore <backup>` | Offline replacement only, preserve displaced state, establish a durable post-restore recovery hold. |

The current milestone targets fresh installations and current-schema databases.
Historical/intermediate-schema compatibility tests, legacy upgrade repairs and
automatic pre-migration backup orchestration are removed by the user's scope
correction. Preserve existing migrations for fresh initialization without
undertaking a migration-system rewrite. Reject unsupported existing schemas
clearly. Future upgrade support receives its own reviewed plan.

### Revised implementation units

1. **Explicit open authority.** Read-only inspection/export uses SQLite read-only
   flags and query-only mode; it neither creates nor migrates a database. Export
   requires the current supported schema. Reuse the stable instance lock inode
   for initialization, backup, restore and hold release. Existing initialization
   remains explicit; no generic observation path may invoke it.
2. **Verified restore points.** Enable the pinned rusqlite backup feature. Use
   SQLite backup so committed WAL data is included. Validate capacity, integrity,
   schema, private ownership and path boundaries. Publish a versioned manifest
   with length/SHA-256 using fsynced staging, same-filesystem rename and parent
   sync. Hash verification proves integrity, not authenticity against an actor
   who can modify both manifest and database. No signing framework is added.
3. **Interruption-safe offline replacement.** Verify the selected restore point
   before mutation. Stage beside live state, journal operation/digest and exact
   per-file moves outside the replaced DB, preserve displaced DB/WAL/SHM in
   quarantine, and never combine a new main file with old sidecars. Journal
   phases distinguish verified, displacing, installing, hold-pending and completed;
   ambiguous boundaries fail closed with an exact recovery action. No quarantine
   deletion is authorized. Before displacement, preserve known process/check,
   claim, worktree and cmux bindings for reconciliation even if absent in the
   selected older snapshot. If the displaced database opens but inventory capture
   fails, stop before displacement. If it is unreadable, persist
   `inventory_unavailable` and a valid current OS boot identity in the journal
   before any displacement; unavailable boot identity refuses replacement.
4. **Durable restore fencing.** Establish a restore hold before any listeners or
   coordinator can act and reapply it on every boot. Prefer an existing nullable
   recovery record only after checking every consumer; a small dedicated current
   schema field is the fallback, not historical compatibility work. Revoke old
   role credentials, permission/check rules including project rules, pending
   deliveries, input leases and launch permits; disable auto-resume and desired
   running state. Cover direct role authentication, permission decisions, selected
   checks, guidance delivery, scheduler claims and launch/resume entrypoints—not
   only the coordinator's in-memory dispatch flag.
5. **Explicit recovery exit (approved).** Add offline
   `llmrelay database release-hold` under the same exclusive instance lock. It
   requires completed reconciliation and verified resolution of process and claim
   uncertainty, and leaves affected projects/tasks paused for explicit normal
   recovery. It never launches an agent, revives a credential/rule, or enables
   automatic resume. If displaced inventory is unavailable, acknowledgment alone
   cannot substitute for positive evidence; report the concrete missing evidence
   and recovery prerequisite. For `inventory_unavailable`, release requires the
   existing `boot_identity_proves_reboot` check to prove a different valid OS boot
   identity from the one recorded before displacement, plus the remaining
   reconciliation checks. Same-boot release and acknowledgment-only release fail.
   No automatic reboot is performed. A subsequent service start reads the durable
   result. Listeners may bind under the hold for observation and safe recovery;
   every execution entrypoint remains fenced.

This restores the database only. Repository files, native CLI history, worktrees,
transcripts and external effects are not rewound. Missing or changed references
remain recovery prerequisites, never evidence that work is safe to resume.

### Storage and retention proposal

- Default backup directory: a private sibling of the canonical instance root,
  named `<instance-directory>.backups`; configurable via an absolute path.
- Reject destinations inside the live instance tree or registered repositories;
  validate symlinks and ownership. Snapshot files contain sensitive state and
  must have owner-only permissions. Encryption at rest is not claimed.
- Proposed retention: at most 10 verified snapshots, 30 days and 5 GiB, preserving
  the newest verified snapshot even if it alone exceeds the size target. Prune
  only app-owned verified snapshots after a replacement snapshot is published;
  never prune unrelated files, incomplete evidence or displaced live quarantine.
- Quarantine is preserved for deliberate operator cleanup. Quota pressure that
  cannot be resolved within these rules blocks backup with a specific reason.

These storage/retention choices and the restore journal are explicit parts of
the proposed solution for approval, not implied authorization to delete data.

### Ownership and size

Expected owners: `src/store.rs` (open modes/migration entry), `src/server.rs`
(lock lifecycle/startup), `src/export.rs` (read-only open), `src/cli.rs` (commands),
`src/config.rs` (backup paths), `src/recovery.rs` (restore hold),
`src/operations.rs`, `src/permissions.rs`, `src/checks.rs`, `src/roles.rs`,
`src/scheduler.rs` (direct authority fences), `Cargo.toml`
(existing dependency feature), `src/lib.rs`, and one bounded `src/database.rs`
module for lock-protected backup/restore orchestration. Existing tests and owning
docs complete the scope. A migration is conditional on evidence that existing
recovery state cannot represent the post-restore hold; no schema change is assumed.

Planning estimate: approximately 700–1,300 production lines and up to 1,000 test/support
lines, using existing test seams. The user approved this bounded verification scope.
One writer owns storage authority first; do not run overlapping store/recovery
writers. Independent docs work can proceed after the command contract is frozen.
No refactoring the large store module merely to make this milestone look cleaner.

### Verification proposal

Up to eight causal scenarios in the existing harness, parameterized where useful:

1. Read-only export/inspection neither creates nor migrates an old/missing database.
2. Instance-lock contention refuses migration/restore without downstream writes.
3. Backup includes committed WAL data; hash or truncation failures are rejected.
4. Corruption, unsupported schema, insufficient space and failed verification prevent replacement.
5. Fresh initialization and current-schema restore succeed; unsupported existing
   schemas fail without historical migration or implicit alteration.
6. Each backup/restore journal boundary survives interruption with exactly one
   explicit recovery state and preserves the displaced live state.
7. Restoring older rows cannot resume agents, replay permission delivery or grant
   old authority; missing external artifacts remain blocked with guidance.
8. Retention respects all three limits while preserving newest verified backup,
   unrelated files and quarantine.

Use disposable app-owned data only. No live provider sessions are needed for this
storage milestone. Use existing synthetic process/permission/recovery fixtures.
Run focused storage checks during work and one aggregate matrix after review
convergence. No new test framework and no computer-use requirement.

### Exit and next handoff

Complete only when the commands are documented, read-only paths cannot migrate,
a verified current-schema backup/restore rehearsal passes, all named interruption states have
safe actions, independent review approves, and the checkpoint is committed.
Then proceed to milestone 5 planning; personal acceptance remains deferred.
The complete milestone 4 crash matrix is step 3 above, not silently included in
the storage task or dropped from the roadmap.

## Effort and decisions

Rough engineering effort, not a promised autonomous wall-clock ETA: milestone 4A
is 3–6 working days; steps 2–5 together 2–4 weeks; protocol/background/menu-bar
work 2–4 weeks; verified distribution and recipes/schedules 2–4 weeks. Sequential
planning range is roughly 7–13 weeks with meaningful uncertainty. Re-estimate
after 4A using actual completed throughput. Signing credentials, macOS packaging,
provider changes and new external requirements can alter the schedule.

Before 4A implementation, approve the concrete storage/restore/retention design
and verification scope, then run the configured independent TRIP plan review.
No earlier milestone's exhausted counters or test budgets silently carry over or
reset; record a new task allowance explicitly. Later milestones receive their
own concrete plans, while routine fixes stay inside a standing milestone scope.

The full product is complete only after all unconditional roadmap deliverables
are implemented and verified, conditional items have an explicit disposition,
and the user completes final hands-on acceptance. Public release signing and
publication remain separately authorized actions.

## Plan review 1 disposition

Fable/medium returned REQUEST_CHANGES. Accept durable hold, direct-entrypoint
fences, pre-listener revocation, displaced inventory, explicit release, per-file
crash boundaries, integrity/authenticity distinction, and read-only open flags.
Reject human acknowledgment as a substitute for process quiescence. Historical
schema upgrade findings are superseded by the user's fresh-install scope.
Keep eight causal scenarios; the user approved up to 1,000 support lines and the explicit offline release
command. Implementation follows successful retained plan recheck.

## Plan review 2 disposition

All eight initial findings closed. The remaining unreadable-displaced-database
ambiguity is resolved by the user-approved verified-reboot prerequisite above,
using existing OS boot proof rather than accepting missing-process risk. User
approved one additional focused Fable/medium plan recheck (call 3). Cover release
refusal/success and unavailable/same/changed boot within scenario 7, not a new
scenario or harness. All current scope and access restrictions remain intact.

## October 1 deferred hardening continuation

Continue the original proposal in four sequential slices: provider behavior
(H2/H4/H11/H12), durable anomaly/repeated-block attention (H3/H15), existing PTY
fixture extensions and a conditional structured capacity reader (H9/H16), then
verified pre-upgrade restore points with free-space checks and recovery. Do not
count the later feature list as part of these slices.

The independently approved H4 plan delivers durable role/lane failure holds and
an exact human release action. Its implementation passed integration checks,
independent code review and fresh final verification. This bounded app-owned H4
increment is complete. The default cooldown does not implement
provider reset parsing, and release never grants idle readiness. H2/H11/H12 and
the remaining slices stay open. See [Milestones](MILESTONES.md#deferred-hardening-continuation-october-1)
and the current [workflow behavior](WORKFLOWS.md).

H3/H15 now delivers verified classify-only durable observations and bounded
recurring-block attention. Focused checks, independent code review, the full
verification matrix and fresh final verification passed. It uses the existing cycle and attention routes, with no automatic
recovery, retry, readiness or lifecycle change. The approved thresholds and exact
conservative scope are in [Workflows](WORKFLOWS.md) and
[Operations](OPERATIONS.md#durable-attention-observations). H9/H16 and the verified
pre-upgrade restore-point slice remain pending; the earlier removal of automatic
backup orchestration described above is historical, superseded by the user's
selection of that fourth slice.

The H3/H15 review repairs preserve exact invocation targets, positive activity
clearing and independent provider holds, isolate recoverable recurrence errors,
and index report lookups. All verification gates for this increment passed;
the broader deferred hardening program remains incomplete.
