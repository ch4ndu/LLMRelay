# Milestone 4B: crash recovery and repeat-safe actions

Status: owner approved the recommended scope, paused recovery policy, bounded
fault-injection approach and standing limits on September 22, 2026. Independent
Fable/medium plan review 3 APPROVED; implementation proceeds under that gate. Material changes to
the approved scope or solution shape still require an explicit owner decision.
Baseline: verified uncommitted M4A/M5 tree. No historical migration support.

## Outcome

For each named external-side-effect boundary, establish what survives an
interruption, which reconciler owns the next boot, and the next safe user action.
No ambiguous delivery may become a retry merely because the host restarted.
Preserve exact identity, permissions, review budgets, and user acceptance gates.

## Proposed recovery policy

Interrupted restart admissions that require recovery must have an idempotently
created, exact-bound recovery record. Distinguish proven nondelivery, exhausted
retry budget, stale admission authority, and uncertain or delivered work; do not
treat these as interchangeable process observations. Preserve counters and
terminal batch membership. Repeated startup must not create another unresolved
record for the same incident.

Recommended post-quiescence outcome: a visible paused hold. Reuse existing
recorded process verification, with transaction-time identity revalidation.
Confirmation resolves only the demonstrated uncertainty; it does not launch work,
clear another hold, renew a consumed budget, or restore stale session authority.
Expose an explicit Resume or fresh Continue only when its existing classifier
independently permits it. Unknown evidence remains held with a specific reason.
Cancel remains available under its existing ownership rules.

Alternative for owner selection: after verified resolution, permit immediate
normal dispatch using existing fresh-dispatch semantics. This is faster but makes
confirmation itself capable of advancing work; the paused outcome is recommended.

## Proposed implementation units

1. Repair interrupted admission recovery and exact action projection/admission.
   Primary owners: src/recovery.rs, src/workflow.rs, src/store.rs; existing
   src/operations.rs/coordinator.rs consumers only if required by the hold policy.
   Frontend continues rendering backend decisions; no general redesign.
2. Protect replay/repeated controls at the actual request boundary. Same operation
   ID replays its receipt. Reject a distinct duplicate
   Continue while paused rework already has a pending Continue. Keep projection
   and command admission consistent, including before coordinator consumption.
3. Complete the interruption matrix with existing Rust fixtures and the narrowly
   approved injection points below. Reuse existing protected-boundary tests.
   A simulated durable state and an actual interrupted code path are different
   evidence; label them separately. Do not claim OS power-loss guarantees.
4. Update operations guidance, stable reason/action mapping, and matrix evidence;
   run affected checks, one integrated review, and the final aggregate matrix.

The capability cwd textual-replacement defect is a separate follow-up. Keep it
visible and unwaived for product completion; do not combine a capability-identity
format change with this crash-recovery repair without a separate reviewed design.

## Crash-boundary matrix

| Boundary | Existing owner | Planned evidence |
| --- | --- | --- |
| Commit / response | store transaction and receipt | Reopen after committed result but missing response; same-ID replay creates no second effect. |
| Backup publication | database::backup | Interruption after staging sync before rename never advertises incomplete backup; newest verified backup remains. |
| Restore phases | database::recover_interrupted_restore_locked | Reuse M4A journal-phase tests; strengthen only a demonstrated missing transition. |
| Worktree creation | scheduler::reconcile_unknown | Existing reserved tuple plus real fixture worktree; exact identity reconciles, mismatch holds without overwriting bytes. |
| Provider launch | recovery::reconcile_prior_boot / supervisor::reconcile | Reuse reserved-versus-spawning/descendant uncertainty tests; no paid provider call. |
| Permission delivery | permissions delivery reconciler | Reuse prior-boot/disconnect/replay fence; ambiguous delivery cannot grant again. |
| Role report | store::save_role_result | Commit-before-response replay returns same receipt; changed payload rejected. |
| Review consumption | coordinator::consume_review_result | Reopen after atomic consumption; no second advancement or review-budget charge. |
| cmux binding | durable create claim / binding owner | Success before binding persistence remains unknown after restart, no duplicate create/focus/close. |
| Drain | operations::begin_drain | Capture-before-signal interruption; next boot cannot assume quiescence or blindly relaunch. |
| Restart admission | recovery / operations / workflow | Exact record, stale-authority rejection, capped/uncertain distinction, paused verified resolution, repeated startup. |
| Restart restoration | recovery / operations | Preserve membership/counters and reject stale outcomes; reuse M5 coverage where inputs remain unchanged. |

## Verification expansion requiring approval

Existing fixtures can reconstruct persisted states, but do not prove that the
real code reaches those states at the relevant side-effect/commit crossover.
Propose at most four small test-only injection locations: commit/response,
backup sync/publication, cmux RPC/binding persistence, and drain capture/signal.
Use existing harnesses and disposable fixtures only. No permanent failure service,
background scheduler, generic fault framework, new dependency, or paid session.
If a subprocess crash harness or broader abstraction is necessary, present that
separately rather than hiding it in this allowance.

Initial estimate: 250–650 production lines for bounded recovery/control fixes;
up to 2,000 normally formatted test/support lines across at most 12 causal areas,
including injection plumbing, in existing files. These are planning estimates,
not permission to expand architecture. Existing aggregate runtime is roughly
1–2 minutes; estimate added focused runtime below 30 seconds, subject to fixtures.
Maintenance cost is keeping four injection points aligned with their boundaries.
The smaller alternative is durable-state fixture tests only, explicitly leaving
the four exact interruption-path proofs unverified.

## Execution and authority

One coupled backend writer, session-selected GPT-6 Sol/medium. Manager owns tests,
integration, docs and completion. Existing Sol/high ordinary review and fresh
Fable/medium final review. Do not overlap a writer and inspector. Use one cmux pane
and completion notifications. Plan is one coherent recovery outcome; stage fixes
before matrix additions so existing-flow regressions surface early.

Recommended standing limits for this milestone: five plan reviews, eight ordinary
code reviews, two final-repair cycles, 12 causal areas / 2,000 support lines.
No real-project/data changes, user-service restart, provider validation, computer
use, installation, publication, deletion, or historical migration work.
Plan review follows the explicit design/verification choice; production edits
still require approval of the reviewed concrete plan.

## Concrete mechanics after plan review 1

The following refines the approved solution shape. No new schema, framework,
runtime fault option or machine authority is introduced.

### Admission classification and recovery identity

Compute delivery evidence independently from admission authority. Missing invocation
is proven nondelivery only with the exact expected ordinal, unchanged resume count,
prior epoch and exited session evidence; task-version/config/control drift must
not itself change a proven delivery fact into uncertainty. Malformed or contradictory
launch evidence remains unknown. Preserve the current compare-and-swap on admitting
candidate state/result and commit candidate, attempt, task, claim and record together.

| Delivery and authority | Durable outcome |
| --- | --- |
| Proven nondelivery, current authority, below cap | Existing failed / needs_input / resume_failed path; existing counted failure semantics. |
| Proven nondelivery, exhausted cap | Blocked candidate, explicit parked hold, retained repository exclusion; no invented unknown-process record. |
| Proven nondelivery, stale authority | Blocked candidate with stale-authority reason and parked hold; do not supersede pending pause/cancel/manager control. |
| Uncertain delivery | needs_recovery and unknown claim, exact bound recovery record; no automatic retry. |
| Another restored lane remains active | Preserve its running status and ownership; expose unresolved recovery independently, never park or clear the active lane's authority. |

Keep failure accounting separate from authority: count only a concrete proven
preflight/nondelivery failure for the captured admission, once; never reset or
spend a count merely because version/configuration changed. Terminalize batch
membership in every interrupted-admission outcome. Existing stale-version test
assertions must change from fictitious unknown delivery to proven nondelivery plus
stale authority; retain a separate genuinely uncertain-delivery case.

For uncertain delivery, within the guarded transaction reuse an unresolved record
for the exact session and attempt if it already exists from general prior-boot
reconciliation. Otherwise create one with detail kind restart_admission_interrupted,
admission identity, expected resume ordinal and invocation state. Copy the session's
current process_identity_json, not an invocation identity substituted for it.
Ensure session status satisfies existing recovery identity validation. Do not
overwrite an unrelated record kind or discard prior evidence when reusing a record.
Persist the associated record ID and admission identity under the candidate's
startup_admission_reconciliation object and expose that exact ID in the decision
subject. Association must match session, attempt, generation and current unresolved
record before it can influence confirmation. Repeated startup sees a non-admitting
candidate and cannot create another incident or increment counts again.

### Confirmation and the explicit next action

For a newly classified interrupted-admission record, or an existing unresolved
record exactly associated with that incident, resolution uses current process
verification and transaction-time identity checks. It must not take the ordinary
remaining-zero running/none plus release_restart_hold_for_fresh_dispatch_in branch.
Instead, once all relevant ownership uncertainties are resolved and no other lane
or control prevents it, use the existing restart_parked attempt/attention shape.
Retain the claim as repository exclusion, record positive quiescence, preserve
all counters, forbid native resume for this recovered incident, and require explicit
fresh Continue. Candidate may remain blocked for stale/capped evidence; otherwise
park it with native_resume_forbidden/fresh_dispatch_requires_explicit_continue.
No resolve call launches or queues work. Existing Continue rechecks quiescence,
controls and exact current candidate before releasing the hold.

Unrelated check, claim, setup, freeze and provider-delivery recovery retains existing
semantics. A reused provider record takes the paused policy only through the exact
interrupted-admission association. Other unresolved records and active lanes remain
visible and fenced; resolving one record must not normalize their status or claims.

Spawning with no recorded provider identity must be proved absent by an applicable
recorded generation anchor or a verified boot change. Same boot without such proof
stays held with quiescence unknown; old exited-process absence is insufficient.
Do not weaken verify_session_quiescent/verify_generation_absent_evidence. Include
the spawning/same-boot/no-anchor refusal and zero downstream effects in coverage.

### Repeated Continue and stale consumption

Share the existing pending-Continue query across projection and insertion for all
Continue paths, excluding terminal/rejected controls. Same-ID receipt replay occurs
before this guard. A new operation ID while Continue is pending is rejected without
a new control, receipt, version change or materialization. At process_one_control,
revalidate current Continue eligibility excluding the current control's own pending
identity. Reject stale/ineligible controls rather than entering the generic running
fallback for reserved/materializing rework. Test same-ID replay and distinct-ID
rejection before the first tick, plus a pre-existing duplicate at consumption.

### Test hook reachability

Use at most four local cfg(test) hook locations and the existing cargo test runner.
Backup and cmux checks belong in their existing in-crate test modules. Commit/report
response and drain checks may add compact cfg(test) modules inside existing store.rs
and operations.rs; this is test placement within the approved existing Rust harness,
not a new integration framework. Integration tests cannot depend on cfg(test) hooks.
Include all hook/module/support lines in the 2,000-line allowance. No public feature,
environment switch, production constructor flag, subprocess harness, or new dependency.
Use one-shot invocation-local or test-thread-local hooks to avoid parallel-test races.
An injected unwind/error proves the exact application boundary and durable reopen,
not OS power failure; ensure the committed/external side effect happened before
the hook and assert no repeated effect after reopening. Review consumption remains
covered through its atomic durable-state seam unless an approved hook is reusable
without adding a fifth location or changing a production API.

### Reason codes and documentation

Retain restart.ownership_reconciliation_required as a defensive fallback for legacy
or malformed unbound evidence; normal newly reconciled uncertain admissions must
instead expose the exact record and an enabled recovery action only when valid.
Add restart.admission_authority_stale for proven nondelivery with stale authority.
Retain replacement-limit reason at the cap and restart.fresh_dispatch_hold_releasable
after verified paused recovery. Every action carries current exact identity and
uses the existing owner classifier. Document these states, same-boot unknown proof,
explicit Continue and retained retry counts in docs/OPERATIONS.md; record matrix
reason/event/reconciler/test references in the milestone evidence. No UI-only rules.

### Resolution ordering and generation evidence (recheck 2 corrections)

The final remaining-zero decision is attempt-wide, independent of which recovery
record the user resolves last. A current, nonterminal restart candidate associated
with interrupted-admission recovery, or an existing parked fresh-dispatch-only
candidate, retains the explicit hold. Evaluate this in the resolution transaction
after the individual record writes. Resolve admission-first/check-last and the
reverse order identically. Include existing graceful-stop parked holds in this
preservation rule. Terminal released/cancelled/resumed candidates or superseded
associations must not reintroduce a hold. Pending control, active-lane, restore and
other unresolved-record precedence remains intact. Do not modify the individual
check/claim/freeze resolution contract; only prevent their last resolution from
implicitly releasing an attempt-level explicit-Continue hold.

For uncertain current resume delivery, bind proof to the exact invocation ordinal
and transcript epoch captured by the admission. The session may still contain the
previous invocation's anchor, group and process rows. Parse and compare its current
anchor to that invocation's prior_recovery_anchor_json; unchanged prior evidence is
not a new-generation anchor. A non-null changed anchor is applicable only with
matching current invocation/session epoch and the existing record_session_anchor
epoch guard. Malformed or ambiguous evidence fails closed. When no applicable
anchor/new-generation process identity is demonstrated, filter out prior-generation
anchor/group/process rows before absence verification and use only the current
launch boot identity for the reboot proof. An absent old provider is never proof
of absence of an unrecorded new spawn. Keep verify_generation_absent_evidence's
existing refusal semantics unchanged. Preserve original invocation launch state
in incident evidence before prior-boot code replaces it with delivery_unknown;
otherwise that state alone cannot establish whether spawning occurred.

Revalidate this proof binding inside the resolution transaction, including exact
invocation/epoch/anchor snapshot so a new observation cannot be resolved with stale
verification. Apply the same applicability rule to any action that could clear the
associated uncertainty, including Cancel, not only confirmation. Read-only preview
must not advertise stale prior-generation absence as verified current quiescence.
Reuse one bounded fact extraction/filter at existing owners, not a second verifier.

Coverage includes spawning on the same boot WITH stale prior anchor and pid rows
(held, zero downstream effects), changed-boot proof, and a valid current anchor.
Normal spawning recovery reuses the general prior-boot record with NULL process
identity; preserve that valid existing representation. An associated already-exited
session also receives positive process_group_quiescent evidence after successful
verification, rather than relying on the old recovery_required-only update filter.

For stale-authority nondelivery with requested pause/cancel, initially retain the
restart_parked attempt/attention hold and leave the requested control untouched.
Its subsequent valid consumption may deliberately change attention/status to paused
or cancelled. This is expected authority, not recovery auto-dispatch. After pause,
explicit ordinary Continue must still satisfy existing restart hold release checks;
after cancellation no Continue is available. Manager-change/stop and active-lane
precedence must not be overwritten by classification or confirmation.
