---
name: trip-explorer-upgrade
description: Stage, review, and apply a TRIP Explorer package upgrade without overwriting project-owned AGENTS.md, role selections, custom adapters, or configuration.
---

# Upgrade TRIP Explorer

Upgrade a copied and initialized project-local workflow from a user-supplied
staging directory. Never download an upgrade implicitly. Prepare review
artifacts in unused staging paths; leave the active installation unchanged
until the user approves the exact proposal.

## Immutable ownership boundary

`AGENTS.md` is project-owned. Upgrade never edits it, including to add new
workflow guidance. Preserve `config.json`, role mappings, profiles, custom
adapters, and preflight evidence. A required schema migration is a proposed
project-config patch, not an automatic package replacement.

## Inventory

Resolve the Git root and reject escaping or symlinked paths. Require a complete
initialized installation and a staged package version newer than the installed
version. Inventory:

- old generic base;
- active workflow and runtime;
- staged new generic workflow and runtime;
- project-owned config/adapters/preflight;
- extra files that exist only in the active installation.

Classify package files as added, removed, unchanged, upstream-only, local-only,
both-changed, or conflicting. Leave active-only files in place and warn; never delete a
user-created file. Validate that every package producer/consumer pair—role
names, profile fields, adapter schema, runtime helpers, receipts, and paths—comes
from one coherent staged version.

## Proposal

If active package-owned files match the recorded base, propose replacing them
with the staged package as one atomic unit. If they are customized, build a
three-way proposal from old base, active customization, and new base entirely
in staging. Preserve semantics and show every conflict. Do not apply a guessed
resolution.

When Python is available, the staged package's optional helper saves this
comparison without executing candidate code or changing the active installation:

```sh
python3 /path/to/trip-explorer-install/scripts/upgrade_preview.py \
  --project /path/to/project \
  --candidate /path/to/trip-explorer-install \
  --output /path/to/project/.local/trip-explorer/upgrade-preview/0.11.0
```

The complete candidate must include `runtime-files.json`. For an already staged
generic tree, replace `--candidate` with `--new-base /path/to/new-base
--target-version 0.11.0`. Use the candidate's actual version and a fresh output
directory. Missing or unverifiable old bases require explicit migration, never
an assumed three-way merge. The optional manifest `skills_root` identifies the
host's skill directory; its default is `.agents/skills`.

Review `review.md`, both comparisons in `changes.diff`, and `proposal.json`.
`snapshot/active` and `snapshot/state` preserve actual installed customizations;
`snapshot/guidance` preserves protected guidance. `proposed/` contains only
unambiguous choices and is incomplete if conflicts exist. Divergent edits, even
in different lines of a file, require manager reconciliation. No helper verdict
proves schema compatibility, successful preflight, or approval. Record resolved
conflicts and migrations as a separate exact final candidate/diff with fresh
source and candidate inventories; never silently edit a saved preview in place.

Without Python, use the same agent-native comparison, snapshots, hash inventories,
conflict report, and pre-activation drift check. Python is an optional evidence
producer, not a prerequisite or a separate approval authority.

If the staged version needs new configuration fields, show an exact config or
adapter patch and identify affected profiles. Never change selected providers,
models, efforts, authorities, or session modes automatically. Re-run live
preflight only for profiles affected by an approved adapter/schema change, and
require success before activation.

Present installed and target versions, package actions, preserved project-owned
paths, conflicts, config migrations, profiles requiring re-probe, validation
plan, and exact final diff. Ask for explicit approval.

## Apply and validate

After approval, assemble the complete candidate in staging, validate it, then
move package-owned files into place. Record the new base and manifest only
after the active package is coherent. Do not mutate `AGENTS.md`. Preserve the
old installation until the new candidate is ready. Immediately before activation,
recheck all source and candidate inventories. For an unchanged, conflict-free
helper proposal, run:

```sh
python3 /path/to/trip-explorer-install/scripts/upgrade_preview.py \
  --check /path/to/project/.local/trip-explorer/upgrade-preview/0.11.0/proposal.json
```

The helper fails on source or artifact drift and on unresolved conflicts. If
inputs changed, regenerate in a new staging directory and review again. Check
manually reconciled candidates against their separately recorded inventories.
The bound candidate inventory includes caches and editor files. Running tests
inside the staged package can therefore invalidate a preview; finish candidate
validation before generating it, or regenerate afterward.
The check grants no activation authority. On failure restore package-owned files
and their prior manifest from the snapshot of the actual active installation,
including customization; the generic old base is not a rollback snapshot. Leave
project configuration and guidance alone unless an exact migration was approved,
in which case restore their captured pre-activation state if that migration fails.

Validate required files, unresolved placeholders, workflow/runtime references,
role/profile/adapter schema, successful preflight, guidance containment, base
parity, installed version, and ignore rules. Show the final diff and ask before
removing staging. Upgrade does not authorize commit, push, tag, release, or
other Git mutation.
