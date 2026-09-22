#!/usr/bin/env python3
"""Best-effort cmux workspace and role-console orchestration."""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import shutil
import subprocess
from pathlib import Path


MAX_MESSAGE = 900
MAX_SOURCE = 80
CMUX_LEVEL = {"debug": "info", "info": "info", "warn": "warning", "error": "error"}
CONTEXT_PATTERN = re.compile(r"^[a-z0-9_]+$")
WORKSPACE_PATTERN = re.compile(
    r"\b(workspace:[0-9]+|[0-9a-fA-F]{8}-[0-9a-fA-F-]{27,36})\b"
)
SURFACE_PATTERN = re.compile(r"\b(surface:[0-9]+|[0-9a-fA-F]{8}-[0-9a-fA-F-]{27,36})\b")


def compact(value: str, limit: int) -> str:
    normalized = " ".join(value.split())
    return normalized if len(normalized) <= limit else normalized[: limit - 1] + "…"


def result(active: bool, **values: object) -> int:
    print(json.dumps({"active": active, **values}, sort_keys=True))
    return 0


def cmux_command(
    binary: str, *arguments: str, workspace: str | None = None
) -> subprocess.CompletedProcess[str]:
    environment = os.environ.copy()
    if workspace:
        environment["CMUX_WORKSPACE_ID"] = workspace
    return subprocess.run(
        [binary, *arguments], check=False, capture_output=True, text=True,
        timeout=5, env=environment,
    )


def available(mode: str) -> tuple[str | None, str | None]:
    selected = os.environ.get("TRIP_CMUX", mode).lower()
    if selected == "off":
        return None, "disabled"
    binary = shutil.which("cmux")
    if binary is None:
        return None, "cmux-not-installed"
    try:
        ping = cmux_command(binary, "ping")
    except (OSError, subprocess.TimeoutExpired):
        return None, "cmux-unreachable"
    if ping.returncode != 0:
        return None, "cmux-unreachable"
    return binary, None


def workspace_arguments(workspace: str | None) -> list[str]:
    return ["--workspace", workspace] if workspace else []


def parse_workspace(output: str) -> str | None:
    match = WORKSPACE_PATTERN.search(output)
    return match.group(1) if match else None


def invocation_workspace(binary: str) -> tuple[str | None, str | None]:
    workspace = os.environ.get("CMUX_WORKSPACE_ID")
    if workspace:
        return workspace, "cmux-environment"
    if os.environ.get("CMUX_CODEX_HOOKS_DISABLED") != "1":
        return None, None
    try:
        command = cmux_command(binary, "current-workspace")
    except (OSError, subprocess.TimeoutExpired):
        return None, None
    workspace = parse_workspace(command.stdout)
    if command.returncode != 0 or not workspace:
        return None, None
    return workspace, "hook-disabled-current-workspace"


def open_role_pane(binary: str, workspace: str) -> tuple[str | None, str | None]:
    try:
        created = cmux_command(
            binary,
            "new-pane",
            "--type",
            "terminal",
            "--direction",
            "right",
            "--workspace",
            workspace,
            "--focus",
            "false",
            workspace=workspace,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None, "role-pane-create-failed"
    match = SURFACE_PATTERN.search(created.stdout)
    if created.returncode != 0 or match is None:
        return None, "role-pane-create-failed"
    surface = match.group(1)
    return surface, None


def launch(args: argparse.Namespace, binary: str) -> int:
    runner = Path(__file__).with_name("cmux_role_runner.py").resolve()
    if not runner.is_file():
        return result(False, reason="role-runner-missing", workspace=args.workspace)
    surface = args.surface
    if surface is None:
        surface, reason = open_role_pane(binary, args.workspace)
        if surface is None:
            return result(False, reason=reason, workspace=args.workspace)
    command_line = shlex.join(
        [
            os.environ.get("PYTHON", "python3"),
            str(runner),
            "--role",
            args.role,
            "--project",
            str(args.project.resolve()),
            "--prompt-file",
            str(args.prompt_file.resolve()),
            "--result-file",
            str(args.result_file.resolve()),
            "--completion-file",
            str(args.completion_file.resolve()),
        ]
    )
    for command in (
        ("rename-tab", "--workspace", args.workspace, "--surface", surface, f"TRIP {args.role.replace('_', ' ').title()}"),
        ("send", "--workspace", args.workspace, "--surface", surface, command_line),
        ("send-key", "--workspace", args.workspace, "--surface", surface, "Enter"),
    ):
        try:
            completed = cmux_command(binary, *command, workspace=args.workspace)
        except (OSError, subprocess.TimeoutExpired):
            return result(False, reason="role-pane-launch-failed", workspace=args.workspace, surface=surface)
        if completed.returncode != 0:
            return result(False, reason="role-pane-launch-failed", workspace=args.workspace, surface=surface)
    return result(True, projection="ordinary-role-console", workspace=args.workspace, surface=surface)


def start(args: argparse.Namespace, binary: str) -> int:
    workspace = args.workspace
    operation_mode = "explicit-workspace" if workspace else None
    if not workspace:
        workspace, operation_mode = invocation_workspace(binary)
    created = False
    if not workspace:
        command = cmux_command(
            binary,
            "new-workspace",
            "--name",
            f"TRIP {args.context}",
            "--description",
            "Manager plus passive role consoles; workflow state remains repository-owned",
            "--cwd",
            str(args.project.resolve()),
            "--focus",
            "false",
        )
        workspace = parse_workspace(command.stdout)
        if command.returncode != 0 or not workspace:
            return result(False, reason="workspace-create-failed")
        created = True
        operation_mode = "dedicated-workspace"
    target = workspace_arguments(workspace)
    commands = [
        ("set-status", "trip-phase", "starting", *target, "--icon", "hammer"),
        ("set-progress", "0.0", "--label", "Starting workflow", *target),
        ("log", "--level", "info", "--source", "manager", *target, "workflow started"),
    ]
    for command in commands:
        try:
            completed = cmux_command(binary, *command, workspace=workspace)
        except (OSError, subprocess.TimeoutExpired):
            return result(False, reason="cmux-command-failed", workspace=workspace)
        if completed.returncode != 0:
            return result(False, reason="cmux-command-failed", workspace=workspace)
    return result(
        True,
        created=created,
        operation_mode=operation_mode,
        projection="role-console-panes",
        workspace=workspace,
    )


def emit(args: argparse.Namespace, binary: str) -> int:
    source = compact(args.source, MAX_SOURCE)
    message = compact(args.message, MAX_MESSAGE)
    target = workspace_arguments(args.workspace)
    commands: list[tuple[str, ...]] = [
        ("log", "--level", CMUX_LEVEL[args.level], "--source", source, *target, f"{args.event}: {message}"),
    ]
    if args.phase:
        commands.append(("set-status", "trip-phase", compact(args.phase, 80), *target, "--icon", "hammer"))
    if args.progress is not None:
        commands.append(("set-progress", str(args.progress), "--label", compact(args.phase or args.event, 80), *target))
    for command in commands:
        try:
            completed = cmux_command(binary, *command)
        except (OSError, subprocess.TimeoutExpired):
            return result(False, reason="cmux-command-failed")
        if completed.returncode != 0:
            return result(False, reason="cmux-command-failed")
    return result(True, workspace=args.workspace)


def finish(args: argparse.Namespace, binary: str) -> int:
    target = workspace_arguments(args.workspace)
    for command in (
        ("set-status", "trip-phase", args.outcome, *target, "--icon", "checkmark"),
        ("set-progress", "1.0", "--label", args.outcome, *target),
        ("log", "--level", "info", "--source", "manager", *target, f"workflow {args.outcome}"),
    ):
        try:
            completed = cmux_command(binary, *command)
        except (OSError, subprocess.TimeoutExpired):
            return result(False, reason="cmux-command-failed")
        if completed.returncode != 0:
            return result(False, reason="cmux-command-failed")
    return result(True, workspace=args.workspace)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=("auto", "on", "off"), default="auto")
    subparsers = parser.add_subparsers(dest="action", required=True)

    start_parser = subparsers.add_parser("start")
    start_parser.add_argument("--context", required=True)
    start_parser.add_argument("--project", required=True, type=Path)
    start_parser.add_argument("--ledger", type=Path)
    start_parser.add_argument("--workspace")

    emit_parser = subparsers.add_parser("emit")
    emit_parser.add_argument("--workspace")
    emit_parser.add_argument("--source", required=True)
    emit_parser.add_argument("--event", required=True)
    emit_parser.add_argument("--message", required=True)
    emit_parser.add_argument("--phase")
    emit_parser.add_argument("--progress", type=float)
    emit_parser.add_argument("--level", choices=("debug", "info", "warn", "error"), default="info")

    finish_parser = subparsers.add_parser("finish")
    finish_parser.add_argument("--workspace")
    finish_parser.add_argument("--outcome", choices=("complete", "incomplete", "failed"), required=True)

    launch_parser = subparsers.add_parser("launch")
    launch_parser.add_argument("--workspace", required=True)
    launch_parser.add_argument("--surface")
    launch_parser.add_argument("--role", choices=("explorer", "plan_reviewer", "implementer", "code_reviewer", "final_verifier"), required=True)
    launch_parser.add_argument("--project", required=True, type=Path)
    launch_parser.add_argument("--prompt-file", required=True, type=Path)
    launch_parser.add_argument("--result-file", required=True, type=Path)
    launch_parser.add_argument("--completion-file", required=True, type=Path)

    args = parser.parse_args()
    if getattr(args, "context", None) and not CONTEXT_PATTERN.fullmatch(args.context):
        return result(False, reason="invalid-context")
    if getattr(args, "progress", None) is not None and not 0.0 <= args.progress <= 1.0:
        return result(False, reason="invalid-progress")
    binary, reason = available(args.mode)
    if binary is None:
        return result(False, reason=reason)
    if args.action == "start":
        return start(args, binary)
    if args.action == "launch":
        return launch(args, binary)
    if args.action == "emit":
        return emit(args, binary)
    return finish(args, binary)


if __name__ == "__main__":
    raise SystemExit(main())
