# Project setup

[Back to README](../README.md)

In the dashboard, choose **Add project**, enter a project name and the absolute path to an existing local Git repository, and submit the form. Field labels and validation explain missing values before submission; the service then validates the physical repository identity and current revision. A relink is accepted only for another path to the same physical repository and only while no attempt is active.

Continue through project setup before running tasks. The five stages organize the existing approvals:

| Stage | What to do |
| --- | --- |
| Discover | Inspect existing files, choose and launch the discovery manager, and follow its actual session output. |
| Project settings | Review suggested guidance, documentation policy, and verification commands. |
| Agents | Choose delegated role profiles; after saving the proposal, authorize and run the selected profile checks here. |
| Review changes | Review preservation choices and save the proposal. After profile checks pass, inspect and approve the exact installation files here. |
| Activate | Apply the authorized installation, then authorize, complete, and publish the separate runtime verification. |

Unsaved edits are retained in this browser for the same project and setup revision, including when you open session output and return or reload the same dashboard address. Saving the proposal or cancelling a correction clears its saved draft; a different setup revision does not inherit stale edits. This browser copy is not a server-side approval.

Stage navigation does not approve anything or mark a check complete. The current server state determines the initial stage and available actions. Corrected configurations and interrupted setup retain their specific recovery and approval steps. Setup and runtime-admission cards rehydrate their cmux notices and controls from the current durable `cmux_surface` projection whenever the dashboard refreshes or reloads. A newer requested control revision is shown as pending before any historical actual-control value, so it never grants newly rendered local input authority. **View output** creates or focuses the selected live session's exact mode-neutral cmux surface in view-only mode; ended sessions show recorded output. **Take keyboard control** sends an explicit revision-bound acquire request to that same authenticated surface. Neither action starts another provider, resumes the setup workflow, or resumes a native conversation.

If saving is unavailable, read the displayed prerequisite. In particular, selecting a manager is not enough: its discovery invocation must finish and record its result before the proposal can be saved.

### Choose an exact model

Model fields retain manual entry and offer **Refresh local suggestions**. For Codex, this reads the installed CLI's local model metadata; it does not contact a provider or start an agent. Select the exact model ID from a suggestion explicitly. A display name or short alias is not necessarily an accepted CLI model ID. The picker reports unavailable or stale metadata and leaves your entered value intact.

Claude uses existing exact profiles and manual entry when no trusted local catalog is available. Suggestions do not prove account access or replace the required capability checks. Changing the provider, model, or reasoning effort remains an explicit configuration change; refreshing suggestions does not alter a running or frozen profile.

### Stop or correct the discovery manager

Before saving a setup proposal, the Discover stage shows the discovery manager's current, requested, and effective profile, together with its next available action.

1. If the manager is still live, choose **Stop discovery manager**. The service blocks further launches and retires the old manager's authority before requesting an interrupt. It stops only that manager; an interrupt request is not proof that the process has exited.
2. Wait for positive quiescence. Until the process is confirmed stopped, Change and resume remain unavailable with a reason. If the manager already exited after an invalid-model error, this step may already be satisfied.
3. Enter the corrected provider, exact model, and reasoning effort, then choose **Change discovery manager**. This creates a fresh discovery revision while retaining the old setup and session history. It does not resume the old conversation with a different model.
4. Choose **Launch manager discovery** explicitly. Changing the profile alone does not spend another agent session or start discovery.

If interrupt delivery fails, the dashboard shows the recorded error and **Retry Stop discovery manager**. Retrying is an explicit new request and is allowed only while the exact recorded manager process identity still matches. A changed identity requires reconciliation; the service does not retry or replace the manager automatically.

A stale-version rejection preserves the edited fields so you can review the latest setup state and try again. Once a proposal has been saved, use the existing configuration-correction and review steps; discovery-manager replacement does not bypass those later approvals. Ordinary task-manager controls are described in [Workflows](WORKFLOWS.md).

After **Stop discovery manager** has positively confirmed exit, **Restart discovery manager** can prepare a fresh discovery revision with the same agent settings. The stopped session stays in history and its authority remains revoked. Preparing the restart does not launch an agent; choose **Launch manager discovery** separately. This also recovers an initial session that finished before newly trusted hooks could record its native identity.

Keep the LLMRelay executable and its data directory outside the repository being initialized. Setup isolates its empty fixture from the target repository and its Git common directory; a development executable built inside that same target cannot also remain accessible to the isolated agent. If this layout is detected, setup explains which location needs to move before launching a probe. Other repositories can still be initialized from a development build when their paths do not overlap.

1. Review the detected TRIP installation and project guidance. A new project needs initialization; an existing installation may need adoption, conflict resolution, or a reviewed upgrade. Existing customized files and alternate skill roots are preserved.
2. Review the discovery manager’s suggested guidance and check commands. **Apply suggestions to draft** copies those policy fields into the editable form; it does not save or approve them. Choose the project test policy and each delegated profile explicitly. Existing draft edits change only when you apply a suggestion or edit the fields yourself. Unsupported adapters remain visible with their reason; the app does not silently substitute another model or provider.
3. Review and authorize the required live profile probes. Setup shows the exact profiles and expected fresh-session cost before spending CLI allowance. Discovery and probes run in a private empty fixture with narrowly scoped authority; they do not grant ordinary task execution in the target repository.
4. Review the proposed guidance and installation changes, including exact destination paths and existing contents. Approval to run probes and approval to install files are separate decisions.
5. Apply the approved installation. Installation validation and ordinary runtime readiness are separate. An interrupted or conflicting installation remains blocked with recovery details; it is never treated as a completed setup.
6. Prepare the missing ordinary runtime capability checks from Project Setup or Role Settings. Review their exact profiles, scope, and fresh-call count, explicitly authorize them, and launch the selected checks. They run in app-owned empty worktrees. Observe their output and any trust or permission requests in Workspace, then explicitly publish valid evidence. Setup probe receipts cannot substitute for these checks because the effective security policies differ. Tasks remain pending until their required current profiles have valid runtime evidence.

Retained runtime checks first exit after observing their bounded commands, then resume once to report from the same native conversation. The fresh final-verifier check does not resume. If a reporting invocation exits without an accepted report, the check becomes failed once the service confirms that its processes have exited. Use **Prepare corrected runtime verification** after addressing the failure; this creates an explicitly authorized fresh scope. It does not replay the old observations or turn the failed check into a pass. Accepted evidence still requires a separate publication action.

The canonical app-created paths are `.agents/skills/` for skills and `.agents/trip-explorer/` for project configuration, adapters, preflight, manifest, and base resources. Optional Python helpers are retained for upstream compatibility; the installed LLMRelay application does not require Python. Upgrades are explicit and compare the previous base, active project contents, and selected new base. The app does not automatically fetch or activate changes from the source checkout.

Install and sign in to each provider through its normal CLI before use. New projects start with their queue paused. LLMRelay sets `DISABLE_AUTOUPDATER=1` and `DISABLE_UPDATES=1` in each managed Claude child and its app-owned settings, including Claude version discovery: the first suppresses background updates and the second also blocks manual update/install paths in that process. This is process-scoped; user/global Claude sessions and manual CLI maintenance remain unchanged. See Claude Code’s [disable-auto-updates setup](https://code.claude.com/docs/en/setup#disable-auto-updates) and [environment-variable reference](https://code.claude.com/docs/en/env-vars). Adding a repository does not grant native provider trust: when a launched CLI asks about the folder or new or changed hooks, choose **Take keyboard control**, review and answer its prompt in the same persistent cmux surface, then press **Ctrl-]** to detach and release control. Trust decisions belong to that CLI; a different worktree or changed hooks may require another prompt. LLMRelay does not automatically accept trust for newly added projects.

See [provider compatibility and permissions](SECURITY.md) for supported Codex accounts, exact CLI versions, and policy restrictions.

Provider profiles remain `Unverified` until matching local preflight succeeds. An imported upstream preflight document is validated as installation data, but cannot by itself prove the local executable and application sandbox behavior. Changing a profile creates a pending configuration revision and requires matching evidence before that profile can run. Failed or declined probes preserve the setup draft and do not activate the project.

During supported Implementer sessions, the Workspace **Approval inbox** handles native action requests the CLI actually emits with **Approve once**, **Always approve matching actions**, and **Deny**. Codex honors approvals already granted through its native policy, so a natively allowed or sandbox-allowed action may run without a new inbox item. LLMRelay does not enumerate, create, edit, or revoke native Codex approvals; dashboard **Revoke** affects only LLMRelay-owned reusable rules. Workflow approvals, review-budget changes, role switching, and final acceptance remain separate human dashboard actions. Trusting a folder does not grant those actions to an agent.



## Existing workflow files

The setup page lists detected workflow locations and asks how to preserve the original content. Both supported resolution values currently install the pinned workflow at the canonical `.agents` paths and preserve originals of destination files that change. They do not automatically merge custom instructions. Preserved files are written under `.agents/trip-explorer/preserved/<setup-id>/`, retaining their original relative paths. Selecting a resolution changes the proposal only; file writes require the later exact installation approval and apply action.

## Capability checks and setup recovery

Normal onboarding uses project setup's explicitly approved profile probes. Each probe is bound to its setup operation, selected profile, fixture, generation, executable, and sandbox policy. It runs in the app-owned empty fixture and cannot activate ordinary task authority in an uninitialized project. Setup discovery and the subsequently selected-profile probe attempt have separate immutable configurations. Live calls use the installed CLI accounts and their normal allowance; a failed probe remains unverified and is not silently retried.

Retained setup discovery and profile probes use exactly two turns. After the first turn reaches a strict native `Stop` boundary, the service verifies the exact managed session and process inventory before marking it idle. It then stops that completed first invocation without creating a replacement-manager control or spending the retained resume. The session must exit with verified process-group quiescence before **Resume retained session** becomes available; the human starts that second and final turn explicitly. The service-owned stop receipt authorizes a new invocation credential only for that one exact resume. At the bounded stop deadline, an attached child is reconciled first and an unattached generation is checked against fresh operating-system PID, start, process-group, and boot evidence. A verified exit wins the race; a proven-live or unverifiable generation enters explicit recovery without another signal. **Stop discovery manager** remains the separate replacement action and does not stand in for this service-owned first-turn completion.

On a first Codex launch, open the session output to review the native folder and hook trust prompts. Trust applies to the displayed fixture and configured hooks; it does not approve arbitrary agent commands. If the initial session-start hook ran before hook trust was established, LLMRelay cannot safely identify that conversation for resume. Stop that session, wait for it to exit, then explicitly choose **Launch exact-profile retry** after reviewing the hooks. This starts another CLI session and uses its normal allowance; LLMRelay retains the first session's history and does not manufacture a session identity or retry automatically.

If a setup or runtime probe reports unresolved process ownership, its project setup view provides **Verify quiescence and reconcile**. Enter a short recovery note and let the service check the recorded operating-system process identities. The note cannot override a live or unknown process. Successful reconciliation refreshes the view and leaves the old credential revoked; a separate eligible resume receives a new credential bound to that exact invocation and its current setup or runtime authority. Use **Resume retained session** for setup or **Resume same native session** for runtime verification. A stale profile, consumed permission, superseded admission, or fresh-only final verifier cannot be resumed through recovery.

After installation, ordinary runtime checks establish evidence for the policy used by task roles. Preparing the scope or editing a setting launches no agent. Human authorization, launching a check, and publishing its evidence are separate actions. Publication is bound to the exact current profile, configuration, adapter, native session, and observed policy. Only selected installed providers are required. A changed executable or proof may require fresh evidence and explicit task-profile reactivation; historical evidence stays available for diagnosis.

When more than one runtime verification group exists, use **Runtime verification group** to return to earlier authorized checks. A correction for one role does not hide the remaining roles in an earlier group or authorize another launch.

For Claude requalification, the runtime-check form asks for the currently live
cmux Unix-socket path. Run `cmux identify` from your cmux terminal and use its
`socket_path` value. The path is frozen in the authorized probe scope; changing
it requires a new preparation and authorization. The connection probe sends no
cmux control request. A missing endpoint or an inconclusive result leaves the
profile unverified. See [Claude terminal confinement](SECURITY.md#claude-terminal-confinement).

Ready admission requires current ordinary proof for all six effective roles. If evidence is missing or stale, creating or updating a Ready task preserves the requested work as a pending backlog task. Runtime reports must include structured observations for every prescribed command; publication checks those observations and rechecks the approved scope against current settings and configuration. A stale report remains historical evidence and cannot grant current authority.

Ordinary task context exposes the activated project policy, exact configured check catalog and IDs, current check selection, and the accepted input shapes for the role's workflow commands. These descriptions do not authorize checks or supply agent results. Guidance identifies whether the current native invocation has actually received a correlated submission and may acknowledge it; queued or merely written guidance cannot be acknowledged. Project guidance paths are listed, but their file contents are not embedded; agents must report unavailable guidance rather than invent its contents. Setup-only reads are advertised only during setup discovery.

Ordinary runtime probes submit observations with the runtime-only `role report --runtime-v1` argument form, avoiding nested JSON and shell quoting. The native agent supplies its observed command outcomes, model evidence, and session observations; the CLI sends them through the existing authenticated role socket. Operation IDs bind those observations to the prescribed commands without treating server-owned command text as an agent observation. This form grants no additional execution permission and is rejected outside a runtime probe. General workflow reports keep their existing JSON interface, and previously recorded runtime reports remain readable and subject to the same publication checks.

Role Settings shows support for the exact project or task scope. Evidence for the same model in another scope does not make the selected role usable. Correcting an inherited project-default role prepares project-scoped evidence; a task override keeps its exact task and settings revision. Explicit task-profile activation remains separate from proof publication. Publishing fresh evidence after invalidation restores only the newly verified scope.

Ordinary runtime verification identifies configured controls using the service-recorded frozen launch-policy identity, checked against the reporting session and again against current policy before publication. This identifies configuration; it is not OS enforcement attestation. Exact native probe outcomes remain separate observations. An agent-supplied sandbox description is optional supplementary information and cannot replace the policy identity or probe checks; supplied malformed descriptions are rejected. Setup preflight uses its separate, unchanged reporting contract.

Retained runtime probes resume with a service-generated recall instruction that does not repeat the original nonce. Their first action must report remembered observations before reading files, role context, or external history. This rule takes precedence over generic provider context instructions, and the service refuses runtime context disclosure for the current resumed invocation. An observed failure or `missing_context` leaves admission unsupported; neither can publish proof. Final-verifier probes use a fresh invocation and cannot resume.

The first retained invocation must finish its exact probes without reporting. An observed premature report makes that check fail instead of offering a recall. Probe commands cannot add shell wrappers, status echoes, or other tokens. Report acceptance and publication compare the prescribed attempts with the scoped hook records, including command counts, recognized shell tools, and the exact service executable for role commands. A fresh final invocation must have exactly one report attempt. A permanent mismatch prevents publication and preserves the report history for diagnosis. Hook payloads remain untrusted: matching them is a consistency requirement, not independent proof of execution or sandbox behavior, and it does not replace the other capability or human-approval gates.

After a completed failed check, choose **Prepare corrected runtime verification**. This creates a new pending scope with its own challenge and cost approval. The failed session and consumed permission remain history; the app does not reset them or automatically retry a paid call.

Imported preflight files are installation evidence, not proof that a current local profile is Supported. Actual local evidence must cover the required role authority and native session semantics. Ordinary roles retain their own conversation; a final verifier always starts fresh for its frozen candidate. An interrupted final review remains incomplete instead of being resumed and relabelled fresh.

The older `capability` CLI remains an advanced diagnostic surface. Use the runtime verification controls to launch or resume an ordinary runtime probe; generic setup dispatch and diagnostic capability resume are not alternate routes for those sessions. Historical L01–L10 receipts belong to their original workflow and instance and cannot be reused as proof for the new TRIP contract. Inspect current command help before a bounded diagnostic; neither a diagnostic report nor provider terminal prose can grant human installation, implementation, or acceptance authority.

Inspect an existing session and its captured public output without launching a provider. These examples use `llmrelay` on PATH; otherwise substitute the full path to your packaged executable:

```sh
llmrelay capability --data-dir "$PWD/.local/my-agenticjira" inspect --session <SESSION_ID>
llmrelay capability --data-dir "$PWD/.local/my-agenticjira" transcript --session <SESSION_ID> --raw
```

The dashboard retains local failure details for diagnosis. The sanitized export retains safe setup-operation correlations, profile and source hashes, stages, normalized failure categories, and recovery state; it omits unrestricted failure text and local paths. Neither stores credentials or hidden reasoning. A copied installation or another project's success cannot silently satisfy a failed local preflight.

## Interrupted setup and workspace recovery

If the service stops while applying an approved installation, startup records
**recovery required** without modifying project files. **Recover the authorized
apply journal** verifies the approved preimages and already-applied bytes before
continuing remaining journal entries. Changed destination content remains a
conflict; recovery does not silently overwrite it.

An uncertain worktree reservation holds the workspace, repository claim, attempt
and task together for recovery. A failed first repository inspection or identity
check after reservation records that complete tuple immediately; a service
restart is not required to expose recovery. **Inspect and retry workspace reservation**
checks the recorded repository, path and base revision. It can recognize the
expected existing worktree or recreate an absent reservation under the same
current authority. A mismatched path stays in recovery.

Recovery evidence identifies the failed stage: `repository_inspection` retains
the inspection error; `repository_identity` retains the expected and observed
repository identities and observed base revision. `policy_materialization` is
reserved for failures while materializing the workspace policy. Successful retry
or cancellation resolves the reservation record and removes its old recovery
actions from the refreshed task.

**Verify and cancel reservation** checks ownership, abandons the reservation,
releases its exact claim and cancels the attempt/task without deleting worktree
contents. Any surviving path is reported for separate cleanup. Startup cannot
reactivate a cancelled attempt's workspace claim. Generic Cancel is not a
substitute for this reservation-specific recovery action.

If **Resume retained session** is rejected because the executable or another
frozen runtime binding changed, the refreshed setup card explains the rejection.
**Start fresh accounted setup session** appears only while the same selected
profile, current setup permit and remaining authority allow it. Changed profile
or setup authority requires its own replacement/verification flow. Runtime
probes instead use **Prepare corrected runtime verification**; final-verifier
invocations remain fresh-only.

An adapter update alone does not qualify existing profiles. After a CLI upgrade,
complete the current setup/runtime verification for each exact role identity you
intend to activate. Historical Supported rows remain evidence for their original
identity and cannot substitute for current proof. A failed retained runtime probe
uses **Prepare corrected runtime verification** rather than silently restarting.
