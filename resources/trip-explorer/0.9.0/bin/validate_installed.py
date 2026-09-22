#!/usr/bin/env python3
"""Validate an installed workflow without modifying it."""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from role_config import validate_role_configuration


def hashes(root: Path) -> dict[str, str]:
    return {
        path.relative_to(root).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(root.rglob("*"))
        if path.is_file() and not path.is_symlink()
    }


def active_package_hashes(project: Path, state: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    skills = project / ".agents" / "skills"
    for name in ("trip-explorer-init", "trip-explorer-upgrade", "trip-explorer-workflow"):
        root = skills / name
        for key, value in hashes(root).items():
            result[f"skills/{name}/{key}"] = value
    for key, value in hashes(state / "bin").items():
        result[f"bin/{key}"] = value
    return result


def config_valid(data: object, project: Path) -> bool:
    if not isinstance(data, dict):
        return False
    verification = data.get("verification")
    documentation = data.get("documentation")
    observability = data.get("observability")
    testing = data.get("testing")
    guidance = data.get("guidance")
    guidance_valid = isinstance(guidance, list)
    if guidance_valid:
        for item in guidance:
            candidate = Path(item) if isinstance(item, str) else Path("..")
            if candidate.is_absolute() or ".." in candidate.parts:
                guidance_valid = False
                break
            cursor = project
            for part in candidate.parts:
                cursor = cursor / part
                if cursor.exists() and cursor.is_symlink():
                    guidance_valid = False
                    break
            if not guidance_valid:
                break
            full = project / candidate
            if not full.exists() or full.is_symlink():
                guidance_valid = False
                break
            resolved = full.resolve(strict=True)
            if project not in resolved.parents:
                guidance_valid = False
                break
    return (
        isinstance(data.get("project_name"), str)
        and guidance_valid
        and isinstance(verification, dict)
        and all(isinstance(verification.get(key), list) and all(isinstance(item, str) for item in verification[key]) for key in ("focused", "broad", "cleanup"))
        and isinstance(documentation, dict)
        and isinstance(documentation.get("no_change_text"), str)
        and isinstance(observability, dict)
        and observability.get("cmux") in ("auto", "on", "off")
        and isinstance(data.get("roles"), dict)
        and isinstance(data.get("profiles"), dict)
        and isinstance(testing, dict)
        and testing.get("coverage") in ("minimal", "moderate", "extensive")
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--project", required=True, type=Path)
    args = parser.parse_args()
    project = args.project.resolve(strict=True)
    state = project / ".agents" / "trip-explorer"
    manifest_path = state / "manifest.json"
    if not manifest_path.is_file():
        raise SystemExit(f"error: missing {manifest_path}")
    manifest = json.loads(manifest_path.read_text())
    version = str(manifest["version"])
    base = state / "base" / version
    installed = project / ".agents" / "skills" / "trip-explorer-workflow"
    init_skill = project / ".agents" / "skills" / "trip-explorer-init" / "SKILL.md"
    upgrade_skill = project / ".agents" / "skills" / "trip-explorer-upgrade" / "SKILL.md"
    config = state / "config.json"
    adapters = state / "adapters.json"
    preflight = state / "preflight.json"
    config_data = json.loads(config.read_text())
    adapters_data = json.loads(adapters.read_text()) if adapters.is_file() else None
    preflight_data = json.loads(preflight.read_text()) if preflight.is_file() else None
    initial_config = hashlib.sha256(config.read_bytes()).hexdigest() == manifest["config_sha256"]
    ignore = __import__("subprocess").run(
        ["git", "-C", str(project), "check-ignore", "-q", ".local/trip-explorer/.ignore-probe"], check=False
    ).returncode == 0
    report = {
        "base_matches_manifest": hashes(base) == manifest["base"],
        "bin_matches_manifest": hashes(state / "bin") == manifest["bin"],
        "adapters_valid": adapters.is_file() and not validate_role_configuration(config_data, adapters_data, preflight_data),
        "config_customized": not initial_config,
        "config_valid": config_valid(config_data, project),
        "customized_from_base": active_package_hashes(project, state) != manifest["base"],
        "installed_skill_present": (installed / "SKILL.md").is_file(),
        "init_skill_present": init_skill.is_file(),
        "local_state_ignored": ignore,
        "version": version,
        "upgrade_skill_present": upgrade_skill.is_file(),
    }
    print(json.dumps(report, indent=2, sort_keys=True))
    required = (
        "adapters_valid", "base_matches_manifest", "bin_matches_manifest",
        "config_valid", "init_skill_present", "installed_skill_present",
        "local_state_ignored", "upgrade_skill_present",
    )
    return 0 if all(report[key] for key in required) else 1


if __name__ == "__main__":
    raise SystemExit(main())
