# Initial v1 engineering delivery

September 25, 2026. The agreed initial-v1 engineering scope is implemented and
has passed its independent review and automated verification gates. This is an
engineering delivery record, not a claim of live-provider or personal acceptance.

## Delivered scope

- Core workflow engine, permission/approval control, manager replacement and cmux session access.
- M4A current-schema backup/restore and guarded restore recovery.
- M4B crash-boundary reconciliation and recovery records.
- M5 explainable holds, guarded retries, restart readiness and the final capability path-boundary repair.
- M6 dashboard live state and attention routing.
- M7 engineering provider-compatibility and qualification contracts.
- M8A client protocol negotiation and stale-client refusal.
- M9 immutable project profile sets, task recipes, draft creation and explicitly enabled daily/weekly UTC intake while the foreground service is running.

## Verification

The final unchanged source candidate passed `./scripts/verify.sh`: 29 dashboard
DOM tests, 67 Rust library tests, 103 contract tests and two runtime tests, plus
frontend type checking/build, Rust formatting and locked compilation. The final
path-boundary repair passed ordinary Sol review and fresh Fable final review with
no findings. Its tests preserve Claude rule syntax, intentional probe-key
equivalence and reservation drift checks while preventing sibling-path corruption.
M9 passed fresh final recheck after repairing actual App-level draft navigation.

Each milestone's review and test evidence is retained under the ignored
`.local/trip-explorer/` task directories. No extra full-suite run is needed merely
for packaging; source or build-input changes invalidate relevant evidence.

## Local artifact

The consolidated archive is produced at
`.local/trip-explorer/llmrelay_path_boundary_20260925/package/llmrelay-0.1.0-macos.zip`.
The adjacent task `package-receipt.json` records its SHA-256, byte count, archive
integrity result and packaged executable version. The package includes both CLI
names and the usage, operations, security, workflow and building documentation.
This is a local build artifact; no live installation, restart, signing, publication,
commit or tag is performed by this delivery step. See [building](BUILDING.md).

## Remaining boundaries

Personal hands-on testing and deferred live/native provider qualification remain
pending. M7's production Claude selector remains unqualified rather than claiming
support from synthetic fixtures. Automated/source evidence does not establish
live-service restart, GUI, real-project or cross-tab acceptance. An already-running
service continues to use its existing executable until separately replaced.

M2 LaunchAgent, M3 menu-bar companion, M8B signed distribution/update activation,
and native notifications are post-v1 and excluded from this completion gate.
Historical installation migration is excluded. See [milestones](MILESTONES.md) and
[the agreed scope](NEXT_MILESTONE_PLAN.md).
