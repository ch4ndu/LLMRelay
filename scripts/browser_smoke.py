#!/usr/bin/env python3
"""Drive an explicitly selected cmux test browser and retain smoke-test evidence."""

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import time


def cmux(*arguments):
    result = subprocess.run(
        ["cmux", *arguments], capture_output=True, text=True, timeout=60
    )
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    return result.stdout.strip()


class Browser:
    def __init__(self, artifacts, record="browser.json"):
        self.artifacts = artifacts
        saved = json.loads((artifacts / record).read_text())
        self.surface = saved["surface_ref"]
        self.workspace = saved["workspace_ref"]

    def call(self, *arguments):
        cmux("focus-panel", "--workspace", self.workspace, "--panel", self.surface)
        return cmux("browser", "--surface", self.surface, *arguments)

    def evaluate(self, expression):
        return json.loads(self.call("eval", f"JSON.stringify({expression})"))

    def wait(self, expression):
        self.call("wait", "--function", expression, "--timeout-ms", "15000")

    def selector(self, expression):
        return self.evaluate("(() => { let node = (" + expression + "); "
            "if (!node) throw Error('Expected UI element is missing'); "
            "const path = []; while (node && node.nodeType === 1) { "
            "path.unshift(node.tagName.toLowerCase() + ':nth-child(' + "
            "([...node.parentNode.children].indexOf(node) + 1) + ')'); "
            "node = node.parentElement; } return path.join(' > '); })()")

    def click(self, text, scope="document"):
        expression = f"[...({scope}).querySelectorAll('button')].find(e => e.textContent.trim() === {json.dumps(text)})"
        self.wait(f"!!({scope}) && !!({expression}) && !({expression}).disabled")
        self.call("click", self.selector(expression))

    def label(self, text):
        return self.selector(
            "[...document.querySelectorAll('label')].find(e => "
            f"e.textContent.trim().startsWith({json.dumps(text)}))?.querySelector('input,textarea,select')"
        )

    def fill(self, label, value):
        self.call("fill", self.label(label), value)

    def expect(self, expression):
        if self.evaluate(expression) is not True:
            raise AssertionError(expression)

    def capture(self, name):
        (self.artifacts / f"{name}.snapshot.txt").write_text(
            self.call("snapshot", "--interactive")
        )
        self.call("screenshot", "--out", str(self.artifacts / f"{name}.png"))


def login(args):
    record = args.artifacts / "browser.json"
    if record.exists():
        raise RuntimeError("Browser already recorded; inspect it instead of consuming another login")
    log = args.service_log.read_text()
    match = re.search(r"Manual dashboard login: (http://[^\s]+)", log)
    if not match:
        raise RuntimeError("No one-use dashboard login was found in the selected service log")
    data = json.loads(cmux(
        "--json", "browser", "open", match.group(1), "--workspace", args.workspace,
        "--profile", args.profile, "--focus", "false",
    ))
    record.write_text(json.dumps(data, indent=2))
    print(json.dumps({key: data.get(key) for key in ("surface_ref", "workspace_ref")}))
    browser = Browser(args.artifacts)
    browser.wait("!!document.querySelector('.app-shell')")
    browser.call("viewport", "1440", "1000")
    browser.capture("initial")


def relogin(args):
    match = re.search(r"Manual dashboard login: (http://[^\s]+)", args.service_log.read_text())
    if not match:
        raise RuntimeError("No one-use dashboard login was found in the selected service log")
    browser = Browser(args.artifacts)
    browser.call("navigate", match.group(1))
    browser.call("reload")
    browser.wait("!!document.querySelector('.app-shell') && !location.hash")
    print("Reauthenticated the recorded test browser")


def smoke(args):
    browser = Browser(args.artifacts)
    observer = Browser(args.artifacts, "observer.json") if (args.artifacts / "observer.json").exists() else None
    title = f"Browser smoke {time.time_ns()}"
    results = {"title": title, "project": str(args.project_path), "checks": []}

    def passed(name):
        results["checks"].append({"name": name, "status": "passed"})
        (args.artifacts / "smoke-results.json").write_text(json.dumps(results, indent=2))
        print(f"PASS {name}", flush=True)

    def card():
        return "[...document.querySelectorAll('.task-card')].find(e => e.querySelector('strong')?.textContent === " + json.dumps(title) + ")"

    try:
        browser.wait("!!document.querySelector('.app-shell')")
        if browser.evaluate("!!document.querySelector('#task-form-title')"):
            browser.call("click", "button[aria-label='Close task form']")
        project = "[...document.querySelectorAll('button.project')].find(e => e.querySelector('span')?.textContent === " + json.dumps(args.project_name) + ")"
        if not browser.evaluate(f"!!({project})"):
            browser.click("Add project")
            browser.fill("Project name", args.project_name)
            browser.fill("Repository folder", str(args.project_path.resolve()))
            browser.click("Add project", "document.querySelector('#add-project-form')")
            browser.wait(f"!!({project})")
        browser.call("click", browser.selector(project))
        browser.expect(f"({project}).textContent.includes('queue paused')")
        if observer:
            observer.call("reload")
            observer.wait(f"!!({project})")
            observer.call("click", observer.selector(project))
            observer.call("click", "nav[aria-label='Main navigation'] button:nth-child(2)")
        passed("project registered with pickup paused")

        for index, heading in enumerate([
            "Roles and session access", "Task board", "Recipes", "Project control center",
            "History and archive", "Diagnostics and setup",
        ], 1):
            browser.call("click", f"nav[aria-label='Main navigation'] button:nth-child({index})")
            browser.wait(f"[...document.querySelectorAll('h1')].some(e => e.textContent === {json.dumps(heading)})")
            passed(f"navigation: {heading}")

        browser.call("click", "nav[aria-label='Main navigation'] button:nth-child(2)")
        browser.click("＋ New task")
        browser.wait("!!document.querySelector('#task-form-title')")
        browser.fill("Title", "")
        browser.click("Save draft")
        browser.wait("document.querySelector('[role=alert]')?.textContent.includes('Enter a task title.')")
        browser.fill("Title", title)
        browser.fill("Description", "Created through the real cmux browser and local HTTP service.")
        browser.fill("Acceptance criteria", "Draft persists after reload\nArchive and restore preserve the draft")
        browser.call("click", "button[aria-label='Close task form']")
        browser.click("＋ New task")
        browser.expect(f"document.querySelector({json.dumps(browser.label('Title'))}).value === {json.dumps(title)}")
        browser.call("reload")
        browser.wait("!!document.querySelector('.app-shell')")
        browser.click("＋ New task")
        browser.expect(f"document.querySelector({json.dumps(browser.label('Title'))}).value === {json.dumps(title)}")
        passed("new task validation and unsaved draft persistence")
        browser.click("Save draft")
        browser.wait("!document.querySelector('#task-form-title')")
        browser.wait(f"!!({card()})")
        passed("create backlog draft through HTTP")
        if observer:
            observer.wait(f"!!({card()})")
            passed("second browser receives created task without reload")
        browser.click("Edit", card())
        title += " edited"
        browser.fill("Title", title)
        browser.click("Save changes")
        browser.wait("!document.querySelector('#task-form-title')")
        browser.wait(f"!!({card()})")
        browser.call("reload")
        browser.wait(f"!!({card()})")
        passed("edit persists after browser reload")
        if observer:
            observer.wait(f"!!({card()})")
            passed("second browser receives edited task without reload")
        browser.call("fill", "input[aria-label='Filter tasks']", "no-match-" + title)
        browser.wait("document.querySelectorAll('.task-card').length === 0")
        browser.call("fill", "input[aria-label='Filter tasks']", title)
        browser.wait("document.querySelectorAll('.task-card').length === 1")
        browser.call("fill", "input[aria-label='Filter tasks']", "")
        passed("board filtering")
        browser.call("click", browser.selector(f"({card()}).querySelector('.card-body')"))
        browser.wait("!!document.querySelector('.detail')")
        browser.click("Archive", "document.querySelector('.detail')")
        browser.wait(f"!({card()})")
        if observer:
            observer.wait(f"!({card()})")
        browser.call("click", "button[aria-label='Close task details']")
        browser.call("click", "nav[aria-label='Main navigation'] button:nth-child(5)")
        history_row = "[...document.querySelectorAll('.history-list article')].find(e => e.textContent.includes(" + json.dumps(title) + "))"
        browser.wait(f"!!({history_row})")
        browser.click("Restore", history_row)
        browser.wait(f"!({history_row})")
        browser.call("click", "nav[aria-label='Main navigation'] button:nth-child(2)")
        browser.wait(f"!!({card()})")
        passed("archive and restore through History")
        if observer:
            observer.wait(f"!!({card()})")
            passed("second browser receives archive and restore without reload")
        browser.expect(f"({project}).textContent.includes('queue paused')")
        browser.call("viewport", "1440", "1000")
        browser.expect("[...document.querySelectorAll('.card-body strong')].every(e => e.scrollWidth <= e.clientWidth)")
        passed("long task titles fit inside board cards")
        browser.capture("smoke-desktop")
        browser.call("viewport", "900", "800")
        browser.capture("smoke-narrow")
        browser.call("viewport", "1440", "1000")
        errors = browser.call("errors", "list")
        (args.artifacts / "browser-errors.txt").write_text(errors)
        (args.artifacts / "browser-console.txt").write_text(browser.call("console", "list"))
        if errors != "No browser errors":
            raise AssertionError(f"Browser errors reported: {errors}")
        passed("browser error check and layout evidence captured")
        results["status"] = "passed"
    except Exception as error:
        results["status"] = "failed"
        results["failure"] = str(error)
        try:
            browser.capture("smoke-failure")
        except Exception as capture_error:
            results["capture_error"] = str(capture_error)
        raise
    finally:
        results["title"] = title
        (args.artifacts / "smoke-results.json").write_text(json.dumps(results, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, required=True)
    modes = parser.add_subparsers(dest="mode", required=True)
    opener = modes.add_parser("login", help="Open the selected isolated service's one-use link")
    opener.add_argument("--service-log", type=Path, required=True)
    opener.add_argument("--workspace", required=True)
    opener.add_argument("--profile", required=True)
    reopener = modes.add_parser("relogin", help="Reauthenticate the recorded browser after a service restart")
    reopener.add_argument("--service-log", type=Path, required=True)
    observer = modes.add_parser("observer", help="Open a second browser in the same isolated profile for live update assertions")
    observer.add_argument("--workspace", required=True)
    observer.add_argument("--profile", required=True)
    browser = modes.add_parser("browser", help="Use the recorded test surface for a cmux browser command")
    browser.add_argument("arguments", nargs=argparse.REMAINDER)
    suite = modes.add_parser("run", help="Exercise LLMRelay draft, navigation, and persistence flows")
    suite.add_argument("--project-path", type=Path, required=True)
    suite.add_argument("--project-name", required=True)
    args = parser.parse_args()
    args.artifacts = args.artifacts.resolve()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    try:
        if args.mode == "login":
            login(args)
        elif args.mode == "relogin":
            relogin(args)
        elif args.mode == "observer":
            record = args.artifacts / "observer.json"
            if record.exists():
                raise RuntimeError("Observer already recorded")
            origin = Browser(args.artifacts).evaluate("location.origin")
            data = json.loads(cmux("--json", "browser", "open", origin,
                "--workspace", args.workspace, "--profile", args.profile, "--focus", "false"))
            record.write_text(json.dumps(data, indent=2))
            print("Recorded observer", data["surface_ref"])
        elif args.mode == "run":
            smoke(args)
        else:
            if not args.arguments:
                parser.error("A browser command is required")
            if args.arguments[0] == "click-text" and len(args.arguments) in (2, 3):
                Browser(args.artifacts).click(args.arguments[1], args.arguments[2] if len(args.arguments) == 3 else "document")
                print("OK")
            else:
                print(Browser(args.artifacts).call(*args.arguments))
    except (AssertionError, RuntimeError, subprocess.TimeoutExpired) as error:
        message = re.sub(r"bootstrap=[^\s\"']+", "bootstrap=[REDACTED]", str(error))
        print(message, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
