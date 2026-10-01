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
| Claude turn failure | Structured StopFailure normalization | A trusted current StopFailure exposes the failure kind while keeping process state separate. | No fabricated role result, review call, paid retry or model fallback. |
| Codex capacity / process-open error | Runner-specific output handling | Recorded launch/process errors and Open agent output remain available. | No equivalent admitted StopFailure/Notification hook is assumed. |
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

The Codex fixture rejected its requested model for the current ChatGPT account.
This was a model-availability error, not a capacity error. LLMRelay has no runtime
Codex session-history reader, and the test established no supported structured
capacity signal. The output route remains available; neither terminal text nor
an installed version establishes model eligibility or permission to retry.
