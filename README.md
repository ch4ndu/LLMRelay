# LLMRelay

LLMRelay is a local dashboard for running development tasks through the TRIP Explorer workflow. It manages projects, native agent sessions, role settings, review gates, permissions, and recovery on your machine.

It runs your installed Codex and Claude Code CLIs using their normal accounts. You use the dashboard in a browser and cmux for live terminal views; manual attachment from another terminal is also available. No provider API integration, Jinn, or cloud service is required.

## Start LLMRelay

You need macOS, Git, and an installed, authenticated CLI for each provider you select. Rust and Deno are needed only when [building from source](docs/BUILDING.md).

Extract the local package and run its executable from a terminal inside cmux:

```sh
/path/to/llmrelay-0.1.0-macos/bin/llmrelay serve --port 0
```

For the package built in this checkout:

```sh
.local/dist/llmrelay-0.1.0-macos/bin/llmrelay serve --port 0
```

Keep that terminal open. Automatic dashboard-to-cmux routing requires the service to verify that it was started inside cmux; another terminal can still host the dashboard and use manual attachment. LLMRelay selects an available local port, opens your default browser, and authenticates the dashboard automatically. The browser then shows a clean URL; you do not need to add a session ID. Adding the executable to `PATH` is optional. LLMRelay does not register a background login service.

Use `serve --no-open` if you prefer to open the browser yourself. Use the one-use login link printed in Terminal; opening the clean URL in a different, unauthenticated browser is not enough. See [browser authentication and troubleshooting](docs/OPERATIONS.md#startup-and-browser-authentication).

## Add and set up a project

1. Choose **Add project** and enter a display name and the absolute path to an existing Git repository.
2. Open its project setup. Inspect the existing workflow and choose the manager that will discover the project's guidance and suggested settings.
3. Launch discovery and use **View output** to watch the manager in cmux. If the CLI asks for folder or hook trust, choose **Take keyboard control**, answer its prompt, then press **Ctrl-]** to detach and release control.
4. Review the suggested project settings, verification commands, and agent profiles. Discovery must finish and record its result before the setup proposal can be saved.
5. Explicitly authorize the required profile checks, review the exact installation changes, then approve and apply the installation.
6. Complete and publish the separate ordinary runtime capability checks before running tasks. Installing TRIP alone does not make a profile ready to execute work.

Existing workflow files are inspected before changes are proposed. Files that will be replaced are backed up; custom instructions are **not automatically merged** into the new workflow. Profile checks use your CLI accounts' allowance and require their own authorization.

Keep the LLMRelay executable and its data directory outside the repository you are initializing. Follow the [project setup guide](docs/PROJECT_SETUP.md) for setup stages, profile validation, existing customizations, and recovery.

## Create and run a task

Choose **New task**, select the project, and enter a title, description, priority, and acceptance criteria. Use **Save draft** to retain the work in Backlog, or **Create Ready task** when its prerequisites are satisfied. New projects start with their queue paused; enable pickup when you want eligible work to begin.

The workflow includes:

```text
Planning → Independent plan review → Your approval and implementation authorization
→ Implementation → Code review → Verification → Fresh final review
→ Manager handoff → Your acceptance → Done
```

The manager owns the task. The delegated roles are Explorer, plan reviewer, implementer, code reviewer, and final verifier; Explorer runs only when the workflow requires it. You can select role profiles before starting and request changes during a task. Changes take effect through validated revisions and safe switching boundaries, rather than interrupting an agent mid-command.

Different repositories can run concurrently within capacity limits. A single repository has one active coding task; parallel implementers within it require a reviewed plan with explicit, non-overlapping ownership. See [tasks, agents, and verification](docs/WORKFLOWS.md).

## Follow output and approve actions

**Workspace** shows what is waiting for you first: **Needs your attention** and **Approvals**, each with a count, stay reachable from the top bar on every page. Below them, tasks are split into **Active** and **Completed**, followed by the agents running now. Opening a task shows its details in a dialog (full screen on narrow windows) with Plan, Build, Verify and Review progress and **Overview**, **Changes**, **Checks** and **Activity** tabs; press Escape or choose the close button to return to where you were. Plans, reports and feedback are shown as formatted Markdown; raw HTML is never rendered and only web, mail and relative links are clickable. **Ready** always means queued and eligible to start, never ready for your review.

Use **Workspace** or a task's **Activity** tab to follow sessions and open their output in cmux. For each service boot, LLMRelay owns one cmux workspace for a task and one active, mode-neutral terminal for each current role session. **View output** creates or focuses that exact terminal in view-only mode; it never changes a keyboard lease. **Take control** lets you answer an agent prompt: it sends a revision-bound acquire request to the same authenticated terminal. **Release control** (or **Ctrl-]**) lets automation continue without stopping the agent, and closing the output view never stops work. An ended session exposes recorded output only. If a terminal result is unknown, explicitly discard its non-live reservation before another View. If validated loss is found while its exact attachment is live, the loss first enters a retirement interval: the client receives `Retire`, clears its in-memory lease, and records the binding ended (or the exact bounded lease expiry is observed) before a later explicit View can reserve a fresh view-only terminal. None of these cmux actions restarts a provider, resumes a workflow, or resumes a native conversation. See [terminal attachment](docs/OPERATIONS.md#attach-from-a-terminal) for reconnect and manual attachment.

**Approvals** offers **Approve once**, **Always approve matching actions**, and **Deny** for supported requests. Reusable rules show their scope before you approve them. Rules do not bypass workflow gates or grant unrestricted machine access.

Codex also honors existing native approvals, so an already-permitted action may not produce a new inbox request. Dashboard revocation applies only to LLMRelay-owned rules. Folder trust, command permission, plan approval, installation approval, and final acceptance are separate decisions. Read [permissions and provider compatibility](docs/SECURITY.md) for the exact boundaries.

## Stop, restart, and resume

Closing the browser leaves the service running. For a planned shutdown, run the same executable from another terminal:

```sh
/path/to/llmrelay-0.1.0-macos/bin/llmrelay stop --drain
```

After a restart or crash, run `serve` again against the **same data directory**. Open **Workspace → Waiting after restart → Open task**, then use the task's **Resume after restart** or recovery action. Only a current, unfinished task whose exact session is still eligible offers a resume; completed and cancelled tasks never do. Automatic resumption is off by default; it can be enabled with **Resume eligible work automatically** under **Restart and recovery tools**.

Eligible sessions resume their saved native conversation. Uncertain process ownership requires recovery first, and missing history or changed provider configuration can block exact resume. Pending approvals remain pending. An interrupted final verifier stays incomplete and cannot be resumed as a fresh review. See [restart and recovery](docs/OPERATIONS.md#restart-and-restore) and the [recovery action guide](docs/OPERATIONS.md#recovery-actions-and-bounded-waits) for fresh-session, stop-deadline, and browser-timeout decisions.

## Data, logs, and troubleshooting

The default macOS data directory is `~/Library/Application Support/AgenticJira`. The historical directory name is retained so existing installations keep their data. To use another instance, pass an absolute `--data-dir` path and reuse it for every command targeting that instance.

Startup prints the selected data and log paths. Use the dashboard's **Diagnostics** page or the executable's `logs` and `diagnostics export` commands when investigating a failure. A diagnostic export omits credentials, raw transcripts, repository source, and native provider history. See [operations and diagnostics](docs/OPERATIONS.md).

## Support and documentation

LLMRelay currently hosts standalone TRIP Explorer **v0.11.0**. Provider support is validated locally for the exact executable, role, model, effort, and policy; proofs are not transferable between installations. The current Codex adapter has a narrow compatibility envelope, including Codex CLI **0.157.1** and supported personal ChatGPT credentials. You can leave global Codex memories enabled: LLMRelay disables the memory feature only for its own fresh and resumed agent processes, without rewriting your preference or deleting memory files. Unsupported configurations are reported rather than silently modified. Review the [provider restrictions](docs/SECURITY.md) before onboarding.

- [Building, verification, and local packaging](docs/BUILDING.md)
- [Project setup and capability checks](docs/PROJECT_SETUP.md)
- [Tasks, roles, and verification](docs/WORKFLOWS.md)
- [Permissions and provider compatibility](docs/SECURITY.md)
- [Sessions, recovery, and diagnostics](docs/OPERATIONS.md)
- [Milestones: service validation, background service, and menu-bar companion](docs/MILESTONES.md)
- [Research, implementation history, and verification boundaries](RESEARCH_AND_PLAN.md)
- [Third-party notices](THIRD_PARTY_NOTICES.md)

Automated checks and prior disposable native validation do not establish visual verification in your browser. Verification boundaries and historical evidence are retained in the research plan; a successful build alone does not establish every runtime behavior.
