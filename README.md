# LLMRelay

LLMRelay is a local app for giving development tasks to coding agents and following their work in a browser. You describe the change, choose how the work should be done, answer questions or permission requests, and review the result before accepting it.

It uses the Codex or Claude Code command-line tools already installed on your Mac, with your existing accounts. You can use **only Codex, only Claude, or a mixture**; you do not need both providers. Model calls use your account's allowance. The dashboard runs locally, but the selected coding tool still communicates with its provider.

## Choose how you want to work

A **workflow** is the sequence of steps a task follows.

| Workflow | What happens | When to choose it |
| --- | --- | --- |
| **Single agent** | One agent works on the task, submits a result, and runs any checks you selected. You review and accept it or request changes. | A small fix, a focused change, or your first task. No TRIP installation is needed. |
| **TRIP** | Separate agent sessions plan, implement and review the change. You approve the plan and implementation, then review the final result. | A larger change where you want a structured plan and independent reviews. |

Both workflows are implemented. Single-agent execution has native verification records for Codex and Claude. Complete TRIP tasks have also reached acceptance using only Codex and only Claude. Your own project profiles still need their setup checks before they can run.

Custom workflow editing and separate **Build and review** or **Research** presets are not available yet. Recipes and schedules currently use TRIP.

## Start LLMRelay

You need macOS, Git, and an installed, signed-in Codex or Claude Code CLI. A CLI is a program you run in a terminal. Rust and Deno are needed only if you [build LLMRelay from source](docs/BUILDING.md).

Extract the local package, then run:

```sh
/path/to/llmrelay-0.1.0-macos/bin/llmrelay serve --port 0
```

Replace `/path/to` with the folder where you extracted the package. If you built it in this checkout, use:

```sh
.local/dist/llmrelay-0.1.0-macos/bin/llmrelay serve --port 0
```

Keep the terminal open. The app chooses a free local port and opens your browser with a signed-in dashboard. It does not install a background login service.

For live agent output that opens directly from the dashboard, start the app inside **cmux**, the terminal application used for those views. Another terminal can host the app too, with [manual terminal attachment](docs/OPERATIONS.md#attach-from-a-terminal).

If the browser does not open, use the one-use login link printed in the terminal. Opening the dashboard's clean URL in another browser does not sign that browser in. You can also start with `serve --no-open` and use the printed link yourself.

## Set up your first project

A **project** is an existing local Git repository. Keep the LLMRelay executable and its data directory outside the repository you are setting up.

1. Choose **Add project**. Enter a name and the repository's full folder path.
2. In **Projects**, choose **Default workflow for new tasks**. Choose **Single agent** for the shorter setup, or **TRIP** for the planning and review process. Save the choice.
3. Follow the matching setup below. Adding a repository alone does not start an agent.

### Single agent setup

1. Open **Single agent profiles and configuration** in the project's settings.
2. Save an **Execution configuration**: instructions the agent should follow and any test commands you want available.
3. Add an **Agent profile**: your provider, exact model name, reasoning effort and instructions. A profile is a saved set of agent settings.
4. Choose **Prepare admission**, then **Probe this profile…**. Review the prompt before choosing **Queue the probe run**. This runs a small trial task and uses your provider allowance.
5. After the trial records its evidence, choose **Activate admission**. This makes that checked profile available for its matching configuration.

If a button is unavailable, read the reason beside it. A model name entered in a form is not enough to prove that your account and installed tool can use it. See [Single agent setup](docs/PROJECT_SETUP.md#single-agent-setup) for changes to profiles and configurations.

### TRIP setup

1. Open project setup and launch the discovery manager. It reads the project's guidance and suggests settings.
2. Review the suggested instructions, test commands and profiles for the six roles.
3. Authorize the profile checks. Review the proposed file changes before approving and applying the TRIP installation.
4. Complete the separate runtime checks and publish their valid results to make the checked roles available. Here, **publish** means activate verification evidence inside LLMRelay; it does not upload your code.

Setup preserves replaced files, but does not automatically merge custom instructions. Checks use provider allowance. See the [TRIP setup steps](docs/PROJECT_SETUP.md#trip-setup) for the full sequence.

If the provider asks you to trust a folder or its hooks, use **Take keyboard control** to read and answer the prompt. Hooks are small commands that tell LLMRelay when an agent starts, stops or needs attention. Press **Ctrl-]** to detach and release keyboard control when finished.

## Create a useful first task

Choose **New task**, select the project and workflow, and write the desired change. Include **acceptance criteria**: specific things you will check before calling the task finished.

For example:

> Title: Make empty search results easier to understand
>
> Change: Show a helpful message when a search has no matches.
>
> Acceptance criteria: The message says “No results found”; it includes the search text; existing matching results still display normally.

Choose **Save draft** if you are still deciding, or **Create Ready task** once setup is complete. **Ready means queued**, not finished. New project queues start paused; enable pickup when you want eligible tasks to begin.

Single agent follows this path:

```text
Agent work → Result → Selected checks, if any → Your review → Done
```

TRIP follows this path:

```text
Planning → Plan review → Your plan and implementation approvals
→ Implementation → Code review → Checks → Fresh final review
→ Manager handoff → Your acceptance → Done
```

TRIP's Manager coordinates the task. Explorer investigates when needed, Implementer changes the code, and the review roles assess the plan and result. All of these roles can use the same provider while keeping separate sessions and responsibilities.

Different repositories can run work at the same time within configured limits. One repository has one active coding task. TRIP can use parallel implementers inside that task only when its reviewed plan assigns separate areas of work.

## Follow the work and review the result

Open **Workspace**. **Needs your attention** and **Approvals** show questions, permission requests and decisions that need you.

| Place or action | Use it to |
| --- | --- |
| **Overview** | Read the task and its progress. |
| **Changes** | Inspect the proposed code changes and integration status. |
| **Checks** | Review test commands, authorize them and inspect results. |
| **Roles** | See agent settings and the task's workflow choice. |
| **Activity** | Follow sessions and open agent output. |
| **View output** | Watch a live agent without taking keyboard control. |
| **Take control** / **Release control** | Answer a terminal prompt, then let automation continue. |
| **View recorded output** | Read, search or download saved output from an ended session. |

Closing an output view does not stop the agent. Releasing keyboard control does not restart or resume a task.

For Single agent, use **Resume agent** when offered to send an answer and continue its conversation. **Request changes** starts a linked new run with your feedback and preserves the previous result.

Read the changes and check the acceptance criteria before choosing **Accept result**. A successful test or an agent saying “done” does not accept the task for you. Acceptance records your decision; it does **not** commit, merge, push or release the code. Integration into your repository is a separate step.

See [tasks and workflow details](docs/WORKFLOWS.md) for switching an unstarted task's workflow, reviews and rework.

## Why the app needs permissions

LLMRelay runs real development tools on your machine. It needs access to the chosen repository, working copies, its own saved state and the agent processes it manages. Agents need permission to edit the intended files and run the commands you allow. A **worktree** is a separate working copy of your Git repository used for a task.

Review what an approval covers. Allowing a test command, trusting a folder, approving a plan and accepting a result are different decisions. Some permissions are handled by the provider's terminal prompt; supported requests appear in **Approvals**. Reusable approvals apply to the matching scope shown on screen.

The app does not require blanket administrator access or permission to every repository. Its setup checks confirm supported behavior; they do not prove that every possible unsafe action is blocked. See the [plain-language permission guide](docs/SECURITY.md#what-you-are-allowing) and the technical details below it.

## Pause, restart and recover

Closing the browser leaves the app running. Use the task's **Pause now** or **Cancel** control when you want to stop its work, and wait for the app to confirm that its processes have stopped.

For a planned app shutdown, run the same executable from another terminal:

```sh
/path/to/llmrelay-0.1.0-macos/bin/llmrelay stop --drain
```

Start it again with `serve`, using the **same data directory**. In **Workspace → Waiting after restart → Open task**, follow **Resume after restart** or the recovery action shown. Resume is available only when the app can safely continue the saved session. Automatic resume is off by default.

If an action's result is unknown, refresh and check what happened before repeating it. Some completed Claude Manager turns have needed an explicit stop before the workflow could continue. Fully unattended completion is not guaranteed; the [current limitation](docs/NEXT_MILESTONE_PLAN.md#claude-manager-idle-boundary-follow-up) remains tracked.

Controlled restart has been tested. That does not establish recovery from every crash, machine reboot or sleep interruption. See [recovery instructions](docs/OPERATIONS.md#restart-and-restore) when work cannot continue.

## Find settings, logs and more help

The default data directory is `~/Library/Application Support/AgenticJira`. The older name is retained to preserve existing installations. If you choose a different absolute path with `--data-dir`, use it consistently for every command for that instance.

Use **Diagnostics** to investigate problems. Diagnostic exports omit credentials, raw agent transcripts, repository source and native provider history. Optional [scheduled backups](docs/OPERATIONS.md#scheduled-backups-while-the-service-runs) can be enabled with `--backup-every-hours 24`; restoring a backup requires the service to be stopped.

The **Providers** page checks local installation and sign-in information. It does not start a model session or authorize coding in a project. Supported versions and account restrictions are in the [provider compatibility guide](docs/SECURITY.md). Unsupported configurations show a reason instead of silently switching providers.

| Guide | What you will find |
| --- | --- |
| [Project setup](docs/PROJECT_SETUP.md) | Set up Single agent or TRIP and prepare profiles. |
| [Tasks and workflows](docs/WORKFLOWS.md) | Understand task stages, reviews, checks and acceptance. |
| [Permissions](docs/SECURITY.md) | Understand access, approvals and supported provider versions. |
| [Operations](docs/OPERATIONS.md) | Start, stop, attach terminals, back up and recover. |
| [Building](docs/BUILDING.md) | Build, test and package the app from source. |
| [Current progress and limits](docs/NEXT_MILESTONE_PLAN.md) | See what is completed and what still needs work. |
| [Architecture plan](docs/MULTI_WORKFLOW_PLAN.md) | Read the original TRIP plus Single agent design. |
| [Native verification record](docs/provider-qualification-20261011.md) | Inspect detailed provider test evidence and its limits. |
| [Milestones](docs/MILESTONES.md) | Explore longer-term plans, including distribution work. |
| [Third-party notices](THIRD_PARTY_NOTICES.md) | Review bundled software notices. |
