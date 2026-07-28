#!/usr/bin/env python3
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
"""Read the CI workflows and assert the two things a green build cannot prove.

The first is supply chain. Every third-party action here is pinned to a full
commit SHA with a trailing version comment, and `@v4` would work exactly as
well -- right up until the tag is moved. A retagged or compromised action then
enters CI silently, with a green check beside it. A passing run looks identical
either way, so nothing notices unless something reads the workflow and says so.

The second is platform evidence. This project ships a Windows installer that
cannot be produced on the machine it is developed on: NSIS bundling needs a
Windows host, so `package (windows nsis)` is the only place that evidence
exists. Rename its bundling step, drop `--bundles`, delete the job -- CI still
passes. The steps that remain succeed, and the Windows installer quietly stops
being built. So the shape of the matrix is asserted rather than trusted: both
platforms present, and each of them building, testing, and packaging.

What this reads, and what it cannot see:

It is a line scanner, not a YAML parser. Taking a dependency to check the file
that decides what runs before dependencies are installed is backwards, so it
does not. It understands indentation, `key: value`, list items, and block
scalars (`run: |`) -- every construct these workflows use, and enough to keep
the PowerShell inside a `run:` body from being read as configuration.

It does not resolve anchors, aliases, flow mappings (`{a: b}`), multi-document
files, or `env:` interpolation, and it reads `uses:` and `runs-on:` as literal
text. A `runs-on:` behind a matrix expression names no runner, so the job's own
text is searched for literal runner names instead and the result is reported as
inferred. Steps reached through a reusable workflow or a composite action are
invisible from here and are checked wherever they are written. Coverage is
judged by the commands a step names, which is a claim about the text of the
workflow and not about what the runner did with it.

Usage:
    python3 scripts/ci_validate.py
    python3 scripts/ci_validate.py --workflows path/to/workflows
    python3 scripts/ci_validate.py --self-test
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

APP_ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_DIR = APP_ROOT / ".github" / "workflows"

# A pin is a full 40-hex commit SHA. An abbreviation is not one: it resolves
# against whatever the action's repository happens to contain, and what it
# resolves to can change under a fixed workflow file.
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")
ANY_CASE_SHA = re.compile(r"^[0-9a-fA-F]{40}$")
HEX = re.compile(r"^[0-9a-f]+$")
# The trailing comment that makes a SHA reviewable. Without it nobody can tell
# which version they are on, so nobody ever bumps it.
VERSION_COMMENT = re.compile(r"#\s*v\d+(\.\d+)*")
# `owner/repo@ref` or `owner/repo/subdir@ref`.
ACTION_REF = re.compile(r"^(?P<action>[^/@\s]+/[^@\s]+)@(?P<ref>\S+)$")
# `key: value`, optionally as a list item. Applied to structural lines only.
KEY = re.compile(r"^(?:-\s+)?(?P<key>[A-Za-z0-9_.-]+):(?:\s+(?P<value>.*))?$")
# `run: |`, `path: >-` and friends: everything indented under them is text.
BLOCK_SCALAR = re.compile(r":\s*[|>][-+\d]*\s*(?:#.*)?$")

# The runner-name prefix that identifies each platform group.
PLATFORM_PREFIX = {"linux": "ubuntu-", "windows": "windows-"}

# The repo's own commands, as a step names them. A capability is claimed by the
# text of a step, so these lists are the definition of "this job builds" --
# rename a command and it has to be added here or coverage goes red, which is
# the intended failure and not a false one.
#
# Each entry is the set of substrings a step must contain, and the entries are
# ordered strongest evidence first. `installer_acceptance` counts as packaging
# because it unpacks a real installer, but the job that ran `tauri build
# --bundles` is the better answer to "where does this platform package?" and is
# reported when both exist.
CAPABILITY_TOKENS: dict[str, tuple[tuple[str, ...], ...]] = {
    "build": (("cargo build",), ("npm run build",), ("tauri build",)),
    "test": (
        ("cargo test",),
        ("npm test",),
        ("npm run test",),
        ("run test:",),
        ("installer_acceptance",),
        ("package_verify",),
    ),
    "package": (("tauri build", "--bundles"), ("package:verify",), ("installer_acceptance",)),
}
CAPABILITY_VERB = {"build": "builds", "test": "tests", "package": "packages"}


@dataclass
class Report:
    """Accumulated findings. Checks record and continue rather than raising.

    One failure usually implies others, and a run that stops at the first one
    hides them -- the reader fixes it, reruns, and finds the next. Every check
    that can run, runs.
    """

    lines: list[str] = field(default_factory=list)
    failures: list[str] = field(default_factory=list)

    def head(self, text: str) -> None:
        self.lines.append(text)

    def ok(self, text: str) -> None:
        self.lines.append(f"  ok    {text}")

    def note(self, text: str) -> None:
        self.lines.append(f"  note  {text}")

    def fail(self, text: str) -> None:
        self.lines.append(f"  FAIL  {text}")
        self.failures.append(text)

    def emit(self) -> None:
        sys.stdout.write("\n".join(self.lines) + "\n")


# --------------------------------------------------------------------------
# Scanner
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class Line:
    """One physical line, told apart from the block-scalar bodies around it."""

    number: int
    indent: int
    text: str
    in_block: bool


def scan_lines(source: str) -> list[Line]:
    """Split a workflow into structural lines and block-scalar body lines.

    The distinction is the whole trick. `run: |` turns everything indented
    under it into shell text, where a `uses:` or a `runs-on:` is a string and
    not a key. Without it this scanner would read the PowerShell that installs
    the NSIS bundle as workflow configuration.
    """
    lines: list[Line] = []
    block_indent: int | None = None
    for number, raw in enumerate(source.splitlines(), start=1):
        text = raw.strip()
        indent = len(raw) - len(raw.lstrip(" "))
        if not text:
            lines.append(Line(number, indent, "", block_indent is not None))
            continue
        if block_indent is not None and indent > block_indent:
            lines.append(Line(number, indent, text, True))
            continue
        block_indent = indent if BLOCK_SCALAR.search(raw) else None
        lines.append(Line(number, indent, text, False))
    return lines


def is_structural(line: Line) -> bool:
    """True when a line carries YAML structure rather than text or nothing."""
    return not line.in_block and bool(line.text) and not line.text.startswith("#")


def split_comment(value: str) -> tuple[str, str]:
    """Separate a scalar from its trailing comment, keeping the comment's `#`."""
    head, marker, comment = value.partition("#")
    return head.strip().strip("'\""), (marker + comment).strip()


def key_of(line: Line) -> re.Match[str] | None:
    return KEY.match(line.text) if is_structural(line) else None


# --------------------------------------------------------------------------
# Rules A and C: every `uses:` is pinned to a reviewable commit
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class Use:
    file: str
    line: int
    raw: str  # the `uses:` value, comment and quotes removed
    comment: str  # the trailing comment including its `#`, or ""
    action: str  # `owner/repo[/sub]`, or "" when this is not an action ref
    ref: str  # whatever followed the `@`, or ""


def scan_uses(file: str, lines: list[Line]) -> list[Use]:
    found: list[Use] = []
    for line in lines:
        match = key_of(line)
        if not match or match.group("key") != "uses":
            continue
        raw, comment = split_comment(match.group("value") or "")
        ref = ACTION_REF.match(raw)
        found.append(
            Use(
                file=file,
                line=line.number,
                raw=raw,
                comment=comment,
                action=ref.group("action") if ref else "",
                ref=ref.group("ref") if ref else "",
            )
        )
    return found


def judge(use: Use) -> tuple[str, str]:
    """Rule the one `uses:` line, as (level, reason). Levels: ok, note, fail.

    Rule C folds in here rather than reporting separately: a branch ref and a
    moving tag are the same defect seen twice, and one line deserves one
    verdict.
    """
    if use.raw.startswith("./"):
        return "note", "local to this repo; pinning does not apply"
    if not use.action:
        return "fail", f"unreadable `uses:` value `{use.raw}`; expected owner/repo@sha or ./local"
    if FULL_SHA.match(use.ref):
        if VERSION_COMMENT.search(use.comment):
            return "ok", ""
        if use.comment:
            return "fail", f"trailing comment `{use.comment}` names no version; use `# vX.Y.Z`"
        return "fail", "pinned but unreviewable: no trailing `# vX.Y.Z` comment"
    if ANY_CASE_SHA.match(use.ref):
        return "fail", f"SHA `{use.ref}` is not lowercase hex"
    if HEX.match(use.ref) and len(use.ref) >= 7:
        length = len(use.ref)
        return "fail", f"abbreviated SHA `{use.ref}` ({length} of 40 hex); a prefix is not a pin"
    if re.match(r"^v?\d", use.ref):
        return "fail", f"moving tag `{use.ref}`; pin the commit SHA it points at today"
    return "fail", f"branch ref `{use.ref}`; a branch is rewritten under you -- pin the commit SHA"


def check_pins(uses: list[Use], report: Report) -> None:
    report.head("pin scan")
    if not uses:
        report.note("no `uses:` lines in any workflow")
        return
    places = [f"{use.file}:{use.line}" for use in uses]
    names = [use.action or use.raw for use in uses]
    place_width = max(len(place) for place in places)
    name_width = max(len(name) for name in names)
    counts = {"ok": 0, "note": 0, "fail": 0}
    for use, place, name in zip(uses, places, names):
        level, reason = judge(use)
        counts[level] += 1
        version = use.comment.lstrip("#").strip() or "--"
        row = f"{place:<{place_width}}  {name:<{name_width}}  {use.ref or '--':<40}  {version}"
        if level == "ok":
            report.ok(row)
        elif level == "note":
            report.note(f"{row}  {reason}")
        else:
            report.fail(f"{row}  {reason}")
    report.head(f"  {counts['ok']} pinned, {counts['note']} exempt, {counts['fail']} rejected")


# --------------------------------------------------------------------------
# Rule B: both platforms build, test, and package
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class Step:
    job: str
    line: int
    name: str
    run: str

    @property
    def text(self) -> str:
        return f"{self.name}\n{self.run}".lower()

    def label(self) -> str:
        return f"{self.job} / {self.name or f'step at line {self.line}'}"


@dataclass(frozen=True)
class Job:
    file: str
    name: str
    line: int
    runs_on: str
    platforms: frozenset[str]
    inferred: bool  # platforms came from a matrix block, not from `runs-on:`
    steps: tuple[Step, ...]


def job_spans(lines: list[Line]) -> list[tuple[str, Line, list[Line]]]:
    """Cut the `jobs:` mapping into one span of lines per job, by indentation."""
    start = next(
        (
            index
            for index, line in enumerate(lines)
            if is_structural(line) and line.indent == 0 and line.text == "jobs:"
        ),
        None,
    )
    if start is None:
        return []
    rest = lines[start + 1 :]
    first = next((line for line in rest if is_structural(line)), None)
    if first is None:
        return []
    spans: list[tuple[str, Line, list[Line]]] = []
    current: list[Line] | None = None
    for line in rest:
        if is_structural(line) and line.indent < first.indent:
            break
        header = key_of(line) if is_structural(line) and line.indent == first.indent else None
        if header and not split_comment(header.group("value") or "")[0]:
            current = []
            spans.append((header.group("key"), line, current))
        elif current is not None:
            current.append(line)
    return spans


def read_runs_on(span: list[Line], key_indent: int) -> str:
    """The job's runner, whether written inline or as a list beneath the key."""
    for index, line in enumerate(span):
        match = key_of(line)
        if line.indent != key_indent or not match or match.group("key") != "runs-on":
            continue
        inline = split_comment(match.group("value") or "")[0]
        if inline:
            return inline
        items: list[str] = []
        for follower in span[index + 1 :]:
            if not is_structural(follower):
                continue
            if follower.indent <= key_indent:
                break
            items.append(follower.text.removeprefix("- ").strip())
        return " ".join(items)
    return ""


def resolve_platforms(runs_on: str, body: str) -> tuple[frozenset[str], bool]:
    lowered = runs_on.lower()
    named = {name for name, prefix in PLATFORM_PREFIX.items() if prefix in lowered}
    if named or "${{" not in lowered:
        return frozenset(named), False
    # A matrix expression names no runner here, and resolving it would mean
    # evaluating GitHub's expression language. The literal runner names are
    # almost always in the job's own `matrix:` block, so read those and say
    # plainly that is where the answer came from.
    return frozenset(name for name, prefix in PLATFORM_PREFIX.items() if prefix in body), True


def read_steps(job: str, span: list[Line], key_indent: int) -> tuple[Step, ...]:
    start = next(
        (
            index + 1
            for index, line in enumerate(span)
            if line.indent == key_indent
            and (match := key_of(line))
            and match.group("key") == "steps"
        ),
        None,
    )
    if start is None:
        return ()
    rest = span[start:]
    item_indent = next(
        (line.indent for line in rest if is_structural(line) and line.text.startswith("- ")),
        None,
    )
    if item_indent is None:
        return ()
    groups: list[list[Line]] = []
    for line in rest:
        if is_structural(line) and line.indent < item_indent:
            break
        if is_structural(line) and line.indent == item_indent and line.text.startswith("- "):
            groups.append([])
        if groups:
            groups[-1].append(line)
    return tuple(
        Step(job=job, line=group[0].number, name=step_name(group), run=step_run(group))
        for group in groups
    )


def step_name(group: list[Line]) -> str:
    for line in group:
        match = key_of(line)
        if match and match.group("key") == "name":
            return split_comment(match.group("value") or "")[0]
    return ""


def step_run(group: list[Line]) -> str:
    """Every `run:` in a step: the inline form, and the block-scalar bodies."""
    parts: list[str] = []
    for index, line in enumerate(group):
        match = key_of(line)
        if not match or match.group("key") != "run":
            continue
        value = (match.group("value") or "").strip()
        if value and value[0] not in "|>":
            parts.append(value)
        for follower in group[index + 1 :]:
            if not follower.in_block:
                break
            if follower.text:
                parts.append(follower.text)
    return "\n".join(parts)


def scan_jobs(file: str, lines: list[Line]) -> list[Job]:
    jobs: list[Job] = []
    for name, header, span in job_spans(lines):
        key_indent = min(
            (line.indent for line in span if is_structural(line)), default=header.indent + 2
        )
        runs_on = read_runs_on(span, key_indent)
        body = "\n".join(line.text for line in span).lower()
        platforms, inferred = resolve_platforms(runs_on, body)
        jobs.append(
            Job(
                file=file,
                name=name,
                line=header.number,
                runs_on=runs_on,
                platforms=platforms,
                inferred=inferred,
                steps=read_steps(name, span, key_indent),
            )
        )
    return jobs


def find_capability(
    steps: list[Step], patterns: tuple[tuple[str, ...], ...]
) -> tuple[Step, str] | None:
    """The first step matching the strongest pattern, and the pattern it matched."""
    for pattern in patterns:
        for step in steps:
            if all(token in step.text for token in pattern):
                return step, " ".join(pattern)
    return None


def check_platforms(jobs: list[Job], report: Report) -> None:
    report.head("platform coverage")
    for platform, prefix in PLATFORM_PREFIX.items():
        members = [job for job in jobs if platform in job.platforms]
        if not members:
            report.fail(f"{platform}: no job runs on a `{prefix}*` runner")
            continue
        runners = ", ".join(
            f"{job.file}:{job.name} ({job.runs_on}"
            f"{', inferred from matrix' if job.inferred else ''})"
            for job in members
        )
        report.note(f"{platform}: {runners}")
        report_capabilities(platform, members, report)


def report_capabilities(platform: str, members: list[Job], report: Report) -> None:
    steps = [step for job in members for step in job.steps]
    for capability, patterns in CAPABILITY_TOKENS.items():
        hit = find_capability(steps, patterns)
        verb = CAPABILITY_VERB[capability]
        if hit is None:
            searched = ", ".join(job.name for job in members)
            report.fail(f"{platform}: no step {verb} (searched job(s): {searched})")
        else:
            step, token = hit
            report.ok(f"{platform:<7} {capability:<7} {step.label()} -- `{token}`")


# --------------------------------------------------------------------------
# Driver
# --------------------------------------------------------------------------


def validate(workflows: Path) -> Report:
    report = Report()
    if not workflows.is_dir():
        report.head(f"workflows: {workflows}")
        report.fail(f"no workflow directory at {workflows}")
        return report
    paths = sorted({*workflows.glob("*.yml"), *workflows.glob("*.yaml")})
    named = ", ".join(path.name for path in paths)
    report.head(f"workflows: {workflows} ({len(paths)} file(s): {named})")
    if not paths:
        report.fail(f"no *.yml or *.yaml workflow files under {workflows}")
        return report
    uses: list[Use] = []
    jobs: list[Job] = []
    for path in paths:
        lines = scan_lines(path.read_text(encoding="utf-8"))
        uses.extend(scan_uses(path.name, lines))
        jobs.extend(scan_jobs(path.name, lines))
    check_pins(uses, report)
    check_platforms(jobs, report)
    return report


# --------------------------------------------------------------------------
# Self-test
# --------------------------------------------------------------------------

PINNED_SHA = "3d3c42e5aac5ba805825da76410c181273ba90b1"
PINNED_USES = f"actions/checkout@{PINNED_SHA} # v7.0.1"

LINUX_JOB = (
    "ubuntu-latest",
    ("npm run build", "npm test -- --run", "npm run tauri build -- --bundles deb,rpm"),
)
WINDOWS_JOB = (
    "windows-latest",
    ("cargo build --release", "cargo test --all-targets", "npm run tauri build -- --bundles nsis"),
)
WINDOWS_NO_PACKAGE = ("windows-latest", ("cargo build --release", "cargo test --all-targets"))
# A `run:` body that would look like a moving-tag violation to a scanner that
# could not tell configuration from shell text.
LINUX_WITH_TRAP = (
    LINUX_JOB[0],
    (*LINUX_JOB[1], "echo 'uses: actions/checkout@v4'"),
)
BOTH_PLATFORMS = {"checks": LINUX_JOB, "package-windows": WINDOWS_JOB}

MATRIX_WORKFLOW = (
    "name: fixture\n"
    "on: [push]\n"
    "jobs:\n"
    "  package:\n"
    "    strategy:\n"
    "      matrix:\n"
    "        os: [ubuntu-latest, windows-latest]\n"
    "    runs-on: ${{ matrix.os }}\n"
    "    steps:\n"
    f"      - uses: {PINNED_USES}\n"
    "      - name: build test package\n"
    "        run: |\n"
    "          npm run build\n"
    "          npm test -- --run\n"
    "          npm run tauri build -- --bundles deb,rpm\n"
)


def render(jobs: dict[str, tuple[str, tuple[str, ...]]], uses: str = PINNED_USES) -> str:
    """A workflow shaped like this repo's: one `uses:` per job, then `run:` steps."""
    out = ["name: fixture", "on: [push]", "jobs:"]
    for name, (runner, commands) in jobs.items():
        out += [f"  {name}:", f"    runs-on: {runner}", "    steps:", f"      - uses: {uses}"]
        for index, command in enumerate(commands):
            out += [f"      - name: step {index}", "        run: |", f"          {command}"]
    return "\n".join(out) + "\n"


def self_test() -> int:
    """Run both rules against synthetic workflows, then break each one in turn.

    A self-test that only proved the passing case would pass just as happily if
    a rule silently stopped looking -- which is the way these rules fail. Each
    negative case names the finding it expects, so a workflow that fails for an
    unrelated reason is a self-test failure and not a pass.
    """
    report = Report()
    report.head("self-test")
    failures: list[str] = []

    def expect(
        label: str, files: str | dict[str, str], should_pass: bool, contains: str = ""
    ) -> None:
        written = {"ci.yml": files} if isinstance(files, str) else files
        with tempfile.TemporaryDirectory(prefix="rocm-ci-validate-") as root:
            directory = Path(root)
            for name, text in written.items():
                (directory / name).write_text(text, encoding="utf-8")
            result = validate(directory)
        passed = not result.failures
        detail = "; ".join(result.failures)
        if passed != should_pass:
            wanted = "pass" if should_pass else "failure"
            report.fail(f"{label}: expected {wanted} -- {detail or 'no findings'}")
            failures.append(label)
        elif contains and contains not in detail:
            report.fail(f"{label}: failed for the wrong reason -- {detail}")
            failures.append(label)
        else:
            report.ok(f"{label}: {'passed' if passed else 'failed as expected'}")

    expect("compliant workflow, both platforms", render(BOTH_PLATFORMS), True)
    expect(
        "coverage may span two workflow files",
        {"lint.yml": render({"checks": LINUX_JOB}), "release.yml": render({"win": WINDOWS_JOB})},
        True,
    )
    expect("matrix runner resolved from the matrix block", MATRIX_WORKFLOW, True)
    expect(
        "`uses:` inside a run body is shell text, not a pin",
        render({"checks": LINUX_WITH_TRAP, "package-windows": WINDOWS_JOB}),
        True,
    )
    expect(
        "local action is exempt from pinning",
        render(BOTH_PLATFORMS, uses="./.github/actions/setup"),
        True,
    )
    expect(
        "moving tag",
        render(BOTH_PLATFORMS, uses="actions/checkout@v4"),
        False,
        "moving tag `v4`",
    )
    expect(
        "branch ref",
        render(BOTH_PLATFORMS, uses="actions/checkout@main"),
        False,
        "branch ref `main`",
    )
    expect(
        "abbreviated SHA",
        render(BOTH_PLATFORMS, uses="actions/checkout@3d3c42e5"),
        False,
        "abbreviated SHA `3d3c42e5`",
    )
    expect(
        "full SHA without a version comment",
        render(BOTH_PLATFORMS, uses=f"actions/checkout@{PINNED_SHA}"),
        False,
        "no trailing `# vX.Y.Z` comment",
    )
    expect(
        "full SHA whose comment names no version",
        render(BOTH_PLATFORMS, uses=f"actions/checkout@{PINNED_SHA} # pinned"),
        False,
        "names no version",
    )
    expect(
        "linux only, no Windows job",
        render({"checks": LINUX_JOB}),
        False,
        "windows: no job runs on a `windows-*` runner",
    )
    expect(
        "Windows job builds but never packages",
        render({"checks": LINUX_JOB, "package-windows": WINDOWS_NO_PACKAGE}),
        False,
        "windows: no step packages",
    )
    expect(
        "empty workflow directory",
        {},
        False,
        "no *.yml or *.yaml workflow files",
    )

    if failures:
        report.fail(f"{len(failures)} self-test expectation(s) did not hold")
    else:
        report.ok("every self-test expectation held; temp roots removed")
    report.emit()
    return 1 if failures else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--workflows",
        type=Path,
        default=WORKFLOW_DIR,
        metavar="DIR",
        help="directory of workflow files to validate (default: .github/workflows)",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="verify these rules against synthetic workflows under a temp root",
    )
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    try:
        report = validate(args.workflows)
    except OSError as error:
        sys.stderr.write(f"ci_validate: {error}\n")
        return 1
    report.head(f"{len(report.failures)} failure(s)" if report.failures else "all checks passed")
    report.emit()
    return 1 if report.failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
