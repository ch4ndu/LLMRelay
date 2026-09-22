#!/usr/bin/env python3
"""Run one configured TRIP role with its provider's ordinary console output."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

from role_config import ALLOWED_ARGUMENT_PLACEHOLDERS, ROLE_REQUIREMENTS, validate_role_configuration


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


def codex_command(executable: str, project: Path, prompt: str, result: Path, profile: dict[str, object]) -> list[str]:
    command = [executable, "exec", "--skip-git-repo-check", "--sandbox", str(profile["authority"]), "--color", "always", "--model", str(profile["model"])]
    if profile.get("effort"):
        command.extend(("-c", f'model_reasoning_effort="{profile["effort"]}"'))
    if profile.get("service_tier"):
        command.extend(("-c", f'service_tier="{profile["service_tier"]}"'))
    command.extend(("--output-last-message", str(result), "--cd", str(project), prompt))
    return command


def claude_command(executable: str, prompt: str, profile: dict[str, object]) -> list[str]:
    allowed = ["Read", "Grep", "Glob", "Bash"]
    command = [executable, "--disable-slash-commands", "--model", str(profile["model"])]
    if profile.get("effort"):
        command.extend(("--effort", str(profile["effort"])))
    if profile["authority"] == "read-only":
        command.extend(("--safe-mode", "--permission-mode", "dontAsk", "--allowedTools", *allowed, "--disallowedTools", "Edit", "Write", "NotebookEdit"))
    else:
        command.extend(("--permission-mode", "dontAsk", "--allowedTools", *allowed, "Edit", "Write"))
    command.extend(("--output-format", "text", "--no-session-persistence", "-p", prompt))
    return command


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
    args = parser.parse_args()

    project = args.project.resolve(strict=True)
    git_root = subprocess.run(["git", "-C", str(project), "rev-parse", "--show-toplevel"], check=False, capture_output=True, text=True)
    if git_root.returncode != 0 or Path(git_root.stdout.strip()).resolve() != project:
        raise RuntimeError("--project must be the Git repository root")
    runtime_root = (project / ".local" / "trip-explorer").resolve()
    prompt_file = runtime_path(args.prompt_file, runtime_root, must_exist=True)
    result_file = runtime_path(args.result_file, runtime_root, must_exist=False)
    completion_file = runtime_path(args.completion_file, runtime_root, must_exist=False)
    result_file.parent.mkdir(parents=True, exist_ok=True)

    profile, adapter = load_role(project, args.role)
    if adapter["kind"] == "native-agent":
        raise RuntimeError("native-agent profiles must be invoked by the active host manager, not the CLI runner")
    executable = resolve_executable(str(adapter["executable"]))
    prompt = DIRECT_ROLE_PREAMBLE.format(role=args.role, authority=profile["authority"], session=profile["session"]).rstrip()
    prompt += "\n\n" + prompt_file.read_text(encoding="utf-8")
    direct_prompt_file = prompt_file.with_name(prompt_file.name + ".direct")
    direct_prompt_file.write_text(prompt, encoding="utf-8")
    builtin = adapter.get("builtin")
    if adapter["kind"] == "builtin-cli" and builtin == "codex":
        command = codex_command(executable, project, prompt, result_file, profile)
        capture = False
    elif adapter["kind"] == "builtin-cli" and builtin == "claude":
        command = claude_command(executable, prompt, profile)
        capture = True
    elif adapter["kind"] == "custom-cli":
        command = custom_command(
            executable,
            adapter,
            profile,
            project,
            direct_prompt_file,
            result_file,
            completion_file,
        )
        capture = True
    else:
        raise RuntimeError(f"unsupported CLI adapter: {adapter['id']}")

    started = dt.datetime.now(dt.timezone.utc).isoformat()
    print(f"TRIP role: {args.role} | {profile['provider']} | {profile['model']} / {profile.get('effort', 'default')} | {profile['authority']}", flush=True)
    print("The stream below is the selected provider's ordinary console output.", flush=True)
    environment = os.environ.copy()
    environment.pop("NO_COLOR", None)
    environment["CMUX_CODEX_HOOKS_DISABLED"] = "1"
    if capture:
        completed = subprocess.run(command, cwd=project, env=environment, check=False, capture_output=True, text=True)
        if not result_file.exists():
            result_file.write_text(completed.stdout, encoding="utf-8")
        if completed.stdout:
            print(completed.stdout, end="" if completed.stdout.endswith("\n") else "\n", flush=True)
        if completed.stderr:
            print(completed.stderr, file=sys.stderr, end="" if completed.stderr.endswith("\n") else "\n", flush=True)
    else:
        completed = subprocess.run(command, cwd=project, env=environment, check=False)
    write_json(completion_file, {
        "adapter": adapter["id"], "authority": profile["authority"], "ended_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "exit_code": completed.returncode, "model": profile["model"], "provider": profile["provider"],
        "reasoning_effort": profile.get("effort"), "result_file": str(result_file), "role": args.role,
        "session": profile["session"], "started_at": started,
    })
    print(f"TRIP role finished with exit code {completed.returncode}.", flush=True)
    return completed.returncode


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
