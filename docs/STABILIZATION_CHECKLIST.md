# Recovery stabilization acceptance checklist

## Current local package — October 14

Source through `17e6be4` is packaged. Implementation and refactor review gates
passed; final evidence/documentation verification approved the checkpoint. This checklist
does not declare publication, personal acceptance or native-provider qualification.

| Outcome | Evidence and status |
| --- | --- |
| Failure escape, explicit release, safe continuation and uncertain-request retention | Existing assembled DOM, authenticated HTTP and process/contract checks passed; original independent reviews approved. |
| Draft cleanup preserves newer stored edits | Five causal regressions plus controls passed; included in the current 118-test frontend suite. |
| Refactoring preserves behavior | Saved history, failure notices, session access, task dialogs, task content/reports and coordinator containment passed independent review and final verification. Direct moves preserve action wiring, error identity and transaction boundaries. |
| Matching local CLI/dashboard archive | 586 tests passed: 118 frontend, 203 library, 262 contract and 3 runtime; 2 live-provider tests ignored. Format, compile, frontend/release builds and archive integrity passed. Both ZIP binaries match staging and release outputs. |
| Actual packaged browser flows | Login succeeded and all 32 checks passed: navigation, draft create/edit/reload, filtering, Markdown safety, setup route, keyboard focus/Escape, tabs, archive/restore and browser-error inspection. |
| Responsive layout and controls | No page overflow or covered board controls at 1440px, 900px, 390px and 1440px with 200% root text. Approvals reachable; role-edit fields usable at 407px/193px; narrow task dialog opens full screen and closes with X. |
| Cleanup | Disposable service drained with no active or unknown workers; final process inventory found no LLMRelay or AgenticJira application process. |

## Evidence boundaries

DOM tests use mocked transport. HTTP recovery tests use authenticated routes
with synthetic exited-session rows, which do not establish process quiescence.
Supervisor/runtime tests separately exercise owned processes and PTY/IPC.
The recovery HTTP fixture has no populated reports/checkpoints or queued guidance;
unchanged counts prove no new effects, not preservation of populated examples.

Report-specific Refresh and Open Activity callbacks retain their source wiring
and existing DOM coverage. The packaged browser task is unstarted: populated
reports, permission requests, live output/control, recovery controls, real task
acceptance, guidance and provider-switch journeys were not exercised there.
These are covered only to the extent documented by deterministic tests.
A single browser-driven live-provider stop/release/resume journey, live allowance
exhaustion and missing-hook classification remain unqualified. No provider reset,
automatic retry, safe idle or native compatibility is inferred from local tests.

The browser run used native dark appearance and root text scaling. Light
appearance and actual browser page zoom were not tested. The older partial
21-check run hit a cmux timeout; the current 32-check run completed successfully.

## Optional hands-on native acceptance

Use a task you choose to inspect Having trouble?, Pause now, explicit hold release
and the continuation offered by its current state. Confirm stop before release,
refresh between actions and check that nothing is sent twice. Do not exhaust an
account solely to manufacture an error. This remains separate from the completed
local engineering checks; no provider process was launched for the browser run.

The archive documentation was refreshed after final review. Only bundled docs
changed; the verified binaries were not rebuilt. ZIP integrity and binary
equality were checked again after this documentation refresh.
