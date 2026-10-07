# Optional maintenance helpers

Use these when diagnosing an installation, reviewing an upgrade, or inspecting
saved role runs. Python is optional; the manager can inspect the same evidence
directly. Commands are run from the target project root. No helper authorizes
provider calls, configuration changes, retry, or workflow completion.

## Guidance audit

Ask for `$trip-explorer-workflow audit guidance` to inspect configured project
guidance against current source. The manager follows the
[read-only audit entry](guidance-quality.md#guidance-drift-audit), reports stale
claims, coverage gaps, and runtime claims that remain unverified, then returns.
This does not launch roles, run tests, or change guidance. It complements the
installation doctor, which checks package/configuration health rather than
the truth of documentation claims.

## Installation doctor

```sh
python3 .agents/trip-explorer/bin/workflow_doctor.py --project .
```

Add `--json` for a saved structured report. The doctor checks installed files,
base and runtime hashes, project configuration, role preflight, and executable
identity. Built-in adapters receive a bounded `--version` check only. Missing
older CLI identity evidence is unverified; version agreement is not a new live
model/authority preflight. Native agents and unsupported custom version checks
are reported without launching an agent. Follow the reported repair or
re-preflight action through the manager; the doctor makes no repairs.

Expected skill customization is distinguished from missing or damaged package
state. Manifest `skills_root` identifies a non-default host skill directory;
legacy manifests default to `.agents/skills`.

## Saved upgrade proposal

Read the installed upgrade skill. A newer package's helper can create a saved
proposal even when the older installed runtime lacks it:

```sh
python3 /path/to/new/trip-explorer-install/scripts/upgrade_preview.py \
  --project . --candidate /path/to/new/trip-explorer-install \
  --output .local/trip-explorer/upgrade-preview/0.12.0
```

Review `proposal.json`, `changes.diff` (old-to-active and old-to-incoming),
`final.diff` (active-to-proposed and incoming-to-proposed), and all conflicts.
Final comparisons are unavailable until a complete candidate exists. The helper binds
the old base, active customization, protected state, and new candidate to
hashes. Recheck `--check <path/to/proposal.json>` immediately before an approved
activation. Any drift requires a new proposal. Conflicts require explicit
resolution and review; the helper does not apply the upgrade. Preserve the
actual active files for rollback, including their customizations.

For a manually reconciled complete candidate, the upgrade skill documents
`--resolved-candidate` and `--resolutions`. Generate a fresh preview; do not
edit the old one. Each conflict needs an explicit disposition, both final
comparisons remain reviewable, and `--check` detects candidate or disposition
drift. This creates evidence only, not approval or automatic activation.

## Completion-driven waits

Prefer native completion notifications or a supported blocking wait. Do not
repeatedly wake the manager to sleep, reread unchanged logs, or check whether a
role finished. Continue useful independent authorized work while waiting.

For an external CLI with no native notification, use the host's documented
completion-receipt watcher if available. Read its instructions before use;
the workflow does not install a watcher or assume every host supports one.
On a host providing `~/.codex/COMPLETION_WAKEUPS.md`, read that document for the
local helper's invocation and receipt contract. Preserve its approval boundary.

- Bind one watcher to the real parent manager's identity and one unique
  invocation/generation receipt in the project's ignored `.local/` directory.
  Never guess a thread, select the newest session, or notify the worker itself.
- Close result output before publishing the atomic terminal receipt, including
  failure outcomes. A missing receipt is unknown, not successful completion.
- Inspect watcher state once to confirm it is armed or delivery was accepted.
  A cmux notice only notifies the terminal UI; it is not a manager wake-up.
- When independent work is exhausted, report what is running, its result path,
  and the verified notification mechanism, then yield. With no usable callback,
  use a supported blocking wait; otherwise explain that manual continuation is
  required. Never promise automatic resumption without an armed mechanism.
- On notification, verify the exact receipt/result once and record the
  invocation as handled in the existing ledger. Duplicate notifications do not
  repeat provider work. Completion never grants approval or authorizes retry.
- Failed, ambiguous, or timed-out delivery requires inspection. Never
  automatically resend, relaunch, resume, or treat silence as a hung worker.
  A watcher deadline is a reason to inspect, not permission to kill a process.

## Saved activity and usage

The CLI runner saves an atomic status companion beside each unique completion
receipt as public activity arrives. Completion receipts remain authoritative.
Missing completion evidence means unknown completion, even if a status file
previously reported activity. Do not infer that a retry is safe.

Run `python3 .agents/trip-explorer/bin/run_report.py --project .` for readable
status/history and usage; add `--format json` for structured output. Reports
contain metadata such as duration, first activity, tool counts, and supported
provider usage. They do not archive raw events, prompts, tool arguments, tool
outputs, or private reasoning. Existing results remain separate artifacts.
Codex and custom adapters report unsupported usage as unknown.

cmux launches send one completion notice to the exact launch workspace after
saving the receipt. Direct runner calls may opt in with `--notify-workspace`.
The `.notice.json` companion records the attempt; ambiguous delivery is never
automatically retried. Notices do not wake agents or grant completion authority.

Claude cost and per-model usage may describe the whole retained session. Use
one cumulative snapshot per session rather than adding every invocation's
totals. Missing fields remain unknown; provider cost is an estimate, not a bill.
The summary separates `unavailable_usage_sessions` (no cumulative metrics in
the latest authoritative snapshot) from `ambiguous_usage_sessions` (cross-role
attribution, unreliable ordering/completion, or regressing cumulative values).
Both remain excluded from cumulative totals. Invocation main-loop tokens stay
separate and may still be available. Only sessions with a known ID enter these
session counters. The legacy `unattributable_or_uncertain_sessions` field remains
their combined count for existing consumers.
Nested/parallel role durations do not equal total workflow elapsed time. Keep
exclusive workflow phases, manual timing, and acceptance decisions in the
manager's existing ledger.
