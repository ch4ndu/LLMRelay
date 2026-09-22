# Project Agent Instructions

## Default Engineering Gate

Before calling any coding task complete, verify the delivered work against the user's actual request.

- Re-read the latest user request and the constraints added during the task.
- Check each named behavior, screen, flow, and platform path against the implementation.
- For UI work, trace the exact clicks/actions the user mentioned and confirm they are wired, not just that the project compiles.
- Treat build/compile success as necessary but not sufficient.
- If anything remains incomplete or descoped, state it explicitly and continue working unless the user has explicitly accepted the gap.
- Final responses should separate request verification from build/compile verification.

## Git Identity and Attribution

- Use only the configured Git `user.name` and `user.email` when creating or amending commits and tags.
- Never add AI attribution to commit messages, annotated tags, pull-request descriptions, release notes, or generated changelogs.
- Never add `Co-Authored-By`, `Generated-By`, `Generated with`, `Claude-Session`, or similar attribution for Codex, OpenAI, Claude, Anthropic, or any other AI system.
- Before creating a commit, inspect the complete proposed commit message for prohibited attribution.
- After creating or amending a commit, run `git log -1 --format=%B` and immediately amend it if prohibited attribution is present.

## Completion-driven engineering waits

- Prefer supported completion notifications or a blocking tool wait over repeated
  model turns that sleep, read unchanged logs, and check again.
- For external CLI roles/jobs, save the result and atomic completion receipt,
  then use a local watcher to call `codex queue --thread <manager-session-UUID>
  --message <completion-reference>` once. Bind the exact task/generation and
  result paths; never infer success from a wake-up message.
- The current project-local operational helper is
  `.local/trip-explorer/completion-wakeup/notify.py`. It is local tooling, not
  shipped application code. Use a unique receipt per invocation, explicit parent
  thread UUID, and the real Codex executable. Keep its `.wake.json` delivery record.
- Do independent useful work while waiting. Once exhausted, leave only safely
  detached work running and yield; the completion message should resume the
  manager. Do not promise automatic resumption until the notification path has
  been verified. Progress updates should follow actual events, not model polling.
- On wake-up, inspect the receipt/result once and continue under existing role,
  approval, review, and accounting gates. A duplicate wake-up must not repeat work.
- A watcher deadline requests inspection; it never kills or retries the worker.
  Failed or ambiguous queue delivery remains visible in its delivery record and
  must not be retried automatically. This helper is not machine-restart recovery.
