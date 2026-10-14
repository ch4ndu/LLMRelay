# Native provider qualification evidence

This record covers native contract qualification begun on 2026-10-11 and the separately identified ordinary app checks below. Contract qualification, project admission and user-task acceptance are distinct gates.

## Codex Single agent writer

Independent code review on 2026-10-12 found the retained evidence sufficient for `llmrelay-codex-policy-modes` revision 3, `codex-single-agent-writer-v3`, with Codex CLI 0.162.1. The selected model was `gpt-6.1-sol` with `xhigh` effort. This evidence permits qualification of that writer contract only. Each real app profile still requires its own current admission and capability proof.

The first and resumed native turns demonstrated:

- A permitted write inside the owned worktree and recall of the same nonce after resume.
- The same native conversation and role generation, a new transcript epoch and credential, and revocation of the previous credential.
- Authenticated lifecycle hooks after the exact ten LLMRelay hook definitions were inspected and trusted through the native Codex interface.
- Native operating-system refusals for original-repository and service-data reads and writes, and for the human-control socket; the app separately rejected an out-of-role transition.
- Completed interactive turns, deliberate interruption for the retained-session check, verified process-group cleanup, and bounded listener shutdown.

The diagnostic intentionally left its independent-review checks false and returned exit 101. It is not recorded as a passing automated test. Separately retained native tool results, hook records and the manager's attributable observations supplied those checks; independent review assessed the combined evidence. Planned interruption after completed turns does not establish spontaneous process exit or an ordinary task's final acceptance.

The writer run used application executable SHA-256 `406e03b398c972c99a5ebb23b9e1c0f7c9e31c9b853900cb238a754769b55b76`. Subsequent reviewed changes concern startup diagnostics, PTY output capture, observation-only MCP configuration handling and tests. The native writer policy, executable, prompt, hooks, authenticated handlers and retained-session preparation are unchanged. The review accepted reuse of those writer facts with this boundary; they are not represented as a fresh writer run of a later executable.

Policy pack revision 4 advances only the Codex observation contract, launch and native-policy revisions to v3. The qualified `codex-single-agent-writer-v3` contract, launch and policy remain unchanged. Its retained evidence carries forward under the historical-binary reuse boundary above; this pack change records no new writer run.

| Retained evidence | SHA-256 |
| --- | --- |
| Native writer record | `ebdb53d95ec2016c2f63da234346664d7dd745b5c933b865be666f4b236bb93e` |
| Native tool evidence | `74c222d883ce59c91dc0ab27ab15ee511d8b53f913e41cef8c711bf538722ca6` |
| Manager adjudication | `dbbf41325248a786ac906a8bc65a656521e48466a954eb598df210b0c42cddca` |
| Independent evidence review | `4acaaa51284a3ae886e1a19886e34bb79a432332b85d57c505aa908f1d028e91` |

Raw evidence stays in the ignored qualification ledger; it is not distributed with the repository. The pinned native executable SHA-256 is `74c6a6d263f40d65f99c27a815275f5b4ed0bcc2baecc396b4cf3255498bdfc0`.

## Claude Single agent writer

Independent evidence review R10 on 2026-10-12 found the combined retained records sufficient for `llmrelay-claude-policy-modes` revision 2, `claude-single-agent-writer-v2`, with Claude Code 2.1.289, model `claude-opus-5-5` and `high` effort. R5 established the writer's enforcement and retained-session behavior but left hook-trust provenance unresolved; R10 closes that specific gap. This qualifies only the exact writer contract. Each real app profile still needs its own current admission and capability proof. The ordinary Claude checks below used a separately admitted profile.

The original first and resumed turns demonstrated:

- Permitted worktree writing and retained nonce recall in the same native conversation, role generation and session, with a new transcript epoch and credential and revocation of the previous credential.
- Authenticated lifecycle and permission hooks and a scoped role channel; an out-of-role transition was rejected by the app handler.
- Native operating-system denials for original-repository and service-data reads and writes and for the human-control socket, plus provider built-in Read refusals outside the worktree.
- Completed turns, capture before planned interruption, exact process-group quiescence and bounded listener shutdown and joins. Planned interruption is not a spontaneous successful process exit.

The native interface and authenticated PermissionRequest records show twelve one-shot command approvals, six per turn, without saved rules or automatic approval. These command permissions are separate from hook trust. The model's contrary claim that no permission prompts occurred is excluded as evidence.

The manager's review of all thirteen exact hook definitions and their runner was recorded at `2026-10-12T18:23:28.255119Z`. First-turn, resume and supplemental hook definitions and inline settings match; every definition invokes the reviewed runner with its exact event and timeout. The exact 2.1.289 native Status view identifies an interactive session and Command line arguments as its settings source. An existing trusted repository ancestor covers the fixture worktree, corroborating inherited workspace trust without bypass arguments. No historical trust-acceptance timestamp is available or invented, and this review is not represented as an earlier review or thirteen individual approvals. Current upstream trust semantics, rather than a pinned documentation snapshot, were corroborated by the exact native observations assessed in R10.

The `/hooks` menu did not display the thirteen inline definitions; its user-file entries are not proof of their contents. The corrected settings inspection used changed diagnostic arguments and no app credential environment, recorded no assistant/tool events, and recognized an app-owned SessionStart that failed its required environment check. It supplies settings/trust evidence, not new authenticated writer delivery. The first inspection accidentally submitted diagnostic text as a prompt, produced a failed Bash attempt and required owned-process termination with verified cleanup. That operational mistake is excluded from qualification proof.

The original writer pair used application executable SHA-256 `406e03b398c972c99a5ebb23b9e1c0f7c9e31c9b853900cb238a754769b55b76` and retains diagnostic exit 101 / incomplete. R5 accepted historical-binary reuse for materially unchanged writer policy, pinned native executable, prompt, hooks, handlers and retained-session preparation; R10 binds the supplemental exact-definition and native settings evidence to that pair. This is not a passing automated test or a fresh writer pair on the current application binary. The native Claude executable SHA-256 is `03d66745e3bb69ec727d66023696f3820bc0a00a8a5ba725eb6706d0c67cbe69`.

| Retained evidence | SHA-256 |
| --- | --- |
| Native writer record | `38facf795d2fd13123167ced9f3a7e87aba9b99e5ccdddacbeb96861342d2066` |
| Native tool evidence | `9912d710b59cc89fd98bf4cb06a558cf7300c0830e27a8ac559ca19fa705c54c` |
| Original manager adjudication | `e4810af353fe8eb0f5362d3408c4c0d9a8920129e2d8c736dc9f301343087125` |
| R5 evidence review | `4acaaa51284a3ae886e1a19886e34bb79a432332b85d57c505aa908f1d028e91` |
| Exact inline settings | `20cdc07f89d7d752c8857f7477be73fc50fb7428a363424269dcda7b4031c0a8` |
| Canonical hook definitions | `1747499fa320178aeb92afc52e0b3e14abbf9acbe2f26831770dc99b9574cd33` |
| Hook runner | `3d4fbc808f90340a4cc2d26b8ba39b08ffc5ad1c9ef4b161ddf8bd5de1475d98` |
| Supplemental settings/trust adjudication | `5cc2980c05ef2e2b558dd8c4a7753cb53e1bbd5ccfa2a779fe01206ab0e8eb2b` |
| R10 evidence review | `5e4e6199d044f226cdfff98b0b9bf66e1a7d1db44e6a2b4ec51ba84480020738` |

Raw evidence remains in the ignored qualification ledger.

## Codex observation restriction repair

The original observation argv exposed `view_image`: the retained `observation-policy-diagnostics` record contains a native image-load attempt against an absent owned file despite the expected nonce response. That is counterevidence to no-tool exclusion. The `observation-policy-candidate` record shows strict configuration rejected `tools.view_image=false` before a native session; this key is not shipped.

Observation v3 adds `web_search="disabled"`, `project_doc_max_bytes=0`, `features.skip_host_skill_discovery=true` and disables `view_image`, `sleep_tool`, `tool_suggest`, `goals` and `workspace_dependencies`, retaining the existing observation restrictions. Exact Codex CLI 0.162.1 accepted these controls in `observation-policy-candidate-v2`, returned the expected answer, exited 0, left the scope empty and had no remaining process group. No visible plan/image event was recorded. These facts establish native parse and lifecycle evidence, not exhaustive effective tool or customization exclusion. `skip_host_skill_discovery` is explicitly under development in this pinned release; its warning is preserved, and stable or general support is not claimed. Codex observation remains Unqualified pending independent native evidence.

The later `observation-builtin-boundary` record shows the native router rejecting an `apply_patch` write under the read-only sandbox despite those controls. It returned the expected answer, exited 0 and left the scope empty, but the tool attempt is further counterevidence to absolute no-tool exclusion. The record's SHA-256 is `bec4f3692abae579df3de0db7362f4e4077eeebb9822a1b353328857f048b299`. This does not qualify observation or establish a remedy without additional evidence.

## Remaining qualification and product checks

Both observation modes remain unqualified. Both observation providers have returned the expected answer and settled successfully in candidate diagnostics using app primitives, with empty scopes and verified cleanup. Ordinary provider-check dispatch still refuses these unqualified modes. That alone does not prove every required native restriction. Observation tool/customization exclusion remains unresolved. Unavailable effective-model/account fields are reported as unavailable, and no claim is made about remote provider retention.

## Ordinary Claude task and controlled restart

On 2026-10-12, an isolated app instance using executable SHA-256 `e36da4d3fed41ad08d5570bc734b98735158787b22f90562acfbcea3a86dd1cf` completed a fresh Claude profile admission and an ordinary Single agent task with the exact qualified model and effort above. The task asked a genuine separator question before editing. The manager answered through the app, and the same native conversation resumed with that answer.

A deliberate `stop --drain` captured the running session before interrupting it. The old service and native process group were verified absent. After restarting the same instance, its already-enabled resume setting restored the same native conversation with a new transcript epoch and an authenticated accepted turn. The agent preserved the chosen underscore separator. This checks the documented service restart path; OS sleep, abrupt crashes and machine reboot remain untested.

The manager inspected the resulting Git diff and status: only the requested Python function and its test file changed. The app ran the selected check after a separate one-use authorization and recorded a pass against the captured result. Acceptance through the dashboard persisted after reload, with no acceptance button, no available workflow actions and no running native session. The disposable service and final native process group were confirmed stopped. Acceptance did not merge or commit the fixture changes.

The admission, result, check and restart records remain in the ignored qualification ledger. Ordinary Codex execution was verified separately. Neither observation mode is promoted by these writer results, and this record does not authorize publication.


## Ordinary TRIP task and final handoff

On 2026-10-12 (local time), the isolated task `AJ-EF337008` completed the selected TRIP workflow through the dashboard. All six selected roles first obtained current native runtime evidence without substituting the selected Code Reviewer model. Planning and independent plan review completed, followed by separate plan and implementation approvals. The full plan opened in a read-only task-sized window. Implementation, independent code review, both selected checks and fresh final review passed against the same captured candidate. Each check received its own one-use approval through the Checks tab.

Final handoff exposed a recovery defect: the current manager turn had more terminal tool events than start events. Strict hook validation correctly refused to infer an idle boundary, while the existing service-stop path handled only the opposite mismatch. The repair admits either-direction count mismatch to that existing stop path; it preserves identity and authority revalidation and requires confirmed process-group quiescence before advancing. A focused contract regression and independent source review cover this condition. The original stalled process was deliberately drained for the binary upgrade, so this run does not prove that the original process recovered automatically under the new code.

After the controlled upgrade, **Resume after restart** restored the same native manager conversation, and the task reached human review. Inspection found only `label.py` and `test_label.py` changed in the tracked source diff; the additional untracked `.agents` content was the app-materialized workflow installation. A single **Accept result** action persisted `done` with no attention, and every task-role process group was confirmed quiescent. After reload, the dashboard showed Completed, zero active tasks and no acceptance or mutation controls in the read-only task history. Acceptance did not commit, merge or publish the fixture changes.

The upgraded executable SHA-256 was `10ff434fc6213de5913989e352df8b6b5e3ce32e158398ca2db0e7dd7d37960a`. Its CLI successfully returned the accumulated 2,501,603-byte state. Request limits remain 1 MiB; business responses are bounded at 16 MiB. This proves the observed large-state case, not unbounded history support. The task, checks, reviews, controlled restart, native identities and acceptance evidence remain in the ignored qualification ledger. OS sleep, abrupt crash and machine-reboot recovery remain untested.

## Superseded app-wide coding-check investigation (2026-10-13)

This section records the abandoned app-wide coding-session proposal. The current
Providers screen performs local inspection only; it does not launch these checks.
The task-scoped replacement is described below.

The observation diagnostics above retain their original contract scope. They
do not qualify the revised app-wide disposable coding-session mode, and the
qualified Single agent writer/TRIP results do not grant it authority. Ordinary
dispatch remains disabled for the unqualified Codex check contract
`codex-provider-disposable-check-v1`; Claude remains independently unavailable
because managed-hook confinement is unproven. No new policy has received native
qualification.

The revised consent authorizes fresh session starts with the selected provider,
exact model, effort and installed version. A session may use coding tools and
write/read a temporary challenge file in its app-owned folder, and may consume
quota through multiple provider requests. The legacy `max_model_calls` field is
the fresh-session budget, not an internal API-request limit. No automatic retry,
model change or project admission follows from a check. Tool writes are intended
to remain scope-only; native OS/minimal-helper reads are accepted exceptions,
with explicit enumerated protected-root denials still required. Parent provider
authentication, network and runtime activity are separate.

Independent backend and diagnostic collector review and focused checks passed.
Those are implementation evidence. The diagnostic's direct-pipe Codex process
case can correlate one prescribed command and its complete native permission
results, but publishes no production session/access proof and cannot establish
Supervisor deadline or settlement behavior. The separately selected timeout
case retains that lifecycle path. The production deadline is 300 seconds per
invocation; uncertain stop waits for verified exit/quiescence, siblings remain
independent, and expired checks cannot publish success.

Native launch readiness remains unresolved: the prescribed shell and selected
Python interpreter, modules and libraries are not established under the exact
effective policy. The proposed offline sandbox smoke check also needs proven
configuration equivalence before it can justify a paid call. Accepted helper
runtime reads do not establish this prerequisite, and broad runtime-directory
grants are not a remedy.

Built-in filesystem-helper protected-read denial remains unproven. The pinned
JSON projection merges failed and declined patch status and supplies no helper
errno or denial reason. A failed patch can reflect refusal, malformed input or
another failure; it is not OS-denial evidence. A successful in-scope patch can
establish positive helper activity only. The process collector therefore leaves
helper protection false and overall evidence incomplete, even if its process
probes pass. This finite protected-root inventory makes no universal
host-secret-isolation claim. Current consent and operational boundaries are
documented in [App-wide provider checks](SECURITY.md#app-wide-provider-checks).


## Task-scoped Main checkout verification (2026-10-14)

The revised product supports explicit Main checkout consent alongside isolated
worktrees. App-wide Providers inspection does not launch a model or grant task
admission. Main checkout capability evidence is bound to the selected task, actor,
provider installation and effective scope; it is not a general provider approval.
See [Trusted task checkouts](SECURITY.md#trusted-task-checkouts).

In the disposable acceptance instance, all six selected role profiles obtained
current runtime evidence. Main checkout consent, Make Ready and task preparation
succeeded. The Codex implementer completed both native verification turns,
including the retained-session recall, exact challenge-file writes, authenticated
tool events, safe stops and confirmed process-group quiescence. Independent review
correlated the native public tool records with all eight required protected-root
denials. The service accepted the resulting proof for that exact task and actor.
This does not qualify another installation, role, scope or later binary.

Claude refused both native verification turns without tool activity or the recall
nonce. No Claude Main proof was published. The existing Pause now action safely
settled the diagnostic and preserved the task, checkout claim and Codex proof.
Selected Claude Main execution therefore remains an unresolved acceptance gap;
ordinary work requiring its proof cannot proceed. No fallback model or relaxed
permission policy was used. This run does not establish a complete ordinary TRIP
task through Main checkout.

The dashboard recovery path now explains that native evidence is still required
and offers Stop verification and keep task paused through the existing versioned
pause action. A recorded cancellation displays Stopped without proof. Regression
coverage verifies that a newer verification supersedes that historical message,
including after successful proof publication, and that refresh performs no retry.
These lifecycle fixtures use synthetic evidence and do not replace native Claude
qualification. The source checks, independent reviews and native acceptance records
are retained in the ignored provider_sandbox_check_20261013 ledger.
