# Guidance and planning quality

## Guidance drift audit

Enter only when the user explicitly asks for a guidance audit, for example
`$trip-explorer-workflow audit guidance`. This is a read-only maintenance path,
not a new engineering task, installation, upgrade, or automatic per-task gate.
The active manager performs it directly without launching roles, invoking
providers, or requiring runtime preflight. Return the report and stop.

1. Read repository guidance and the existing configuration, if present. Use
   `guidance` from `.agents/trip-explorer/config.json` to identify configured
   files or directories, plus the root `AGENTS.md`. Follow only task-relevant
   documentation links. If no installation exists, use the root guidance and
   user-named documentation; say the audit is not an installation health check.
   Treat a malformed config or unavailable guidance path as an unresolved gap,
   not permission to invent configuration or silently omit coverage.
2. Declare the files/sections in scope. For directory guidance, inventory its
   regular files without traversing symlinks outside the project. Do not read
   credentials, private runtime logs, or unrelated documents. A broad audit
   request can expand coverage; ordinary audits remain bounded to the request.
3. List concrete claims: paths, commands, named modules, configuration keys,
   routes, and behavior steps. Separate intended policy from descriptions of
   current implementation. Do not rewrite a desired policy just because source
   currently violates it; report that discrepancy as an implementation gap.
4. Use the cheapest adequate read-only evidence: `rg --files` for paths,
   manifest or script inspection for commands, and source tracing for flows.
   A filename match does not prove behavior. Do not run builds, tests,
   generators, documented commands, or network/provider probes merely to
   validate a claim. Mark runtime-dependent claims unverified when current
   evidence is insufficient. Apply the absence-claim discipline in
   [review scope](#completion-criteria-and-review-scope) before reporting something missing.
5. Report each mismatch as `file/section · claim · source evidence · reality ·
   proposed correction`. Include concrete source locations and distinguish
   stale claims, implementation gaps, missing coverage, and unverified claims.
   Count checked claims and list exclusions; do not call a partial audit clean.
   Missing coverage means an important contract within the declared scope is
   undocumented, not that every source directory needs a documentation entry.
6. Present proposed edits in chat. Leave project-owned guidance untouched unless
   the user separately requests corrections; existing explicit edit authority
   applies when it already covers those exact files and corrections. Do not
   create a report file or workflow ledger without write authorization. Do not
   compress or reorganize guidance as a side effect of fixing factual drift.

Example report row:

| Claim | Evidence | Finding | Proposed correction |
| --- | --- | --- | --- |
| `CONTRIBUTING.md` says to run `make verify` | No Makefile; `package.json` defines a `check` script | Stale command; execution not tested | Replace the command with `npm run check`, subject to the project's intended verification policy |

## Discovery recommendations

Before drafting an engineering plan, reconcile the request with current source
and existing decisions. Summarize the intended outcome, then ask only questions
whose answers materially affect behavior, scope, constraints, ownership, or
verification. Do not ask for decisions already made in the conversation.

Use the host's available question mechanism and respect its question/option
limits. Put a source-backed recommendation first, explain its relevant tradeoff,
and allow a free-text answer where the host supports it. If evidence does not
favor an option, state that instead of inventing a recommendation. Bundle
related questions to minimize interruptions and continue independent work.

Stop asking when no consequential ambiguity remains. If the user says to use
recommendations for remaining choices, record the resulting non-blocking
assumptions and proceed to the plan. This does not resolve missing authority,
override a constraint, or replace required approval of the exact plan. No
question count, elapsed time, silence, or empty response grants approval. Avoid
fixed question quotas and repeated confirmation for already authorized work. For
unresolved blocking decisions, name the specific missing decision and continue
only work independent of it.

## Explanations the user can assess

Lead with the result, recommendation, or decision required.

For technical explanations, aim for “80% of the way to ASD-STE100” Simplified
Technical English. Use plain words, direct sentences, named actors, consistent
terminology, and one main idea per sentence. Treat this as a readability target,
not a compliance score. Preserve technical precision, conditions, uncertainty,
and requirement strength; relax strict vocabulary and grammar restrictions when
they make the explanation less natural or accurate. Do not impose strict
dictionary compliance, sentence quotas, banned verb forms, or a new writing
linter.

Choose the simplest useful representation. These are examples of fit, not a
required mapping:

| Question | Suitable representation |
| --- | --- |
| What changed or remains? | Concise prose or a small table |
| Who owns a state or decision? | Ownership diagram |
| Which event causes a transition? | Sequence diagram or timeline |
| How do alternatives differ? | Comparison table and focused before/after diagram |
| What happens under different inputs or event orders? | Disposable interactive explanation when it materially helps |
| How does a difficult mechanism evolve over time? | Animation or narration selectively |

The agent selects the format without routinely asking the user to choose. Do not
generate every format, turn a simple change into a presentation, or make an
artifact a mandatory gate. Respect existing authorization for external services,
paid narration, installations, publishing, or runtime access; this guidance adds
none.

Ground technical claims in inspected source or recorded evidence. Label current
implementation, proposed design, observed runtime behavior, and simplified
simulations. A simulation does not verify the application. Include relevant
source/candidate identity so an explanation is not silently reused after its
inputs change. Check rendered diagrams and interactive behavior proportionately
using available tools; report unavailable checks without claiming they passed.

Generate views from existing task evidence. Keep disposable artifacts in the
project's designated local ignored location. Keep lasting requirements and
decisions in their existing owners. Do not add permanent explainer
infrastructure. This guidance applies to user explanations; the optional cmux
role consoles retain their existing plain-terminal observation restrictions.

## Corrections and durable lessons

First correct the active task. Classify the input as missed accepted
requirement, unsupported claim, scope creep, new scope, changed preference, or
clarification. Mixed cases can carry more than one classification; do not force
a false single cause.

For an actual process failure, identify whether guidance is missing, ambiguous,
stale, conflicting, or simply not applied. Repairing compliance with an existing
rule does not require another copy of it.

Propose a durable lesson only when it generalizes beyond the task and has
concrete evidence. State its scope, source, authority, date, exceptions, and any
superseded instruction as appropriate. Update the established owning instruction
through its authorized mechanism; do not create tasks/lessons.md everywhere or
automatically mutate managed memories. New user preferences and requests are not
automatically agent failures.

## Repair or revisit a decision

Use existing recovery and scope owners. The implementer reports potentially
invalidating evidence. The manager assesses it against the approved plan and
existing scope rules; material changes return to the existing plan authority and
user authorization gates. The implementer does not redesign unilaterally. An
ordinary compile error, failed hypothesis, or focused defect within the approved
solution calls for diagnosis and repair. Evidence that the approved design
cannot meet an accepted requirement calls for revisiting the affected decision.
A new subsystem, material architecture change, or unrelated defect follows the
existing scope-expansion rules.

Pause only affected work when safe independent work remains. State the changed
evidence, its decision impact, and the next unresolved question. Do not restart
the entire plan or repeat valid checks solely because execution became
difficult. Avoid unchanged retries; retain existing rescue and retry limits.

## Completion criteria and review scope

Record the accepted outcome, exclusions, platforms, and completeness criteria in
the existing plan. Review defects against that bar. Report consequential new
risks separately and route any expanded work through the scope owner. Preserve
finding dispositions and their evidence across handoffs so another round does
not reopen settled questions without changed evidence.

Separate discovery audits, changed-diff reviews, and exact finding rechecks.
Discovery remains independent within an explicit area and contract. Absence
claims stay provisional until search scope, variants, and a known-positive
search control have been checked. This discipline improves evidence quality; it
is not a guarantee that search proves universal absence.

## Proportionate execution

Diagnose and fix routine failures within authorized scope without asking the
user to direct every step. Distinguish task-caused defects from unrelated CI
failures and infrastructure issues. Preserve existing device, external action,
scope, and release authority.

Assess design by demonstrated cause, cohesive ownership, preserved accepted
behavior, proportionate permanent maintenance, and the smallest adequate
solution. Smallest adequate does not mean smallest diff; a justified structural
fix remains possible through existing approval rules. Do not replace these
checks with subjective staff-engineer approval or unbounded elegance criteria.
