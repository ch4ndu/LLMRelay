# Optional maintenance helpers

Use these when diagnosing an installation, reviewing an upgrade, or inspecting
saved role runs. Python is optional; the manager can inspect the same evidence
directly. Commands are run from the target project root. No helper authorizes
provider calls, configuration changes, retry, or workflow completion.

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
  --output .local/trip-explorer/upgrade-preview/0.11.0
```

Review `proposal.json`, the readable diff, and all conflicts. The helper binds
the old base, active customization, protected state, and new candidate to
hashes. Recheck `--check <path/to/proposal.json>` immediately before an approved
activation. Any drift requires a new proposal. Conflicts require explicit
resolution and review; the helper does not apply the upgrade. Preserve the
actual active files for rollback, including their customizations.

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
Nested/parallel role durations do not equal total workflow elapsed time. Keep
exclusive workflow phases, manual timing, and acceptance decisions in the
manager's existing ledger.
