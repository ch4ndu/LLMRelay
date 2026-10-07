#!/usr/bin/env python3
"""Save a conservative three-way upgrade proposal without changing an installation."""

from __future__ import annotations

import argparse
import difflib
import hashlib
import json
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Optional

sys.dont_write_bytecode = True

SKILLS = ("trip-explorer-init", "trip-explorer-upgrade", "trip-explorer-workflow")
SCHEMA = 1


def absolute_path(value: Path) -> Path:
    path = value.expanduser().absolute()
    if ".." in path.parts:
        raise ValueError(f"parent traversal is not supported: {path}")
    return path


def checked_path(value: Path) -> Path:
    path = absolute_path(value)
    for ancestor in reversed((path, *path.parents)):
        if ancestor.is_symlink():
            raise ValueError(f"symlinks are not supported: {ancestor}")
    return path


def input_path(value: Path, project: Path, project_alias: Path, *, external: bool = False) -> Path:
    """Resolve external root aliases without hiding project-internal symlinks."""
    path = absolute_path(value)
    for root in (project_alias, project):
        if path == root or root in path.parents:
            return checked_path(project / path.relative_to(root))
    # A caller may mix canonical project paths with a different external alias
    # (for example /private/tmp/project and /tmp/project). Find the project
    # boundary before checking its descendants, rather than resolving them away.
    for ancestor in reversed(path.parents):
        if ancestor.resolve(strict=False) == project:
            return checked_path(project / path.relative_to(ancestor))
    if not external:
        raise ValueError("path must be beneath the supplied or canonical project root")
    canonical = path.resolve(strict=True)
    if canonical == project or project in canonical.parents:
        raise ValueError("in-project candidates must use the supplied or canonical project root")
    return checked_path(canonical)


def relative_path(value: Any) -> Path:
    if not isinstance(value, str) or not value:
        raise ValueError("expected a nonempty project-relative path")
    path = Path(value)
    if path.is_absolute() or ".." in path.parts or path == Path("."):
        raise ValueError(f"path must remain inside the project: {value}")
    return path


def version(value: Any) -> tuple[int, ...]:
    if not isinstance(value, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", value):
        raise ValueError(f"invalid version: {value}")
    return tuple(int(part) for part in value.split("."))


def read_object(path: Path) -> dict[str, Any]:
    value = json.loads(checked_path(path).read_text())
    if not isinstance(value, dict):
        raise ValueError(f"expected a JSON object: {path}")
    return value


def tree(path: Path) -> dict[str, dict[str, Any]]:
    """Inventory directory entries as well as bytes, permissions, and absence."""
    checked_path(path)
    if not path.exists():
        return {}
    if not path.is_dir():
        raise ValueError(f"expected a directory: {path}")
    result = {}
    for item in sorted(path.rglob("*")):
        checked_path(item)
        metadata = item.stat()
        entry: dict[str, Any] = {"mode": stat.S_IMODE(metadata.st_mode)}
        if item.is_file():
            entry.update(kind="file", sha256=hashlib.sha256(item.read_bytes()).hexdigest())
        elif item.is_dir():
            entry["kind"] = "directory"
        else:
            raise ValueError(f"special files are not supported: {item}")
        result[item.relative_to(path).as_posix()] = entry
    return result


def selected_tree(roots: dict[str, Path]) -> tuple[dict[str, Any], dict[str, Path]]:
    inventory, sources = {}, {}
    for prefix, root in roots.items():
        checked_path(root)
        if not root.is_dir():
            raise ValueError(f"required installation directory is missing: {root}")
        inventory[prefix] = {"kind": "directory", "mode": stat.S_IMODE(root.stat().st_mode)}
        sources[prefix] = root
        for name, entry in tree(root).items():
            key = f"{prefix}/{name}"
            inventory[key], sources[key] = entry, root / name
    return inventory, sources


def inspect_inputs(project: Path, candidate: Optional[Path], new_base: Optional[Path],
                   target_version: Optional[str]) -> tuple[dict[str, Any], dict[str, dict[str, Path]]]:
    project_alias = absolute_path(project)
    project = project_alias.resolve(strict=True)
    git = subprocess.run(["git", "-C", str(project), "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=False)
    if git.returncode or Path(git.stdout.strip()).resolve() != project:
        raise ValueError("project must be the Git repository root")
    state = checked_path(project / ".agents/trip-explorer")
    manifest = read_object(state / "manifest.json")
    config = read_object(state / "config.json")
    read_object(state / "adapters.json")
    read_object(state / "preflight.json")
    installed_version = manifest.get("version")
    version(installed_version)
    skills_root = relative_path(manifest.get("skills_root", ".agents/skills"))
    active_roots = {"bin": state / "bin"}
    active_roots.update({f"skills/{name}": checked_path(project / skills_root / name) for name in SKILLS})
    if any(state == root or state in root.parents for root in list(active_roots.values())[1:]):
        raise ValueError("skills_root must not overlap installation state")
    old_base = checked_path(state / "base" / installed_version)
    if not old_base.is_dir():
        raise ValueError(f"old base is missing; explicit migration is required: {old_base}")
    old = tree(old_base)
    old_hashes = {name: entry["sha256"] for name, entry in old.items() if entry["kind"] == "file"}
    if old_hashes != manifest.get("base"):
        raise ValueError("old base does not match manifest; reconcile its provenance before previewing")
    active, active_sources = selected_tree(active_roots)
    # The generic base includes the shared skills directory; the active mapping
    # deliberately excludes other, unrelated project skills.
    active["skills"] = {"kind": "directory", "mode": stat.S_IMODE((project / skills_root).stat().st_mode)}
    active_sources["skills"] = checked_path(project / skills_root)
    if candidate is not None:
        candidate = input_path(candidate, project, project_alias, external=True)
        candidate_inventory = tree(candidate)
        target_version = (candidate / "VERSION").read_text().strip()
        runtime_files = json.loads((candidate / "runtime-files.json").read_text())
        if (not isinstance(runtime_files, list) or not runtime_files or
                not all(isinstance(name, str) and re.fullmatch(r"[a-z][a-z0-9_]*\.py", name) for name in runtime_files) or
                len(runtime_files) != len(set(runtime_files))):
            raise ValueError("runtime-files.json must contain unique Python basenames")
        incoming_roots = {f"skills/{name}": candidate / "assets/templates" / name for name in SKILLS}
        incoming, incoming_sources = selected_tree(incoming_roots)
        for name in runtime_files:
            source = checked_path(candidate / "scripts" / name)
            if not source.is_file():
                raise ValueError(f"candidate runtime file is missing: {source}")
            incoming[f"bin/{name}"] = candidate_inventory[f"scripts/{name}"]
            incoming_sources[f"bin/{name}"] = source
        for key, root in (("bin", candidate / "scripts"), ("skills", candidate / "assets/templates")):
            incoming[key] = {"kind": "directory", "mode": stat.S_IMODE(root.stat().st_mode)}
            incoming_sources[key] = root
        source_binding = {"kind": "package", "path": str(candidate), "inventory": candidate_inventory}
    else:
        if new_base is None:
            raise ValueError("a candidate package or new-base is required")
        new_base = input_path(new_base, project, project_alias, external=True)
        if not new_base.is_dir():
            raise ValueError(f"new-base directory is missing: {new_base}")
        incoming = tree(new_base)
        incoming_sources = {name: new_base / name for name in incoming}
        source_binding = {"kind": "new-base", "path": str(new_base), "inventory": incoming}
    if version(target_version) <= version(installed_version):
        raise ValueError("target version must be newer than installed version")
    allowed_roots = {"bin", "skills", *(f"skills/{name}" for name in SKILLS)}
    for label, nodes in (("old base", old), ("candidate", incoming)):
        for name in nodes:
            if name not in allowed_roots and not any(name.startswith(root + "/") for root in ("bin", *(f"skills/{skill}" for skill in SKILLS))):
                raise ValueError(f"unexpected package path in {label}: {name}")
        for name in ("bin", *(f"skills/{skill}/SKILL.md" for skill in SKILLS)):
            if name not in nodes:
                raise ValueError(f"incomplete {label}: missing {name}")
    guidance = config.get("guidance", [])
    if not isinstance(guidance, list):
        raise ValueError("config guidance must be an array")
    configured_paths = {relative_path(item).as_posix() for item in guidance}
    protected_paths = {"AGENTS.md", *configured_paths}
    protected, protected_sources = {}, {}
    directories = []
    for name in sorted(protected_paths, key=lambda item: (len(Path(item).parts), item)):
        path = checked_path(project / name)
        if name in configured_paths and not path.exists():
            raise ValueError(f"configured guidance is missing: {path}")
        if path.exists() and not (path.is_file() or path.is_dir()):
            raise ValueError(f"protected guidance must be a regular file or directory: {path}")
        if any(name.startswith(parent + "/") for parent in directories):
            continue
        if not path.exists():
            protected[name] = None
            continue
        protected_sources[name] = path
        protected[name] = {"kind": "directory" if path.is_dir() else "file", "mode": stat.S_IMODE(path.stat().st_mode)}
        if path.is_file():
            protected[name]["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
        else:
            directories.append(name)
            for child, entry in tree(path).items():
                protected[f"{name}/{child}"] = entry
                protected_sources[f"{name}/{child}"] = path / child
    bindings = {"project": str(project), "installed_version": installed_version,
                "target_version": target_version, "skills_root": skills_root.as_posix(),
                "old_base": old, "active": active, "installed_state": tree(state),
                "protected_guidance": protected, "candidate": source_binding}
    sources = {"old": {name: old_base / name for name in old}, "active": active_sources,
               "incoming": incoming_sources, "state": {name: state / name for name in bindings["installed_state"]},
               "guidance": protected_sources}
    return bindings, sources


def classify(old: Any, active: Any, incoming: Any) -> tuple[str, str, Optional[str]]:
    if old == active == incoming:
        return "unchanged", "keep", "active"
    if old is None:
        if active is None:
            return "added", "add", "incoming"
        if incoming is None:
            return "local-only", "preserve-active-only", "active"
        if active == incoming:
            return "both-changed", "keep-identical-addition", "active"
        return "conflict", "added-path-collision", None
    if active == incoming:
        return ("removed", "remove", None) if active is None else ("both-changed", "keep-identical-change", "active")
    if active == old:
        return ("removed", "remove", None) if incoming is None else ("upstream-only", "replace", "incoming")
    if incoming == old:
        return "local-only", "preserve-local-change", "active" if active is not None else None
    return "conflict", "divergent-changes", None


def decisions(bindings: dict[str, Any]) -> list[dict[str, Any]]:
    old, active = bindings["old_base"], bindings["active"]
    source = bindings["candidate"]
    if source["kind"] == "new-base":
        incoming = source["inventory"]
    else:
        incoming = {}
        # The package mapping is already checked by inspect_inputs. Reconstruct
        # only its installed members, without loading any candidate Python.
        package = Path(source["path"])
        files = json.loads((package / "runtime-files.json").read_text())
        for name, item in source["inventory"].items():
            if name.startswith("assets/templates/") and any(name == f"assets/templates/{skill}" or name.startswith(f"assets/templates/{skill}/") for skill in SKILLS):
                incoming[name.replace("assets/templates/", "skills/", 1)] = item
            elif name.startswith("scripts/") and name[8:] in files:
                incoming["bin/" + name[8:]] = item
        for key, mapped in (("skills", "assets/templates"), ("bin", "scripts")):
            incoming[key] = source["inventory"][mapped]
    entries = []
    for name in sorted(set(old) | set(active) | set(incoming)):
        category, action, choice = classify(old.get(name), active.get(name), incoming.get(name))
        entries.append({"path": name, "classification": category, "action": action, "source": choice,
                        "old": old.get(name), "active": active.get(name), "incoming": incoming.get(name)})
    for entry in entries:
        descendants = [item for item in entries if item["source"] and item["path"].startswith(entry["path"] + "/")]
        if not descendants:
            continue
        if entry["classification"] == "removed" and entry["active"] and entry["active"]["kind"] == "directory":
            entry.update(classification="local-only", action="preserve-active-only-directory", source="active")
        elif entry["old"] and entry["old"]["kind"] == "directory" and entry["active"] is None:
            entry.update(classification="conflict", action="local-directory-removal-vs-upstream-change", source=None)
    chosen = {entry["path"]: entry for entry in entries if entry["source"]}
    for name, entry in chosen.items():
        node = entry["active"] if entry["source"] == "active" else entry["incoming"]
        if node["kind"] != "file":
            continue
        descendants = [item for path, item in chosen.items() if path.startswith(name + "/")]
        if descendants:
            for collision in [entry, *descendants]:
                collision.update(classification="conflict", action="file-directory-collision", source=None)
    for entry in entries:
        if entry["classification"] == "conflict":
            for descendant in entries:
                if descendant["path"].startswith(entry["path"] + "/"):
                    descendant.update(classification="conflict", action="unresolved-ancestor", source=None)
    return entries


def copy_nodes(destination: Path, nodes: dict[str, Any], sources: dict[str, Path]) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    for name, item in sorted(nodes.items(), key=lambda pair: (len(Path(pair[0]).parts), pair[0])):
        if item is None:
            continue
        target = destination / name
        if item["kind"] == "directory":
            target.mkdir(parents=True, exist_ok=True)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            checked_path(sources[name])
            shutil.copyfile(sources[name], target)
            target.chmod(item["mode"])
            if hashlib.sha256(target.read_bytes()).hexdigest() != item["sha256"]:
                raise ValueError(f"source changed while copying snapshot: {sources[name]}")
    for name, item in sorted(nodes.items(), key=lambda pair: len(Path(pair[0]).parts), reverse=True):
        if item and item["kind"] == "directory":
            (destination / name).chmod(item["mode"])


def readable_diff(entries: list[dict[str, Any]], sources: dict[str, dict[str, Path]],
                  comparisons: tuple[tuple[str, str], ...] = (("old", "active"), ("old", "incoming"))) -> str:
    lines = []
    for entry in entries:
        if entry["classification"] == "unchanged":
            continue
        name = entry["path"]
        lines.append(f"\n### {name}: {entry['classification']} ({entry['action']})\n")
        for before_side, side in comparisons:
            before, after = entry[before_side], entry[side]
            if before == after:
                continue
            if any(item and item["kind"] != "file" for item in (before, after)):
                lines.append(f"{before_side} -> {side}: {json.dumps(before)} -> {json.dumps(after)}\n")
                continue
            try:
                old_text = sources[before_side][name].read_text() if before else ""
                new_text = sources[side][name].read_text() if after else ""
                if "\0" in old_text + new_text:
                    raise UnicodeError("binary content")
                lines.extend(difflib.unified_diff(old_text.splitlines(keepends=True), new_text.splitlines(keepends=True),
                                                  fromfile=before_side + "/" + name, tofile=side + "/" + name))
                if before and after and before["mode"] != after["mode"]:
                    lines.append(f"mode: {oct(before['mode'])} -> {oct(after['mode'])}\n")
            except UnicodeError:
                lines.append(f"binary {before_side} -> {side}: {json.dumps(before)} -> {json.dumps(after)}\n")
    return "".join(lines) or "No package changes.\n"


def inspect_resolution(resolved: Path, reasons: Path, entries: list[dict[str, Any]]) -> dict[str, Any]:
    nodes = tree(resolved)
    dispositions = read_object(reasons)
    conflicts = {entry["path"] for entry in entries if entry["classification"] == "conflict"}
    if set(dispositions) != conflicts or any(not isinstance(reason, str) or not reason.strip() for reason in dispositions.values()):
        raise ValueError("resolutions must record a nonempty reason for every conflict and no other path")
    if set(nodes) - {entry["path"] for entry in entries}:
        raise ValueError("resolved candidate contains unexpected paths; reconcile the source package first")
    for entry in entries:
        if entry["classification"] != "conflict":
            expected = entry[entry["source"]] if entry["source"] else None
            if nodes.get(entry["path"]) != expected:
                raise ValueError(f"resolved candidate changes a non-conflicting decision: {entry['path']}")
    for name, kind in (("bin", "directory"), *((f"skills/{skill}/SKILL.md", "file") for skill in SKILLS)):
        if nodes.get(name, {}).get("kind") != kind:
            raise ValueError(f"resolved candidate is incomplete: {name}")
    return {"path": str(resolved), "inventory": nodes, "resolutions_file": str(reasons),
            "resolutions_sha256": hashlib.sha256(reasons.read_bytes()).hexdigest(), "dispositions": dispositions}


def create_preview(project: Path, output: Path, candidate: Optional[Path] = None,
                   new_base: Optional[Path] = None, target_version: Optional[str] = None,
                   resolved_candidate: Optional[Path] = None, resolutions: Optional[Path] = None) -> Path:
    project_alias = absolute_path(project)
    project = project_alias.resolve(strict=True)
    output = input_path(output, project, project_alias)
    if project / ".local" not in output.parents:
        raise ValueError("preview output must be a new directory under the project's .local/")
    if output.exists():
        raise ValueError(f"preview output already exists: {output}")
    candidate = input_path(candidate, project, project_alias, external=True) if candidate is not None else None
    new_base = input_path(new_base, project, project_alias, external=True) if new_base is not None else None
    source = candidate if candidate is not None else new_base
    if source is not None and (source == output or source in output.parents or output in source.parents):
        raise ValueError("preview output and candidate must not overlap")
    bindings, sources = inspect_inputs(project, candidate, new_base, target_version)
    protected_roots = [project / ".agents/trip-explorer", project / bindings["skills_root"], *sources["guidance"].values()]
    if any(path == output or path in output.parents or output in path.parents for path in protected_roots):
        raise ValueError("preview output must not overlap active installation or protected guidance")
    entries = decisions(bindings)
    conflicts = [entry["path"] for entry in entries if entry["classification"] == "conflict"]
    resolution = None
    if (resolved_candidate is None) != (resolutions is None):
        raise ValueError("--resolved-candidate and --resolutions must be supplied together")
    if resolved_candidate is not None:
        resolved_candidate = input_path(resolved_candidate, project, project_alias)
        resolutions = input_path(resolutions, project, project_alias)
        for path in (resolved_candidate, resolutions):
            if project / ".local" not in path.parents:
                raise ValueError("reconciliation inputs must be under the project's .local/")
            if any(path == other or path in other.parents or other in path.parents for other in (output, *protected_roots)):
                raise ValueError("reconciliation inputs overlap preview output, active installation, or guidance")
        resolution = inspect_resolution(resolved_candidate, resolutions, entries)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".upgrade-preview-", dir=output.parent) as temp:
        staging = Path(temp)
        copy_nodes(staging / "snapshot/active", bindings["active"], sources["active"])
        copy_nodes(staging / "snapshot/state", bindings["installed_state"], sources["state"])
        copy_nodes(staging / "snapshot/guidance", bindings["protected_guidance"], sources["guidance"])
        chosen = {entry["path"]: entry[entry["source"]] for entry in entries if entry["source"]}
        selected_sources = {entry["path"]: sources[entry["source"]][entry["path"]] for entry in entries if entry["source"]}
        if resolution is not None:
            chosen = resolution["inventory"]
            selected_sources = {name: resolved_candidate / name for name in chosen}
        copy_nodes(staging / "proposed", chosen, selected_sources)
        (staging / "changes.diff").write_text(readable_diff(entries, sources))
        if not conflicts or resolution is not None:
            final_entries = [{**entry, "proposed": chosen.get(entry["path"])} for entry in entries]
            final_sources = {**sources, "proposed": {name: staging / "proposed" / name for name in chosen}}
            (staging / "final.diff").write_text(readable_diff(final_entries, final_sources,
                                                            (("active", "proposed"), ("incoming", "proposed"))))
        else:
            (staging / "final.diff").write_text("No complete candidate: unresolved conflicts prevent final comparisons.\n")
        summary = {category: sum(entry["classification"] == category for entry in entries) for category in
                   ("added", "removed", "unchanged", "upstream-only", "local-only", "both-changed", "conflict")}
        review = [f"# Upgrade preview {bindings['installed_version']} → {bindings['target_version']}\n",
                  "No active installation files were changed. Activation still requires manager review, validation, and approval.\n",
                  "`proposed/` is incomplete while unresolved conflicts exist. It is never an automatic activation payload.\n",
                  "`snapshot/active` and `snapshot/state` preserve the actual customization for rollback.\n",
                  "Project configuration, adapters, preflight, and guidance are preserved without migration.\n",
                  "Input hashes and the complete generated artifact inventory are checked with `--check proposal.json`.\n",
                  "## Counts\n", *[f"- {key}: {value}\n" for key, value in summary.items()],
                  "\n## Changes\n", *[f"- `{entry['path']}`: {entry['classification']} / {entry['action']}\n" for entry in entries if entry["classification"] != "unchanged"],
                  "\nReview both old-to-active and old-to-incoming comparisons in `changes.diff`.\n",
                  "Review active-to-proposed and incoming-to-proposed comparisons in `final.diff` when the candidate is complete.\n"]
        if resolution is not None:
            review.extend(["## Recorded conflict dispositions (not approval)\n",
                           *[f"- `{name}`: {reason}\n" for name, reason in resolution["dispositions"].items()]])
        (staging / "review.md").write_text("\n".join(review))
        fresh, _ = inspect_inputs(project, candidate, new_base, target_version)
        if fresh != bindings:
            raise ValueError("inputs changed during preview; generate a new preview after reconciling changes")
        if resolution is not None and inspect_resolution(resolved_candidate, resolutions, entries) != resolution:
            raise ValueError("reconciliation inputs changed during preview; generate a new preview")
        proposal = {"schema_version": SCHEMA, "bindings": bindings, "entries": entries,
                    "summary": summary, "conflicts": conflicts if resolution is None else [],
                    "resolution": resolution, "activation_approved": False,
                    "artifacts": tree(staging)}
        (staging / "proposal.json").write_text(json.dumps(proposal, indent=2, sort_keys=True) + "\n")
        if output.exists():
            raise ValueError(f"preview output appeared during generation: {output}")
        staging.rename(output)
    return output / "proposal.json"


def check_preview(proposal_path: Path) -> dict[str, Any]:
    requested = absolute_path(proposal_path)
    git = subprocess.run(["git", "-C", str(requested.parent), "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=False)
    if git.returncode:
        raise ValueError("preview must remain inside its project repository")
    project = Path(git.stdout.strip()).resolve(strict=True)
    project_alias = next((path for path in reversed(requested.parents) if path.resolve(strict=True) == project), None)
    if project_alias is None:
        raise ValueError("preview must remain inside its project repository")
    proposal_path = input_path(requested, project, project_alias)
    proposal = read_object(proposal_path)
    if proposal.get("schema_version") != SCHEMA:
        raise ValueError("unsupported preview schema")
    previous = proposal["bindings"]
    if previous.get("project") != str(project):
        raise ValueError("preview project does not match its repository")
    source = previous["candidate"]
    if source["kind"] not in ("package", "new-base"):
        raise ValueError("unsupported candidate kind")
    current, _ = inspect_inputs(Path(previous["project"]), Path(source["path"]) if source["kind"] == "package" else None,
                               Path(source["path"]) if source["kind"] == "new-base" else None, previous["target_version"])
    if current != previous:
        changed = [name for name in current if current[name] != previous.get(name)]
        raise ValueError("preview is stale; changed inputs: " + ", ".join(changed))
    if decisions(current) != proposal["entries"]:
        raise ValueError("proposal decisions do not match bound inputs")
    artifacts = tree(proposal_path.parent)
    artifacts.pop(proposal_path.name, None)
    if artifacts != proposal["artifacts"]:
        raise ValueError("preview artifacts changed; regenerate rather than activating an altered preview")
    conflicts = [entry["path"] for entry in proposal["entries"] if entry["classification"] == "conflict"]
    resolution = proposal.get("resolution")
    if resolution is not None:
        resolved = input_path(Path(resolution["path"]), project, project)
        reasons = input_path(Path(resolution["resolutions_file"]), project, project)
        if inspect_resolution(resolved, reasons, proposal["entries"]) != resolution:
            raise ValueError("reconciliation inputs changed; generate a new preview")
        if tree(proposal_path.parent / "proposed") != resolution["inventory"]:
            raise ValueError("proposed files do not match the recorded reconciliation")
    unresolved = conflicts if resolution is None else []
    return {"current": True, "conflicts": unresolved, "ready_for_review": not unresolved,
            "resolved_conflicts": conflicts if resolution is not None else [],
            "activation_approved": False, "proposal": str(proposal_path)}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path)
    parser.add_argument("--output", type=Path)
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--candidate", type=Path, help="complete package containing runtime-files.json")
    source.add_argument("--new-base", type=Path, help="already staged bin/ and skills/ generic tree")
    parser.add_argument("--target-version", help="required with --new-base")
    parser.add_argument("--resolved-candidate", type=Path, help="complete manually reconciled bin/ and skills/ tree under project .local/")
    parser.add_argument("--resolutions", type=Path, help="JSON object mapping every conflicting path to its explicit resolution reason")
    parser.add_argument("--check", type=Path, help="verify source and artifact inventories before manual activation")
    args = parser.parse_args()
    try:
        if args.check:
            if any((args.project, args.output, args.candidate, args.new_base, args.target_version, args.resolved_candidate, args.resolutions)):
                parser.error("--check cannot be combined with preview arguments")
            report = check_preview(args.check)
            print(json.dumps(report, indent=2, sort_keys=True))
            return 0 if report["ready_for_review"] else 1
        else:
            if not args.project or not args.output or not (args.candidate or args.new_base):
                parser.error("--project, --output, and --candidate or --new-base are required")
            if args.new_base and not args.target_version:
                parser.error("--new-base requires --target-version")
            if args.candidate and args.target_version:
                parser.error("--candidate reads its own VERSION; omit --target-version")
            path = create_preview(args.project, args.output, args.candidate, args.new_base, args.target_version,
                                  args.resolved_candidate, args.resolutions)
            print(json.dumps({"proposal": str(path), "activation_approved": False}, sort_keys=True))
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
