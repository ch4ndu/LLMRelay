# M8A — Versioned local client protocol

Status: independently APPROVED G3, September25 2026. Standing roadmap authority
covers finalized in-scope plans; no implementation before independent approval.
Coverage moderate. Broad cross-transport contract, one writer sequential delivery.

## Outcome and exclusions

Browser, human CLI and terminal attachment clients negotiate one exact local wire
generation and required descriptive features before operational requests. Old,
missing, malformed and future protocol declarations fail with stable actionable
errors before business effects. Authentication and all operation/revision/lease/
provider capability checks remain independent and unchanged. No migration support
for old clients, transport/framework/dependency, database schema, provider API,
background service, updater, signing, publication, GUI or live installation.
Role-agent socket protocol is separate and unchanged; nested human CLI use from
managed agents must still fail existing peer-authority checks.

## Decisions

1. Exact generation1 only, not speculative min/max compatibility. Static closed
   client kinds browser, human_cli, attachment; static feature identifiers per
   supported transport. Required features must be subset of advertised features;
   unknown required feature rejects, optional unrecognized advertisement does not
   grant authority. No M7 provider support inferred from transport capabilities.
2. Gate all operational HTTP routes and control requests, not just apparent
   mutations. Status/capability reads can reconcile, Attach changes custody, and
   mixed operation endpoints make a permissive old-client read policy misleading.
   Allow only existing bootstrap/static assets and an authenticated protocol
   descriptor without prior negotiation. Bootstrap establishes browser identity
   only; it never grants business-operation compatibility.
3. A bounded shared src/protocol.rs module holds DTOs/constants/pure validation
   and stable error categories. No generic policy engine or second authority
   store. Existing server/control dispatch owns transport enforcement.
4. Descriptor includes generation, server app version, supported client features
   and service incarnation (existing instance/boot identity). Guidance says reload
   dashboard/use matching CLI and explicitly avoids automatic service restart.
   It reveals no executable paths, secrets, session state or native-provider data.

## HTTP/browser

- Authenticated GET /api/protocol exposes the descriptor. Existing Host/Origin/
  cookie rules run before interpreting protocol input or exposing metadata.
- All operational requests carry a bounded protocol declaration in a header
  (generation, client kind, required feature identifiers; one strict encoding).
  Exact generation and required features checked every request on server; cached
  successful browser negotiation is never server authority. Duplicate/oversized/
  malformed declarations fail, unknown properties rejected. No arbitrary JSON
  paths/command text or user operation content in errors.
- Shared route layer or explicit shared pre-handler guard must authenticate and
  validate protocol before JSON/query extraction and dispatch. Preserve existing
  body bounds and auth-first state-wait validation. Scope layer only operational
  API routes; bootstrap/static/descriptor must not be trapped in negotiation.
- Return HTTP409 with stable protocol_error object containing reason, observed
  generation when valid, expected generation and safe guidance, plus ordinary
  error text for current clients. Reasons missing, malformed, incompatible_generation,
  missing_required_feature, wrong_client_kind. No operation receipt or diagnostics
  side effect from rejected business requests; don't parse rejected operation body
  just to echo its operation ID. Authentication failures retain existing response.
- api.ts negotiates after bootstrap, shares one in-flight descriptor request,
  validates descriptor schema and adds protocol headers centrally without allowing
  RequestInit header overwrite. Read and mutation paths both use the same contract.
  Negotiation requests themselves don't recursively negotiate.
- Existing live-state incarnation reset invalidates cached descriptor; subsequent
  calls renegotiate. Server per-request guard remains safe before reset is observed.
  Protocol mismatch shows clear persistent upgrade guidance, not endless fast
  retry or an ambiguous mutation. Do not automatically replay mutations after
  renegotiation; maintain existing operation identity/reconciliation rules.

## Control socket/CLI/attachment

- Keep Unix peer, managed-agent ancestry and process-generation authentication
  before reading client payload. A bounded strict Hello frame is the first frame
  on every connection; it carries generation/client kind/required features.
  Response includes descriptor or structured protocol failure. Wrong or missing
  hello/business-first must produce a structured refusal and close without dispatch.
  Use existing frame-size/time bounds; no second listener or transport.
- Once negotiated, connection retains immutable generation/client kind. All business
  requests require successful hello. Duplicate hello refuses safely; client kind
  must not bypass any current request/lease/process rules. A connection with an
  attached session remains governed by exact input/custody and existing disconnect
  cleanup, never by feature advertisement. Invalid frames cannot acquire a lease.
- ControlConnection::connect (or bounded explicit connect-for-kind entrypoints)
  performs hello and validates server response before returning. Existing request
  helper and CLI print_control use human_cli; run_attach uses attachment. Inventory
  all constructors/callers including generated cmux commands and fixture connections.
  No new automatic attachment reconnect: existing timeout/disconnect behavior and
  exact lease release remain. New service connections always negotiate afresh.
- An old server lacking Hello yields actionable incompatible-server guidance with
  no fallback that sends the intended business request. ControlResponse evolution
  must preserve ordinary success/error callers and bound malformed-response handling.

## Ownership, callers and verification

Backend owned: src/protocol.rs(new), lib.rs, server.rs, control.rs, cli.rs; cmux.rs
only if existing generated invocation needs adjustment. Frontend types.ts/api.ts/
liveState.ts/App.tsx and existing status display only as needed. Tests existing
inline server/control/CLI, tests/contracts.rs/runtime.rs, frontend flows.test.tsx.
Manager owns docs BUILDING/OPERATIONS/SECURITY/WORKFLOWS and milestone status.
Other source protected; additional mechanical callers reconciled before editing.
No writer/inspector overlap. One build slot. Freeze current tree baseline before
implementation, preserving all prior milestone changes. Inventory affected fixture
constructors/headers/connection helpers in first handoff, not only new tests.

Initial allowance eight causal areas/up to1500 test-support lines existing harness;
standing authority covers justified increases, no new permanent harness implied.
1. Strict pure descriptor/hello validation: missing/malformed/old/future/unknown
   required feature and matching success. Features cannot confer business authority.
2. Auth-first HTTP denial before malformed protocol/body/query; operational-route
   inventory rejects absent/mismatch without durable operations/receipts/dispatch.
3. Browser bootstrap/descriptor recursion, concurrent negotiation, required features,
   per-request header, mismatches and no automatic mutation replay.
4. Incarnation/reset/reconnect invalidates negotiated browser state while preserving
   M6 ordering, form state and bounded failure/backoff behavior.
5. Control peer denial precedes hello; business-first/malformed/duplicate/mismatch
   cannot dispatch or acquire custody; matching hello reaches ordinary guards.
6. All one-shot CLI helpers and attachment startup negotiate; older-server refusal
   never falls back to business request; structured guidance instead of blank exit.
7. Exact attachment input/lease/resize/detach and disconnect cleanup unchanged;
   managed role-agent boundary remains distinct and protected.
8. Current fixtures and protocol discovery redaction; final local package contains
   matching browser/CLI/server generation. No liveprovider or GUI claim.

Focused checks per slice then independent integrated code review. One stable
scripts/verify.sh matrix after convergence, final cross-layer trace if activated,
fresh independent final review and task-local package. Native service restart,
real-world old-client exercise and user acceptance remain deferred. No protocol
compatibility or feature-discovery result replaces native provider qualification.

## G1 review resolutions (normative)

1. Build a separate operational Router with the authentication/protocol guard,
   then merge it with the bootstrap/static/protocol-descriptor routes. Health,
   state/wait, restart-preview, model-catalog, role-preparations, diagnostics,
   command and operation are all gated. Only static assets/index, bootstrap and
   authenticated GET /api/protocol are negotiation-exempt. No blanket layer on
   the current flat router. Route guard executes before handler JSON extraction.
2. The new control client maps EOF or a non-descriptor hello response to clear
   guidance: the server may be older or may have closed the connection. Do not
   assert an old server was conclusively detected. Never send a fallback business
   request, reconnect automatically, or retry an ambiguous operation.
3. Descriptor incarnation is exactly instance_id, on both transports, matching
   the state cursor/reset identity. boot_id is not a substitute. api.ts may clear
   cached negotiation on state/state-wait instance mismatch without changing the
   liveState algorithm; subsequent request must negotiate afresh. Preserve current
   incarnation checks and do not treat a late descriptor from a superseded fetch as
   the current cache. No mutable server negotiation authority across requests.
4. Hello is a separate bounded first-frame struct with deny_unknown_fields, parsed
   before any ControlRequest. Later frames remain existing business enums; do not
   silently impose new strictness on all variants. Duplicate hello cannot parse as
   a business request and closes through normal exact attachment cleanup. Malformed
   business frames produce bounded structured errors where possible, without parsing
   or dispatching a fallback. Existing sanitized control rejection diagnostics may
   remain; HTTP pre-dispatch protocol refusal creates no business receipt/audit.
5. First handoff must inventory/update all nineteen frontend fetch stubs, including
   fixed-body stubs, to supply the authenticated protocol descriptor. One narrow
   shared fixture response helper is allowed in the existing DOM test file; no new
   harness. Update inline server headers/router fixture and the control g14 raw
   status/ConnectionState fixture. Managed role socket tests preserve their distinct
   authentication contract. cmux-generated attach inherits the CLI negotiation and
   requires no new command flag by default; connect-only diagnostic probes need no
   Hello because they perform no request and close cleanly.
6. Keep HTTP409 but classify the strict protocol_error shape before business-conflict
   status/operation_id handling in api.ts. Represent it distinctly from ordinary
   ApiError reconciliation so UI callers don't treat it as a stale task revision or
   ambiguous mutation. It is a definitive pre-dispatch refusal; show persistent
   protocol guidance and do not automatically replay. Unknown/malformed error shapes
   retain conservative existing transport handling, not a forged definitive refusal.
   Include causal frontend coverage for both ordinary409 and protocol409 paths.

All G1 findings accepted. No change to eight areas, initial1500support allowance,
transport/auth authority, deferred scopes or existing dependencies.

## G2 review resolutions (normative)

- Protocol refusals use a distinct ProtocolError class that does not extend ApiError.
  It cannot satisfy existing `cause instanceof ApiError && cause.status === 409`
  branches in ApprovalInbox. Preserve ordinary business-conflict behavior. Causal
  DOM coverage must show protocol guidance rather than stale-revision reconciliation
  for that existing caller; ApprovalInbox production edits are not required.
- EOF before any first frame is an ordinary clean connection close, not a protocol
  refusal or control.ipc.rejected diagnostic. Missing Hello means a business frame
  arrived first. Invalid later frames still close after any bounded structured error,
  using the existing exact attachment/lease cleanup; no retry or fallthrough.
- Instance mismatch must invalidate browser negotiation before subsequent operational
  calls. api.ts response observation may implement that obligation without changing
  liveState; it is not optional to leave the cache valid across the observed reset.

G2's one blocking representation gap and minor close/reset clarifications accepted.
Standing roadmap authority permits the focused third plan review without resetting
the count; no additional user action or expanded product scope is required.

G3 advisory incorporated: api.ts transport catch must rethrow ProtocolError unchanged
alongside ApiError, never wrap a definitive pre-dispatch refusal as an ambiguous
mutation. The existing caller-level causal test must cover this catch path.
