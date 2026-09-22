---
name: trip-explorer-upgrade
description: Stage, review, and apply a TRIP Explorer package upgrade without overwriting project-owned AGENTS.md, role selections, custom adapters, or configuration.
---

# Upgrade TRIP Explorer

Upgrade a copied and initialized project-local workflow from a user-supplied
staging directory. Never download an upgrade implicitly. Remain read-only until
the user approves the exact proposal.

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

Classify package files as new, removed, unchanged, updated uncustomized, or
updated customized. Leave active-only files in place and warn; never delete a
user-created file. Validate that every package producer/consumer pair—role
names, profile fields, adapter schema, runtime helpers, receipts, and paths—comes
from one coherent staged version.

## Proposal

If active package-owned files match the recorded base, propose replacing them
with the staged package as one atomic unit. If they are customized, build a
three-way proposal from old base, active customization, and new base entirely
in staging. Preserve semantics and show every conflict. Do not apply a guessed
resolution.

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
old installation until the new candidate is ready; on failure restore only
package-owned files from the recorded old base and leave project state alone.

Validate required files, unresolved placeholders, workflow/runtime references,
role/profile/adapter schema, successful preflight, guidance containment, base
parity, installed version, and ignore rules. Show the final diff and ask before
removing staging. Upgrade does not authorize commit, push, tag, release, or
other Git mutation.
