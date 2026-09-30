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


# Paths this suite cannot create through ordinary dashboard actions against a
# disposable candidate service without live provider processes. They are
# reported, never simulated by editing the service database.
UNTESTED_BOUNDARIES = [
    "pending permission request and its approval destination (needs a live agent asking for access)",
    "actionable and non-actionable recovery records (need an interrupted agent process)",
    "View output, Take control and Release control (need a running agent session)",
    "board and header Review plan, Review request, Answer question and recovery routes "
    "(need the matching live workflow states; covered by DOM flow tests)",
    "completed task without a resume offer after real acceptance (needs a finished workflow)",
    "queued guidance, startup wait, permission wait, reviewer switch and service restart "
    "(need live agents; covered by Rust contract tests)",
]

# The dashboard has no theme switch; it follows prefers-color-scheme. cmux
# cannot emulate that preference, and this suite does not change system
# settings, so each run verifies the browser's native appearance and records
# the other one as untested. Browser page zoom has no cmux command either;
# only the root text size is scaled, and it is reported as text scaling.
NATIVE_SCHEME = "matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'"

HOSTILE_MARKDOWN = "\n".join([
    "# Smoke heading",
    "",
    "Created through the real cmux browser and local HTTP service.",
    "",
    "[unsafe link](javascript:alert(1)) and [safe link](https://example.com)",
    "",
    "<img src=x onerror=alert(1)>",
    "",
    "| Column | Value |",
    "| --- | --- |",
    "| one | two |",
    "",
    "- [x] checked item",
])


def smoke(args):
    browser = Browser(args.artifacts)
    observer = Browser(args.artifacts, "observer.json") if (args.artifacts / "observer.json").exists() else None
    title = f"Browser smoke {time.time_ns()}"
    results = {
        "title": title,
        "project": str(args.project_path),
        "checks": [],
        "untested_boundaries": list(UNTESTED_BOUNDARIES),
    }

    def passed(name):
        results["checks"].append({"name": name, "status": "passed"})
        (args.artifacts / "smoke-results.json").write_text(json.dumps(results, indent=2))
        print(f"PASS {name}", flush=True)

    def card():
        return "[...document.querySelectorAll('.task-card')].find(e => e.querySelector('strong')?.textContent === " + json.dumps(title) + ")"

    try:
        browser.call("viewport", "1440", "1000")
        browser.evaluate("document.documentElement.style.fontSize = ''")
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
            "What's happening", "Tasks", "Recipes", "Projects",
            "Completed and archived tasks", "Diagnostics and setup",
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
        browser.fill("Description", HOSTILE_MARKDOWN)
        browser.fill("Acceptance criteria", "Draft persists after reload\nArchive and restore preserve the draft")
        browser.call("click", "button[aria-label='Close task form']")
        browser.click("＋ New task")
        browser.expect(f"document.querySelector({json.dumps(browser.label('Title'))}).value === {json.dumps(title)}")
        browser.call("reload")
        browser.wait("!!document.querySelector('.app-shell')")
        browser.click("＋ New task")
        browser.expect(f"document.querySelector({json.dumps(browser.label('Title'))}).value === {json.dumps(title)}")
        passed("new task validation and unsaved draft persistence")
        browser.evaluate("([...document.querySelector('#task-form-title').closest('form').querySelectorAll('details')].find(e => e.querySelector('summary')?.textContent === 'Role overrides').open = true)")
        browser.call("check", browser.label("Override Manager"))
        browser.call("select", "select[aria-label='Manager provider']", "codex")
        browser.fill("Manager model", "gpt-5.6-sol")
        browser.call("select", "select[aria-label='Manager effort']", "medium")
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
        browser.wait("!!document.querySelector('[role=dialog]')")
        browser.expect("document.querySelector('[role=dialog]').getAttribute('aria-modal') === 'true'")
        browser.expect("document.querySelector('main.content').hasAttribute('inert')")
        browser.expect("document.activeElement?.getAttribute('aria-label') === 'Close task details'")
        browser.expect("document.querySelector(\"button[aria-label='Close task details']\").textContent.trim() === '×'")
        description = "document.querySelector('[role=dialog] .task-description')"
        browser.wait(f"!!{description}")
        browser.expect(f"!!{description}.querySelector('h1, h2, h3')")
        browser.expect(f"!{description}.querySelector('img, script, [onerror]')")
        browser.expect(f"![...{description}.querySelectorAll('a')].some(a => !/^https?:|^mailto:|^[./#]/.test(a.getAttribute('href') || ''))")
        browser.expect(f"!!{description}.querySelector('.markdown-inert-link')")
        browser.expect(f"!!{description}.querySelector('table')")
        browser.expect(f"!!{description}.querySelector('input[type=checkbox]')")
        browser.capture("smoke-markdown-safety")
        passed("task description Markdown renders structure and keeps unsafe content inert")
        # The smoke project is not set up, so the draft explains that plainly.
        waiting = "document.querySelector('[role=dialog] [aria-labelledby=task-waiting-title]')"
        browser.wait(f"!!{waiting}")
        browser.expect(f"{waiting}.querySelector('p').textContent.includes('has not been set up yet')")
        browser.expect("![...document.querySelectorAll('[role=dialog] .next-step > p, [role=dialog] .decision-summary, [role=dialog] .controls > small.hint')].some(e => e.textContent.includes('not_initialized'))")
        browser.expect(f"[...{waiting}.querySelectorAll('button')].some(e => e.textContent.trim() === 'Open project setup')")
        passed("unset-up project is explained plainly with an Open project setup route")
        # Keyboard-only: Tab and Shift+Tab never leave the open dialog.
        for key in ["Tab"] * 12 + ["Shift+Tab"] * 12:
            browser.call("press", key)
            browser.expect("!!document.activeElement?.closest('[role=dialog]')")
        passed("keyboard focus stays inside the task dialog")
        browser.evaluate("document.dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape', bubbles: true}))")
        browser.wait("!document.querySelector('[role=dialog]')")
        browser.expect(f"document.activeElement === ({card()}).querySelector('.card-body')")
        browser.expect("document.querySelector('input[aria-label=\"Filter tasks\"]').value === ''")
        passed("task dialog closes with Escape and returns focus to its card")
        browser.call("click", browser.selector(f"({card()}).querySelector('.card-body')"))
        browser.wait("!!document.querySelector('[role=dialog]')")
        for tab in ["Changes", "Checks", "Activity", "Overview"]:
            browser.click(tab, "document.querySelector('[role=dialog]')")
            browser.wait(f"[...document.querySelectorAll('[role=tab]')].some(e => e.textContent === {json.dumps(tab)} && e.getAttribute('aria-selected') === 'true')")
        passed("task dialog tabs")
        browser.click("Activity", "document.querySelector('[role=dialog]')")
        browser.click("Archive", "document.querySelector('[role=dialog]')")
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
        section = "document.querySelector('[aria-label=\"Task section\"]')"
        browser.call("click", browser.selector(f"{section}.querySelector('button:nth-child(2)')"))
        browser.wait(f"!({card()})")
        browser.call("click", browser.selector(f"{section}.querySelector('button:nth-child(1)')"))
        browser.wait(f"!!({card()})")
        passed("board keeps completed tasks separate from active work")
        # More drafts make the Drafts lane much taller than the other lanes in
        # its row, so a wrapped row sized shorter than its content shows up.
        for index in range(1, 6):
            browser.click("＋ New task")
            browser.wait("!!document.querySelector('#task-form-title')")
            browser.fill("Title", f"{title} lane filler {index}")
            browser.click("Save draft")
            browser.wait("!document.querySelector('#task-form-title')")
        drafts = "document.querySelector('.board > .lane[aria-label=\"Drafts\"]')"
        browser.wait(f"{drafts}?.querySelectorAll('.task-card').length >= 6")
        # Every card must sit inside its own lane and no two lanes may overlap;
        # page width alone cannot see cards spilling down into the next row.
        board_layout = (
            "(() => { const box = e => e.getBoundingClientRect(); "
            "const lanes = [...document.querySelectorAll('.board > .lane')]; const problems = []; "
            "lanes.forEach((lane, i) => { const l = box(lane); const name = lane.getAttribute('aria-label'); "
            "lane.querySelectorAll('.task-card').forEach(card => { const c = box(card); "
            "if (c.top < l.top - 1 || c.bottom > l.bottom + 1 || c.left < l.left - 1 || c.right > l.right + 1) "
            "problems.push(`card outside ${name}: card ${Math.round(c.top)}-${Math.round(c.bottom)}, "
            "lane ${Math.round(l.top)}-${Math.round(l.bottom)}`); }); "
            "lanes.slice(i + 1).forEach(other => { const o = box(other); "
            "if (l.left < o.right - 1 && o.left < l.right - 1 && l.top < o.bottom - 1 && o.top < l.bottom - 1) "
            "problems.push(`${name} overlaps ${other.getAttribute('aria-label')}`); }); }); "
            "return { rows: new Set(lanes.map(lane => Math.round(box(lane).top))).size, problems }; })()")
        # The move and edit controls of every draft must be the element under
        # the pointer, not a lane painted over them.
        covered_controls = (
            f"[...{drafts}.querySelectorAll('.card-actions button')].filter(button => {{ "
            "button.scrollIntoView({block: 'center'}); const r = button.getBoundingClientRect(); "
            "const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); "
            "return !hit || !button.contains(hit); }).map(button => "
            "button.getAttribute('aria-label') || button.textContent.trim())")
        no_overflow = "document.documentElement.scrollWidth <= window.innerWidth + 1"
        # Narrow windows fold the navigation into the Menu button.
        open_menu = ("(() => { const toggle = document.querySelector('.menu-toggle'); "
                     "if (toggle && toggle.offsetParent && toggle.getAttribute('aria-expanded') === 'false') toggle.click(); "
                     "return true; })()")
        theme = browser.evaluate(NATIVE_SCHEME)
        other = "light" if theme == "dark" else "dark"
        results["native_color_scheme"] = theme
        browser.expect(f"getComputedStyle(document.documentElement).colorScheme.includes({json.dumps(theme)})")
        results["untested_boundaries"].append(
            f"{other} appearance: cmux cannot emulate prefers-color-scheme and this suite does not "
            f"change system settings; rerun with the system set to {other} appearance")
        results["untested_boundaries"].append(
            "browser page zoom: cmux exposes no zoom command; only root text size was scaled")
        for width, height, scale in [("1440", "1000", "100%"), ("900", "800", "100%"),
                                     ("390", "844", "100%"), ("1440", "1000", "200%")]:
            browser.call("viewport", width, height)
            browser.evaluate(f"document.documentElement.style.fontSize = {json.dumps(scale)}")
            label = f"text-{scale.rstrip('%')}"
            for index, name in [(1, "workspace"), (2, "board"), (4, "projects"), (1, "workspace")]:
                browser.evaluate(open_menu)
                browser.call("click", f"nav[aria-label='Main navigation'] button:nth-child({index})")
                browser.wait("!!document.querySelector('h1')")
                browser.expect(no_overflow)
                if name == "board":
                    browser.wait(f"!!{drafts}")
                    layout = browser.evaluate(board_layout)
                    layout["covered_controls"] = browser.evaluate(covered_controls)
                    browser.evaluate("(window.scrollTo(0, 0), true)")
                    results.setdefault("board_layout", {})[f"{width}-{label}"] = layout
                    if layout["problems"] or layout["covered_controls"]:
                        browser.capture(f"smoke-{theme}-board-overlap-{width}-{label}")
                        raise AssertionError(f"board lanes overlap at {width}px, text {scale}: {layout}")
                    passed(f"board cards stay inside their lanes and controls stay reachable at {width}px, "
                           f"text {scale} ({layout['rows']} lane rows)")
                browser.capture(f"smoke-{theme}-{name}-{width}-{label}")
            browser.expect("document.getElementById('workspace-attention') !== null")
            browser.evaluate("(window.scrollTo(0, 0), true)")
            browser.call("click", browser.selector("document.querySelector('.waiting-links button:nth-child(2)')"))
            browser.wait("document.activeElement?.id === 'workspace-approvals'")
            browser.expect("document.getElementById('workspace-approvals').getBoundingClientRect().top < window.innerHeight")
            passed(f"{theme} (native): no page overflow and approvals reachable at {width}px, root text scaled to {scale}")
        browser.evaluate("document.documentElement.style.fontSize = ''")
        browser.call("reload")
        browser.wait("!!document.querySelector('.app-shell')")
        # The reported RoleSettings defect: at a 193px-wide layout the provider
        # and effort fields collapsed to about 2px while model and Save
        # overflowed. Check the edit fields at 407px and 193px.
        browser.call("viewport", "1440", "1000")
        browser.evaluate(open_menu)
        browser.call("click", "nav[aria-label='Main navigation'] button:nth-child(2)")
        browser.wait(f"!!({card()})")
        browser.call("click", browser.selector(f"({card()}).querySelector('.card-body')"))
        browser.wait("!!document.querySelector('[role=dialog]')")
        browser.call("click", browser.selector("[...document.querySelectorAll('[role=tab]')].find(e => e.textContent === 'Activity')"))
        settings = "[...document.querySelectorAll('[role=dialog] details')].find(e => e.querySelector('summary')?.textContent === 'Agent settings')"
        browser.wait(f"!!({settings})")
        browser.evaluate(f"(({settings}).open = true, true)")
        browser.click("Edit", f"({settings}).querySelector('.role-list article')")
        fields = "document.querySelector('[role=dialog] .role-edit-fields')"
        browser.wait(f"!!{fields}")
        field_check = (f"(() => {{ const box = {fields}.getBoundingClientRect(); "
                       f"const controls = [...{fields}.querySelectorAll('select, input, button')]; "
                       "return controls.length >= 4 && controls.every(control => { "
                       "const rect = control.getBoundingClientRect(); "
                       "return rect.width >= 44 && rect.height >= 24 && "
                       "rect.left >= box.left - 1 && rect.right <= box.right + 1; }); })()")
        measure = (f"[...{fields}.querySelectorAll('select, input, button')].map(control => "
                   "{ const rect = control.getBoundingClientRect(); "
                   "return [control.tagName, Math.round(rect.width), Math.round(rect.height)]; })")
        for width in ["407", "193"]:
            browser.call("viewport", width, "900")
            browser.wait(f"!!{fields}")
            results.setdefault("role_settings_controls", {})[width] = browser.evaluate(measure)
            browser.expect(field_check)
            browser.expect(no_overflow)
            browser.capture(f"smoke-{theme}-role-settings-{width}")
            passed(f"role settings edit fields keep usable size without overflow at {width}px")
        browser.call("viewport", "1440", "1000")
        browser.click("Cancel", f"({settings})")
        browser.evaluate("document.dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape', bubbles: true}))")
        browser.wait("!document.querySelector('[role=dialog]')")
        browser.call("viewport", "390", "844")
        browser.evaluate(open_menu)
        browser.call("click", "nav[aria-label='Main navigation'] button:nth-child(2)")
        browser.wait(f"!!({card()})")
        browser.call("click", browser.selector(f"({card()}).querySelector('.card-body')"))
        browser.wait("!!document.querySelector('[role=dialog]')")
        browser.expect("Math.abs(document.querySelector('.task-dialog').getBoundingClientRect().width - window.innerWidth) <= 1")
        close = "document.querySelector(\"button[aria-label='Close task details']\")"
        browser.expect(f"(() => {{ const rect = {close}.getBoundingClientRect(); "
                       f"return {close}.textContent.trim() === '×' && rect.width >= 44 && rect.height >= 44 && "
                       "rect.right <= window.innerWidth; })()")
        browser.capture("smoke-dialog-390")
        browser.call("click", "button[aria-label='Close task details']")
        browser.wait("!document.querySelector('[role=dialog]')")
        passed("narrow task details open full screen and close with the icon-only X")
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
