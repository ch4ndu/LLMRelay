# M7 — Bundled provider compatibility contracts

Status: independently approved G2, 2026-09-23. Standing user authorization
covers finalized in-scope plans and proportionate verification. Coverage moderate.
M6 live-state/attention complete; preserve all preexisting uncommitted changes.

## Outcome and boundaries

Embed versioned provider compatibility data in the executable, bind effective
contracts to existing capability/launch/resume authority, and show actionable
compatibility explanations in existing screens. No remote manifests, downloaded
flags, provider API, new signing pipeline, dependency, generic policy engine or
parallel authority store. Rust remains the only command/policy constructor.

This is engineering implementation plus fixture verification. Native six-role
product requalification is deferred under the unattended-work decision. A manifest
match never marks a capability Supported; no old proof is assigned a new binding.
No live provider probes, service restart, real-data changes, installation, GUI,
credentials/config changes or publication. Historical migration support is excluded
by user instruction. No new database columns or migration required initially:
missing bindings fail admission and are projected as Unverified with original
evidence preserved. If existing gates cannot enforce that, return for reconciliation.

## Design

1. Add src/provider_compatibility.rs and resources/provider-compatibility/codex.json
   and claude.json, embedded with include_str. Strict serde schema1, deny unknown
   fields, duplicate IDs, malformed/incomplete contracts, overlapping exact-version
   selectors and unknown compiled revision IDs. No semver/ranges needed. Codex's
   current exact raw version codex-cli 0.155.1 is the initial candidate selector.
   Claude bundle has descriptive metadata and no production version selector until
   exact native qualification is available. An empty reviewed selector set is valid
   data but always returns unknown_version; never use wildcard or installed version
   inferred from engineering-role preflight. Test-only fixtures exercise Claude
   mechanics without being selectable by production or environment overrides.
2. Separate bundle provenance from authority. Hash full embedded bytes for display.
   Hash a deterministic typed canonical resolved authority record for capability
   identity: provider, pack ID/schema, selected exact predicate, role, session class,
   compiled launch/native-policy/hook/credential/evidence revisions, and applicable
   resume revision. Full bundle hash, human text and packaging revision are display
   metadata outside capability-key serialization. A role's contract revision is
   authoritative and changes with its behavior. Descriptive edits or unrelated
   role/provider changes must not invalidate that selected authority record.
   Stable sorted collections, no HashMap iteration or raw JSON whitespace hashing
   for effective identity. Bundled hook asset digest stays independently bound.
3. Session classes are existing workflow semantics: final verifier fresh-only;
   retained roles include both initial and resume contracts in their identity.
   A retained role's initial and resumed preparation share that identity/key as
   today, while fresh-final excludes resume-only revision. Do not normalize away
   the retained contract's resume revision. If a real supported role deviates from
   this existing mapping, explicitly document and review rather than guessing.
4. Resolve after executable/version discovery and before authority-bearing command
   preparation/reservation. Verify manifest revision IDs against compiled constants.
   Attach authoritative binding to LaunchConfig and CapabilityIdentity; diagnostics
   provenance travels separately. Historical deserialization may allow absent binding
   solely for display; all current admission/proof/reuse paths require Some matching
   current resolved contract. Reselect on current-native-policy validation before
   existing effective argument/environment/sandbox/hook/credential checks. A match
   never skips those checks. Capability key, proof JSON, setup/runtime scopes and
   exact resume records must preserve the binding through existing identity fields.
   No indexed columns or second proof ledger. Old missing-binding Supported rows
   remain historical but cannot authorize or display current Supported status.
5. Add typed compatibility error categories (unsupported, contract_changed,
   invalid_manifest) with sanitized structured detail, not error-string matching.
   Thread these through existing rejection recording and decision/attention routing
   without replacing other rejection categories. Unknown/changed compatibility must
   reject before credentials, permits, generation/budget or process authority is spent.
   A fresh route is offered only when a current matching contract and required proof
   exist; never offer an endlessly failing retry for an unknown contract.
6. Shared explanatory DTO: status matched/unknown_version/ambiguous_manifest/
   contract_changed/evidence_stale/manifest_invalid; pack ID/revision, contract
   ID/revision/short hash, predicate ID, missing-evidence IDs, safe-action enum and
   human message. Closed safe actions: install_supported_provider_version,
   update_llmrelay_release, requalify_exact_profile, inspect_local_provider_configuration,
   contact_operator. No executable action from descriptive text. Unknown version
   cannot expose proof-launch/resume buttons, since no reviewed contract exists.
   Matched but unverified requires existing qualification, explicitly pending.
7. Render in existing global capability list, RoleSettings, ProjectSetup and
   DiagnosticsPanel plus current compatibility attention reasons. Preserve M6
   exact navigation and M5 decision authority. No new settings/management screen.
   Model catalog stays advisory and never expands a selector. Redaction excludes
   provider paths, arguments, user inputs, credentials and arbitrary configuration.

## Ownership and delivery

Broad cross-layer security contract: one writer, sequential backend then UI handoff.
Backend paths: new resolver/resources; src/lib.rs, domain.rs, providers/mod.rs,
providers/codex.rs, providers/claude.rs, store.rs, trip.rs, operations.rs, workflow.rs;
tests/contracts.rs and tests/runtime.rs plus existing inline tests. Exact additional
callers must be reconciled before assignment. No migration initially.
UI: frontend/src/types.ts, api.ts, App.tsx, components/RoleSettings.tsx,
ProjectSetup.tsx, DiagnosticsPanel.tsx, flows.test.tsx and styles.css only as needed.
Manager owns docs SECURITY/OPERATIONS/BUILDING/WORKFLOWS and milestone status.
One build owner per handoff; no writer/inspector overlap. Baseline current tree,
not HEAD. No source edits until independent plan approval; standing user approval
then applies. Resolve next milestone implementer tuple before launch; do not silently
carry M6-only Opus override forward. Reviewers remain configured roles.

## Verification

Initial moderate scope: 10 causal areas, up to2000 support lines, existing harness.
Standing authorization permits justified increases recorded before new handoff.

1. Strict parse/exact unique selectors; malformed/unknown/ambiguous fail closed.
2. Deterministic effective hash; prose/unrelated contract changes do not invalidate;
   selected authoritative change does; full bundle provenance remains visible.
3. Retained fresh/resume key equality and fresh-final resume independence; no
   fresh-only proof usable for retained authority.
4. Missing/stale binding rejects current admission and proof reuse; newer unverified
   row blocks old Supported; legacy proof bytes preserved, no fabricated binding.
5. Unknown version/changed contract rejects before reservation/spawn/budget effects,
   stable typed rejection and truthful safe route.
6. Manifest match cannot bypass native argument/environment/hook/sandbox/credential
   checks; both provider fixture contracts exercised without production override.
7. Setup/runtime/adapter/profile scopes retain their independent exact bindings.
8. Existing UI shows pack/predicate/evidence/action, no unsupported launch/resume,
   affected vs unaffected roles and M6 exact compatibility navigation preserved.
9. Redaction and advisory model/detection cannot confer capability authority.
10. Embedded artifact contains immutable manifest identity; no runtime files or
    remote update dependency. Packaging is local unsigned engineering evidence.

Focused checks per handoff; consolidated independent review, then one integrated
scripts/verify.sh after convergence, final cross-layer Explorer, fresh final reviewer,
manager request/conformance gate and task-local package. No live native qualification
claim from synthetic fixture success; mark engineering slice complete only.

## Explicit risks for review

Broad identity serialization can accidentally invalidate unrelated roles if bundle
metadata enters the key. Legacy display must not overclaim support despite failing
admission. Resume category plumbing must preserve existing recovery gates. An empty
Claude production selector deliberately blocks product launch until qualification;
do not hide this behind a matched badge or infer a qualified version. Preserve
separate engineering-role tooling: this change concerns LLMRelay product adapters.

## G1 review refinements (normative)

These details refine the design above and take precedence over shorthand wording.

- A selector identifies a reviewed contract candidate eligible for the existing
  capability validation lifecycle, not an already Supported profile. Adding an
  exact Claude predicate is a reviewed source change with named executable/version,
  compiled launch/resume, native-policy, hook and credential contract evidence.
  It then undergoes separate native role qualification. Until a candidate exists,
  Claude product roles stay Unverified with update_llmrelay_release; offer
  install_supported_provider_version only when an actual candidate is named.
  This deliberate fresh-install restriction is part of this plan; fixture success
  never supplies native qualification or manufactures a candidate.
- Thread a resolved bundle set through an explicit public test-support constructor
  following Application's synthetic_dispatch_for_tests precedent (external contract
  tests cannot call cfg(test)-only code). Fixed synthetic bundles are selected only
  there. All linked preparation/revalidation uses that same context. Normal
  constructors always use embedded constants; no CLI/HTTP/config/environment or
  serialized identity selects test data. Synthetic contexts cannot launch real
  provider processes, and persisted fixture identities cannot pass normal admission.
  No general runtime dependency-injection or configurable policy subsystem.
- Downcast typed compatibility errors before any string scan in
  operations::permanent_resume_rejection_category. Add categories to the Store
  rejection allowlist, workflow::permanent_resume_rejection_route, and explicit
  permanent_resume_rejection_is_current rules. Record observed version, frozen
  contract hash and session/key binding in sanitized detail. New applicable current
  preparation/proof evidence retires a stale rejection; unrelated evidence does
  not. Projection never probes providers; explicit retry/preparation/qualification
  supplies fresh observations. Unknown categories cannot remain permanently current
  after a supported configuration is established.
- Cross-check the exact Codex predicate against compiled EXACT_CODEX_VERSION as
  well as revision IDs. A matched badge cannot contradict the compiled predicate.
- Compute missing-binding Unverified status/gaps in workflow's raw capability
  projection; do not rewrite stored proof/status during reads. Assert pack_revision
  and bundle_hash are absent from capability-identity serialization.
- Authority-spend tests assert no persisted credential/reservation/spawn/budget
  effect and release of an already-acquired permit unconsumed. In-memory token
  generation before preparation is not delivery and need not be forbidden.
- Known callers for assignment: Store proof/resume/admission (discovery lines3637,
  7942,8263,8400,9913,10519), TRIP scope/preflight reuse (572,2875,3628), workflow
  preparation/projection (49,3031,3233,3278–3292). Verify symbols, not stale offsets.
  Only raw capability display is not already key-based.
- Extend existing ten verification areas for synthetic/production context isolation,
  typed-error classification before string fallback, rejection allowlist/route and
  currency, compiled-version crosscheck and serialization exclusions. No new harness.

G2 assignment clarifications: Store must carry the same bundle context to its
revalidation seams; normal Application construction refuses a synthetic Store.
Candidate evidence means reviewed compiled contract revisions, not prior native
qualification proof. Preserve typed errors through anyhow context without string
rewrapping; test that compatibility cannot become frozen_runtime_identity_changed.
Unsupported/contract_changed use ReplaceStaleAuthority with fresh dispatch gated
on matching current contract and proof; invalid_manifest is an external non-retry
route with update_llmrelay_release. No new action categories beyond existing policy.
