#!/usr/bin/env python3
"""Check an optional acceptance/evidence ledger without executing its checks."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
from pathlib import Path
from typing import Any


IDENTIFIER = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,79}\Z")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")


def reject_constant(value: str) -> None:
    raise ValueError(f"invalid JSON constant: {value}")


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON field: {key}")
        result[key] = value
    return result


def contained_file(project: Path, value: Any) -> Path:
    if not isinstance(value, str) or not value:
        raise ValueError("expected a nonempty file path")
    path = Path(value)
    if ".." in path.parts:
        raise ValueError("parent traversal is not allowed")
    candidate = path if path.is_absolute() else project / path
    try:
        relative = candidate.relative_to(project)
    except ValueError:
        raise ValueError("path must be inside the project") from None
    cursor = project
    for part in relative.parts:
        cursor = cursor / part
        if cursor.is_symlink():
            raise ValueError("symlinked files or ancestry are not allowed")
    if not candidate.is_file():
        raise ValueError("file does not exist or is not a regular file")
    return candidate


def check_ledger(project: Path, ledger: Any) -> dict[str, Any]:
    problems: list[dict[str, str]] = []

    def issue(code: str, path: str, message: str, severity: str = "error") -> None:
        problems.append({"code": code, "path": path, "message": message, "severity": severity})

    def nonempty(value: Any) -> bool:
        return isinstance(value, str) and bool(value.strip())

    def records(name: str) -> dict[str, dict[str, Any]]:
        rows = ledger.get(name)
        if not isinstance(rows, list):
            issue("schema", name, "expected an array")
            return {}
        result: dict[str, dict[str, Any]] = {}
        for index, row in enumerate(rows):
            path = f"{name}[{index}]"
            if not isinstance(row, dict):
                issue("schema", path, "expected an object")
                continue
            identifier = row.get("id")
            if not isinstance(identifier, str) or not IDENTIFIER.fullmatch(identifier):
                issue("schema", path + ".id", "expected a stable ID of 1-80 letters, digits, dots, underscores, or hyphens")
            elif identifier in result:
                issue("duplicate_id", path + ".id", f"duplicate ID: {identifier}")
            else:
                result[identifier] = row
        return result

    def references(value: Any, choices: dict[str, Any], path: str, required: bool = False) -> list[str]:
        if not isinstance(value, list) or (required and not value):
            issue("schema", path, "expected a nonempty ID array" if required else "expected an ID array")
            return []
        found = []
        for index, identifier in enumerate(value):
            if not isinstance(identifier, str) or identifier not in choices:
                issue("unknown_reference", f"{path}[{index}]", f"unknown ID: {identifier!r}")
            elif identifier in found:
                issue("duplicate_reference", f"{path}[{index}]", f"duplicate ID: {identifier}")
            else:
                found.append(identifier)
        return found

    if not isinstance(ledger, dict):
        ledger = {}
        issue("schema", "$", "ledger must be a JSON object")
    if type(ledger.get("schema_version")) is not int or ledger.get("schema_version") != 1:
        issue("schema", "schema_version", "supported schema_version is 1")
    criteria = records("criteria")
    evidence = records("evidence")
    findings = records("findings")
    gaps = records("gaps")
    if not criteria:
        issue("empty_acceptance", "criteria", "at least one named requested outcome is required")
    for identifier, row in criteria.items():
        if not nonempty(row.get("description")):
            issue("schema", f"criteria.{identifier}.description", "describe the requested outcome")
        if row.get("state") not in ("pending", "implemented", "deferred"):
            issue("schema", f"criteria.{identifier}.state", "expected pending, implemented, or deferred")

    coverage: dict[str, list[str]] = {identifier: [] for identifier in criteria}
    current_evidence: set[str] = set()
    build_tests = []
    repeated: dict[tuple[Any, ...], str] = {}
    for identifier, row in evidence.items():
        path = f"evidence.{identifier}"
        before = len(problems)
        kind = row.get("kind")
        status = row.get("status")
        if kind not in ("outcome", "build", "test"):
            issue("schema", path + ".kind", "expected outcome, build, or test")
        if status not in ("pass", "fail", "blocked"):
            issue("schema", path + ".status", "expected pass, fail, or blocked")
        covered = references(row.get("criteria"), criteria, path + ".criteria", kind == "outcome")
        if not nonempty(row.get("check")):
            issue("schema", path + ".check", "name the command or inspection; it will not be executed")
        if row.get("scope") not in ("focused", "broad"):
            issue("schema", path + ".scope", "expected focused or broad")
        for optional in ("platform", "candidate", "invalidation_reason"):
            if optional in row and not nonempty(row[optional]):
                issue("schema", path + "." + optional, "expected nonempty text when provided")
        elapsed = row.get("elapsed_seconds", 0)
        if type(elapsed) not in (int, float) or (isinstance(elapsed, float) and not math.isfinite(elapsed)) or elapsed < 0:
            issue("schema", path + ".elapsed_seconds", "expected a nonnegative number")
        try:
            contained_file(project, row.get("artifact"))
        except (OSError, ValueError) as error:
            issue("artifact", path + ".artifact", str(error))
        inputs = row.get("inputs")
        if not isinstance(inputs, dict) or not inputs:
            issue("schema", path + ".inputs", "record nonempty scoped file paths and their SHA-256 hashes")
            inputs = {}
        for filename, expected in inputs.items():
            input_path = f"{path}.inputs.{filename}"
            if not isinstance(expected, str) or not SHA256.fullmatch(expected):
                issue("schema", input_path, "expected a lowercase SHA-256 digest")
                continue
            try:
                source = contained_file(project, filename)
                actual = hashlib.sha256(source.read_bytes()).hexdigest()
                if actual != expected:
                    issue("stale_input", input_path, "file changed since this evidence was recorded")
            except (OSError, ValueError) as error:
                issue("input", input_path, str(error))
        valid = not any(item["severity"] == "error" for item in problems[before:])
        if valid and status == "pass":
            current_evidence.add(identifier)
            if kind == "outcome":
                for criterion in covered:
                    coverage[criterion].append(identifier)
        if kind in ("build", "test"):
            build_tests.append({"id": identifier, "kind": kind, "status": status, "inputs_and_artifact_valid": valid})
        if valid and row["scope"] == "broad" and status != "blocked":
            signature = (row["check"], row.get("platform"), tuple(sorted(inputs.items())))
            if signature in repeated and not row.get("invalidation_reason"):
                issue("repeated_broad_check", path, f"same check and scoped inputs as {repeated[signature]}; record the reason for repeating it", "warning")
            repeated[signature] = identifier

    accepted_gaps = []
    unaccepted_gaps = []
    deferred_criteria: set[str] = set()
    for identifier, row in gaps.items():
        path = f"gaps.{identifier}"
        before = len(problems)
        covered = references(row.get("criteria"), criteria, path + ".criteria", True)
        if row.get("kind") not in ("manual", "device", "external", "deferred"):
            issue("schema", path + ".kind", "expected manual, device, external, or deferred")
        if not nonempty(row.get("description")):
            issue("schema", path + ".description", "describe the remaining gap")
        if type(row.get("accepted")) is not bool:
            issue("schema", path + ".accepted", "expected an explicit boolean")
        accepted = row.get("accepted") is True
        if accepted and not nonempty(row.get("approval")):
            issue("schema", path + ".approval", "record the user's specific acceptance; a boolean alone is insufficient")
        gap = {"id": identifier, "criteria": covered, "kind": row.get("kind"), "description": row.get("description"), "approval": row.get("approval")}
        if accepted and len(problems) == before:
            accepted_gaps.append(gap)
            deferred_criteria.update(covered)
        else:
            unaccepted_gaps.append(gap)
            issue("unaccepted_gap", path, "remaining gap lacks valid recorded user acceptance")

    unresolved_findings = []
    for identifier, row in findings.items():
        path = f"findings.{identifier}"
        before = len(problems)
        disposition = row.get("disposition")
        if disposition not in ("open", "fixed", "rejected", "deferred"):
            issue("schema", path + ".disposition", "expected open, fixed, rejected, or deferred")
        if disposition != "open" and not nonempty(row.get("reason")):
            issue("schema", path + ".reason", "record the disposition rationale")
        linked = references(row.get("evidence", []), evidence, path + ".evidence")
        if disposition == "fixed" and not any(item in current_evidence and evidence[item].get("kind") == "outcome" for item in linked):
            issue("finding_evidence", path, "a fixed finding needs passing, current outcome evidence")
        if disposition == "deferred" and (row.get("accepted") is not True or not nonempty(row.get("approval"))):
            issue("finding_acceptance", path, "a deferred finding needs explicit user acceptance and an approval record")
        if disposition == "open" or len(problems) != before:
            unresolved_findings.append(identifier)
            issue("unresolved_finding", path, "finding disposition is unresolved")

    acceptance = []
    for identifier, row in criteria.items():
        state = row.get("state")
        if state == "implemented" and coverage[identifier]:
            status = "evidenced"
        elif state == "deferred" and identifier in deferred_criteria:
            status = "accepted-gap"
        else:
            status = "missing-outcome-evidence" if state == "implemented" else "incomplete"
            issue("acceptance_gap", f"criteria.{identifier}", "needs passing current outcome evidence for implemented work, or an explicitly accepted deferred gap")
        acceptance.append({"id": identifier, "description": row.get("description"), "state": state, "status": status, "outcome_evidence": coverage[identifier]})
    return {
        "schema_version": 1,
        "ledger_checks_passed": not any(item["severity"] == "error" for item in problems),
        "request_verification": acceptance,
        "build_test_verification": build_tests,
        "unresolved_findings": unresolved_findings,
        "accepted_gaps": accepted_gaps,
        "unaccepted_gaps": unaccepted_gaps,
        "problems": problems,
        "boundary": "Checks recorded evidence consistency only; the manager verifies meaning, approval, and completion.",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--ledger", required=True, help="file relative to the project, or an absolute path inside it")
    parser.add_argument("--json", action="store_true", help="emit a machine-readable report")
    args = parser.parse_args()
    try:
        if args.project.is_symlink():
            raise ValueError("project must not be a symlink")
        project = args.project.resolve(strict=True)
        if not project.is_dir():
            raise ValueError("project must be a directory")
        path = contained_file(project, args.ledger)
        ledger = json.loads(path.read_text(), parse_constant=reject_constant, object_pairs_hook=unique_object)
        report = check_ledger(project, ledger)
    except (OSError, UnicodeError, ValueError, RecursionError) as error:
        report = {"schema_version": 1, "ledger_checks_passed": False, "problems": [{"code": "ledger", "path": args.ledger, "message": str(error), "severity": "error"}]}
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print("Ledger checks: " + ("passed" if report["ledger_checks_passed"] else "gaps found"))
        for row in report.get("request_verification", []):
            print(f"Outcome {row['id']}: {row['status']} — {row['description']}")
        for row in report.get("build_test_verification", []):
            print(f"{row['kind'].title()} {row['id']}: {row['status']} (inputs/artifact valid: {row['inputs_and_artifact_valid']})")
        for gap in report.get("accepted_gaps", []):
            print(f"Accepted gap {gap['id']}: {gap['description']}")
        for problem in report["problems"]:
            print(f"{problem['severity']}: {problem['path']}: {problem['message']}")
        print("This is evidence consistency checking, not a completion verdict.")
    return 0 if report["ledger_checks_passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
