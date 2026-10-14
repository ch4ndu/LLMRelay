# Recovery stabilization acceptance checklist

Status: implementation, independent code review, package matrix, cleanup and
fresh final verification passed. This checklist does not declare
publication or native-provider qualification.

| Outcome | Evidence and status |
| --- | --- |
| Unclassified live help reaches output and exact task controls; current unfinished exits show factual guidance | 113 frontend tests passed; source trace and independent code review passed. |
| Lost release reply remains explicitly checkable after its hold disappears, including reload | DOM regression passed; real HTTP receipt replay check passed. |
| Correlated release refusal settles the saved request; uncertain failures retain it | DOM regression passed; HTTP stale-version and changed-payload checks passed. |
| Stop pauses safely, verifies process settlement and fences continuation | Existing real-process supervisor/runtime checks passed. |
| Release sends no work; eligible resume/replacement respects current checkpoints | Focused HTTP and resume/replacement contract checks passed. |
| Matching local CLI/dashboard archive and actual browser login/navigation | Package matrix and archive inspection passed. Packaged login and 21 browser checks passed; remaining checks hit a cmux JavaScript timeout (see below). |
| All temporary instances and owned test processes stopped | Initial old instance and disposable package instance drained. Final process inventory found no LLMRelay or AgenticJira application processes. |

The package matrix passed 113 frontend, 203 library, 262 contract and 3 runtime
tests (581 total; 2 library tests ignored), plus formatting and frontend build.
The archive contains matching CLI/dashboard assets and passed ZIP integrity
inspection. Browser checks covered all navigation destinations, editing/reload,
archive/restore, Markdown safety, dialog keyboard behavior and desktop layout.
The runner stopped on a cmux JavaScript timeout after the 1440px layout check;
remaining responsive widths, enlarged text and narrow role controls were not
verified by this package run. This is partial browser evidence, not a complete
smoke-suite pass.

## Evidence boundaries

DOM tests drive assembled React actions with mocked transport. The server test
uses real authenticated HTTP routes with synthetic exited-session rows; those
rows do not prove process quiescence. The supervisor and runtime tests separately
exercise actual owned child processes and PTY/IPC behavior. The HTTP fixture has
no populated reports/checkpoints or queued guidance, so its unchanged counts
prove absence of new effects, not preservation of populated examples.

Packaged browser smoke verifies the actual embedded application, authentication
and navigation. A single browser-driven live-provider stop/release/resume journey
has not been demonstrated by these fixtures. Live allowance exhaustion and
missing-hook classification remain unqualified. No reset time, automatic retry,
provider readiness or native compatibility is inferred from these tests.

## Optional hands-on acceptance after the checkpoint

In a task you choose, confirm **Having trouble?** opens useful output and task
controls; use **Pause now** when immediate interruption is intended. For an
attributed provider hold, wait for confirmed stop, release it separately, then
use only the continuation or replacement offered by the current task state.
Refresh between actions and verify no duplicate request is sent. Do not exhaust
an account merely to create this scenario. This optional native journey is
separate from the automated engineering evidence above.

## Refactoring package refresh

The October 14 task-dialog extraction follows the saved-history, failure-notice
and session-access extractions. Its local package refresh passed 113 frontend,
203 library, 262 contract and 3 runtime tests (581 total, 2 library tests ignored),
plus frontend/release builds and ZIP integrity checks. Fresh final verification
approved the checkpoint.
All four slices preserve the moved code and parent behavior. The new archive
has its own full test/build and integrity evidence; the 21 browser
checks above belong to the earlier `76d0b29` package. No new browser, live-provider
or native recovery qualification is claimed by this refresh.
