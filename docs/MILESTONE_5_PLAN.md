# Milestone 5: explain waits and bound restart work

Status: implementation verified on 2026-09-22 (local time). Ordinary review 11
and fresh final review 3 APPROVED. Aggregate Rust/DOM checks plus focused final
repair checks passed; personal hands-on acceptance remains deferred by the user.
Local packaging is recorded in the milestone evidence ledger.
Baseline: verified milestone 4A working tree, not a new committed baseline.
Evidence: `.local/trip-explorer/llmrelay_m5_20260922/explorer_g1_result.md`.
The user's instruction to proceed authorizes this concrete plan. Production edits
begin after independent plan approval. Routine refinements within the approved
shape use the standing allowance; material scope changes still need a decision.

## Outcome

Every non-progressing task should explain what prevents progress, which evidence
is missing or stale, who owns the work, and the next safe action. The explanation
must come from the checks that actually admit the action. Restart preview must
show what would happen without draining, signaling, reconciling or changing state.
Bulk resume must be bounded, durable and explicit about queued versus resumed work.

Milestone 4A is independently verified and packaged. Its optional observations
about count-only restore prerequisites and sessionless recovery selection are
included only where needed for accurate explanations and exact action binding.
Historical migrations, provider APIs, new services, notifications, distribution,
live provider checks and unrelated recovery changes are excluded.

## Recommended product choices

| Question | Proposed choice |
| --- | --- |
| Coverage | All nonterminal Ready, In progress, Validation and Awaiting review tasks, including skipped or capacity-blocked tasks; Backlog shows readiness, completed/cancelled work shows terminal status. |
| Blockers | Deterministic authoritative precedence with one primary blocker; include additional safely observable blockers and satisfied/stale evidence. An unevaluated prerequisite must say so, never imply success. |
| Next action | At most one primary exact action, with required task/attempt/session/generation/revision/recovery identity. Existing secondary controls remain explicitly described by backend policy. |
| Bulk resume | Exactly one distinct selected session uses the direct path; Resume eligible or two-plus selected sessions queue at most four. Deduplicate selected IDs, take the deterministic first four and return queued IDs plus the omitted IDs/count. At most one queued/automatic resume admission per coordinator tick. |
| Capacity backoff | 2, 4, 8, 16, 30, then 60 seconds, capped at 60. Capacity deferral does not spend a provider replacement attempt. |
| Manual retry | Explicit single-session retry may bypass a capacity delay, but never ownership, human gates, attempt caps or uncertain-delivery recovery. |
| Replacement limit | Three failed preflight/proven-nondelivery attempts per candidate; preserve existing review/provider accounting. No newly automatic retries of failed deliveries. At the cap show explicit recovery/fresh-dispatch actions only when independently valid; no timer resets. |
| Preview | Dashboard plus `llmrelay restart-preview` against the live authenticated service. It does not imply drain has happened. |
| Restore hold | One instance-level explanation and exact prerequisite list; affected tasks link to it. Share prerequisite enumeration with offline hold release. |
| Payload version | `decision_schema: 1`; no separate general client-negotiation framework in this milestone. |

The user approved these defaults as one scope decision. No live process claim or
launch permit expires because time passes. Existing human input leases retain
their separate expiry semantics.

## Architecture and ownership

Use typed explanation DTOs in `src/domain.rs`: stable reason code, subject,
observed revision, primary blocker, prerequisite evidence, ownership, next-action
binding and control policy. Explanations convey no authority. Every mutation
rechecks the current facts in its existing transaction.

Keep evaluation beside its existing owner:

- `src/coordinator.rs`: preserve phase precedence and action ordering; use the
  same evaluated conditions for wait/hold results and explanation. Include tasks
  skipped by a tick. Audit canonical changes, not every idle tick.
- `src/scheduler.rs`: share Ready admission evaluation with explanations; preserve
  IMMEDIATE claims, physical repository exclusion, dependencies and capacity.
  The shared evaluator is read-only classification over the same query/gate
  functions. Existing attention/lifecycle mutations occur only in claim_next.
- `src/recovery.rs`: extract read-only restart facts/classification from mutation.
  Distinguish pre-drain eligibility after verified quiescence from admission now.
- `src/operations.rs`: retain exact admission and ownership transactions; persist
  bounded human-batch intent and receipt before external work; consume due entries
  through existing coordinator ticks. Preserve backoff/counters across restart.
- `src/workflow.rs`: aggregate owner decisions instead of recomputing admission
  from JSON. Bind ResolveRecovery to the exact displayed recovery ID and reject
  stale or mismatched IDs without mutation. recovery_id is required, including
  session-qualified resolution; remove newest-record fallback. Update every
  command match/caller/fixture and prove ID, attempt, kind and subject agreement.
- `src/database.rs`: share read-only restore prerequisite enumeration with release.
- `src/server.rs`, `src/control.rs`, `src/cli.rs`: authenticated preview transport
  and accurate queued/result responses. No new listener or authentication scheme.
- `src/store.rs`: only touched restart-hold release/candidate metadata consumers
  and required exact command bindings, preserving established quiescence rules.

Prefer existing audit_events, restart_candidates/result_json and operation
receipts. No schema migration is planned. Review must verify every consumer of
new queued states, startup behavior and receipt replay before implementation.
Batch authorization does not survive generation/configuration drift as authority:
each entry revalidates; cancellation, human gates and stale identity remain fences.
Backoff must not starve unrelated eligible candidates. Repeated operation IDs
must return the original batch and never queue more work.

### Durable retry and batch metadata

Reserve `result_json.llmrelay_restart_v1` for batch operation identity, ordinal,
membership status, replacement failures, capacity-deferral count and next due
time. Preserve it independently from last-outcome fields, including when a batch
terminates; terminal membership prevents redispatch without discarding evidence.
Every touched writer must preserve this namespace explicitly using existing
SQLite JSON operations or an equivalent single transactional merge. Malformed
metadata is a visible refusal, never an implicit counter reset.

Audit these current writes as one coupled change:

1. prepare_restart_candidates upsert in recovery.rs: preserve namespace and
   active queued membership rather than mapping unknown state to parked. Newly
   failed eligibility/uncertain process evidence still supersedes dispatchability.
2. reconcile_restart_candidates parked/blocked updates in recovery.rs: preserve
   namespace and counters; proven-safe members return to their queued schedule,
   uncertain members remain held and cannot be resumed by a timer.
3. operations.rs admission rejection: retain metadata and terminalize membership.
4. operations.rs resume success: retain metadata and complete membership.
5. operations.rs capacity or failed outcome: merge counters/backoff, retain active
   batch membership only for capacity deferral; failed/blocked leaves the batch.
6. store.rs explicit release/cancel and graceful-stop upsert, database.rs restore
   cancellation, and any other candidate-state writer found by the implementation
   census: preserve counts and terminalize membership where the owning action
   ends dispatch authority. Graceful-stop upsert must not erase the namespace.

Startup must reconcile an interrupted admitting candidate against existing
process/launch evidence before it can return to a queue. Lost response, crash or
ambiguous delivery is not proof of nondelivery. Never reconstruct a consumed
batch from an old receipt after terminal state or generation change.

Consume due human-batch members first, including queued_capacity members even
when auto-resume is disabled; then consider due automatic candidates when enabled.
Within each set retain manager-first and stable created-at/ID ordering. Skip
not-yet-due candidates rather than blocking unrelated work. Failed/blocked/
released/cancelled entries have no automatic retry. Exactly one distinct selected
session may use the existing direct human path, with the same classifier and
counts; it is serialized by the coordinator lock but not delayed by a batch tick.
New explicit human operations cannot overwrite an already active batch member.
A direct retry of a queued_capacity member uses its existing membership metadata;
it may advance that member, not replace it with a different batch authorization.

Reject stale outcome writes: compare the captured candidate state/identity at
admission refusal, and require the matching admitting ownership for delivered
outcomes. Commit any associated task/attempt changes with that guarded outcome;
a stale result must not resurrect work released by a newer human decision.

### Stable explanations and on-demand preview

Canonical audit identity includes only reason code, ordered blocker codes/states,
evidence identities, exact action binding and relevant observed revisions. Exclude
evaluated_at, countdowns, all human text and the audit event's own revision so
unchanged waits cannot produce a new audit row every tick. Preserve observed
revision consistency across the explanation and authoritative decision.

Read-only preview must not call begin_drain, desired-running capture, candidate
preparation, reconciliation or supervisor mutation. It classifies resumable,
fresh-only, blocked, uncertain, awaiting approval and complete sessions. Running
work is labelled eligible only after verified quiescence. In-flight side effects
requiring re-verification stay visible. Unknown provider authentication or reset
times remain unknown; do not infer structured authority from terminal prose.

Preview is a separate on-demand authenticated read endpoint/control request,
never part of periodic /api/state projection and never automatically polled by
React. Use one read-only OS process inventory per explicit request and reuse it
across session classifications. Unknown inventory yields uncertainty, not a new
reconciliation. The existing drain capture includes running sessions only;
launch_reserved and interrupt_requested are not promised exact resume. Explain
their fresh-only/not-captured boundary unless stronger human/uncertainty gates
apply. Fresh-only is a classification, not authority to launch. Ordinary state
explanations use recorded facts and must label unobserved current OS facts unknown.

Frontend ownership: existing types, WorkflowControls, Workspace, AttentionInbox,
TaskDetail and RecoveryPanel. Render backend decisions, exact actions and control
policy; remove duplicated authorization predicates only as their shared backend
replacement is wired. UI field-presence validation is still appropriate.
Keep scope to these flows, not a general dashboard redesign.

Classification: broad, one coherent recovery-decision contract across the
coordinator, restart admission and dashboard. Implementation proceeds through
the internal units below with one integrated review and final aggregate gate.

One session-selected Sol writer owns the coupled backend changes. Frontend follows the frozen
contract; do not overlap writers on these seams. Manager owns integration,
documentation and all build/test commands. Reviewer roles remain Sol/high and
Fable/medium, with a fresh final verifier.

## Bounded implementation sequence

1. Freeze current decision precedence and define DTO/action bindings; wire shared
   coordinator/scheduler explanations without changing admission semantics.
2. Share restart classification and implement the zero-mutation preview.
3. Add durable capped/staggered bulk intent, backoff and retry-limit explanation;
   preserve exact-once accounting and uncertain-process recovery.
4. Wire dashboard and exact recovery-ID mutation, update owning docs and review
   the integrated feature. Use focused checks between units, one stable aggregate
   at the final integration gate rather than broad tests at each handoff.

All units belong to one milestone; internal checkpoints do not drop later scope.

## Verification proposal and footprint

The initial estimate was 1,000–1,600 net production lines. Review of the first
checkpoint found duplicated coordinator decisions and estimated 2,700–3,700 net
lines for the unchanged whole milestone after consolidating that duplication.
This revised estimate preserves the approved owner-local architecture; it is not
a line quota or authorization for additional features. Up to 2,800 normally
formatted test/support lines gives repair headroom without a new framework.
No structural rewrite of the large coordinator/store modules.
If evidence requires new schema, framework or architectural scope, stop that
expansion and present the smallest alternative first.

Eight causal areas in the existing Rust/DOM harness:

1. Same-facts explanation equals coordinator wait/hold and its durable audit;
   timestamp/text-only changes do not flood audit records.
2. Scheduler reasons cover capacity/queue/dependency/repository/profile gates;
   repeated ticks cannot duplicate attempts, claims or workspaces.
3. Preview repeated reads preserve durable tables, audit/receipts, desired-running
   and in-memory drain/dispatch state; no signal or launch occurs.
4. Exact restart explanation agrees with admission/rejection; stale identity,
   capability, human gate or uncertainty cannot gain authority.
5. Bulk cap/remainder, atomic receipt, deduplication, stagger, crash continuation,
   capacity backoff/fairness and replacement cap remain bounded, including
   auto-resume disabled, selected multi-session mode and every metadata writer.
6. Claims/permits never expire by time; human input leases retain their own rules.
7. Recovery actions settle precisely the displayed ID once; stale/superseded IDs
   and poisoned controls fail without affecting unrelated work.
8. Dashboard renders backend policy consistently in the named flows and sends
  exact action IDs; restore-hold explanation matches release prerequisites.

Prefer focused cases reusing existing seed_supported_restart_identity and other
helpers instead of extending unrelated monolithic test bodies. No new test
framework or duplicated bootstrap. Required recovery_id propagates through all
existing Rust/DOM fixtures; do not keep a permissive fallback for old callers.

Approved standing allowance: 5 plan reviews, 8 ordinary code reviews, 2 final
repair cycles with focused fresh rechecks, up to 12 causal cases across these
eight areas and 2,800 support lines (the user increased this ceiling on September
22 for exact recovery-ID/dashboard integration and fix-driven rechecks).
Routine in-scope repairs, docs, builds/tests
and local packaging included. Zero paid product-validation sessions; engineering
roles consume normal existing CLI allowance. No real-project/data restore,
service restart, global changes, computer use, deletion or publication.

Estimated engineering range: 2–4 working days, with significant uncertainty in
extracting shared predicates without changing behavior. Re-estimate after plan
review and first integrated checkpoint; this is not an overnight guarantee.

## Exit

All covered stalled states have an authoritative explanation and an honest next
action or explicit missing-evidence prerequisite. No preview mutates state;
retry/batch work cannot bypass user gates or duplicate effects. Focused and
aggregate checks, independent code review and fresh final review pass. Update
OPERATIONS, WORKFLOWS and SECURITY as needed. Hands-on acceptance remains deferred.
Next roadmap step is milestone 4B crash-boundary coverage. Git checkpoint and
publication authority remain separate from implementation approval.
