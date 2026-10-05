#!/usr/bin/env python3
"""Read privacy-minimal role status and usage evidence; never control providers."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import os
import re
import stat
import sys
import tempfile
import time
import uuid
from collections import defaultdict
from pathlib import Path

sys.dont_write_bytecode = True

STATUS_SUFFIX = ".status.json"
TOKEN_FIELDS = ("input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens")
MODEL_TOKEN_FIELDS = dict(zip(TOKEN_FIELDS, ("inputTokens", "outputTokens", "cacheReadInputTokens", "cacheCreationInputTokens")))
TOOLS = {"Read", "Grep", "Glob", "Bash", "Edit", "Write", "Agent", "Task", "WebFetch", "WebSearch", "NotebookEdit", "ToolSearch"}
ACTIVITIES = {"init", "assistant", "tool_start", "tool_done", "tool_failed", "tool_progress", "status", "result", "output"}


def nonnegative(value: object, *, integer: bool = False) -> bool:
    if type(value) is int:
        return 0 <= value <= 2**63 - 1
    return not integer and type(value) is float and math.isfinite(value) and 0 <= value <= 2**63 - 1


def numeric_fields(value: object, fields: tuple[str, ...]) -> dict[str, int]:
    return {key: value[key] for key in fields if nonnegative(value.get(key), integer=True)} if isinstance(value, dict) else {}


def claude_usage(event: dict) -> dict:
    """Keep only numeric result metrics, never provider metadata or model-map keys.

    Claude >=2.1.277 restores modelUsage and cost across resumed sessions.
    result.usage covers this invocation's main loop and is kept separately.
    https://code.claude.com/docs/en/agent-sdk/cost-tracking
    """
    usage = {"source": "claude-result", "scope": "session-cumulative", "cost_is_estimate": True}
    cost = event.get("total_cost_usd")
    if nonnegative(cost):
        usage["estimated_cost_usd"] = cost
    models = event.get("modelUsage")
    if isinstance(models, dict) and models:
        totals = {}
        for normalized, provider_field in MODEL_TOKEN_FIELDS.items():
            values = [item.get(provider_field) if isinstance(item, dict) else None for item in models.values()]
            if all(nonnegative(item, integer=True) for item in values) and nonnegative(sum(values), integer=True):
                totals[normalized] = sum(values)
        if totals:
            usage["tokens"] = totals
    main_loop = numeric_fields(event.get("usage"), TOKEN_FIELDS)
    if main_loop:
        usage["main_loop"] = {"scope": "invocation-main-loop", "tokens": main_loop}
    usage["available"] = any(key in usage for key in ("estimated_cost_usd", "tokens", "main_loop"))
    return usage


def unknown_usage() -> dict:
    return {"available": False, "source": "unavailable", "scope": "unknown"}


def atomic_json(path: Path, value: dict, *, exclusive: bool = False) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile("w", encoding="utf-8", dir=path.parent, delete=False) as stream:
            temporary = Path(stream.name)
            json.dump(value, stream, indent=2, sort_keys=True, allow_nan=False)
            stream.write("\n")
        if exclusive:
            # Link publishes the complete record only if the invocation path is still unused.
            os.link(temporary, path)
        else:
            temporary.replace(path)
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()


def status_path(completion: Path) -> Path:
    return completion.with_name(completion.name + STATUS_SUFFIX)


class RuntimeStatus:
    def __init__(self, completion: Path, identity: dict, *, structured_tools: bool):
        self.path = status_path(completion)
        self.started_clock = time.monotonic()
        self.value = {
            "schema_version": 1, "record_type": "trip-role-status", "invocation_id": str(uuid.uuid4()), **identity,
            "started_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "state": "started", "first_activity_at": None, "last_activity_at": None,
            "first_activity_seconds": None, "last_activity_seconds": None,
            "last_activity": None, "activity_count": 0, "tool_counts_known": structured_tools,
            "tool_counts": {}, "status_persistence": "ok",
        }
        # A failed initial write prevents launch; later failures never abandon a running child.
        atomic_json(self.path, self.value, exclusive=True)

    def publish(self) -> None:
        try:
            atomic_json(self.path, self.value)
        except OSError:
            if self.value["status_persistence"] != "failed":
                print("[TRIP status] Metadata update failed; await the completion receipt.", file=sys.stderr, flush=True)
            self.value["status_persistence"] = "failed"

    def activity(self, kind: str, tool: str | None = None) -> None:
        if kind not in ACTIVITIES:
            return
        timestamp = dt.datetime.now(dt.timezone.utc).isoformat()
        elapsed = max(0.0, time.monotonic() - self.started_clock)
        self.value.update(state="activity-observed", last_activity_at=timestamp,
                          last_activity_seconds=elapsed, last_activity=kind)
        self.value["activity_count"] += 1
        if self.value["first_activity_at"] is None:
            self.value.update(first_activity_at=timestamp, first_activity_seconds=elapsed)
        if kind in ("tool_start", "tool_done", "tool_failed"):
            name = tool if tool in TOOLS else "other"
            counts = self.value["tool_counts"].setdefault(name, {"started": 0, "completed": 0, "failed": 0})
            counts[{"tool_start": "started", "tool_done": "completed", "tool_failed": "failed"}[kind]] += 1
        self.publish()

    def finish(self, outcome: str) -> dict:
        self.value.update(state=outcome, ended_at=dt.datetime.now(dt.timezone.utc).isoformat(),
                          duration_seconds=max(0.0, time.monotonic() - self.started_clock))
        return {**{key: value for key, value in self.value.items() if key != "state"}, "record_type": "trip-role-completion"}


def read_json(path: Path) -> dict | None:
    try:
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > 1024 * 1024:
            return None
        value = json.loads(path.read_text(encoding="utf-8"))
        return value if isinstance(value, dict) else None
    except (OSError, ValueError):
        return None


def clean_label(value: object) -> str:
    return re.sub(r"[^A-Za-z0-9_.:+/ -]", "?", value[:200]) if isinstance(value, str) else "unknown"


def timestamp(value: object) -> str | None:
    if not isinstance(value, str) or len(value) > 64:
        return None
    try:
        parsed = dt.datetime.fromisoformat(value[:-1] + "+00:00" if value.endswith("Z") else value)
        if parsed.tzinfo is None or parsed.utcoffset() is None:
            return None
        return parsed.astimezone(dt.timezone.utc).isoformat(timespec="microseconds")
    except (ValueError, TypeError, OverflowError):
        return None


def clean_usage(value: object) -> dict:
    if not isinstance(value, dict) or value.get("source") != "claude-result" or value.get("scope") != "session-cumulative":
        return unknown_usage()
    result = {"source": "claude-result", "scope": "session-cumulative", "cost_is_estimate": True}
    if nonnegative(value.get("estimated_cost_usd")):
        result["estimated_cost_usd"] = value["estimated_cost_usd"]
    tokens = numeric_fields(value.get("tokens"), TOKEN_FIELDS)
    if tokens:
        result["tokens"] = tokens
    main = value.get("main_loop")
    if isinstance(main, dict) and main.get("scope") == "invocation-main-loop":
        tokens = numeric_fields(main.get("tokens"), TOKEN_FIELDS)
        if tokens:
            result["main_loop"] = {"scope": "invocation-main-loop", "tokens": tokens}
    result["available"] = any(key in result for key in ("estimated_cost_usd", "tokens", "main_loop"))
    return result


def read_runs(project: Path) -> list[dict]:
    project = project.resolve(strict=True)
    root = project / ".local" / "trip-explorer"
    runs = []
    if any(path.is_symlink() for path in (project / ".local", root)) or not root.exists():
        return runs
    statuses, receipts = {}, {}
    # Only bounded JSON objects carrying this runtime's markers become report rows.
    for path in sorted(root.rglob("*")):
        if any(parent.is_symlink() for parent in (path, *path.parents) if parent != project and project in parent.parents):
            continue
        value = read_json(path)
        if not value or type(value.get("schema_version")) is not int or value["schema_version"] != 1 or value.get("project") != str(project):
            continue
        try:
            uuid.UUID(value.get("invocation_id"))
        except (ValueError, TypeError, AttributeError):
            continue
        if path.name.endswith(STATUS_SUFFIX) and value.get("record_type") == "trip-role-status":
            statuses[path.with_name(path.name[:-len(STATUS_SUFFIX)])] = value
        elif value.get("record_type") == "trip-role-completion":
            receipts[path] = value
    for receipt_path in sorted(statuses.keys() | receipts.keys()):
        status = statuses.get(receipt_path)
        # Completion filenames need not end in .json; statuses give their exact sibling path.
        receipt = receipts.get(receipt_path) or (read_json(receipt_path) if status else None)
        identity = status or receipt
        invocation = identity["invocation_id"]
        valid = bool(receipt and receipt.get("record_type") == "trip-role-completion"
                     and type(receipt.get("schema_version")) is int and receipt["schema_version"] == 1
                     and receipt.get("invocation_id") == invocation and receipt.get("project") == str(project)
                     and all(receipt.get(key) == identity.get(key) for key in ("role", "profile", "provider", "adapter", "model"))
                     and receipt.get("status") in ("completed", "failed")
                     and type(receipt.get("exit_code")) is int
                     and (receipt["exit_code"] == 0) == (receipt["status"] == "completed"))
        source = receipt if valid else status
        if source is None:
            continue
        run = {key: clean_label(source.get(key)) for key in ("role", "profile", "provider", "adapter", "model")}
        run.update(invocation_id=invocation, completion_file=str(receipt_path), status_file=str(status_path(receipt_path)),
                   status=receipt["status"] if valid else "unknown", completion_authoritative=valid,
                   last_observed_state=clean_label(status.get("state")) if status else "status-file-unavailable",
                   guidance="Completion receipt verified." if valid else "No matching terminal receipt; inspect before deciding whether to resume. Never infer safe retry.")
        for key in ("started_at", "ended_at", "first_activity_at", "last_activity_at"):
            run[key] = timestamp(source.get(key))
        activity = source.get("last_activity")
        run["last_activity"] = activity if isinstance(activity, str) and activity in ACTIVITIES else None
        for key in ("duration_seconds", "first_activity_seconds", "last_activity_seconds", "activity_count"):
            run[key] = source.get(key) if nonnegative(source.get(key)) else None
        if not valid:
            run["duration_seconds"] = None
        counts = source.get("tool_counts", {})
        run["tool_counts_known"] = source.get("tool_counts_known") is True
        run["tool_counts"] = {name: numeric_fields(value, ("started", "completed", "failed")) for name, value in counts.items() if name in TOOLS | {"other"}} if isinstance(counts, dict) else {}
        run["usage"] = clean_usage(receipt.get("usage")) if valid else unknown_usage()
        run["native_session_id"] = None
        if valid:
            try:
                run["native_session_id"] = str(uuid.UUID(receipt.get("native_session_id")))
            except (ValueError, TypeError, AttributeError):
                pass
        runs.append(run)
    return runs


def aggregate(runs: list[dict]) -> dict:
    groups: dict[tuple, dict] = {}
    seen = set()
    sessions = defaultdict(list)
    for run in runs:
        if run["invocation_id"] in seen:
            continue
        seen.add(run["invocation_id"])
        key = tuple(run[name] for name in ("role", "profile", "provider"))
        group = groups.setdefault(key, {"role": key[0], "profile": key[1], "provider": key[2], "invocations": 0,
            "completed": 0, "failed": 0, "unknown": 0, "duration_seconds": 0.0, "duration_known": 0,
            "activity_count": 0, "activity_known": 0, "usage_invocations_known": 0,
            "session_usage_snapshots": 0, "estimated_cost_usd": 0.0, "cost_sessions_known": 0,
            "tokens": {field: 0 for field in TOKEN_FIELDS}, "token_sessions_known": {field: 0 for field in TOKEN_FIELDS},
            "main_loop_tokens": {field: 0 for field in TOKEN_FIELDS}, "main_loop_invocations_known": {field: 0 for field in TOKEN_FIELDS}})
        group["invocations"] += 1
        group[run["status"]] += 1
        for source, total, coverage in (("duration_seconds", "duration_seconds", "duration_known"), ("activity_count", "activity_count", "activity_known")):
            if nonnegative(run.get(source)):
                group[total] += run[source]
                group[coverage] += 1
        usage = clean_usage(run.get("usage"))
        if usage["available"]:
            group["usage_invocations_known"] += 1
        for field, value in usage.get("main_loop", {}).get("tokens", {}).items():
            group["main_loop_tokens"][field] += value
            group["main_loop_invocations_known"][field] += 1
        if run.get("native_session_id"):
            sessions[(run["provider"], run["native_session_id"])].append((key, run, usage))
    uncertain_sessions = 0
    for values in sessions.values():
        identities = {key for key, _, _ in values}
        # A session reused across groups cannot be attributed to any one role without inventing deltas.
        if len(identities) != 1:
            uncertain_sessions += 1
            continue
        endings = [timestamp(item[1].get("ended_at")) for item in values]
        # Missing/invalid dates or a tie cannot establish a unique latest snapshot.
        if None in endings or len(set(endings)) != len(endings):
            uncertain_sessions += 1
            continue
        values = [value for _, value in sorted(zip(endings, values), key=lambda item: item[0])]
        key, latest, usage = values[-1]
        if not latest.get("completion_authoritative") or not latest.get("ended_at") or not usage["available"]:
            uncertain_sessions += 1
            continue
        # Regressing counters can mean a reset, changed CLI semantics, or crash; do not silently add/max them.
        metrics = [("estimated_cost_usd", None), *(("tokens", field) for field in TOKEN_FIELDS)]
        regressed = False
        for field, token in metrics:
            observed = [item[2].get(field, {}).get(token) if token else item[2].get(field) for item in values]
            observed = [value for value in observed if nonnegative(value)]
            regressed |= any(new < old for old, new in zip(observed, observed[1:]))
        if regressed:
            uncertain_sessions += 1
            continue
        group = groups[key]
        group["session_usage_snapshots"] += 1
        if nonnegative(usage.get("estimated_cost_usd")):
            group["estimated_cost_usd"] += usage["estimated_cost_usd"]
            group["cost_sessions_known"] += 1
        for field, value in usage.get("tokens", {}).items():
            group["tokens"][field] += value
            group["token_sessions_known"][field] += 1
    for group in groups.values():
        for total, coverage in (("estimated_cost_usd", "cost_sessions_known"), ("duration_seconds", "duration_known"), ("activity_count", "activity_known")):
            if group[coverage] == 0:
                group[total] = None
        for total, coverage in (("tokens", "token_sessions_known"), ("main_loop_tokens", "main_loop_invocations_known")):
            for field in TOKEN_FIELDS:
                if group[coverage][field] == 0:
                    group[total][field] = None
    return {"groups": list(groups.values()), "unattributable_or_uncertain_sessions": uncertain_sessions,
            "usage_basis": "Latest compatible session-cumulative snapshot; no per-invocation deltas. Cost is a provider estimate, not billing.",
            "duration_basis": "Sum of measured invocation durations, not workflow wall time; concurrent runs overlap.",
            "explorer_value": "Not inferred from runtime telemetry; record accepted findings and avoided work in the workflow ledger."}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--format", choices=("text", "json"), default="text")
    args = parser.parse_args()
    try:
        project = args.project.resolve(strict=True)
        if not project.is_dir():
            raise ValueError("project path is not a directory")
    except (OSError, ValueError) as error:
        print(f"error: cannot read project directory: {error}", file=sys.stderr)
        return 1
    runs = read_runs(project)
    report = {"project": str(project), "runs": runs, "summary": aggregate(runs)}
    if args.format == "json":
        print(json.dumps(report, indent=2, sort_keys=True, allow_nan=False))
    else:
        print(f"TRIP run report: {len(runs)} invocation(s)")
        for run in runs:
            print(f"{run['role']} | {run['profile']} | {run['status']} | {run['completion_file']}")
            print(f"  {run['guidance']} Last observed: {run['last_observed_state']}; activity count: {run['activity_count']}.")
        for group in report["summary"]["groups"]:
            duration = f"{group['duration_seconds']:.2f}s" if group["duration_seconds"] is not None else "unknown"
            cost = f"${group['estimated_cost_usd']:.6f}" if group["estimated_cost_usd"] is not None else "unknown"
            print(f"{group['role']} / {group['profile']} / {group['provider']}: {group['invocations']} runs, {group['failed']} failed, {group['unknown']} unknown; duration {duration} ({group['duration_known']} known); estimated cost {cost} ({group['cost_sessions_known']} sessions known)")
            tokens = ", ".join(f"{field}={value if value is not None else 'unknown'} ({group['token_sessions_known'][field]} sessions known)" for field, value in group["tokens"].items())
            print(f"  Session token totals: {tokens}")
            print(f"  Usage evidence: {group['usage_invocations_known']}/{group['invocations']} invocations; main-loop invocation token totals are separately available in JSON.")
        if report["summary"]["unattributable_or_uncertain_sessions"]:
            print(f"Excluded uncertain or cross-role sessions: {report['summary']['unattributable_or_uncertain_sessions']}")
        print(report["summary"]["usage_basis"])
        print(report["summary"]["duration_basis"])
        print(report["summary"]["explorer_value"])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
