"""Display public Claude CLI events while keeping the final result separate."""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path
from typing import Callable

from run_report import claude_usage, unknown_usage


MAX_EVENT_BYTES = 8 * 1024 * 1024


def terminal_text(value: str, limit: int | None = None) -> str:
    value = re.sub(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))", "", value)
    value = "".join(char for char in value if char in "\n\t" or (ord(char) >= 32 and not 127 <= ord(char) < 160))
    return value if limit is None or len(value) <= limit else value[:limit] + "\n[output truncated]"


class ClaudeConsole:
    def __init__(self, session_id: str, activity: Callable[[str, str | None], None] | None = None):
        self.expected_session_id = session_id
        self.session_id: str | None = None
        self.result: str | None = None
        self.error: str | None = None
        self.tools: dict[str, str] = {}
        self.last_text = ""
        self.result_seen = False
        self.activity = activity or (lambda kind, tool=None: None)
        self.usage = unknown_usage()

    def fail(self, message: str, *, preserve_usage: bool = False) -> None:
        if not preserve_usage:
            self.usage = unknown_usage()
        if self.error is None:
            self.error = message
            print(f"[Claude stream error] {message}", flush=True)

    def accept(self, line: bytes) -> None:
        try:
            event = json.loads(line)
            if not isinstance(event, dict):
                raise ValueError("event is not an object")
            self.event(event)
        except (ValueError, TypeError, KeyError, AttributeError):
            # Never echo malformed raw events: they may contain private reasoning.
            self.fail("Malformed Claude event; final result cannot be trusted.")

    def event(self, event: dict) -> None:
        kind = event.get("type")
        if kind == "system" and event.get("subtype") == "init":
            self.check_session(event)
            self.activity("init", None)
            model = terminal_text(str(event.get("model", "unreported")), 200)
            print(f"[Claude session] {self.session_id} | model: {model}", flush=True)
        elif kind == "assistant":
            for block in event.get("message", {}).get("content", []):
                if block.get("type") == "text":
                    self.last_text = block["text"]
                    self.activity("assistant", None)
                    print(terminal_text(self.last_text), flush=True)
                elif block.get("type") == "tool_use":
                    name, tool_id = block["name"], block["id"]
                    self.tools[tool_id] = name
                    self.activity("tool_start", name)
                    arguments = block.get("input", {})
                    details = []
                    for key in ("file_path", "path", "command", "pattern", "glob", "query"):
                        if isinstance(arguments.get(key), str):
                            details.append(f"{key}: {arguments[key]}")
                    suffix = " | " + " | ".join(details) if details else ""
                    print(terminal_text(f"[Tool start] {name}{suffix}", 4000), flush=True)
        elif kind == "user":
            content = event.get("message", {}).get("content", [])
            if not isinstance(content, list):
                return
            for block in content:
                if block.get("type") != "tool_result":
                    continue
                name = self.tools.pop(block.get("tool_use_id"), "tool")
                failed = block.get("is_error") is True
                self.activity("tool_failed" if failed else "tool_done", name)
                print(terminal_text(f"[Tool {'failed' if failed else 'done'}] {name}", 300), flush=True)
                if failed or name in ("Bash", "Grep", "Glob"):
                    output = block.get("content", "")
                    if isinstance(output, list):
                        output = "\n".join(item.get("text", "") for item in output if isinstance(item, dict) and item.get("type") == "text")
                    if isinstance(output, str) and output:
                        print(terminal_text(output, 2000), flush=True)
        elif kind == "result":
            if self.result_seen:
                self.fail("Duplicate Claude result event.")
                return
            self.result_seen = True
            self.check_session(event)
            self.activity("result", None)
            if self.error is None and event.get("subtype") != "error_during_execution":
                self.usage = claude_usage(event)
            if event.get("is_error") is not False or event.get("subtype") != "success":
                self.fail("Claude reported an unsuccessful result.", preserve_usage=True)
                return
            result = event.get("result")
            if not isinstance(result, str) or not result.strip():
                self.fail("Claude returned no nonempty final answer.")
                return
            self.result = result
            if result != self.last_text:
                print(terminal_text(result), flush=True)
        elif kind == "tool_progress":
            name = event.get("tool_name", self.tools.get(event.get("tool_use_id"), "tool"))
            elapsed = event.get("elapsed_time_seconds")
            if isinstance(elapsed, (int, float)):
                self.activity("tool_progress", None)
                print(terminal_text(f"[Tool running] {name} | {elapsed:g}s", 300), flush=True)
        elif kind == "system" and event.get("subtype") == "status" and event.get("status"):
            self.activity("status", None)
            print(terminal_text(f"[Claude status] {event['status']}", 300), flush=True)

    def check_session(self, event: dict) -> None:
        session_id = event.get("session_id")
        if session_id != self.expected_session_id:
            self.fail("Claude event session ID does not match the requested session.")
        else:
            self.session_id = session_id

    def finish(self) -> None:
        if not self.result_seen:
            self.fail("Claude exited without a final result event.")


def run_claude(command: list[str], project: Path, environment: dict[str, str], session_id: str,
               activity: Callable[[str, str | None], None] | None = None) -> tuple[int, ClaudeConsole]:
    console = ClaudeConsole(session_id, activity)
    with subprocess.Popen(command, cwd=project, env=environment, stdout=subprocess.PIPE) as process:
        assert process.stdout is not None
        discarding = False
        while True:
            line = process.stdout.readline(MAX_EVENT_BYTES + 1)
            if not line:
                break
            if discarding or len(line) > MAX_EVENT_BYTES:
                console.fail("Claude event exceeded the size limit.")
                discarding = not line.endswith(b"\n")
                continue
            if line.strip():
                console.accept(line)
        exit_code = process.wait()
    console.finish()
    return exit_code, console
