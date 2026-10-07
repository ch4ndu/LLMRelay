#!/usr/bin/env python3
"""Run one configured TRIP role with visible provider activity."""

from __future__ import annotations

import argparse
import codecs
import datetime as dt
import json
import os
import re
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path
from typing import Callable

sys.dont_write_bytecode = True
from claude_console import run_claude
from cmux_observer import WORKSPACE_ID_PATTERN
from role_config import ALLOWED_ARGUMENT_PLACEHOLDERS, ROLE_REQUIREMENTS, validate_role_configuration
from run_report import RuntimeStatus, status_path, unknown_usage


DIRECT_ROLE_PREAMBLE = """You are already the TRIP Explorer workflow's {role} role.
Perform this role directly. Do not invoke any TRIP-* or codex-* orchestration
skill or script, launch another agent process, or delegate the role.
Honor the assigned {authority} authority and {session} session contract.
"""


def resolve_executable(name: str) -> str:
    candidate_name = Path(name)
    candidates = [candidate_name] if candidate_name.is_absolute() else [Path(item) / name for item in os.environ.get("PATH", "").split(os.pathsep) if item]
    for candidate in candidates:
        if not candidate.is_file() or not os.access(candidate, os.X_OK):
            continue
        resolved = candidate.resolve()
        if "/cmux-cli-shims/" in candidate.as_posix():
            continue
        if resolved.name == "cmux-codex-wrapper" or "/cmux.app/" in resolved.as_posix().lower():
            continue
        return str(candidate)
    raise RuntimeError(f"real executable not found outside cmux shims/wrappers: {name}")


def runtime_path(path: Path, root: Path, *, must_exist: bool) -> Path:
    candidate = path.resolve(strict=must_exist)
    if candidate == root or root not in candidate.parents:
        raise RuntimeError(f"runtime path escapes {root}: {candidate}")
    return candidate


def write_json(path: Path, payload: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile("w", encoding="utf-8", dir=path.parent, delete=False) as stream:
        json.dump(payload, stream, indent=2, sort_keys=True)
        stream.write("\n")
        temporary = Path(stream.name)
    temporary.replace(path)


def load_role(project: Path, role: str) -> tuple[dict[str, object], dict[str, object]]:
    state = project / ".agents" / "trip-explorer"
    config = json.loads((state / "config.json").read_text(encoding="utf-8"))
    adapters = json.loads((state / "adapters.json").read_text(encoding="utf-8"))
    preflight = json.loads((state / "preflight.json").read_text(encoding="utf-8"))
    problems = validate_role_configuration(config, adapters, preflight)
    if problems:
        raise RuntimeError("invalid installed role configuration: " + "; ".join(problems))
    profile_id = config["roles"][role]["profile"]
    profile = dict(config["profiles"][profile_id])
    profile["id"] = profile_id
    adapter_id = profile["adapter"]
    adapter = dict(adapters["adapters"][adapter_id])
    adapter["id"] = adapter_id
    return profile, adapter


def codex_command(executable: str, project: Path, prompt: str, result: Path, profile: dict[str, object], resume: str | None = None) -> list[str]:
    command = [executable, "exec"]
    if resume:
        command.extend(("resume", "-c", "sandbox_mode=" + json.dumps(profile["authority"])))
    else:
        command.extend(("--sandbox", str(profile["authority"]), "--color", "always", "--cd", str(project)))
    command.extend(("--skip-git-repo-check", "--model", str(profile["model"]), "-c", 'approval_policy="never"'))
    if profile.get("effort"):
        command.extend(("-c", "model_reasoning_effort=" + json.dumps(profile["effort"])))
    if profile.get("service_tier"):
        command.extend(("-c", "service_tier=" + json.dumps(profile["service_tier"])))
    if profile["session"] == "fresh":
        command.append("--ephemeral")
    command.extend(("--output-last-message", str(result)))
    if resume:
        command.append(resume)
    command.append(prompt)
    return command


def claude_command(executable: str, prompt: str, profile: dict[str, object], session_id: str, resume: bool = False) -> list[str]:
    allowed = ["Read", "Grep", "Glob", "Bash"]
    command = [executable, "--disable-slash-commands", "--model", str(profile["model"])]
    if profile.get("effort"):
        command.extend(("--effort", str(profile["effort"])))
    if profile["authority"] == "read-only":
        command.extend(("--safe-mode", "--restricted", "--strict-mcp-config", "--tools", "Read,Grep,Glob", "--permission-mode", "dontAsk", "--permission-prompts", "none", "--allowedTools", "Read,Grep,Glob"))
    else:
        command.extend(("--permission-mode", "dontAsk", "--allowedTools", *allowed, "Edit", "Write"))
    command.extend(("--resume" if resume else "--session-id", session_id))
    if profile["session"] == "fresh":
        command.append("--no-session-persistence")
    command.extend(("--output-format", "stream-json", "--verbose", "-p", prompt))
    return command


def run_visible(command: list[str], project: Path, environment: dict[str, str], *, codex: bool,
                activity: Callable[[str, str | None], None] | None = None) -> tuple[int, str, str | None]:
    # Codex puts its public console/header on stderr and writes the final result separately.
    with subprocess.Popen(command, cwd=project, env=environment,
                          stdout=None if codex else subprocess.PIPE,
                          stderr=subprocess.PIPE if codex else None) as process:
        source = process.stderr if codex else process.stdout
        destination = sys.stderr if codex else sys.stdout
        assert source is not None
        decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        output: list[str] = []
        header = ""
        session_id = None
        while True:
            chunk = source.read1(4096)
            text = decoder.decode(chunk, final=not chunk)
            if text and activity:
                activity("output", None)
            destination.write(text)
            destination.flush()
            if codex:
                if len(header) < 16384 and session_id is None:
                    header += text[:16384 - len(header)]
                    plain = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", header)
                    match = re.search(r"(?m)^session id:\s*([0-9a-fA-F-]{36})\s*$", plain)
                    if match:
                        session_id = str(uuid.UUID(match.group(1)))
            else:
                output.append(text)
            if not chunk:
                break
        return process.wait(), "".join(output), session_id


def custom_command(executable: str, adapter: dict[str, object], profile: dict[str, object], project: Path, prompt_file: Path, result_file: Path, completion_file: Path) -> list[str]:
    invocation = adapter.get("invocation")
    arguments = invocation.get("arguments") if isinstance(invocation, dict) else None
    if not isinstance(arguments, list) or not all(isinstance(item, str) for item in arguments):
        raise RuntimeError("custom adapter has invalid structured arguments")
    values = {
        "{authority}": str(profile["authority"]), "{completion_file}": str(completion_file),
        "{effort}": str(profile.get("effort", "")), "{model}": str(profile["model"]),
        "{project}": str(project), "{prompt_file}": str(prompt_file), "{result_file}": str(result_file),
        "{service_tier}": str(profile.get("service_tier", "")), "{session_id}": str(profile.get("session_id", "")),
    }
    rendered: list[str] = []
    for argument in arguments:
        value = argument
        for placeholder in ALLOWED_ARGUMENT_PLACEHOLDERS:
            value = value.replace(placeholder, values[placeholder])
        rendered.append(value)
    return [executable, *rendered]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--role", choices=tuple(ROLE_REQUIREMENTS), required=True)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--prompt-file", type=Path, required=True)
    parser.add_argument("--result-file", type=Path, required=True)
    parser.add_argument("--completion-file", type=Path, required=True)
    parser.add_argument("--resume-session", help="Exact retained session UUID; never a latest-session selector")
    parser.add_argument("--notify-workspace", help="Exact cmux workspace ref or UUID for best-effort completion notification")
    args = parser.parse_args()

    if args.notify_workspace and not WORKSPACE_ID_PATTERN.fullmatch(args.notify_workspace):
        raise RuntimeError("--notify-workspace must be an exact cmux workspace ref or UUID")

    project = args.project.resolve(strict=True)
    git_root = subprocess.run(["git", "-C", str(project), "rev-parse", "--show-toplevel"], check=False, capture_output=True, text=True)
    if git_root.returncode != 0 or Path(git_root.stdout.strip()).resolve() != project:
        raise RuntimeError("--project must be the Git repository root")
    runtime_root = (project / ".local" / "trip-explorer").resolve()
    prompt_file = runtime_path(args.prompt_file, runtime_root, must_exist=True)
    result_file = runtime_path(args.result_file, runtime_root, must_exist=False)
    completion_file = runtime_path(args.completion_file, runtime_root, must_exist=False)
    activity_file = runtime_path(status_path(completion_file), runtime_root, must_exist=False)
    direct_prompt_file = runtime_path(prompt_file.with_name(prompt_file.name + ".direct"), runtime_root, must_exist=False)
    if len({prompt_file, direct_prompt_file, result_file, completion_file, activity_file}) != 5:
        raise RuntimeError("prompt, direct prompt, result, completion and status paths must be distinct")
    if result_file.exists() or completion_file.exists() or activity_file.exists():
        raise RuntimeError("result, completion and status paths must be unused for each invocation")
    result_file.parent.mkdir(parents=True, exist_ok=True)

    profile, adapter = load_role(project, args.role)
    resume = str(uuid.UUID(args.resume_session)) if args.resume_session else None
    if resume and profile["session"] != "retained":
        raise RuntimeError("fresh roles cannot resume a session")
    if adapter["kind"] == "native-agent":
        raise RuntimeError("native-agent profiles must be invoked by the active host manager, not the CLI runner")
    executable = resolve_executable(str(adapter["executable"]))
    prompt = DIRECT_ROLE_PREAMBLE.format(role=args.role, authority=profile["authority"], session=profile["session"]).rstrip()
    prompt += "\n\n" + prompt_file.read_text(encoding="utf-8")
    direct_prompt_file.write_text(prompt, encoding="utf-8")
    builtin = adapter.get("builtin") if adapter["kind"] == "builtin-cli" else None
    native_session_id = resume
    if adapter["kind"] == "builtin-cli" and builtin == "codex":
        command = codex_command(executable, project, prompt, result_file, profile, resume)
    elif adapter["kind"] == "builtin-cli" and builtin == "claude":
        native_session_id = resume or str(uuid.uuid4())
        command = claude_command(executable, prompt, profile, native_session_id, bool(resume))
    elif adapter["kind"] == "custom-cli":
        if resume:
            arguments = adapter.get("invocation", {}).get("arguments", [])
            if not any("{session_id}" in argument for argument in arguments):
                raise RuntimeError("custom adapter cannot resume without a {session_id} argument")
            profile["session_id"] = resume
        command = custom_command(
            executable,
            adapter,
            profile,
            project,
            direct_prompt_file,
            result_file,
            completion_file,
        )
    else:
        raise RuntimeError(f"unsupported CLI adapter: {adapter['id']}")

    requested_session_id = native_session_id
    session_id_source = "cli-argument" if builtin is None and native_session_id else None
    if builtin:
        native_session_id = None
    tracker = RuntimeStatus(completion_file, {
        "project": str(project), "role": args.role, "profile": profile["id"],
        "provider": profile["provider"], "adapter": adapter["id"], "model": profile["model"],
    }, structured_tools=builtin == "claude")
    started = tracker.value["started_at"]
    print(f"TRIP role: {args.role} | {profile['provider']} | {profile['model']} / {profile.get('effort', 'default')} | {profile['authority']}", flush=True)
    print("Claude public activity events are displayed below." if builtin == "claude" else "The stream below is the selected provider's ordinary console output.", flush=True)
    environment = os.environ.copy()
    environment.pop("NO_COLOR", None)
    environment["CMUX_CODEX_HOOKS_DISABLED"] = "1"
    provider_exit_code = None
    error = None
    usage = unknown_usage()
    try:
        if builtin == "claude":
            provider_exit_code, console = run_claude(command, project, environment, requested_session_id, tracker.activity)
            usage = console.usage
            native_session_id = console.session_id
            session_id_source = "stream-event" if native_session_id else None
            if console.error:
                raise RuntimeError(console.error)
            result_file.write_text(console.result, encoding="utf-8")
        else:
            provider_exit_code, output, observed_session_id = run_visible(command, project, environment, codex=builtin == "codex", activity=tracker.activity)
        if builtin == "codex":
            native_session_id = observed_session_id
            session_id_source = "console-header" if native_session_id else None
            if resume and observed_session_id != resume:
                raise RuntimeError("Codex console session ID does not match the requested resume session")
            if profile["session"] == "retained" and not observed_session_id:
                raise RuntimeError("Codex console did not expose a retained session ID; do not guess or retry")
        elif builtin != "claude" and not result_file.exists():
            result_file.write_text(output, encoding="utf-8")
        if provider_exit_code == 0 and (not result_file.is_file() or not result_file.read_text(encoding="utf-8").strip()):
            raise RuntimeError("provider exited successfully without a nonempty final result")
    except (OSError, RuntimeError, ValueError) as failure:
        error = str(failure)
        print(f"error: {error}", file=sys.stderr, flush=True)
    exit_code = provider_exit_code if provider_exit_code not in (None, 0) else (1 if error else 0)
    outcome = "completed" if exit_code == 0 else "failed"
    write_json(completion_file, {
        **tracker.finish(outcome),
        "adapter": adapter["id"], "authority": profile["authority"], "ended_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "exit_code": exit_code, "provider_exit_code": provider_exit_code, "error": error,
        "model": profile["model"], "provider": profile["provider"], "profile": profile["id"],
        "project": str(project),
        "reasoning_effort": profile.get("effort"), "result_file": str(result_file), "role": args.role,
        "session": profile["session"], "started_at": started,
        "native_session_id": native_session_id, "resumed": bool(resume),
        "requested_session_id": requested_session_id, "service_tier": profile.get("service_tier"),
        "status": outcome,
        "session_id_source": session_id_source,
        "console_mode": "claude-activity" if builtin == "claude" else "provider-text",
        "usage": usage,
    })
    tracker.publish()
    print(f"TRIP role finished with exit code {exit_code}.", flush=True)
    print(f"[TRIP completion] {args.role} | {outcome} | receipt: {completion_file}", flush=True)
    if args.notify_workspace:
        try:
            from cmux_observer import notify_completion
            notice = notify_completion(completion_file, args.notify_workspace)
            delivered = notice.get("active") is True
        except (OSError, RuntimeError, ValueError):
            delivered = False
        if not delivered:
            print("[TRIP notice] Completion notification unavailable; the saved receipt remains authoritative.", file=sys.stderr, flush=True)
    return exit_code


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
