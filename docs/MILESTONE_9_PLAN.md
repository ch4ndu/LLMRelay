# Milestone 9 — Reviewable recipes and foreground scheduled intake

Status: APPROVED by independent plan review G4 on September 25, 2026. Standing
roadmap authorization permits implementation of this reviewed plan. Coverage moderate. M8A is complete and
locally packaged. M2/M3/M8B/native notifications remain outside initial v1.

## Outcome and boundaries

A project can save six-role profile sets and task recipes, create an ordinary
editable draft from an exact recipe revision, and explicitly enable daily/weekly
scheduled creation of such drafts while the foreground service runs. Recipes are
reusable task content/configuration, not another workflow engine. Only the normal
human Make Ready path admits work. Scheduling never grants implementation approval,
run-next override, process authority, selected-check attribution or final acceptance.
No organization hierarchy, workflow canvas, cron/DST engine, background service,
provider integration, historical migration compatibility, or new dependency.
Literal title/description/acceptance templates are copied as editable text; no
interpolation language or executable placeholders. Priority uses existing semantics.

## Persisted revision and authority contract

Use additive migration030 in the existing locked migration chain. Do not edit001–029.
Fresh setup and current29->30 are covered; no historical installation work. Extend
backup current-schema expectation and existing fixture schema assertions mechanically
where required; do not change backup recovery semantics. A cohesive src/recipes.rs
owns recipes/profile sets/schedule transactions and pure UTC recurrence calculation.
Use existing storage/clock/error conventions, no new generic persistence framework.

Persist project_profile_sets and immutable project_profile_set_revisions;
task_recipes and immutable task_recipe_revisions; task_recipe_bindings;
recipe_schedules and recipe_schedule_fires. Store small bounded ordered required
check-ID arrays as JSON in immutable recipe revisions and copy them into immutable
task bindings (reuse existing JSON convention; no separate redundant check tables).
Immutable profile revisions contain exactly the six role configurations and project
TRIP config revision/hash. Recipe revisions reference an exact profile revision and
pin project/config/workflow version+hash, literal content, required check IDs and
priority. Current records carry optimistic version, name, archived state, timestamps.
Task bindings retain recipe/profile pins, required checks and optional schedule/fire
provenance; task edits never rewrite history. Do not copy capability proofs, task
activations, role generations, launch permits, approvals or selected-check rows.

Use foreign keys and uniqueness for cross-record integrity; validate same-project
links in transactions. Bound names/templates/check arrays/profile JSON by existing
field conventions or explicit conservative limits in module validation. Reject unknown
roles, missing roles, unparseable providers, empty model/effort, duplicate check IDs,
unenabled/foreign checks and references to archived records. Saved profiles are
configuration, not a statement of current native qualification. Saving sets/recipes
requires currently active project workflow/config pins, but does not require creating
or copying new task-specific activation. New draft role_settings use revision1 and
must use the existing activation flow before Ready. Existing capability checks stay.

For materialization validate all pins, exact recipe/profile revisions, active project
TRIP configuration and enabled required checks. Reject stale manual requests without
creating a draft. At Ready and scheduler claim revalidate task binding pins/checks;
no silently rebinding stale drafts. Ordinary unbound tasks remain unchanged. At
Manager check selection require all bound required checks in submitted selection,
then retain existing current-manager/planning/immutable-revision checks. Never
prepopulate trip_selected_checks or fake selected_by_generation_id.
Task role edits remain ordinary revisions and require ordinary requalification;
recipe bindings constrain workflow/config/checks, not permanently forbid human role
changes. Existing tasks survive recipe/profile archival and retain readable provenance;
archiving source does not revoke an already-created ordinary task by itself.

## Human commands and atomic materialization

Add explicit HumanCommand variants for upsert/archive profile set, upsert/archive
recipe, create draft from exact recipe revision, upsert schedule and pause/resume/
archive schedule. Creation versus update must be explicit: expected_version absent
only for creation; updates/archive require current version. Unknown identity cannot
silently create or upsert over another project. Reuse operation-ID request hash and
receipt replay inside existing IMMEDIATE human command transaction. No new HTTP or
control route: current authenticated generic command handles variants. Human actor
validation remains unchanged; roles cannot invoke these via role socket.

CreateDraftFromRecipe includes exact recipe_revision_id and expected_recipe_version;
response gives ordinary task identity for navigation. Extract only the existing task
row/role_settings insertion into a narrow transactional helper to share with CreateTask,
preserving normal task validation and normal CreateTask Ready semantics. Recipe paths
always pass backlog, ready_at NULL, run_next_requested false, no attempts/claims.
Task insertion, provenance, command receipt and audit commit or roll back together.
Every successful mutation moves existing state revision through projected-row triggers.

Archive recipe atomically pauses enabled schedules; archive schedule retains history.
Profile-set archive refuses while referenced by an active current recipe revision
or an unarchived schedule's pinned recipe revision; require archiving/editing references
first. Historical references from existing tasks do not prevent archival. Schedule
and recipe edits never silently update other pinned revisions.

## Foreground scheduling semantics

Use UTC RFC3339 canonical instants at whole-second precision and daily24h/weekly7d
periods from an explicit anchor. Reject invalid dates, unsupported cadence, overflow,
noncanonical/mixed offsets on wire; UI provides clear UTC input and browser-local
read-only display. No timezone library, cron/monthly/custom intervals. New schedules
start paused; explicit Enable/Resume first arms them strictly after command time.
Editing an enabled schedule preserves enabled state but recomputes its next fire
strictly in the future. Pause/resume/archive are version-bound human commands.

Capture service_started_at once for this process, inject now and start into intake
logic for tests. Wire one scheduled_intake_tick adjacent to foreground coordinator
inside its existing spawn_blocking tick; no second daemon/timer. Skip intake while
service drain or restore/recovery execution hold is active. Do not alter coordinator
or SchedulerRunOnce semantics. Project queue pause does not forbid creating backlog
but UI displays paused queue; no Ready task means no queue bypass.

Process due schedules in deterministic next_fire/id order with a bounded batch
(e.g.32 schedules/tick); each schedule under an IMMEDIATE transaction re-reads current
version, paused/archive and next-fire before work. Serialize with human edits so an
old snapshot cannot fire after pause/reanchor commits. At most one newest eligible
due occurrence per schedule/tick; older occurrences are summarized by range/count,
not expanded into one row per missed day. Instants strictly before service start are
missed; no offline catch-up. Exact start-time fire is eligible. For sleep/forward
clock jump, choose newest due >=start, summarize earlier missed occurrences and
advance next_fire to first future instant, all transactionally. Clock rollback cannot
move next_fire backward or recreate completed fires. Arithmetic checked and bounded.

recipe_schedule_fires primary key(schedule_id,canonical scheduled_for_utc) is durable
dedupe; row includes pinned recipe revision, task or skipped outcome, sanitized reason
and missed range/count as relevant. A due valid occurrence inserts one draft +six
role rows+provenance, fire outcome, next-fire and service audit in one transaction.
A duplicate identity returns existing outcome without a second task; reanchor/edit
cannot reuse identity to duplicate a task. Expected eligibility failures commit
skipped_ineligible and advance next-fire without retrying that instant. Unexpected
DB/IO/transaction failures roll everything back; no partial receipt or draft. Emit
bounded diagnostics for unexpected failures, no per-tick audit flood for idle/paused.
Service-fired records use a distinct deterministic audit identity namespace, never
forge a human operation receipt/approval. Do not expose a manual fire-as-human bypass.

## Projection and actual dashboard actions

Add typed profile_sets, task_recipes, recipe_schedules and task recipe provenance to
AppStateDto/state snapshot; bump projection schema7->8. Keep protocol generation1
because transport declaration/framing unchanged. New state fields get explicit
frontend validation/default fixtures; malformed data must not enable mutation controls.
All projected tables need state-revision triggers. Project scope filters references.
Project choice/navigation changes must clear or bind drafts to their originating
project; polling same project preserves dirty forms. Cross-project stale submission
must fail server-side even if a client retains an old form.

Add Recipes navigation and project-scoped page with three sections. With All projects
selected, require selecting a project before editing. Profile editor six actual role
provider/model/effort controls using existing patterns and clear config-only guidance.
Recipe editor literal task content, priority, exact profile revision, selectable
required check IDs from active matrix, read-only pinned workflow/config. Create draft
uses exact revision and opens ordinary task detail/editor; no Ready checkbox here.
Schedules show exact recipe revision, UTC/local time, cadence, next fire, last task/
skipped/missed outcome and clear reason; create paused, explicit enable/pause/resume/
edit/archive with disabled-reason text. Include draft provenance in task detail and
normal board identification without new task lifecycle. Reuse stable operation IDs
and ordinary conflict/ambiguous-outcome behavior; never replay automatic mutations.
Preserve forms on live updates; conflict shows guidance rather than silently rebinding.
No manually entered raw JSON as primary product UI.

## Ownership and integration

Single retained Sol6medium writer with manager integration and one build slot.
Backend: migrations/030_recipes.sql, src/recipes.rs(new), lib.rs, domain.rs,
workflow.rs, operations.rs, server.rs, store.rs, trip.rs, scheduler.rs. Mechanical
current-schema assertions in database.rs allowed only if necessary, no semantic change.
Frontend: components/Recipes.tsx(new), App.tsx, types.ts, api.ts only typed validation,
styles.css, components/TaskDetail.tsx, components/TaskBoard.tsx and
components/History.tsx only archived-draft visibility/restore.
Tests: existing contracts.rs/runtime.rs/inline ownedRust and flows.test.tsx. Manager
owns README and docs WORKFLOWS/OPERATIONS/SECURITY/MILESTONES/NEXT_MILESTONE_PLAN.
No edits to other migrations, coordinator phase state machine, provider adapters,
role result ingestion, cmux/control protocol, .agents/global config or dependencies.
Check actual task-detail path before handoff; outside-scope changes require manager
reconciliation, not uncontrolled file expansion. Preserve all earlier uncommitted work.
Implement internal persistence/mutation contract first then schedule, projection/UI;
these are integration steps within one milestone, not separately approved features.
Inventory every exhaustive HumanCommand match, state/schema fixture and task caller
at first handoff. No omission of existing suite fixture updates.

## Moderate causal verification and final gate

Initial allowance12 causal areas/up to2200 test/support lines in existing Rust/DOM
harness; standing authority covers justified fix-driven adjustments. No permanent
new harness/dependency. Table cases share existing fixture. Inject clock into pure
intake function, no sleeps/time daemon. Areas:
1 fresh/current schema +state triggers/projection; 2 optimistic revisions and
operation replay/mismatch; 3 exact project/profile/recipe pins and immutable history;
4 single manual draft/roles/no capability copy; 5 requiredchecks Ready/claim/manager
selection; 6 dailyweekly/boundary/overflow; 7 dedupe/rollback; 8 offline/sleep/clock
rollback boundedmiss; 9 pause/resume/edit/archive racing due intake; 10 invalid config/
checks/project hold/drain/queuepaused behavior; 11 actual UI create/profile/recipe/
schedule controls payloads/navigation/provenance;12 polling draft retention/conflict/
crossproject and no Ready/run_next/approval injection. Test normal unbound task behavior
through affected existing contracts, current backup schema compatibility as needed.

Focused checks during implementation, independent consolidated code review then one
stable scripts/verify.sh matrix, crosslayer final Explorer and fresh Fablemedium final,
task-local release package. No live restart/provider/GUI/native service acceptance.
Document exact scheduling UTC/missedfire/draft-only policy and pins in user guides.
Native qualification and user acceptance remain explicit deferred boundaries.

## Separate v1 follow-up

M5 capability path normalization in src/providers/mod.rs uses unrestricted worktree
substring replacement, so directory c collides with sibling control.sock. Confirmed
by M9 Explorer; do not entangle with recipe changes. Separate bounded TRIP fix after
M9 with actual c sibling regression before integrated v1 completeness. Do not weaken
identity comparison or quietly mark it resolved by fixture renaming.

## G1 review dispositions — normative refinements

All nine G1 findings accepted (including optional simplification); these details
resolve earlier shorthand. Plan remains in independent review, no implementation yet.

1. A stale config/workflow/check pin rejects Ready/claim with explicit guidance:
   re-save the profile set and recipe under the active configuration, archive the
   stale draft using existing task Archive, and Create draft from the new revision.
   No new rebind command and no silent mutation of original provenance. Recipe editor
   visibly labels superseded pinned configuration; editing opens current active check
   options and requires explicit reselection rather than mapping old IDs by name.
   Preserve old draft content for inspection/copy before archive. The UI guidance must
   make this recoverable even if the original recipe was archived (save a replacement).
2. Missed-only tick persists one fire row at newest missed instant with outcome
   `missed`, no task, and bounded first/last/count summary; next_fire advances in the
   same transaction. Include missed in wire/DB outcome enums/checks. Schedule last
   outcome projects this row. Mixed eligible/missed tick associates summary with the
   newest eligible fire; no per-occurrence expansion. Fire dedupe still covers both.
3. Explicit migration fixture census: tests/contracts.rs STATE_PROJECTED_TABLES
   currently58 entries, per-column mutation trigger test, and schema29/read-only28
   migration test which drops only029 objects. Update table census/count to include
   M9 tables; all030 projected tables retain rowids and UPDATE triggers compare every
   column null-safely. Rework downgrade fixture to remove030 objects safely before
   replaying029+030, or test genuine pre030 schema separately without CREATE collisions;
   preserve original029 trigger-upgrade coverage rather than deleting it. Current
   database/backup expectations follow CURRENT_SCHEMA_VERSION consistently.
4. Save-time profile validation is exactly six known roles, typed provider parses,
   model/effort nonempty per existing validation semantics, current active config
   revision/hash pins. Reuse or minimally expose the existing helper if useful; no
   adapter launch, model catalog call, provider probe, capability validation or new
   task activation at save time. Real combination eligibility remains the ordinary
   task activation/Ready path. UI labels these as saved configurations, not qualified
   models. No profile-save bootstrap loop.
5. In existing foreground spawn_blocking closure run intake independently of the
   coordinator Result; do not short-circuit intake behind coordinator success. Each
   uses its own existing execution-hold/draining/dispatch-enabled checks. Return or
   handle both outcomes separately with bounded diagnostics; coordinator deferral
   alone does not disable scheduled intake. No new loop or lock-order inversion;
   follow existing store/execution fence ordering. A panic/join error retains existing
   service failure behavior. Held/disabled intake does not generate task/fire writes.
6. TaskBoard.tsx explicitly owned for ordinary board recipe/schedule provenance;
   TaskDto and frontend Task add typed optional provenance, task detail shows exact
   revision/firetime. Actual existing detail file verified as TaskDetail.tsx.
7. Every recipe/profile/schedule command and duefire requires an existing project
   with internal_purpose NULL. Projection excludes internal projects; direct calls
   cannot create invisible drafts in setup/validation fixtures. Test via fixture
   marking internalpurpose, without real-project edits.
8. ResumeRecipeSchedule is the sole arming command. Enable is the UI label for a
   newly created paused schedule; subsequent paused schedules may say Resume. Both
   reanchor strictly future; neither catches up or creates a task immediately.
9. Manual create checks expected_recipe_version plus exact recipe_revision_id and
   config/workflow/check pins; unrelated project queue-version changes do not reject
   creation. The exact revision must belong to requested recipe/project and remain
   eligible; never substitute latest silently. Recipe/profile human updates retain
   expected_version guards and immutable history.

Verification additions stay inside12areas/2200support: area1 genuine migration and
per-column trigger census; area5 stale-pin Ready refusal/re-save+archive+newdraft
remedy; area8 missed-only restart persisted withouttask; area9 archive-recipe versus
exact duefire serialization; area10 internalpurpose denial and coordinator error
independence. Tests remain in existing harness; no new acceptance authority.

## G2 review disposition — draft retirement recovery

Confirmed current Archive accepts only done, not backlog; the earlier reference to
an existing draft Archive path was incorrect. Extend existing Archive (same revision
and operation-ID semantics) to done OR backlog with no attempts and ready_at NULL.
Both recipe-bound and ordinary unbound never-started drafts receive this ability;
ready/in_progress/cancelled and previously attempted backlog remain refused. Guard
and mutation are in the same existing command transaction. No process cancellation
or approval is implied. TaskDetail offers Archive for eligible drafts with visible
reason when unavailable. Existing done behavior and Restore mutation semantics stay.

Archived drafts must remain discoverable/restorable: minimally include archived
backlog tasks in existing History list and reuse its existing Restore action, while
retaining completed-task history. History.tsx now explicitly owned only for this
visibility/restore change. Restore returns the task to its unchanged backlog/pinned
state; it does not fix stale configuration or make Ready. Guidance still offers
re-save current recipe and create a fresh draft. No deleted rows or lost provenance.

Area5 adds stale draft Archive/History/Restore and refusal of ready/in_progress/
cancelled/attempted tasks; ordinary unbound safe draft covered by same tablecase.
Area11 traces actual Archive click and History Restore. If exposing existing
validate_role_override, make error text role-neutral. Same12areas/2200support;
standing authorization covers this necessary recovery remedy. Third focused plan
review explicitly selected by manager under standing authority; no counter reset.

## G3 review disposition — archived draft Ready guard

SetReady must refuse archived tasks: retain its existing version/lifecycle/authority
checks and add archived_at IS NULL to the transactional update predicate. No direct
command may turn an archived draft Ready. Restore only clears archived_at and leaves
the task backlog; Ready requires a separate subsequent valid human command. Add
archived-draft SetReady refusal (zero readiness mutation) to the existing area5/12
tablecase, alongside Archive/Restore transitions. Existing idempotent replay returns
its prior receipt without rerunning mutation; it cannot resurrect readiness. No new
file/command/allowance. Manager authorizes fourth focused recheck under standing
roadmap authority for this concrete finding, counters retained.
