# Meaningful behavior testing

Apply this reference with the workflow’s installed coverage profile and
expansion limits in every writer and accepted-finding repair prompt.
Tautological tests and tests that detect only incidental implementation changes
are harmful. Do not create a bug-fix regression test without a genuine
behavior-coverage gap. This guidance does not mandate TDD for every task.

For test-driven development, write a failing test only for an accepted
observable behavior or invariant that existing coverage does not adequately
protect. Confirm that its failure comes from the missing or incorrect behavior,
then implement and verify the behavior. Do not manufacture a failing assertion
merely to satisfy a red-green sequence.

A tautological test restates its own setup or proves a mock's configured answer
rather than exercising the production behavior. Derive expectations
independently from the accepted contract; do not copy the implementation
algorithm into the expected result. A test that fails when the guarded behavior
is bypassed is a useful causal check, but does not alone establish that the
asserted behavior is meaningful.

A change-detector test fails on incidental implementation changes while accepted
behavior remains correct, such as an arbitrary private helper sequence or
internal representation. Assert stable observable outcomes and relevant
invariants. Exact output, ordering, or interaction assertions remain justified
when those details are themselves part of the accepted contract; not every
snapshot or interaction test is inherently invalid. Examples include accepted
persisted formats and migrations, wire formats, public API contracts, and
accessibility semantics. Their expected values must still come from the
contract, not merely the current implementation.

Before adding a bug-fix regression test, identify the genuine behavior-coverage
gap and the observable defect the test would catch. A defect observed while
tests pass is a reason to investigate verification coverage, not automatic proof
that another automated test is appropriate. Check whether relevant tests ran on
the affected candidate, whether their inputs or assertions miss the behavior,
and whether the defect requires native or device evidence. Reuse or repair an
existing meaningful test where sufficient. Do not add one test per bug as a
ritual, duplicate existing coverage, or preserve erroneous expectations after an
intentional behavior change. A new test may be justified when a distinct
boundary, input, or ordering reveals a real uncovered contract, even if the
feature already has tests.

If no new automated test is chosen, state why existing coverage, an appropriate
existing-test repair, or the shortest meaningful manual/platform check addresses
the verification need. Mark unperformed checks as pending; do not claim coverage
merely from a checklist. Deleting or loosening an existing assertion requires
explaining the accepted behavior, why the old assertion does not represent it,
and the replacement or existing evidence that protects it. Such edits follow the
normal authorized implementation and review path; this creates no extra per-test
approval gate and does not authorize removing valid coverage.

Keep the selected coverage profile, test-expansion limits, existing-seam
preference, and manual/platform boundaries intact. This guidance does not
require TDD for every task, weaken accepted behavior, authorize a new harness,
or permit dismissing a valid failing test as a change detector without evidence.

## Causal evidence

Every test must protect a distinct causal guarantee. Accept an automated test
as causal evidence only when it would fail if the guarded production behavior
were bypassed. This necessary check does not by itself make the asserted
contract meaningful.

For a permission or early-return path, one case may assert both the outward
result and zero downstream effects using an existing counter, spy, or fail-fast
fake. Do not count a mock that independently returns the expected denial, a
compile-only check, or an assertion disconnected from the production path. This
rule does not authorize a new integration harness, server fixture, simulator,
emulator, or device run.
