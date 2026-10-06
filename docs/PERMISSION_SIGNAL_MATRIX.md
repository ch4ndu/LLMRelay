# Permission and provider signal matrix

LLMRelay owns task state, permission decisions and workflow review. Provider hooks describe observed native activity; a notification, response write or open process is not task acceptance. This matrix distinguishes supported signals from prompts that remain visible only in agent output.

Reference snapshot: [Jinn](https://github.com/hristo2612/jinn/tree/62f026aa6cb740c807ac74c350a356538ba6eea8), commit `62f026aa6cb740c807ac74c350a356538ba6eea8`. The comparison used its Codex/Claude interactive runners, Claude permission prompt handler, rollout reader and prompt-handler tests. The inspected Jinn runners include terminal prompt handling and automatic safety-prompt approval. LLMRelay intentionally retains explicit human decisions and supported structured signals instead.

Provider boundary: embedded compatibility candidates are Codex 0.157.1 and Claude Code 2.1.283. Installed versions alone do not qualify a role. Each role must satisfy the current manifest, generated hook identity and capability evidence. The Claude Notification registration changes its hook revision to `agenticjira-hook-v4-claude-notification` and compatibility pack revision to 3; prior Claude evidence requires requalification. Codex retains its existing hook revision and pack.

| Case | Reference handling | Supported LLMRelay signal and action | Boundary / intentional difference |
| --- | --- | --- | --- |
| Tool approval | Interactive permission handling | An actionable managed PermissionRequest opens the exact approval request. | A decision, response reservation, response delivery and native tool resolution are separate facts. |
| Native safety permission | Claude Notification plus terminal prompt parsing and optional auto-approval | A trusted supported Notification shows a native wait and opens agent output. | No terminal parser, keystroke approval or generic permission grant. |
| Sandbox network approval | Interactive terminal handling | Claude documents `permission_prompt` notifications for network prompts in versions 2.1.246 and later. | This is generic attention, not an app-reviewable network approval. Actual delivery under the admitted version needs live verification. |
| Plugin or onboarding dialog | Interactive runner may handle startup prompts | Open agent output when no admitted structured signal proves a wait. | No plugin/onboarding detection is inferred from elapsed time or terminal text. |
| Elicitation / manual input | Prompt handling and PTY interaction | Supported `elicitation_dialog`, `elicitation_url_dialog` and `agent_needs_input` notifications route to output. | Text sent is distinct from a trusted UserPromptSubmit proving native acceptance. |
| Native permission resolution | Terminal-driven interaction | Exact current hook, tool-call identity, request correlation and ordering can make a request non-actionable. | Ambiguous same-tool or missing correlation remains pending. Native resolution never fabricates an app decision or delivery. |
| Compound command, heredoc, redirect or interpreter | Reference automatic response policy differs | Existing structured permission families retain their exact argument, path, cwd and invocation fences. | Unsupported families require one-time review or the native route. No blanket Python/shell permission. |
| Delayed or duplicate signals | Runner-specific event checks | Current invocation identity and accepted state determine visibility; repeated notifications retain a stable event identity. | Without provider turn identifiers, hook arrival order is weaker evidence. No stronger causal claim is made. |
| Reconnect / initial snapshot | Reference parity not established | In-app notices list current waits; initial/reset state does not replay historical browser alerts. | Browser delivery does not change provider or workflow state. |
| Claude turn failure | Structured StopFailure normalization and durable role/lane hold | A trusted current StopFailure exposes the failure kind and restricts affected automatic work. Rate-limit/overload uses an app cooldown; other kinds require Release provider hold. Holds remain visible on exited sessions. | No fabricated role result, readiness promotion, review call, paid retry, model fallback or provider-reset claim. |
| Codex capacity / process-open error | Runner-specific output handling | Exact current admitted Codex history with a structured capacity completion opens the affected agent output. Other recorded launch/process errors retain their existing output route. | Attention only; no fabricated failure hook, readiness, hold, retry or fallback. Missing or ambiguous history is unclassified. |
| Submitted implementation / open process | Native output and result channel | Authenticated role report is awaiting processing or verification; the session may remain open. | A candidate report and hook completion are separate from human acceptance. |
| Resume recovery | Runner/session machinery | Exact accepted resumed-turn evidence may supersede its own earlier blocker and release its matching hold atomically. | Independent controls, current blockers, original reports and rejection history remain intact. |
| Completion / cleanup | Runner completion and process handling | Report consumption, workflow verification, process exit and human acceptance retain separate owners. | No completion inferred from silence, an open process or a notification. |

## Verification boundaries

The existing contract tests exercise production hook ingestion, request correlation, native failure/prompt projection and atomic recovery. Dashboard DOM tests exercise approval navigation, uncertain mutation handling, accepted-snapshot notifications, report/session distinctions and theme controls. Tests do not prove real operating-system notification delivery or the occurrence of every provider modal.

Real browser checks must use an isolated service and cover focused/unfocused delivery, denied/unavailable notification support, reconnect, responsive geometry, appearance persistence and open surfaces. Supported live provider checks must use current qualified profiles in an isolated fixture. Unsupported plugin/onboarding/Codex modal signals remain explicit output-route boundaries rather than successful automated detection.

Claude's [hook reference](https://code.claude.com/docs/en/hooks) documents Notification and StopFailure payloads. Generic notifications cannot authorize a tool, resolve an unrelated request or establish account/model eligibility. Jinn's fake-terminal tests demonstrate its own handler behavior; they are not live provider or LLMRelay acceptance evidence.

October 1 isolated observations used admitted Claude Code 2.1.283 and Codex
0.157.1. Claude normal Stop events carried empty background-task and scheduled
wakeup lists, and notifications carried a turn identifier. That identifier did
not uniquely associate a notice with a tool permission request. No trustworthy
StopFailure quiescence evidence was established; these observations do not
authorize automatic readiness after failure or retirement of ambiguous notices.
The observed fixture hooks had untrusted payload provenance, so successful
transport observations do not qualify runtime workflow authority.

The initial Codex fixture rejected its requested model for the current ChatGPT
account. That model-availability error did not establish capacity behavior.
A later isolated Codex 0.157.1 interactive observation used a confined local
endpoint with dummy authentication. One HTTP 503 response with
`error.code=server_is_overloaded` produced a persisted
`event_msg/task_complete` containing `error.codex_error_info=server_overloaded`.
The noncapacity control, HTTP 503 with `error.code=slow_down`, instead persisted
`rate_limit_exceeded`. Both completed-turn records matched their native
`task_started.turn_id`; each case made one request and no tool calls. The saved
history format was paginated. These controlled faults establish native
classification and persistence, not an actual upstream outage or LLMRelay
invocation attribution. The attention-only reader uses the exact path and
explicit `turn_id` from the current managed acceptance hook, revalidates the
invocation before recording one audit observation, and retires its current
projection after superseding activity. It does not infer model eligibility or
permission to retry from terminal text or an installed version.

Provider failure holds reuse the current invocation and accepted-turn identity
checks. Matching prompt IDs, arrival-order attribution when an ID is absent, and
startup-invocation attribution are distinct strengths of evidence for a
restriction. None proves that a failed turn is quiescent. Releasing a hold and
expiry of its app cooldown remove only that restriction; all other workflow,
permission and readiness prerequisites remain in force.

The initial deferred-program local failure probe stopped at the CLI's startup network
check before a prompt, hook or local API request. It did not establish failure
readiness or provider reset timing. Managed-process provenance establishes the
origin of a hook, while its payload assertions remain untrusted; that label alone
does not disqualify the origin. The missing failed-turn quiescence and unique
notification/request correlation remain separate evidence gaps.

An October 2 follow-up used the same pinned Claude 2.1.283 executable, dummy API
authentication, isolated configuration and a child network sandbox. Two fixed
startup connectivity responses were served locally through a process-scoped
proxy and temporary CA; no external request was forwarded. Native onboarding
and trust produced a genuine `SessionStart`, an exact matching `UserPromptSubmit`
and an `unknown` `StopFailure` for the same session and prompt. That failure
payload omitted both background-task and scheduled-work registries, consistent
with the pinned emitter. Their absence does not establish quiescence.

Two Messages requests could not be positively attributed to the experimental
prompt, so the fixture stopped without running its intended retry sequence.
It provides no retry-ordering or attention-delay conclusion. Its process group
was quiescent, both local endpoints closed, and temporary private keys removed.
The native trust screen displayed a pre-existing inherited WebFetch permission;
the pinned `--restricted` behavior ignored those local settings, `--tools ""`
disabled built-in tools, and the network self-check confirmed external access
was denied. No inherited permission was changed or used.

Pinned source also confirms that generic `Notification` lacks a unique
permission-request edge. Claude's structured status-line quota windows omit the
rejecting limit and response identity, so a reset value cannot be attributed to
the current failure. LLMRelay retains generic output attention and its bounded
app cooldown; neither observation supplies additional execution authority.

Static tracing of the same pinned executable places ordinary retry and query
recovery paths before terminal `StopFailure`. The hook call is not awaited, so
delivery can lag termination. Native `/goal` behavior can schedule a distinct
subsequent prompt; this does not demonstrate a pending retry of the failed query
or prove background-work quiescence. H11 therefore introduces no arbitrary
attention delay. Failure notices describe only LLMRelay's response to the notice,
preserving the distinction from retries or follow-ups performed by the provider.
Current activity still supersedes its own prior failure through existing rules;
independent provider holds remain separate. This conclusion is pinned-source
evidence, not a measured successful retry experiment.

The H9 PTY fixture now exercises four surviving-process/input cases through the
existing managed test executable: a dropped Enter remains unacknowledged without
resending, failure without Stop remains busy with its hold visible, a turn without
completion stays owned, and a permission notification opens output without an
approval or injected response. Hook receipt and stored identity assertions reject
false passes from untrusted-event demotion. These checks cover host behavior only.
The initial image observations were insufficient: one PNG was invalid, and a
valid relative path was accepted as text without conversion. The corrected
Claude 2.1.283 observation used a valid 1920×1080, 5,275,734-byte synthetic PNG
and its bare absolute path. After a no-tool ready turn, one bracketed paste and
one Enter at a measured 104.9–117.4 ms spacing produced an `[Image#1]` attachment
but no new `UserPromptSubmit` or tool call during 75 seconds. This reproduces
the stranded attachment/Enter condition for that version, input and timing;
it does not establish how every image or machine load behaves.

A separate observation of the existing JSON-encoded guidance form also remained
in the composer without a submit for 75 seconds after one paste and one Enter
at 105.1–116.7 ms. It produced no image attachment. Encoding prevents native
path/command interpretation; it does not guarantee acceptance after 100 ms.
Both isolated sessions and services were stopped, their owned process groups
were quiescent, and the generated images were removed. No input was resent.
The app's existing input-written/acceptance distinction and report-only
attention remain necessary; neither observation authorizes automatic Enter
retries, readiness or completion.

H16's native structured-history condition is demonstrated by the local faults
above. Its reader and managed attribution passed integration checks, independent
code review and fresh final verification. The focused existing unit fixtures
exercise actual hook ingestion, bounded file reading, transactional revalidation,
audit deduplication and snapshot attention without a live provider process.
The reader's finite limits are documented in [Operations](OPERATIONS.md#permission-notices-and-appearance).

## H2 and H12 local controls and provider contract dependencies

The user selected local controls on October 2. Failed task sessions now offer an
exact graceful stop coupled to a durable task pause; explicit Continue or Run
next can release that pause only after verified exit and normal continuation
checks. Generic native notices offer a durable shared dismissal of the displayed
notifications, while retaining an honest unknown native status. Independent code
review, the configured verification suite and fresh final verification passed.
All six acceptance criteria for these local alternatives are complete.

Dismissal affects presentation only. The raw native wait remains available to
the observation logic, exact permission requests remain independent, and any
later notification surfaces again even within the same turn. Neither local
control supplies automatic provider readiness or an exact native permission
resolution. See [Operations](OPERATIONS.md#recovery-actions-and-bounded-waits)
for the recovery and acknowledgement actions.

The October 2 compatibility investigation also inspected installed Claude
2.1.287 and current public documentation. Application admission remains 2.1.283.
Neither upgrading the executable alone nor the inspected structured controls
qualifies the two remaining outcomes for the current interactive CLI contract.
The task-list control lacks a complete scheduled-work and ordering contract;
identifiable structured permission callbacks do not identify an existing generic
hook notice or establish live terminal recovery. These are source/documentation
findings, not a new runtime qualification.

Two provider enhancement drafts remain local and **not submitted**. Publication
was cancelled when the user selected the local approach. The contracts below
describe the original automatic outcomes, not prerequisites for the local
controls:

| Outcome | Required provider contract | LLMRelay admission and acceptance |
| --- | --- | --- |
| H2 automatic readiness after failure | Complete background and scheduled-work inventory at the failed-turn boundary, with explicit availability, exact session/turn and failure origin, and ordering sufficient to reject stale observations. | Admit the documented provider version and qualify the current empty case plus running/pending work, scheduled wakeups, unavailable inventory and late/replaced-session evidence. Preserve independent permission and execution holds; readiness does not authorize retry. |
| H12 exact permission-notice retirement | A native request identity shared by actionable generic notifications and acknowledged resolution/cancellation, including sandbox-network requests and defined duplicate, coalescing and reconnect behavior. | Retire only the exact notice after the provider applies a decision or cancellation. Verify simultaneous same-turn requests, duplicates, cancellation, reconnect and stale acknowledgements. Retain ambiguous historical notices conservatively. |

The bounded upstream search found related requests, not a qualified implementation:

- [#58303](https://github.com/anthropics/claude-code/issues/58303) concerns broader
  background-work visibility; H2 specifically needs the failure boundary.
- [#85955](https://github.com/anthropics/claude-code/issues/85955) and
  [#91419](https://github.com/anthropics/claude-code/issues/91419) report teammate
  activity and failure-origin ambiguities. These are external reports, not new
  locally reproduced defects.
- [#77996](https://github.com/anthropics/claude-code/issues/77996) requests
  permission outcomes and was closed for inactivity; its closing comment asks
  for a new issue if still relevant. H12's proposed follow-up focuses on exact
  lifecycle identity, since the hook inputs are now documented.
- [#11128](https://github.com/anthropics/claude-code/issues/11128) covers notification
  context, while [#54597](https://github.com/anthropics/claude-code/issues/54597)
  covers local resolution on the separate channel relay. Neither reference
  establishes hook-based sandbox-network correlation.

Issue states above were checked on October 2, 2026. The current
[hook reference](https://code.claude.com/docs/en/hooks) and
[channel relay reference](https://code.claude.com/docs/en/channels-reference#relay-permission-prompts)
remain the public contract sources. An upstream issue closure, installed version
change or new field alone does not qualify runtime authority: the documented
contract, admitted tuple and focused causal checks must agree. No polling service,
SDK/transport migration or automatic fallback is selected by this follow-up.

## Codex usage evidence boundary

The Codex-first reader admits the 0.157.1 top-level `token_usage_record` contract
from exact tagged source (commit `36650394c5b38c2990ccf2a3457165ca3e9d9726`)
and retained-binary static evidence. Required response IDs and numeric fields,
optional cache-write coverage, the best-effort writer and unconditional rollout
persistence policy are recorded in local contract evidence. App tests exercise
bounded reading, managed attribution, durable deduplication and projection with
synthetic records; they do not prove live provider delivery or accuracy.

Only current managed turn records contribute. Cumulative/context snapshots,
inherited history and sub-agent records do not. All displayed totals are partial;
usage never supplies a permission decision, readiness or failure-reset deadline.
Claude telemetry, account quota windows and costs require separate qualification
and scope.
