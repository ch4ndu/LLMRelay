# Permissions and provider compatibility

[Back to README](../README.md)

This guide describes the implemented native CLI permission boundaries. Setup and ordinary runtime proof remain separate; see [project setup](PROJECT_SETUP.md).

## Recovery and decision authority

Dashboard state waits use the same browser cookie, Host and Origin checks as
ordinary state reads. Their service-identity/revision cursor is presentation
metadata, not approval or execution authority. Responses use the existing
redacted state projection. Waits are capped at 64; backing database reads retain
their capacity slot after cancellation until the read finishes. Shutdown closes
wait responses with a retryable failure rather than granting continued authority.
Every mutation still validates its own exact identity and revision independently
of the dashboard's snapshot or attention navigation.

Decision explanations and restart previews convey no launch or recovery authority.
Recovery mutations require the exact displayed record and matching task, attempt,
session and version; stale or mismatched requests cannot resolve another record.
Bulk restoration saves its bounded membership and operation receipt before
provider work, then revalidates each admission. A timer cannot reset ownership,
human approvals, consumed membership, failure limits or uncertain delivery.
Preview reads do not reconcile processes or change saved authority, and unknown
environment evidence is never reported as proof of safety.

## Supported Codex installation

The v1 Codex path additionally requires exact `codex-cli 0.157.1`, the ordinary native CLI signed in through file-backed personal ChatGPT Free/Plus/Pro credentials, no parent `CODEX_HOME`, no shared app-server control socket or cloud-config bundle cache beneath `~/.codex`, and no `/etc/codex/managed_config.toml` or `/etc/codex/requirements.toml`. On macOS the native managed preferences `config_toml_base64` and `requirements_toml_base64` under `com.openai.codex` must also be absent. LLMRelay reports an actionable preflight error for an unsupported form; it does not modify provider configuration, credentials, organization policy, or native approvals to make the check pass.

Codex implementer sessions restrict the inherited system temporary-directory
grants to read-only access. The assigned worktree remains writable, including
when it is located beneath a temporary directory. This prevents a temporary
host location from making sibling repositories or service data writable.
Earlier implementer proofs without this restriction require fresh verification.

## Claude terminal confinement

The embedded compatibility bundle accepts exact Claude Code `2.1.283 (Claude
Code)` for all six roles. Other versions require a reviewed compatibility
update. Matching the bundle makes a profile eligible for verification; it does
not mark the profile Supported or reuse evidence from another executable,
model, effort, role, or policy. Run the selected profile's setup and runtime
verification before assigning it work. Claude's `default` permission mode is
displayed as Manual; Implementers retain human permission ownership, while
the other roles use `dontAsk`.

Managed Claude sessions require the native sandbox, fail closed when it is
unavailable, disable unsandboxed command bypass, and allow only the canonical
LLMRelay `role.sock` Unix socket. The launch retains the role's filesystem,
network, tool and hook restrictions. Removing cmux environment variables alone
would not establish this boundary.

The policy change invalidates earlier Claude capability evidence. Historical
proofs remain available, but affected profiles require fresh exact runtime
verification before launch or resume. The capability gap identifies this next
step. A configured sandbox setting and a reported connection failure are
different evidence: qualification must preserve the prescribed native result
against a live, human-confirmed cmux endpoint. Missing or refused sockets,
timeouts, and provider refusal do not prove sandbox enforcement. This is not
an OS attestation or isolation from arbitrary malicious programs running as
the same macOS user.

## Action permissions

cmux supplies the agent's terminal; LLMRelay still owns workflow approvals.

| Request | Where to respond |
| --- | --- |
| Native CLI folder or hook trust, or an interactive question | The session's cmux terminal, while you own keyboard control |
| Supported native command/tool permission request | LLMRelay's Approval inbox |
| Plan approval, implementation authorization, setup/install approval, or final acceptance | The corresponding LLMRelay dashboard action |

Typing an answer in a terminal does not approve an inbox request or a workflow
gate. Taking keyboard control is also separate from granting an agent permission
to run a command. Native Codex approvals already in effect can still apply as
described below; opening cmux does not give the agent unrestricted access.

Agents should issue direct executable commands and use supported output capture instead of adding shell wrappers or redirection solely to save logs. This keeps eligible actions matchable against existing human-approved rules. Only the service and provider determine whether an approval applies; an agent cannot declare its own permission. A pending request holds the affected action, while independent eligible work may continue within repository locks, lane ownership, provider capacity, and the single build slot. A waiting process still occupies its actual resources; permission waits do not authorize bypassing those limits.

The Approval inbox identifies the project, task, role, native session, requested action, available access details, age, and status. **Approve once** applies to that single native request. **Always approve matching actions** opens a scope preview without submitting or saving anything. When the backend supplies a valid Project preview, **Project** is the recommended default and covers current and later verified engine-owned worktrees of that registered project; **Session** remains selectable for the exact native role session and worktree and is the fallback when only that preview is available. The human must explicitly confirm before LLMRelay creates a reusable rule. **Deny** remains available, and no suggestion grants blanket agent access. Any provider- or app-originated permission suggestion must preserve these same human choices; this policy does not introduce an app integration.

A reusable command-family rule identifies one stable, verified absolute or project-relative executable and covers all current and future arguments. Changed filenames, counts, flags, targets, and configuration values therefore do not require tiny new grants when the same executable family remains faithful. The preview shows the executable family, all-arguments coverage, provider and role, complete validated configuration binding, and either the registered Project boundary or exact Session worktree/native identity. The same generic matcher remains in place for every provider; there are no command-specific families. Compounds, redirects, substitutions, shell or interpreter wrappers, different executables, unsafe paths, and ambiguous input cannot inherit a reusable rule. A completely represented action outside the matcher may still receive exact one-time human review. Bare PATH names and shell builtins whose executable identity cannot be verified remain exact-once, deny, or faithful native-fallback decisions; denial never triggers automatic rewriting or rerouting. Saved rules remain bound to the complete validated configuration disclosed in the preview.

Inspect saved rules in **Reusable permission rules** and choose **Revoke** to stop their future matching decisions. Revocation cannot undo an already dispatched command and does not revoke native Codex approvals. Requests are bound to the current role generation, native session, and live hook connection; stale or restarted requests cannot replay an allow. Only Implementers can receive provider action requests; managers and reviewers retain their read-only policy. For Codex, every role uses a strict named profile that denies the exact canonical sibling `control.sock` and sets `network.enabled=true` only so Codex retains an enforcing limited proxy. The complete proxy override has no allowed domains, allows only the exact canonical `role.sock`, and explicitly disables local binding, upstream proxies, SOCKS5 and SOCKS5 UDP, credential brokerage, non-loopback proxy exposure, and arbitrary Unix sockets. `features.exec_permission_approvals` and `features.request_permissions_tool` are pinned off, so the under-development additive-permission path is not admitted. An approval or inherited native allow can skip an inbox interaction, but it cannot discard the local-command sandbox; arbitrary approved writes outside the Implementer worktree are not supported.

Codex Implementer is currently **eligible and validation-required**, not fixed Unsupported. LLMRelay pins `--strict-config`, `--ask-for-approval on-request`, `approvals_reviewer="user"`, the denied-read floor, the complete restricted-proxy configuration with an empty domain allowlist and one identity-bound role socket, and both additive-permission features false. Native approvals are honored; only new requests Codex actually emits can reach the inbox. A current `Supported` proof additionally requires an actual completed decision whose response was delivered for the exact session and generation, alongside the existing worktree-write, protected-path-denial, native-history, quiescence, provenance, and typed-checkpoint contracts. Pending, reserved, unknown, stale, or other-generation requests do not count. The app neither claims that every action produces an inbox request nor introduces an API-key fallback.

The Codex 0.157.1 adapter has a deliberately narrow local compatibility envelope. It structurally parses bounded `/etc/codex/config.toml`, `~/.codex/config.toml`, and every working-directory ancestor `.codex/config.toml`; it rejects malformed, unreadable, wrong-typed, profile-selected, managed, conditional project-MCP, shared-app-server, parent-`CODEX_HOME`, and unsupported credential-store forms. A boolean `features.memories` preference may remain enabled in local configuration; LLMRelay retains `--disable memories` on each fresh and resumed child launch and validates that flag in its launch identity. Non-boolean values still fail validation, and the other excluded features, including `external_agent_memory_import`, must remain disabled. The adapter does not rewrite the global preference or delete memory files; this feature-level restriction is not a claim that native Codex performs zero memory-related writes. Every discovered unconditional local MCP name is disabled inside one serialized inline-table value passed with the plain `mcp_servers` override key. Server names remain table keys inside that value, so Codex’s dotted-path argument parser cannot reinterpret periods or quotes in their names. The adapter requires file-backed `auth_mode=chatgpt` credentials with a known personal Free/Plus/Pro ID-token classification and a usable access-token expiry outside the native five-minute refresh window plus a ten-minute margin. It rejects API-key-in-file, keyring/external-provider, missing, malformed, expired, managed, workspace, and unknown forms. It reads this bounded metadata only in memory and does not copy, refresh, log, persist, or pass tokens or account identifiers.

This is `personal_ineligible_observed_prelaunch`, not universal cloud attestation. `~/.codex/cloud-config-bundle-cache.json` must be absent at preparation, proof publication before receipt replay, and both resume paths; LLMRelay never deletes or parses it. Local configured MCP servers are disabled at those gates, but native credential refresh or external account/configuration changes after preflight can introduce cloud policy before the next gate, and a native cache write failure can leave no cache even after cloud policy was returned. This residual is disclosed rather than hidden behind a broad `external_tools: disabled` claim. Native organization controls are not bypassed or reimplemented.

The expandable decision audit retains the original decision, actor, time, and matching rule separately from response reservation and delivery. A response is marked delivered only after the authenticated local socket write and flush succeed; this does not prove that the native command ran. Failed or interrupted delivery remains unknown and cannot replay an allow. Malformed input on a configured PermissionRequest hook produces a bounded deny response.

Select a request's project/task heading to open its exact requesting session. Use that session's explicit keyboard-control action before answering a native trust or fallback prompt in cmux. Unsupported, incomplete, or incorrectly targeted requests explain why an app grant is unavailable. A failed hook alone is not evidence that every native action was denied; actual provider behavior must be established by the matching capability check.

## CLI upgrades and qualification

The source review uses upstream tag `rust-v0.157.1` (`36650394c5b38c2990ccf2a3457165ca3e9d9726`), including `tui/src/daemon_startup.rs`, `exec-server/src/environment.rs`, the hook schema, and permission/network configuration.

Codex 0.157.1 launches and resumes with `--no-daemon`, with daemon auto-start and enterprise MCP authorization disabled. Preflight rejects `~/.codex/environments.toml` without reading or changing it: configured executor environments are outside the local adapter contract. These checks complement the cleared child environment and existing shared-server restrictions. Like the other local configuration checks, the file-absence observation is prelaunch evidence, not protection against external configuration changes after launch.

The adapter accepts exact Codex CLI 0.157.1. Older and future versions are not
implicitly supported. Updating adapter compatibility does not mark a role or
profile Supported: current role-bound native observations and a matching
capability proof are still required. Proofs from an older executable or policy
cannot authorize the new identity. Static configuration checks and automated
regressions do not establish observed sandbox enforcement.

Codex API-key model discovery, the additional Codex Apps MCP protocol, and
realtime conversation are explicitly disabled in the qualified launch. Local
system, user, or project configuration enabling these excluded features is
rejected during preflight; default-off settings alone are not the boundary.
The disabled feature list participates in the capability identity.

## Database restore confidentiality and authority

Database restore points contain sensitive application state, unlike sanitized
diagnostic exports. Keep their private directories and quarantine under the
instance owner's control. Manifest hashes detect corruption; they do not prove
authenticity against an actor who can replace both the database and its manifest.
Restore trusts the operator's selected backup. Encryption and signing are not
provided by this milestone.

Restoring a snapshot invalidates old credentials, permission rules and launch/
resume authority before service execution becomes available. A durable hold
survives restarts until verified reconciliation and explicit offline release.
Missing inventory requires positive OS reboot evidence, never operator assertion.
Release preserves paused work. See [database operations](OPERATIONS.md#database-restore-points).

## Bundled provider compatibility

Provider compatibility manifests are embedded in the executable. They select
reviewed compiled contracts; they cannot supply command arguments, environment
overrides or executable policy. Matching a contract does not qualify a native
profile or bypass sandbox, hook, credential and launch-identity checks.

Effective contract identity is bound to capability and launch evidence. Descriptive
bundle metadata is separate from authority. Missing historical bindings remain
visible but do not authorize current execution. Synthetic compatibility contexts
are restricted to fixture constructors and cannot launch provider processes.

## Local transport compatibility gate

Protocol generation and feature declarations supplement authentication. Browser
Host/Origin/session checks precede protocol validation, and validation precedes
operational request extraction and dispatch. Bootstrap and static assets remain
available without negotiation; descriptor discovery still requires authentication.
The control socket admits the OS peer and checks managed-process ancestry before
reading Hello. A compatible declaration cannot elevate a role process to human
authority or replace exact attachment bindings and input leases. The separate
role-agent socket keeps its existing contract.

## Recipe and schedule authority

Recipe/profile mutations use authenticated human commands with optimistic revisions
and operation receipts. Saving a profile performs structural validation, not native
qualification. Materialization copies task content and role settings into a backlog
draft without copying capability proofs, activations, approval or launch authority.
Workflow/configuration pins and required checks are revalidated at Ready and claim;
only the current Manager selects checks. Internal setup projects cannot receive
recipe intake. Scheduled fire records, task insertion, provenance and next-fire
advancement are transactional, with a unique schedule/time identity.

## Observed native resolution and notification identity

Native tool resolution is recorded independently from the app decision and response delivery. Only exact current invocation, tool-call identity and request correlation make an observed native resolution non-actionable. Ambiguous or missing correlation cannot clear a pending approval. Response delivery does not prove execution. Generic Claude Notification prompts route to agent output and cannot create an approval or reusable permission rule.

Registering Claude Notification changes the generated hook identity and compatibility pack. Old Claude capability proofs are stale; qualify each needed profile against the current identity before a new launch. Exact resume cannot reuse earlier hook evidence. Codex retains its own hook identity. Provider versions and admission restrictions are unchanged. See the [signal matrix](PERMISSION_SIGNAL_MATRIX.md) for supported and unsupported prompt types.
