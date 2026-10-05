#!/usr/bin/env python3
"""Read-only installation and CLI drift diagnostics; never run provider probes."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import selectors
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from validate_installed import check_item, inspect_installation


VERSION_TIMEOUT = 5.0
VERSION_OUTPUT_LIMIT = 65536


def adapter_fingerprint(adapter: dict[str, Any]) -> str:
    """Hash UTF-8 canonical JSON: sorted keys, compact separators, default ASCII escaping."""
    return hashlib.sha256(json.dumps(adapter, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()


def resolve_executable(executable: str, project: Path) -> str:
    if not executable or "\x00" in executable:
        raise ValueError("invalid executable")
    # Keep selection aligned with cmux_role_runner.resolve_executable: its PATH
    # lookup happens in the manager cwd before the provider starts in project.
    # Canonicalize only after selecting the same non-cmux launcher candidate.
    candidate_name = Path(executable)
    candidates = [candidate_name] if candidate_name.is_absolute() else [Path(item) / executable for item in os.environ.get("PATH", "").split(os.pathsep) if item]
    for candidate in candidates:
        if not candidate.is_file() or not os.access(candidate, os.X_OK):
            continue
        resolved = candidate.resolve(strict=True)
        if "/cmux-cli-shims/" in candidate.as_posix():
            continue
        if resolved.name == "cmux-codex-wrapper" or "/cmux.app/" in resolved.as_posix().lower():
            continue
        return str(resolved)
    raise ValueError("real executable not found outside cmux shims/wrappers")


def cli_version(executable: str, project: Path) -> str:
    """Read only --version, with bounded time/output and no raw output disclosure."""
    process = subprocess.Popen(
        [executable, "--version"], cwd=project, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True,
        env={**os.environ, "CMUX_CODEX_HOOKS_DISABLED": "1"},
    )
    output = bytearray()
    deadline = time.monotonic() + VERSION_TIMEOUT
    try:
        with selectors.DefaultSelector() as selector:
            assert process.stdout is not None
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise ValueError("version check timed out")
                ready = selector.select(remaining)
                if not ready:
                    raise ValueError("version check timed out")
                for key, _ in ready:
                    chunk = os.read(key.fd, 4096)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    output.extend(chunk)
                    if len(output) > VERSION_OUTPUT_LIMIT:
                        raise ValueError("version output exceeded limit")
        remaining = max(0.01, deadline - time.monotonic())
        if process.wait(timeout=remaining) != 0:
            raise ValueError("version check failed")
        value = output.decode("utf-8", errors="strict").strip()
        match = re.fullmatch(r"(?:(?:codex(?:-cli)?|Claude Code)\s+)?v?([0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?)(?:\s+\(Claude Code\))?", value)
        if not match:
            raise ValueError("unrecognized version output")
        return match.group(1)
    except (subprocess.TimeoutExpired, UnicodeError) as error:
        raise ValueError("version check unavailable") from error
    finally:
        # The version invocation is our own child; clean it up on timeout or oversized output.
        if process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except PermissionError:
                # Some host sandboxes permit signalling the child but not a group.
                process.kill()
        process.wait()
        if process.stdout is not None:
            process.stdout.close()


def unresolved_alias(recorded: str, current: str) -> bool:
    """Recognize noncanonical legacy evidence without trusting its past target."""
    try:
        path = Path(recorded)
        return path.is_absolute() and recorded != current and str(path.resolve(strict=True)) == current
    except (OSError, ValueError, RuntimeError):
        return False


def diagnose(project: Path) -> dict[str, Any]:
    installation, documents = inspect_installation(project)
    checks = list(installation["checks"])
    report: dict[str, Any] = {
        "schema_version": 1,
        "version": installation["version"],
        "skills_root": installation["skills_root"],
        "installation": {key: value for key, value in installation.items() if key != "checks"},
        "checks": checks,
        "runtimes": [],
    }
    adapter_document = documents.get("adapters", {})
    adapters = adapter_document.get("adapters", {})
    config = documents.get("config", {})
    profiles = config.get("profiles", {})
    roles = config.get("roles", {})
    receipts = documents.get("preflight", {}).get("receipts", [])
    profiles = profiles if isinstance(profiles, dict) else {}
    roles = roles if isinstance(roles, dict) else {}
    receipts = receipts if isinstance(receipts, list) else []
    used_profiles = {value.get("profile") for value in roles.values() if isinstance(value, dict) and isinstance(value.get("profile"), str)}
    used_adapters = {profiles[profile].get("adapter") for profile in used_profiles if isinstance(profiles.get(profile), dict) and isinstance(profiles[profile].get("adapter"), str)}
    if isinstance(adapters, dict):
        for adapter_id in sorted(used_adapters):
            adapter = adapters.get(adapter_id)
            if not isinstance(adapter, dict):
                continue
            kind = adapter.get("kind")
            runtime: dict[str, Any] = {"adapter": adapter_id, "kind": kind if kind in ("native-agent", "builtin-cli", "custom-cli") else "invalid", "adapter_sha256": adapter_fingerprint(adapter)}
            report["runtimes"].append(runtime)
            relevant = [
                receipt for receipt in receipts if isinstance(receipt, dict)
                and receipt.get("adapter") == adapter_id and receipt.get("result") == "pass"
                and isinstance(receipt.get("profile_ids"), list)
                and any(profile in used_profiles for profile in receipt["profile_ids"] if isinstance(profile, str))
            ]
            if kind in ("native-agent", "custom-cli") and any(
                isinstance(receipt.get("adapter_sha256"), str) and receipt["adapter_sha256"]
                and receipt["adapter_sha256"] != runtime["adapter_sha256"] for receipt in relevant
            ):
                checks.append(check_item("runtime.adapter_drift", "error", f"{adapter_id}: adapter differs from recorded preflight fingerprint.", "Review the adapter change and repeat affected profile preflight before use."))
            if adapter.get("kind") == "native-agent":
                runtime["status"] = "host-managed"
                checks.append(check_item("runtime.native", "info", f"{adapter_id}: native-agent runtime is supplied by the host.", "Verify native runtime identity through the host when repeating preflight."))
                continue
            if adapter.get("kind") == "custom-cli":
                runtime["status"] = "unverified"
                checks.append(check_item("runtime.custom", "warning", f"{adapter_id}: custom CLI runtime identity is unverified; no arbitrary version command was executed.", "Review the adapter and perform its documented safe identity check during affected profile preflight."))
                continue
            if adapter.get("kind") != "builtin-cli" or adapter.get("builtin") not in ("codex", "claude"):
                runtime["status"] = "invalid"
                continue
            executable = adapter.get("executable")
            if not isinstance(executable, str):
                runtime["status"] = "unavailable"
                continue
            try:
                resolved = resolve_executable(executable, project.resolve(strict=True))
                runtime["cli_executable"] = resolved
                runtime["cli_version"] = cli_version(resolved, project.resolve(strict=True))
            except (OSError, ValueError, RuntimeError):
                runtime["status"] = "unavailable"
                checks.append(check_item("runtime.unavailable", "error", f"{adapter_id}: executable or bounded --version check is unavailable.", "Check the configured executable and PATH, then repeat affected profile preflight. Raw command output is intentionally omitted."))
                continue
            runtime["status"] = "verified"
            missing: set[str] = set()
            mismatched: set[str] = set()
            alias_evidence = False
            for field in ("cli_executable", "cli_version", "adapter_sha256"):
                if not relevant or any(not isinstance(receipt.get(field), str) or not receipt[field] for receipt in relevant):
                    missing.add(field)
                for receipt in relevant:
                    recorded = receipt.get(field)
                    if isinstance(recorded, str) and recorded and recorded != runtime[field]:
                        if field == "cli_executable" and unresolved_alias(recorded, runtime[field]):
                            alias_evidence = True
                        else:
                            mismatched.add(field)
            if mismatched:
                runtime["status"] = "drifted"
                checks.append(check_item("runtime.drift", "error", f"{adapter_id}: runtime differs from recorded preflight evidence ({', '.join(sorted(mismatched))}).", "Review the changed runtime/adapter and repeat affected profile preflight before use; do not substitute models or authority."))
            if missing:
                if not mismatched:
                    runtime["status"] = "unverified"
                checks.append(check_item("runtime.evidence_missing", "warning", f"{adapter_id}: preflight identity evidence is missing ({', '.join(sorted(missing))}).", "Repeat affected profile preflight and record cli_executable, cli_version, and adapter_sha256. A version check alone does not qualify model, authority, or session behavior."))
            if alias_evidence:
                if not mismatched:
                    runtime["status"] = "unverified"
                checks.append(check_item("runtime.evidence_alias", "warning", f"{adapter_id}: recorded cli_executable is a noncanonical alias of the current executable; its historical target is unverified.", "Record the chosen executable's absolute realpath at the next affected profile preflight. Resolving today's alias cannot establish the executable used by the earlier preflight."))
            if not missing and not mismatched and not alias_evidence:
                checks.append(check_item("runtime.matches", "pass", f"{adapter_id}: executable, CLI version, and adapter match recorded preflight identity.", ""))
    report["status"] = "error" if any(item["severity"] == "error" for item in checks) else "unverified" if any(item["severity"] == "warning" for item in checks) else "healthy"
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", required=True, type=Path)
    parser.add_argument("--json", action="store_true", help="Print machine-readable diagnostics")
    args = parser.parse_args()
    report = diagnose(args.project)
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(f"TRIP workflow doctor: {report['status']} | installed version: {report['version'] or 'unknown'}")
        for check in report["checks"]:
            # Keep terminal controls from malformed configuration names out of human output.
            message = "".join(char if char.isprintable() else "?" for char in check["message"])
            print(f"[{check['severity'].upper()}] {message}")
            if check["action"]:
                print(f"  Next: {check['action']}")
        print("No repairs or provider/model preflight probes were performed.")
    return {"healthy": 0, "error": 1, "unverified": 2}[report["status"]]


if __name__ == "__main__":
    raise SystemExit(main())
