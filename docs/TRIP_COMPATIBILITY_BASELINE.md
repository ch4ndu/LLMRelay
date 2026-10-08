# TRIP compatibility baseline at 8c6f1e7

**Status:** MW-01 redacted comparison baseline; source and authenticated context evidence only.
**Recorded:** October 7, 2026.
**Checkpoint:** `8c6f1e72cb3282db4dd4e381ef8fd83b6b37ea9e`.
**Task / attempt:** AJ-4FCAAAE2 / `2412323b-91f2-48a9-a42e-d896873c92d0`.
**Approved plan hash:** `5247a194c717b0fe098f31ba58b1a8c4b52988affef05ea7c63bf58988c57925`.
**Scope hash supplied by the app:** `d1cbcb560bfc0d9d5d6d2e43bf5304d691e586e04b240430d893e617597abb6a`.

Use this alongside the [architecture specification](MULTI_WORKFLOW_PLAN.md) and [requirement ledger](MULTI_WORKFLOW_REQUIREMENTS.md). The hashes and settings below establish comparison inputs; they do not independently qualify native behavior.

## Evidence boundaries and collection

| Evidence | Origin | What it establishes | Limit |
| --- | --- | --- | --- |
| E1: source checkpoint and clean entry | Direct `git rev-parse HEAD`, `git status --short`, `pwd -P` | Exact assigned attempt worktree and unchanged entry source | No build, launch, migration or native proof |
| E2: role context | Direct fixed-host `role context` | Approved plan, implementation phase, default lane, exact bindings, budgets and selected checks | The implementer view does not expose the human approval receipt or independent review verdict text |
| E3: focused source tracing | Bounded `sed` and `grep` reads of the anchors below | Current launch policy, scope fences, session rules and TRIP couplings as implemented | Provider launch preparation and generated native settings were not executed |
| E4: source-file digests | Direct `shasum -a 256` of selected source/resources | Reproducible content anchors for future comparisons | Raw file digests are not workflow, adapter or capability authority hashes |
| E5: RT-01 through RT-03 historical runtime proofs | Replacement task description and approved plan | Historical setup activation and six ordinary runtime role proofs were verified through LLMRelay | Not rerun or independently inspected here; no app-owned bootstrap-completion claim |
| E6: regression inventory | Named cases located in current Rust/React source | Existing causal expectations to preserve | No tests were run by this lane |

Read-only inspection used the existing workspace, with no traversal of app/native state, direct database access, raw transcript collection or denied control-socket read. `rg` was unavailable; bounded `grep`/file reads were used. No native role/provider launch, browser session or live permission probe was performed. The exact role channel executable remains `/private/tmp/llmrelay-development-20261006/host-runtime-0.12.0/llmrelay`; its bytes and active supervision are protected.

## Source observations
- Entry inspection returned the required full checkpoint and an empty `git status --short`. The physical worktree path matches the assigned attempt. These are read-only observations, not a rerun of historical runtime proofs.
- src/domain.rs:37-95 defines the six TRIP roles and final_reviewer compatibility alias. LaunchConfig and CapabilityIdentity at 106-152 bind executable fingerprint/version, role, model, effort, permission/security policy, normalized argv, environment, hooks and compatibility authority. RoleContext at 425 binds project/task/attempt/generation/session, channel identity, transcript epoch, provider, configuration revision, lane and operations.
- src/providers/mod.rs:432,497,504,604,608 prepares role launches, hashes capability identity and normalizes only bounded paths/prompt/native resume arguments. src/provider_compatibility.rs:251,319,417 requires all six compiled role contracts and derives fresh-final versus retained behavior. Preserve the existing TRIP serializers and effective authority hashes byte-for-byte; descriptive catalog metadata is separate.
- src/providers/codex.rs:120-272 selects workspace/on-request for Implementer and read-only/never for the other roles, user approval ownership, strict-config/no-daemon, disabled additive permissions/delegation, exact hooks, disabled MCP/features, denied control-socket floor, singleton role socket and restricted proxy, and read-only temporary roots for writers.
- src/providers/claude.rs:106-236 selects Read/Grep/Glob/Bash plus Edit/Write only for the writer, dontAsk versus default, restricted native sandbox, strict empty MCP, disabled delegation/web/Chrome/slash commands, exact role command allowlists and hooks. Compare the generated settings JSON, tool allow/deny lists, sandbox refusal and singleton socket semantics as well as identity hashes.
- src/permissions.rs:354,428,1078 currently makes only Implementer permission requests actionable and fences their delivery against exact current policy, account-channel authority, generation, session, native identity, boot and connection. Labels cannot replace these fences.
- src/trip.rs:445 task_profile_descriptor joins activated TRIP configuration, role settings, adapter capability, current authority and retained/fresh contract. src/scheduler.rs:329-545 requires project TRIP readiness and six roles; 944-973 creates TRIP planning attempts and 2/2/1 budgets. src/workflow.rs:1733 and 2900-3030 routes creation, acceptance and linked rework through TRIP checks.
- src/review.rs:reserve_freeze and freeze_reserved require quiescent current source generations; src/snapshot.rs:SnapshotManifest/capture includes original base, candidate HEAD, dirty/untracked bytes, deletions, modes and bounded symlink handling. src/workflow.rs:RecordIntegration separately verifies the accepted manifest against a reachable repository commit.
- src/recovery.rs:RestartSessionFacts explicitly checks retained history, current policy/capability, current attempt, holds, quiescence and fresh-final refusal. This is preserved infrastructure, with workflow eligibility supplied by the pinned run instead of global TRIP constants.
- frontend TaskForm exposes six-role inheritance and TRIP readiness; TaskBoard and TaskDetail hardcode Plan/Build/Verify/Review; ReviewPanel claims selected checks and final review passed before acceptance. Single agent needs truthful workflow-specific projections and wording.
- Schema is 38 (src/store.rs:9905); service startup upgrades are explicitly admitted from 31 through 37. Existing migrations 018, 020, 021, 031, 032, 037 and 038 carry TRIP identity, profile activation, final repair and history; they must not be rewritten.

## Twelve role/provider comparison cells

These are source-derived policy expectations for both providers. Only the provider selected for each activated task role appears in the context table below. A source policy cell is not a newly observed native outcome.

| Role | Codex source policy | Claude source policy | Session contract |
| --- | --- | --- | --- |
| manager | `:read-only`, approvals `never`; user reviewer; shared denied-read floor | `Read,Grep,Glob,Bash`, `dontAsk`; manager operation allowlist | Retained |
| explorer | `:read-only`, approvals `never`; user reviewer; shared denied-read floor | `Read,Grep,Glob,Bash`, `dontAsk`; context/report allowlist | Retained |
| plan_reviewer | `:read-only`, approvals `never`; user reviewer; shared denied-read floor | `Read,Grep,Glob,Bash`, `dontAsk`; context/report allowlist | Retained |
| implementer | `:workspace`, approvals `on-request`; user reviewer; read-only temporary roots | `Read,Grep,Glob,Edit,Write,Bash`, `default`; scoped edit and yield-lane allowlist | Retained |
| code_reviewer | `:read-only`, approvals `never`; user reviewer; shared denied-read floor | `Read,Grep,Glob,Bash`, `dontAsk`; context/report allowlist | Retained |
| final_verifier | `:read-only`, approvals `never`; user reviewer; shared denied-read floor | `Read,Grep,Glob,Bash`, `dontAsk`; context/report allowlist | Fresh for every frozen candidate |

Codex preparation uses strict configuration, no daemon, user-owned approvals, disabled additive permission features and delegation, explicit native hooks, configured local MCP disablement, and a restricted proxy with no allowed domains and only the canonical role socket. Local binding, upstream proxy, SOCKS, credential broker, arbitrary Unix sockets and non-loopback proxy are disabled. The canonical human control socket remains in the denied-read floor. Writers honor current native approval coverage; only newly emitted requests enter the app inbox. App revocation affects app-owned rules.

Claude preparation selects the applicable read-only/writer hook settings, strict MCP configuration, restricted native sandbox, context/report command allowlists, disabled slash commands, Chrome, delegation and web tools. Read-only branches deny Edit/Write. Manager-only commands remain manager-only; writer tool availability does not authorize every Bash action. Safe socket canonicalization, deny roots and refusal to combine unsupported probe/setup confinement remain comparison inputs.

## Comparison criteria and failure consequences

Before any future policy refactor, capture both the existing identity bytes and effective prepared launch/settings for all twelve cells in the existing isolated harness. Do not infer settings equality from a hash alone. Future native observation requires the separate app-owned finite qualification authorization in MW-04.

| Dimension | Before/after equality required for unchanged TRIP | Causal failure case / expected consequence |
| --- | --- | --- |
| Provider/executable | Exact provider pin, canonical fingerprint, adapter, model and effort | Same version with changed contract is refused pending exact requalification |
| Launch identity | Serialized legacy `CapabilityIdentity`, normalized argv, permission/security policy, environment-key contract, hooks and compatibility binding | A policy-bearing argv/settings difference fails preservation even if a hash was retained |
| Codex native configuration | Permission base, approval ownership, temporary-root contract, denied-read floor, MCP/features/network proxy and all native hooks/timeouts | An outside/read-only write or forbidden socket action remains denied |
| Claude native configuration | Tool lists, allow/deny rules, generated settings JSON, restricted sandbox, hooks, strict MCP and singleton socket | Removing a denial or broadening an allowlist requires qualification; read-only write remains denied |
| Permission actionability | Only current TRIP Implementer can receive a human grant; exact action and safely represented family preview | Stale native ID, policy, account authority, boot, connection, workspace or generation cannot deliver a grant |
| Credential/channel scope | Project/task/attempt/generation/session/epoch/provider/configuration/lane and permitted operations; current credential and invocation binding | Cross-task/lane/session, revoked, stale or wrong actor reports are refused; human-only operations remain inaccessible |
| Retained history | Exact eligible conversation, current capability/settings, holds, active attempt, correlated hooks and positive quiescence | Missing native history or drift never falls back to a fresh or latest conversation |
| Fresh final verification | Fresh conversation for every frozen candidate and exact final-review request | Interrupted final verifier cannot resume or be relabeled fresh |
| Review and switching | Current role revisions, retained review accounting, 2/2/1 budgets and separately authorized dedicated final-repair recheck | Descriptive catalog additions cannot change spent counts, approval lineage or current authority |
| Snapshot/result authority | Full original base, candidate HEAD, dirty/untracked bytes, deletions, modes, bounded symlinks and all writers quiescent | Active/unresolved writer or changed candidate blocks freeze and stale acceptance |
| Historical provenance | Exact stored workflow/upstream/overlay hashes, approvals, legacy migration flags and acceptance history | Missing definition bytes remain unavailable; current bundled bytes cannot reconstruct old authority |
| Process/resource ownership | Native identity, PTY ownership, Stop/quiescence, claims and build/capacity reservations | Uncertain launch/cancel cannot release ownership or dispatch a duplicate |

Normalize only the transient paths, prompt and native resume identifiers already normalized by `src/providers/mod.rs`; never erase a policy-bearing difference to make a comparison pass. The new Single agent actor has a new policy identity and admission; it cannot relabel an Implementer proof. New catalog/profile labels carry no permission authority.

Environment comparisons record names and contracts only. Codex names `AGENTICJIRA_PROVIDER`, `AGENTICJIRA_ROLE_SOCKET`, `AGENTICJIRA_ROLE_TOKEN`, `AGENTICJIRA_ROLE_GENERATION_ID` and `AGENTICJIRA_SESSION_ID`; Claude also disables native automatic updates. Credential values, native account contents and raw environment values are excluded. Preserve the permission response fences in `src/permissions.rs:1078`, including current credential, generation/provider, native session, policy, service boot, connection, deadline, configuration and canonical repository/workspace identity.

## Activated identity baseline

Configuration revision `b4147f19-6849-4fb6-a3aa-ec9c1176ab1e`; project configuration hash `296d23edaadf54a627b63275e770aca01d2e6b69a7cae2508875a0e92a4160c6`.
Each role below is at settings revision 1 with source `project_default`. The context marks the Implementer binding effective for this generation. Other bindings are configured task authority, not evidence of live sessions in this turn.

- Workflow version: `trip-explorer-0.12.0-llmrelay-1`.
- Workflow authority hash: `3aa88edf5d634306db41f61bd251efabc7e5fbfae15e2895d6e76f75661d366e`.
- Upstream source authority hash: `0b2f1cce2be37220071864c604de7bc41ac58aaf962eeaa8803c8efa3da8e8eb`.
- Overlay authority hash: `d8742263c1b02d608d09706e08b0a2343eb2094e59a237c86ee7a35b67474bca`.

These activated authority hashes are copied exactly from the supplied contract/approved context. They are not recomputed or replaced by the raw source-file SHA-256 values below.

| Role | Provider | Model | Effort | Authority | Session | Adapter |
| --- | --- | --- | --- | --- | --- | --- |
| code_reviewer | codex | gpt-6.1-sol | xhigh | read-only | retained | llmrelay_codex |
| explorer | codex | gpt-6-sol | high | read-only | retained | llmrelay_codex |
| final_verifier | claude | claude-fable-5-1 | medium | read-only | fresh | llmrelay_claude |
| implementer | codex | gpt-6.1-sol | xhigh | workspace-write | retained | llmrelay_codex |
| manager | codex | gpt-6.1-sol | xhigh | read-only | retained | llmrelay_service_native_manager |
| plan_reviewer | claude | claude-fable-5-1 | high | read-only | retained | llmrelay_claude |

### code_reviewer

- Settings revision: 1; source: `project_default`.
- Profile hash: `ba8bf92170bdd5e982b8a0b858f8342939d20ba1ba1d697485c07fc306aaa708`.
- Capability key: `62d91935c2afde5a578bb8b9de783f885a58f5e9e480e06000be546b9ec05eaa`.
- Capability proof hash: `b2530e5ce51074a713d15d0bdf8608920972d54a59bca56e7dd7ad8daa501732`.
- Adapter hash: `0bce9512890e43109282cb58b1b084ce3386010faacbcadd7a3212c5156d3a90`.

### explorer

- Settings revision: 1; source: `project_default`.
- Profile hash: `0839877e189c88c0444895ebabd657793567037423c3f05303d85609b68c640b`.
- Capability key: `191e229348c72b9ac7baa8313b7f00fd3440804601f8bf4a60d7c068711bf5e2`.
- Capability proof hash: `977226f7c744d7dcec4b1d3e060021fbf6b137cb5a0a69e093d3276a73a943f8`.
- Adapter hash: `0bce9512890e43109282cb58b1b084ce3386010faacbcadd7a3212c5156d3a90`.

### final_verifier

- Settings revision: 1; source: `project_default`.
- Profile hash: `2286d7b6a663cc80572f2def7b61a568521ee0927b93a1d90c06994294c095d9`.
- Capability key: `7958227f981484f1c9410e7b76e55c2bbd45576ae353c339c04b2d29d91d0da9`.
- Capability proof hash: `b2530e5ce51074a713d15d0bdf8608920972d54a59bca56e7dd7ad8daa501732`.
- Adapter hash: `11d69d3dab56ac43779bbf3b60ab54a6a00f1581bad3a90c12a3156c7bf0442d`.

### implementer

- Settings revision: 1; source: `project_default`.
- Profile hash: `25877491ece7d27d1bf9d67ee17e688130f38240d4e409b462d94de548a2d735`.
- Capability key: `0e06e300251f5b027e13962d5f55334be31ae9f79c9c481abb4e8800fce7dea1`.
- Capability proof hash: `d2d3dad0a67f48bccf451a6c218a016fc8facc5a88555acb24b46085bad8226d`.
- Adapter hash: `0bce9512890e43109282cb58b1b084ce3386010faacbcadd7a3212c5156d3a90`.

### manager

- Settings revision: 1; source: `project_default`.
- Profile hash: `58a4d1650438e642fe289d8abbb7e9cbf297955b99a8f4ac83e572f801123440`.
- Capability key: `551c7b648cfe4d44e73f18c52c2368565d9f4dd2d7855d0b23ccdc25768017b4`.
- Capability proof hash: `b2530e5ce51074a713d15d0bdf8608920972d54a59bca56e7dd7ad8daa501732`.
- Adapter hash: `017204044b94aaf1df05fa9359860e263ba20f5484f7473f88088014033031fc`.

### plan_reviewer

- Settings revision: 1; source: `project_default`.
- Profile hash: `7506dc862e67439672f6e2bc9b664cd4c89a4293a81d89c1fd8940b7a6d96a06`.
- Capability key: `644d997eae3b0d8b83f78e63f5dc811e0e8b952a439983ca6ad5dbb63a545e02`.
- Capability proof hash: `b2530e5ce51074a713d15d0bdf8608920972d54a59bca56e7dd7ad8daa501732`.
- Adapter hash: `11d69d3dab56ac43779bbf3b60ab54a6a00f1581bad3a90c12a3156c7bf0442d`.

## Regression anchors

The following cases were located as source inventory in `tests/contracts.rs`; none was executed by this documentation lane:

- Admission/materialization: `trip_uninitialized_admission_rejects_scheduler_and_direct_store_launch`, `trip_materialization_and_profile_drift_fail_closed`.
- Channel/native report scope: `trip_runtime_scope_exactly_fences_project_and_task_authority`, `trip_runtime_native_hook_commands_bind_report_acceptance_and_preserve_sibling`.
- Review/legacy/lane history: `trip_review_budgets_and_final_sessions_preserve_fresh_only_compatibility`, `trip_legacy_migration_preserves_review_accounting_and_lineage`, `trip_disjoint_lanes_keep_independent_credentials_and_require_quiescence`.
- Current compatibility and resume: `m7_same_version_contract_change_rejects_actual_resume_with_typed_category`, `m7_retained_launch_and_resume_share_exact_bound_identity`, `exited_sessions_offer_resume_only_while_current_and_unfinished`.
- Accounting/rework: `t08_review_resume_replacement_and_extension_account_exactly`, `t09_rework_is_idempotent_materializes_full_candidate_and_fences_carry`, `second_final_review_requires_the_approved_dedicated_recheck_of_the_candidate`.
- Permission fences: `t18_permission_decision_is_idempotent_one_shot_and_restart_fenced`, `t18_permission_rules_match_only_current_verified_scope_and_reservation`.

Current `frontend/src/flows.test.tsx` contains T14 inheritance/failed draft, T15 role and human-review authority, T19 permission inbox, T21 ambiguous mutation reconciliation, and Active/Completed/Ready semantics. These expectations remain authoritative when later tasks adapt legitimate interfaces.

## Explorer evidence and app triage

Historical planning decision 59c3e7ca-f615-4b74-b262-c402880a1aac, contract_change, activated=true, bounded 800-word compatibility question. The service context subsequently reported evidence_submitted=true. It does not expose the accepted Explorer summary/evidence/outcome, so no Explorer finding or approving verdict is claimed. The manager independently grounded decisions with current source after this gap was observed; Explorer evidence remains non-approving service history.

### D1: transport evidence-access gap

invoke the exact role record-explorer-decision under planning with the configured contract_change census, then the exact role context after completion. Actual context includes decision_id/stage/trigger/limits/evidence_submitted=true but no outcome/summary/evidence or supported fetch command. Source src/operations.rs:2037 projects only evidence_submitted. Expected manager-readable bounded accepted findings so they can be incorporated. Redacted reproduction omits channel/account material. This is recorded for app triage, not repaired or bypassed in MW-01; plan does not depend on inaccessible findings.

### D2: prompt drift

src/coordinator.rs:1681 requests findings_ready while activated reporting contract lists evidence_ready. Explorer evidence was submitted, so no runtime failure is inferred; record the literal mismatch for a separate bounded app correction.

### D3: provenance wording drift

the transport prose says embedded upstream 0.11.0, while checkpoint src/trip.rs and resources/trip-explorer/0.12.0/source-manifest.json identify 0.12.0/a3edbad8b10953ea9dbdba39b8d3523713da3e5c. Preserve the activated workflow/upstream/overlay hashes; do not normalize them or infer a new authority from the version wording. The baseline distinguishes these observations. These triage items are known limitations, not open architecture choices or authority to change the supervising host.

The MW-01 implementer reproduced the limited D1 context projection and inspected its source without repeating the manager-only Explorer-decision mutation. D1's original activation sequence above is historical manager evidence. D2 and D3 were independently confirmed by source reads; no live failure is claimed. Expected triage disposition is a separately scoped app task with reproducible actions and redacted evidence. No supervising-host repair is part of MW-01.

## Raw source content anchors

Collected with direct `shasum -a 256`; these identify regular repository files, not live accounts or native state.

```text
5bf1a001b588596fbab6621bc85bdeb88df27d71396b9e7ead50b68e36315ed7  src/domain.rs
014570c3e89675088a24197014205ad422a9e82428317aff09c5c336f4dde280  src/providers/mod.rs
0d15c4683e9a5304b3c1de0855fbccc2aad83e77cca269db1e296bb49f3e22bf  src/providers/codex.rs
56f2b5b2ef4c833c95eb8b198f5a7a798a91458dfe1a1f2a57845ace37c99dd5  src/providers/claude.rs
1802a507287ba2dbea52eda2517d54b6d600b7d7595f24d5aec025a93bc7eb96  src/provider_compatibility.rs
23e2fb63d5e1da5ad1997cabf4d797827ce76138ae57e96d7d7bfc8955f25ef5  src/permissions.rs
788d9432be0b98f030a63b90b4bbc872cde13aae4e0ae0c67bbb01d3667f23bc  src/trip.rs
895203c80ef412a65a08faf0752d7cc27b0ab40a1e7e9e77f7ba271c1d5e5578  src/roles.rs
900acfd9bd841e09c7cec2a5eae6f7537be1186f47119f07ad9fa4a4ae6ab0e6  src/recovery.rs
fdee930968497e67a96bc825f3f8d6a4bd3206f48d52a8b5b6f34665966b1472  src/store.rs
a4fff45b12454376851b635dcf4dd2793427e62cc3d915c42b717085a244fdd5  src/scheduler.rs
95b14250b053d21c0dc69ea793c047af654d9c4798581fb206363c904cd244d6  src/coordinator.rs
257cf1ccfb5afae40371c9a69c776b3238d046b3c1a069d4fecfcced8b4bd1ff  src/operations.rs
0cd81e8601dc6bef512b18096ec8974b53e776b238ac52d0ab64cc69536681b3  src/workflow.rs
544f23d34ea87c65a2b6b1beaf3979ec9779a1b7914443f00a480147c6875ee9  src/review.rs
4124951d760015069e55666b94b4b9fa62681a56f7903345b74a329f1f9dea20  src/snapshot.rs
df5147872b46b483eb3730c44e086cec723e36babe5181e00b45eb56fd5fa02c  resources/provider-compatibility/codex.json
1de65ba948e8c08c43f8f90c742bdc1f176728e37d4e86b1c0575f4e334b5282  resources/provider-compatibility/claude.json
0b2f1cce2be37220071864c604de7bc41ac58aaf962eeaa8803c8efa3da8e8eb  resources/trip-explorer/0.12.0/source-manifest.json
7834cb92440de0f38025e20ddde0ba2fff5fb3a89d85a841e9552204067fd2be  resources/workflows/trip-explorer-0.12.0-llmrelay-1.json
ed8602a401935aaf21852e9cf5053d26928277df7ea62b6b28cb3fc3a3a86127  resources/prompts/trip-overlay.md
```

## Qualification still required

Source inspection, historical proof hashes and documentation do not demonstrate new policy behavior, native permission denial, migration execution, Single agent admission/result handling, actual retained resume or browser clicks. Gate A uses isolated causal tests, Gate B uses service-owned host checks, Gate C observes authorized native/dashboard behavior, and Gate D requires independent code review, manager conformance and fresh final verification before human acceptance. Codex qualifies first; Claude remains visibly unqualified for Single agent until separately proved. No new paid native budget is authorized here.

The selected MW-01 checks are revision 1 under configuration `b4147f19-6849-4fb6-a3aa-ec9c1176ab1e`: build/Rust check `3449578b-7d7c-4574-a040-5ff0c2942871` and DOM check `133320df-738c-4bdb-8f3a-3d7ea36fe8c0`. The lane did not execute either. Their authoritative output and exact frozen-candidate receipts belong to the service's one build/check slot.
