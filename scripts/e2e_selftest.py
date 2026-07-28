#!/usr/bin/env python3
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT

"""Prove the desktop harness fails the way it claims to.

A green end-to-end suite says nothing until its failure path has run. This
drives `tests/e2e/wdio.selftest.conf.ts`, whose single spec fails against a
healthy app on purpose, and then asserts the three properties the real suite
leans on:

1. **The run stays red.** `specFileRetries` is turned *up* for this config. If
   a bounded retry could turn a repeated functional failure green, every flake
   allowance in CI would be hiding regressions instead of tolerating noise.
2. **The bound is a number, not a claim.** The spec appends one line per
   attempt, so the retry count is measured rather than trusted. Exactly
   `1 + RETRY_BOUND` attempts must happen: fewer means retries did not run at
   all, more means the bound leaks.
3. **The artifacts survive.** A failed desktop test that leaves nothing behind
   cannot be diagnosed from a CI log. A screenshot, the page source, the
   failure text, the stand-in CLI's journal, and the driver logs must all be
   in a deterministic directory — and none of them may carry a sentinel
   marker, which `fresh_user_smoke.py --verify --scan` re-checks separately.

Run it from the repository root: `npm run test:e2e:fixture`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CONFIG = "tests/e2e/wdio.selftest.conf.ts"
RUN_ID = "selftest"

#: Mirrors `RETRY_BOUND` in `tests/e2e/wdio.selftest.conf.ts`. Restated here
#: rather than parsed out of TypeScript: a check that reads its expectation
#: from the thing under test can never fail.
RETRY_BOUND = 2
EXPECTED_ATTEMPTS = 1 + RETRY_BOUND

#: What a failed desktop test must leave behind.
REQUIRED_ARTIFACTS = (
    "screenshot.png",
    "page-source.html",
    "failure.txt",
    "browser-log.json",
    "fixture-journal.jsonl",
    "tauri-driver.log",
)

SENTINEL = re.compile(r"ROCM-E2E-SENTINEL-[0-9a-f]+")


class Report:
    """One line per check, and a non-zero exit if any of them failed."""

    def __init__(self) -> None:
        self.failures: list[str] = []

    def ok(self, message: str) -> None:
        print(f"  ok    {message}")

    def note(self, message: str) -> None:
        print(f"  note  {message}")

    def fail(self, message: str) -> None:
        print(f"  FAIL  {message}")
        self.failures.append(message)

    def check(self, condition: bool, message: str) -> bool:
        if condition:
            self.ok(message)
        else:
            self.fail(message)
        return condition

    def finish(self) -> int:
        if self.failures:
            print(f"{len(self.failures)} violation(s)")
            return 1
        print("harness failure path behaves as designed")
        return 0


def run_suite(report: Report) -> tuple[int, str]:
    """Run the always-failing spec and hand back its exit code and output."""
    env = dict(os.environ)
    env["ROCM_E2E_RUN_ID"] = RUN_ID
    # `tauri build` and friends set CI=1, which the Tauri CLI rejects; nothing
    # here needs it, and inheriting it only creates a confusing failure.
    env.pop("CI", None)
    completed = subprocess.run(
        ["npx", "wdio", "run", CONFIG],
        cwd=REPO,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    output = completed.stdout + completed.stderr
    report.note(f"wdio exited {completed.returncode}")
    return completed.returncode, output


def count_attempts(root: Path) -> int:
    log = root / "attempts.log"
    if not log.is_file():
        return 0
    return len([line for line in log.read_text().splitlines() if line.strip()])


def artifact_dirs(root: Path) -> list[Path]:
    artifacts = root / "artifacts"
    if not artifacts.is_dir():
        return []
    return sorted(path for path in artifacts.iterdir() if path.is_dir())


def check_artifacts(report: Report, root: Path) -> None:
    dirs = artifact_dirs(root)
    if not report.check(bool(dirs), f"a failure artifact directory exists under {root / 'artifacts'}"):
        return
    report.note(f"artifact directory: {dirs[0].relative_to(REPO)}")
    present = {path.name for path in dirs[0].iterdir()}
    report.note("retained: " + ", ".join(sorted(present)))
    for name in REQUIRED_ARTIFACTS:
        report.check(name in present, f"retained {name}")
    screenshot = dirs[0] / "screenshot.png"
    if screenshot.is_file():
        report.check(
            screenshot.stat().st_size > 1024,
            f"the screenshot has real pixels ({screenshot.stat().st_size} bytes)",
        )
    leaked = [
        path.name
        for path in dirs[0].rglob("*")
        if path.is_file() and _has_marker(path)
    ]
    report.check(not leaked, f"no sentinel marker survived sanitisation (checked {len(present)} file(s))")
    if leaked:
        report.note("leaked in: " + ", ".join(leaked))


def _has_marker(path: Path) -> bool:
    try:
        blob = path.read_bytes()
    except OSError:
        return False
    if b"\0" in blob[:8192]:
        return False
    return bool(SENTINEL.search(blob.decode("utf-8", "replace")))


def self_test(report: Report) -> int:
    """Exercise this script's own reasoning with no wdio and no app."""
    print("e2e self-test (dry)")
    cases = [
        ("a red run with the bound reached is correct", 1, EXPECTED_ATTEMPTS, True),
        ("a green run means retries hid the failure", 0, EXPECTED_ATTEMPTS, False),
        ("too few attempts means retries never ran", 1, 1, False),
        ("too many attempts means the bound leaks", 1, EXPECTED_ATTEMPTS + 1, False),
    ]
    for label, code, attempts, expected in cases:
        verdict = code != 0 and attempts == EXPECTED_ATTEMPTS
        report.check(verdict == expected, f"{label}: {'accepted' if verdict else 'rejected'}")
    report.check(
        SENTINEL.search("x ROCM-E2E-SENTINEL-0123abcd y") is not None,
        "the sentinel pattern matches a real marker",
    )
    report.check(
        SENTINEL.search("nothing here") is None,
        "the sentinel pattern does not match ordinary text",
    )
    return report.finish()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="check this script's own reasoning; runs no browser and no app",
    )
    parser.add_argument(
        "--keep",
        action="store_true",
        help="leave the run directory in place for inspection",
    )
    args = parser.parse_args()

    report = Report()
    if args.self_test:
        return self_test(report)

    root = REPO / "test-results" / "e2e" / RUN_ID
    shutil.rmtree(root, ignore_errors=True)
    print(f"e2e harness self-test  (config {CONFIG}, retry bound {RETRY_BOUND})")

    code, output = run_suite(report)
    report.check(code != 0, "a repeated functional failure is still a failure after every retry")

    attempts = count_attempts(root)
    report.check(
        attempts == EXPECTED_ATTEMPTS,
        f"the spec ran {attempts} time(s); the bound allows exactly {EXPECTED_ATTEMPTS}",
    )
    check_artifacts(report, root)

    if report.failures:
        tail = "\n".join(output.splitlines()[-40:])
        print("--- wdio output (tail) ---")
        print(tail)
    elif not args.keep:
        # The passing case leaves nothing behind; a failure keeps everything.
        shutil.rmtree(root, ignore_errors=True)
    else:
        report.note(f"kept {root}")

    print(json.dumps({"exit": code, "attempts": attempts}, sort_keys=True))
    return report.finish()


if __name__ == "__main__":
    sys.exit(main())
