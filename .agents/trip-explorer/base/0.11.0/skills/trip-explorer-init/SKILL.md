---
name: trip-explorer-init
description: Initialize the project-local TRIP Explorer workflow without Python, including approved AGENTS.md guidance, provider-neutral delegated-role selection, live invocation preflight, and installed-state validation.
---

# Initialize TRIP Explorer

Initialize the copied TRIP Explorer skills in the current Git repository. The
active host agent remains manager and user-facing authority. Configure only
Explorer, Plan reviewer, Implementer/fixer, Code reviewer, and Final verifier.

Do not invoke the engineering workflow until initialization is complete. Do not
create `ARCHI.md`, `ARCHI-rules.md`, or a workflow-owned documentation tree.

## Pre-write gate

Before changing the target:

1. Resolve `git rev-parse --show-toplevel` and require the current project to be
   exactly that non-root directory.
2. Inventory status and existing instructions. Preserve unrelated changes.
3. Require both copied skills: `trip-explorer-workflow` and
   `trip-explorer-upgrade`. Stop on a partial prior installation beneath
   `.agents/trip-explorer/`.
4. Reject symlinked destination ancestry or any path escaping the repository.
5. Keep all choices and probe evidence in conversation until every required
   profile passes. A failed initialization writes no installation state.

## Project guidance

Read existing root `AGENTS.md` completely when present. It is project-owned.
Never replace it. Identify only missing durable guidance needed by installed
agents: project shape, authoritative documents, existing conventions and
constraints, applicable verification commands, manual/device boundaries, and
authority limits. Present one exact proposed patch and wait for explicit
approval before applying it.

When `AGENTS.md` is absent, present the exact concise file proposed from current
repository evidence and wait for approval before creating it. Names and source
structure should carry ordinary architecture; do not generate an exhaustive
architecture encyclopedia. Record `AGENTS.md` in configured guidance. Agent-
specific instruction files may point to it when the host requires that, but do
not duplicate or overwrite existing instructions.

## Project choices

Ask the user to select `minimal`, `moderate`, or `extensive` test coverage.
Collect project name, task-relevant guidance paths, focused/broad/cleanup
verification commands, documentation no-change text, and `cmux` mode. Commands
must come from repository evidence or user input; never invent them. An empty
verification matrix is allowed but blocks verified-complete implementation
claims where no applicable evidence exists.
Guidance accepts existing project-relative regular files or directories. Reject
symlinks and special files, including within guidance directories; preserve the
selected paths during upgrades and snapshot directory contents recursively.

## Delegated role selection

Discover available native-agent mechanisms and installed CLIs without assuming
that an executable is authenticated or can invoke a requested model. Offer
built-in Codex and Claude adapters when available and a structured custom CLI
adapter. For each delegated role, pause and ask for adapter, provider, exact
model, effort when supported, optional service tier, authority, and session
behavior. Put a source-backed recommendation first, but never select it
silently.

The required contracts are fixed:

| Role | Authority | Session |
| --- | --- | --- |
| `explorer` | `read-only` | `retained` |
| `plan_reviewer` | `read-only` | `retained` |
| `implementer` | `workspace-write` | `retained` |
| `code_reviewer` | `read-only` | `retained` |
| `final_verifier` | `read-only` | `fresh` |

The same model may serve several roles through independent sessions. Warn when
writer and every reviewer use the same model because diversity is weaker, but
do not prohibit it. The manager is never selectable: it is the active host
agent.

Store no credentials. A custom CLI adapter uses an executable plus a structured
argument array; prohibit shell strings, `eval`, command substitutions, secret
values, and placeholders other than `{project}`, `{prompt_file}`,
`{result_file}`, `{completion_file}`, `{model}`, `{effort}`,
`{service_tier}`, `{session_id}`, and `{authority}`.

## Invocation preflight

Before installation writes, show the unique profile probes and warn that live
model calls may use network access, subscription/API quota, and provider-side
history. Deduplicate only identical adapter/provider/model/effort/tier/
authority/session tuples.

For each unique profile:

1. Verify adapter schema, executable/native definition, authentication where a
   non-mutating status check exists, authority support, fresh-session support,
   and resume support required by retained roles.
2. Reject recursive workflow wrappers and unintended terminal shims.
3. Invoke the exact configured model with a random nonce prompt that prohibits
   tools, file access, delegation, and workflows. Require the exact nonce,
   successful completion, and no reported fallback.
4. Use provider-reported effective model identity when exposed. Otherwise
   record `requested-model-accepted; provider did not expose effective identity`
   without claiming stronger evidence.
5. Probe read-only profiles in a disposable empty Git repository with enforced
   read-only permissions. Probe workspace-write profiles in a disposable Git
   repository by creating one nonce file with exact content. Remove the
   disposable repository after inspection.
6. For retained roles, follow up in the exact native session UUID and verify
   continuity without a latest-session selector. Start the final-verifier
   probe fresh without a resume identity. Record the installed CLI version
   separately from model evidence; help/flag acceptance alone is not a live
   model, authority, or session qualification.

Normalize successful evidence in `preflight.json`: result, nonce match,
profile ids, adapter, provider, model, model evidence, effort, service tier when
selected, authority, and session. Built-in Claude profiles cannot select a
service tier. Do not store raw logs, hidden reasoning, environment dumps, or secrets.
For CLI profiles, also record `cli_executable` as the absolute **realpath** of
the chosen executable after excluding cmux shims/wrappers and resolving every
symlink; the selected PATH alias is insufficient. Record `cli_version` as the
semantic version and `adapter_sha256` as SHA-256 of the exact adapter object
at `adapters[adapter_id]`, without its ID-keyed wrapper. Hash UTF-8 bytes of JSON
with sorted keys, compact separators `(',', ':')`, and ASCII escaping
(`ensure_ascii=True`), with **no trailing newline**. These identify the proven
runtime for later doctor checks; missing or alias-only older evidence remains
unverified and requires canonical identity at the next affected preflight.

When Python is available, this optional snippet uses the staged package's
published helpers for builtin CLI identity. Replace the package path, adapter
file, and adapter ID; run it from the same working directory and PATH as the
role launcher. It runs only bounded `--version`, never a model probe:

```sh
python3 - /path/to/trip-explorer-install/scripts /path/to/adapters.json codex-cli "$PWD" <<'PY'
import json
import sys
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, sys.argv[1])
from workflow_doctor import adapter_fingerprint, cli_version, resolve_executable

adapter = json.loads(Path(sys.argv[2]).read_text())["adapters"][sys.argv[3]]
if adapter.get("kind") != "builtin-cli" or adapter.get("builtin") not in ("codex", "claude"):
    raise SystemExit("Use the custom adapter's documented safe identity check instead.")
project = Path(sys.argv[4]).resolve(strict=True)
executable = resolve_executable(adapter["executable"], project)
print(json.dumps({"cli_executable": executable,
                  "cli_version": cli_version(executable, project),
                  "adapter_sha256": adapter_fingerprint(adapter)}, sort_keys=True))
PY
```

Without Python, record these same canonical forms using equivalent local tools;
Python remains optional. Identity collection does not replace the required live
model, authority, and session qualification above.
Any failure blocks installation. Let the user correct, retry, choose another
profile, or abort; never substitute automatically.

## Apply after approval

After all probes pass, present the complete guidance patch, project config,
adapters, role matrix, probe summary, and exact paths. Ask once for installation
approval. Then:

1. Apply the separately approved `AGENTS.md` patch, if any.
2. Install the generic workflow under the host's project skill root without
   rewriting its provider-neutral content.
3. Create `.agents/trip-explorer/config.json`, `adapters.json`,
   `preflight.json`, `manifest.json`, and an unchanged versioned base snapshot.
   Record the chosen project-relative skill directory as manifest `skills_root`.
4. Copy every helper named by package `runtime-files.json` beneath
   `.agents/trip-explorer/bin/`; record their hashes and the unchanged base.
5. Add `/.local/trip-explorer/` to the repository-local Git exclude file only
   when not already ignored, then verify with `git check-ignore`.
6. Preserve `AGENTS.md`, config, adapters, and preflight as project-owned state.

Build the complete staged result before moving it into final paths. On any
write failure, remove only artifacts proven to have been created by this
attempt; never delete pre-existing files.

## Validate and report

Validate required files, skill/base parity, config shape, guidance containment,
exact role coverage, profile-to-adapter references, role capabilities,
successful preflight for every referenced profile, absence of secrets, safe
custom placeholders, and the local ignore rule. Report request-level results
separately from package/helper validation. Initialization is complete only when
all required checks pass and the user can invoke `$trip-explorer-workflow`.
