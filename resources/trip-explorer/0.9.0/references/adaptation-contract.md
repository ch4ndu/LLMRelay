# Adaptation contract

The generic package owns workflow roles, approval boundaries, proportional
ownership/dependency mapping, review accounting, scoped snapshot safety,
verification pacing, final-repair semantics, reconciled task timing, and
optional cmux projection.
A project installation owns repository-specific guidance, delegated-role
profiles/adapters, preflight evidence, documentation ownership, build/test
commands, platform gates, and final report language.

## Required invariants

- Keep planning read-only until exact-plan approval and separate implementation
  authorization.
- Use one retained writer by default. Use several only for disjoint exact
  ownership after shared seams are frozen, with one manager integration owner.
- Name one serialized build owner per checkout; other writers do not run
  competing write-producing build, test, formatter, generator, or cleanup work.
- Require one installed `testing.coverage` selection and honor it for every task:
  `minimal` defaults to zero new tests and allows at most one justified cheap
  causal test; `moderate` expects focused causal coverage of each meaningful
  deterministic changed behavior or accepted defect using existing seams; and
  `extensive` expects comprehensive relevant success, failure, boundary,
  regression, and interaction coverage wherever deterministic existing seams
  permit. Carry the selection through planning, writer/repair prompts, review,
  verification, and final conformance. It is not a percentage or count quota.
  Every profile pauses before a test-only production seam, new fake/helper
  framework, dependency, permanent harness, exhaustive low-value permutations,
  or materially disproportionate test growth. Do not run a device, emulator,
  simulator, or live runtime path unless explicitly authorized.
- Run one initial code review, one consolidated repair, and one retained
  recheck automatically. Any third call requires explicit maintainer authority
  and remains inside the five-call absolute cap.
- Track drift in owned/shared/protected paths and freeze the aggregate candidate
  once; unrelated outside-scope drift does not invalidate every lane.
- Run focused invalidated checks during development and one post-review final
  matrix for an unchanged candidate. Later repairs rerun only invalidated
  dimensions unless a shared/build/release input invalidates the aggregate.
- Reconcile total wall time against exclusive phases and report unattributed
  time, waits, overlap, retries, and discarded work. Timing measures work; it
  never imposes a phase time budget.
- Keep cmux optional and observational. Missing or failed cmux must never block
  the workflow, and cmux must never hold authoritative state. When active, use
  one unfocused plain terminal pane per CLI-backed workflow role to show
  ordinary provider console output. Never replace it with summaries, inject Git status/diff
  commands, add pane-locking machinery, or open Markdown, diff, browser, or
  simulator previews.
- Launch each role directly through the installed cmux role runner. Never route
  it through another `TRIP-*` or `codex-*` orchestration wrapper, and require
  every role prompt to prohibit nested delegation or agent launches.
- Reuse an idle role pane only through the observer's launch action. Never send
  prompt or control prose directly to a pane because its shell will execute the
  text after the prior agent process exits.
- Keep active user updates within 60 seconds and require causal controls for
  focused behavior tests.
- Do not infer commit, push, merge, tag, version, publish, release, device, or
  destructive authorization from implementation authorization.
- Treat concise comment/KDoc discipline as an essential installed invariant.
  Prefer fewer comments: names and structure explain the normal path, and
  ordinary control flow, assignments, delegation, or already-clear behavior
  receives no explanatory comment or automatic KDoc. Improve naming or
  structure first when that can carry the meaning without adding unjustified
  abstraction. Brief comments remain for non-obvious contracts, invariants,
  rationale, lifecycle or concurrency constraints, security boundaries,
  external-system behavior, and platform limitations; they never narrate code
  or restate the next line. Enforce this in writer handoff, manager integration,
  independent code review, and final conformance. A target project may
  strengthen the rule but must not remove it or replace semantic review with a
  comment quota.

## Project configuration

`.agents/trip-explorer/config.json` contains:

- `project_name`
- `guidance`: project-relative files to read when relevant
- `verification.focused`: development and repair commands
- `verification.broad`: broad/project-wide final commands
- `verification.cleanup`: cleanup commands after verification
- `documentation.no_change_text`
- `observability.cmux`: `auto`, `on`, or `off`; `TRIP_CMUX` may override it
- `roles`: exact mappings for Explorer, Plan reviewer, Implementer, Code
  reviewer, and Final verifier
- `profiles`: user-selected adapter, provider, model, effort, authority, and
  session contracts referenced by roles
- `testing.coverage`: `minimal`, `moderate`, or `extensive`; required and
  binding for every task

Commands are declarative evidence for the manager, not an automatic pipeline.
Select only checks applicable to the candidate and record freshness and
invalidation.

`.agents/trip-explorer/adapters.json` defines built-in CLI, native-agent, and
structured custom-CLI adapters without credentials. `preflight.json` records a
successful live invocation and capability receipt for every referenced profile.
The active host agent remains manager. Never substitute another adapter,
provider, model, effort, authority, or session mode.

`auto` uses cmux when its CLI and socket are reachable. `on` requests the same
best-effort integration explicitly. Both fall back to direct collaboration on
any cmux failure; `off` skips probing. The installed `cmux_observer.py` attaches
to hook-disabled current workspaces and opens passive role terminals backed by
`cmux_role_runner.py`. The provider-aware runner invokes the selected CLI
adapter, writes only the final response and completion receipt for manager
orchestration, and does not auto-approve commands. Native-agent profiles remain
owned by the active host manager rather than the CLI runner.
