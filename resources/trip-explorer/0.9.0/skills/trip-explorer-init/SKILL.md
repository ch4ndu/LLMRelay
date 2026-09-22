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
6. Start the final-verifier probe fresh without a resume identity.

Normalize successful evidence in `preflight.json`: result, nonce match,
profile ids, adapter, provider, model, model evidence, effort, authority, and
session. Do not store raw logs, hidden reasoning, environment dumps, or secrets.
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
4. Copy optional runtime helpers beneath `.agents/trip-explorer/bin/`.
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
