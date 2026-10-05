# Optional acceptance and evidence check

The manager may export the current acceptance matrix and verification receipts
into this small JSON format when deterministic consistency checks help. It is
an optional view of the existing task ledger, not another scheduler, authority,
or required workflow state. Normal agent-native operation does not require
Python or this file. Keep exports under the project's ignored
`.local/trip-explorer/` directory.

With the optional helper installed:

```sh
python3 .agents/trip-explorer/bin/check_evidence.py \
  --project "$PWD" --ledger .local/trip-explorer/task/evidence.json --json
```

Omit `--json` for a readable report. The checker only reads files; it never runs
recorded commands, launches models, accepts deferrals, or changes the ledger.
Exit `0` means the recorded structure and evidence checks passed. Exit `1`
means there are errors or unaccepted gaps. Neither result is a completion or
review verdict. The manager still verifies causal relevance, actual requested
behavior, user approval, and the final outcome.

## Schema version 1

The root object contains `schema_version: 1` and four arrays: `criteria`,
`evidence`, `findings`, and `gaps`. The last three may be empty. At least one
criterion is required. Each record has a unique `id` within its array, using
1-80 letters, digits, dots, underscores, or hyphens, starting with a letter or
digit. References must name existing IDs. Unknown extra fields are preserved
in the source ledger but are not interpreted by this version of the checker.

- **Criteria:** `id`, a nonempty `description` of the requested behavior, and
  `state` (`pending`, `implemented`, or `deferred`). Each implemented criterion
  needs at least one passing, current `outcome` evidence record. A deferred
  criterion needs an explicitly accepted gap referring to it. Pending work
  remains incomplete even when a related check passes.
- **Evidence:** `id`, `kind` (`outcome`, `build`, or `test`), `status` (`pass`,
  `fail`, or `blocked`), `criteria` (an ID array), `artifact` (a file path),
  `inputs` (a nonempty object mapping scoped file paths to lowercase SHA-256
  digests), `check` (the command or inspection description), and `scope`
  (`focused` or `broad`). An `outcome` record must reference at least one
  criterion. Record `platform`, `candidate`, `elapsed_seconds`, and
  `invalidation_reason` when applicable; optional text must be nonempty and
  elapsed seconds must be nonnegative and finite.
- **Findings:** `id` and `disposition` (`open`, `fixed`, `rejected`, or
  `deferred`). Non-open dispositions require `reason`. A fixed finding requires
  `evidence` IDs with at least one passing, current outcome record. A rejected
  finding requires a reasoned disposition, not fabricated evidence. A deferred
  finding requires `accepted: true` and nonempty `approval` recording the
  user's actual acceptance. Open, missing, and invalid dispositions are
  reported separately from acceptance coverage.
- **Gaps:** `id`, `kind` (`manual`, `device`, `external`, or `deferred`),
  nonempty `criteria` IDs, `description`, and an explicit `accepted` boolean.
  Accepted gaps also require `approval` recording the user's specific decision
  and where it was given. An approval string is a record, not proof or new
  permission. Unaccepted gaps always remain visible errors. Accepted gaps
  remain visible in the report and can cover only criteria marked `deferred`.

Use `kind: outcome` only when the manager has mapped a causal test, direct
inspection, or other evidence to the requested behavior. Generic build success
and unqualified test-suite success use `build` or `test`; they cannot satisfy
an outcome by merely naming its criterion. An outcome record can describe a
test command: it need not duplicate that same receipt as a `test` record.
The checker cannot determine whether the manager's causal mapping is correct.

Artifact and input paths are relative to the project, or absolute paths inside
it. Parent traversal, missing/non-file targets, and symlinked paths or ancestry
are rejected. Inputs are hashed exactly as recorded; choose all relevant
source, configuration, dependency, and build inputs. Include the evidence
artifact itself among the hashed inputs when its byte identity matters.
The checker verifies artifact existence; it does not interpret its contents.
It cannot discover omitted dependencies or external state changes.

Export receipts relevant to the current candidate. Retain obsolete historical
receipts in the original workflow ledger rather than copying stale evidence
into this current-candidate view. Failed or blocked checks cannot cover an
outcome. Repeated broad checks with identical `check`, `platform`, and scoped
input hashes produce a warning unless the later record includes an
`invalidation_reason`. Evidence array order is chronological. This warning
identifies avoidable repetition without invalidating otherwise usable evidence
or requiring another run.

## Example

This illustrates a console-activity outcome, a separate compile check, a fixed
finding, and an accepted manual boundary. Replace paths, approval text, and
digest placeholders with evidence from the actual task. The placeholders
deliberately fail validation; copying this example does not establish success.

```json
{
  "schema_version": 1,
  "criteria": [
    {"id": "A1", "description": "Tool activity appears before the final answer", "state": "implemented"},
    {"id": "A2", "description": "The activity display is legible in the target terminal", "state": "deferred"}
  ],
  "evidence": [
    {
      "id": "E1", "kind": "outcome", "status": "pass", "criteria": ["A1"],
      "artifact": ".local/trip-explorer/task/activity-order.txt",
      "inputs": {"src/runner.py": "SHA256_RECORDED_AT_VERIFICATION_TIME"},
      "check": "Inspect actual tool-start and final-answer timestamps",
      "scope": "focused", "platform": "macOS", "candidate": "recorded diff identity",
      "elapsed_seconds": 8.4
    },
    {
      "id": "E2", "kind": "build", "status": "pass", "criteria": [],
      "artifact": ".local/trip-explorer/task/compile.txt",
      "inputs": {"src/runner.py": "SHA256_RECORDED_AT_VERIFICATION_TIME"},
      "check": "Recorded project compile command", "scope": "focused"
    }
  ],
  "findings": [
    {"id": "F1", "disposition": "fixed", "reason": "Activity forwarding is now immediate", "evidence": ["E1"]}
  ],
  "gaps": [
    {
      "id": "G1", "kind": "manual", "criteria": ["A2"],
      "description": "Target-terminal visual inspection remains pending",
      "accepted": true, "approval": "Record the user's exact accepted boundary and message reference"
    }
  ]
}
```

The JSON report separates `request_verification` from
`build_test_verification`, lists `unresolved_findings`, `accepted_gaps`, and
`unaccepted_gaps`, and supplies actionable `problems` with code, field path,
message, and severity. `ledger_checks_passed` is a consistency result only;
accepted gaps remain gaps in the manager's request-level handoff.
