#!/usr/bin/env python3
"""Validate an installed workflow without modifying it or following state symlinks."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
from pathlib import Path
from typing import Any

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from role_config import validate_role_configuration


SKILLS = ("trip-explorer-init", "trip-explorer-upgrade", "trip-explorer-workflow")
REQUIRED = (
    "adapters_valid", "base_matches_manifest", "bin_matches_manifest",
    "config_valid", "init_skill_present", "installed_skill_present",
    "local_state_ignored", "upgrade_skill_present",
)


def safe_path(project: Path, relative: str) -> Path:
    """Resolve only project-owned relative paths; reject even dangling symlinks."""
    if not isinstance(relative, str) or not relative or "\x00" in relative:
        raise ValueError("path must be a nonempty project-relative string")
    candidate = Path(relative)
    if candidate.is_absolute() or ".." in candidate.parts or not candidate.parts:
        raise ValueError("path must stay inside the project")
    cursor = project
    for part in candidate.parts:
        cursor = cursor / part
        if cursor.is_symlink():
            raise ValueError("symlinks are not supported in installation paths")
    return cursor


def read_json(project: Path, relative: str) -> dict[str, Any]:
    path = safe_path(project, relative)
    if not stat.S_ISREG(path.stat().st_mode):
        raise ValueError("expected a regular JSON file")
    if path.stat().st_size > 4 * 1024 * 1024:
        raise ValueError("JSON file exceeds the 4 MiB size limit")
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError("expected a JSON object")
    return value


def hashes(root: Path) -> dict[str, str]:
    if root.is_symlink():
        raise ValueError("symlinks are not supported in installation paths")
    if not root.is_dir():
        raise ValueError("expected an installed directory")
    result: dict[str, str] = {}

    def fail(error: OSError) -> None:
        raise error

    for directory, names, files in os.walk(root, followlinks=False, onerror=fail):
        for name in sorted(names + files):
            path = Path(directory) / name
            mode = path.lstat().st_mode
            if stat.S_ISLNK(mode):
                raise ValueError("symlinks are not supported in installation paths")
            if stat.S_ISDIR(mode):
                continue
            if not stat.S_ISREG(mode):
                raise ValueError("installation contains a non-regular file")
            digest = hashlib.sha256()
            with path.open("rb") as stream:
                for chunk in iter(lambda: stream.read(65536), b""):
                    digest.update(chunk)
            result[path.relative_to(root).as_posix()] = digest.hexdigest()
    return result


def active_package_hashes(project: Path, state: Path, skills_root: str = ".agents/skills") -> dict[str, str]:
    result: dict[str, str] = {}
    skills = safe_path(project, skills_root)
    for name in SKILLS:
        for key, value in hashes(skills / name).items():
            result[f"skills/{name}/{key}"] = value
    for key, value in hashes(state / "bin").items():
        result[f"bin/{key}"] = value
    return result


def guidance_path(project: Path, relative: str) -> Path:
    """Accept regular guidance files or directory trees without links/special files."""
    path = safe_path(project, relative)
    mode = path.stat().st_mode
    if stat.S_ISREG(mode):
        return path
    if not stat.S_ISDIR(mode):
        raise ValueError("guidance must be a regular file or directory")

    def fail(error: OSError) -> None:
        raise error

    for directory, names, files in os.walk(path, followlinks=False, onerror=fail):
        for name in names + files:
            mode = (Path(directory) / name).lstat().st_mode
            if not (stat.S_ISREG(mode) or stat.S_ISDIR(mode)):
                raise ValueError("guidance directory contains a symlink or special file")
    return path


def config_valid(data: object, project: Path) -> bool:
    if not isinstance(data, dict):
        return False
    verification = data.get("verification")
    documentation = data.get("documentation")
    observability = data.get("observability")
    testing = data.get("testing")
    guidance = data.get("guidance")
    if not isinstance(guidance, list):
        return False
    try:
        for item in guidance:
            guidance_path(project, item)
    except (OSError, ValueError, TypeError):
        return False
    return (
        isinstance(data.get("project_name"), str)
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


def check_item(code: str, severity: str, message: str, action: str = "") -> dict[str, str]:
    return {"code": code, "severity": severity, "message": message, "action": action}


def inspect_installation(project: Path) -> tuple[dict[str, Any], dict[str, Any]]:
    """Return legacy validation keys plus diagnostics, and safely loaded state."""
    report: dict[str, Any] = {key: False for key in REQUIRED}
    report.update(config_customized=False, customized_from_base=False, version=None,
                  skills_root=".agents/skills", checks=[], role_errors=[])
    documents: dict[str, Any] = {}
    checks = report["checks"]
    try:
        project = project.resolve(strict=True)
        if not project.is_dir() or project == Path(project.anchor):
            raise ValueError("project must be a non-root directory")
        state = safe_path(project, ".agents/trip-explorer")
    except (OSError, ValueError, RuntimeError):
        checks.append(check_item("installation.path", "error", "Project or installation path is missing, invalid, or contains a symlink.", "Select the repository root and reconcile the installation path."))
        return report, documents
    for name in ("manifest", "config", "adapters", "preflight"):
        try:
            documents[name] = read_json(project, f".agents/trip-explorer/{name}.json")
        except (OSError, ValueError, UnicodeError, RecursionError):
            checks.append(check_item(f"installation.{name}", "error", f"{name}.json is missing, malformed, oversized, or unsafe to read.", f"Restore or reconcile .agents/trip-explorer/{name}.json from reviewed installation evidence."))
    manifest = documents.get("manifest", {})
    version = manifest.get("version")
    if isinstance(version, str) and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        report["version"] = version
        try:
            report["base_matches_manifest"] = hashes(safe_path(project, f".agents/trip-explorer/base/{version}")) == manifest.get("base")
        except (OSError, ValueError):
            pass
    else:
        checks.append(check_item("installation.version", "error", "Manifest version is missing or invalid.", "Recover the correct versioned installation manifest."))
    checks.append(check_item("installation.base", "pass" if report["base_matches_manifest"] else "error", "Versioned base matches its manifest." if report["base_matches_manifest"] else "Versioned base is missing, unsafe, or differs from its manifest.", "" if report["base_matches_manifest"] else "Recover the exact installed package base before preparing an upgrade; preserve active customizations."))
    try:
        report["bin_matches_manifest"] = hashes(safe_path(project, ".agents/trip-explorer/bin")) == manifest.get("bin")
    except (OSError, ValueError):
        pass
    checks.append(check_item("installation.runtime", "pass" if report["bin_matches_manifest"] else "error", "Runtime helpers match their manifest." if report["bin_matches_manifest"] else "Runtime helpers are missing, unsafe, or differ from their manifest.", "" if report["bin_matches_manifest"] else "Review helper changes and recover or upgrade the complete runtime with its manifest."))
    skills_root = manifest.get("skills_root", ".agents/skills")
    try:
        skills = safe_path(project, skills_root)
        report["skills_root"] = skills_root
        for name, key in zip(SKILLS, ("init_skill_present", "upgrade_skill_present", "installed_skill_present")):
            skill = safe_path(project, f"{skills_root}/{name}/SKILL.md")
            report[key] = skill.is_file()
            if not report[key]:
                checks.append(check_item(f"installation.{name}", "error", f"{name}/SKILL.md is missing.", "Restore the missing skill at the manifest's skills_root."))
        report["customized_from_base"] = active_package_hashes(project, state, skills_root) != manifest.get("base")
        skill_customized = any(
            {key: value for key, value in hashes(skills / name).items()} != {
                key[len(f"skills/{name}/"):]: value for key, value in manifest.get("base", {}).items()
                if key.startswith(f"skills/{name}/")
            } for name in SKILLS
        ) if isinstance(manifest.get("base"), dict) else False
        if skill_customized:
            checks.append(check_item("installation.skill_customization", "info", "Active skills differ from the generic base; project customization is expected.", "Preserve and review these changes during the three-way upgrade."))
    except (OSError, ValueError, TypeError):
        checks.append(check_item("installation.skills", "error", "Skill root or contents are missing or unsafe to read.", "Use a project-relative manifest skills_root without symlinks and recover missing skills."))
    config, adapters, preflight = (documents.get(key) for key in ("config", "adapters", "preflight"))
    report["config_valid"] = config_valid(config, project)
    if not report["config_valid"]:
        checks.append(check_item("configuration.project", "error", "Project configuration has invalid fields or missing/unsafe guidance paths.", "Check project_name, guidance files or directories, verification command arrays, documentation, observability, testing.coverage, roles, and profiles."))
    try:
        config_path = safe_path(project, ".agents/trip-explorer/config.json")
        report["config_customized"] = "config" in documents and hashlib.sha256(config_path.read_bytes()).hexdigest() != manifest.get("config_sha256")
    except (OSError, ValueError):
        pass
    problems = validate_role_configuration(config, adapters, preflight)
    # Validator messages name fields, never dump configuration or credential values.
    problems = [re.sub(r"(invalid adapter kind for [^:]+):.*", r"\1", item) for item in problems]
    problems = [re.sub(r"(unsupported placeholders for [^:]+):.*", r"\1", item) for item in problems]
    report["role_errors"] = problems
    report["adapters_valid"] = not problems
    for problem in problems:
        checks.append(check_item("configuration.role", "error", problem, "Correct the named role/adapter/profile field and rerun affected profile preflight before use."))
    try:
        report["local_state_ignored"] = subprocess.run(
            ["git", "-C", str(project), "check-ignore", "-q", ".local/trip-explorer/.ignore-probe"],
            check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5,
        ).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        pass
    checks.append(check_item("installation.local_ignore", "pass" if report["local_state_ignored"] else "error", "Local workflow artifacts are ignored by Git." if report["local_state_ignored"] else "Local workflow artifact ignore rule is missing or could not be verified.", "" if report["local_state_ignored"] else "Add /.local/trip-explorer/ to project Git ignore rules."))
    return report, documents


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--project", required=True, type=Path)
    args = parser.parse_args()
    report, _ = inspect_installation(args.project)
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if all(report[key] for key in REQUIRED) and not any(check["severity"] == "error" for check in report["checks"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
