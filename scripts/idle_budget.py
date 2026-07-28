#!/usr/bin/env python3
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
"""Measure what the installed app costs to leave running.

The product's pitch includes "sits in the tray"; the bill for that is idle CPU
and memory, and a claim about either is worthless unless it was measured on
the artifact users install, with the CLI the installer shipped, over a window
long enough to include several scheduled probes. So this script extracts the
built .deb, launches `usr/bin/rocm-app --hidden` under the same isolation the
fresh-user smoke proves (private session bus, StatusNotifierWatcher stand-in,
every user-state root pointed at a scratch directory), and samples the whole
process tree -- app, webview helpers, and probe children -- for the window.

Accounting is tree-complete on both axes:

- CPU is `(utime+stime+cutime+cstime)` of the app plus `(utime+stime)` of its
  live descendants, end minus start. A probe child that lives 200ms between
  samples is not missed: the app reaps it and its time lands in `cutime`.
- Memory is the per-sample sum of resident set sizes over the live tree.
  WebKit's helper processes are most of the footprint; a number that omits
  them is marketing, not measurement.

Timing is captured on the way in: launch-to-tray-registered (the watcher's
registry file names the item) and launch-to-first-probe-finished (a `rocm`
child was seen and is gone), both observed from outside the process.

Exit is non-zero when a budget is exceeded, so this can gate a release.

Usage:
    python3 scripts/idle_budget.py                      # 10 min, deb artifact
    python3 scripts/idle_budget.py --duration 60        # quick look
    python3 scripts/idle_budget.py --app path/to/rocm-app
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(Path(__file__).resolve().parent))
import fresh_user_smoke as smoke  # noqa: E402  (single owner of isolation policy)

CLK_TCK = os.sysconf("SC_CLK_TCK")
PAGE_SIZE = os.sysconf("SC_PAGE_SIZE")


def reexec_on_private_bus() -> None:
    """Replace this process with itself under `dbus-run-session --`."""
    if os.environ.get("ROCM_IDLE_BUS") == "1":
        return
    env = dict(os.environ, ROCM_IDLE_BUS="1")
    argv = ["dbus-run-session", "--", sys.executable, str(Path(__file__).resolve())]
    argv += sys.argv[1:]
    os.execvpe(argv[0], argv, env)


def newest_deb() -> Path:
    debs = sorted(
        (REPO / "src-tauri" / "target" / "release" / "bundle" / "deb").glob("*.deb"),
        key=lambda p: p.stat().st_mtime,
    )
    if not debs:
        raise SystemExit("no .deb under src-tauri/target/release/bundle/deb; build bundles first")
    return debs[-1]


def extract_deb(deb: Path, into: Path) -> Path:
    subprocess.run(["dpkg-deb", "-x", str(deb), str(into)], check=True)
    app = into / "usr" / "bin" / "rocm-app"
    if not app.is_file():
        raise SystemExit(f"{deb} carries no usr/bin/rocm-app")
    return app


# ---------------------------------------------------------------------------
# /proc accounting
# ---------------------------------------------------------------------------


def read_stat(pid: int) -> tuple[str, int, int, int] | None:
    """(comm, ppid, own jiffies, reaped-children jiffies) or None if gone."""
    try:
        text = Path(f"/proc/{pid}/stat").read_text()
    except OSError:
        return None
    # comm may contain spaces and parens; the closing paren is the last one.
    head, _, tail = text.rpartition(")")
    comm = head.split("(", 1)[1] if "(" in head else "?"
    fields = tail.split()
    # after ')': state=0, ppid=1, ... utime=11, stime=12, cutime=13, cstime=14
    ppid = int(fields[1])
    own = int(fields[11]) + int(fields[12])
    reaped = int(fields[13]) + int(fields[14])
    return comm, ppid, own, reaped


def read_rss(pid: int) -> int:
    try:
        return int(Path(f"/proc/{pid}/statm").read_text().split()[1]) * PAGE_SIZE
    except (OSError, IndexError, ValueError):
        return 0


def descendants(root_pid: int) -> list[int]:
    children: dict[int, list[int]] = {}
    for entry in os.scandir("/proc"):
        if not entry.name.isdigit():
            continue
        stat = read_stat(int(entry.name))
        if stat is not None:
            children.setdefault(stat[1], []).append(int(entry.name))
    tree, queue = [], [root_pid]
    while queue:
        pid = queue.pop()
        for child in children.get(pid, ()):
            tree.append(child)
            queue.append(child)
    return tree


def tree_sample(root_pid: int) -> dict[str, object] | None:
    """One observation of the whole process tree, or None if the app is gone."""
    root = read_stat(root_pid)
    if root is None:
        return None
    live = [(root_pid, root)]
    for pid in descendants(root_pid):
        stat = read_stat(pid)
        if stat is not None:
            live.append((pid, stat))
    return {
        # Root's reaped-children time covers every probe child that already
        # exited; live descendants are counted directly.
        "cpuJiffies": root[3] + sum(stat[2] for _, stat in live),
        "rssBytes": sum(read_rss(pid) for pid, _ in live),
        "processes": len(live),
        "rocmChildren": sum(1 for _, stat in live if stat[0] == "rocm"),
    }


# ---------------------------------------------------------------------------
# The run
# ---------------------------------------------------------------------------


def wait_for_tray(registry: Path, deadline: float) -> float | None:
    while time.monotonic() < deadline:
        try:
            if json.loads(registry.read_text()).get("items"):
                return time.monotonic()
        except (OSError, ValueError):
            pass
        time.sleep(0.05)
    return None


def wait_for_first_probe(pid: int, deadline: float) -> float | None:
    seen = False
    while time.monotonic() < deadline:
        sample = tree_sample(pid)
        if sample is None:
            return None
        if sample["rocmChildren"]:
            seen = True
        elif seen:
            return time.monotonic()
        time.sleep(0.1)
    return None


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--app", type=Path, help="installed app binary (default: extract the .deb)")
    parser.add_argument("--deb", type=Path, help="deb to extract (default: newest built)")
    parser.add_argument("--duration", type=float, default=600.0, help="measure window seconds")
    parser.add_argument("--settle", type=float, default=30.0, help="pre-window settle seconds")
    parser.add_argument("--interval", type=float, default=5.0, help="sample every N seconds")
    parser.add_argument("--budget-cpu", type=float, default=1.0, help="max average CPU percent")
    parser.add_argument("--budget-rss-mib", type=float, default=512.0, help="max tree RSS MiB")
    parser.add_argument("--out", type=Path, default=REPO / "test-results" / "idle")
    args = parser.parse_args()

    reexec_on_private_bus()
    report = smoke.Report()
    report.head("installed idle budget")
    args.out.mkdir(parents=True, exist_ok=True)

    root = Path(tempfile.mkdtemp(prefix="rocm-idle-", dir="/tmp"))
    watcher = app = xvfb = None
    code = 0
    try:
        app_binary = args.app
        if app_binary is None:
            deb = args.deb or newest_deb()
            app_binary = extract_deb(deb, root / "installed")
            report.ok(f"extracted {deb.name} ({deb.stat().st_size:,} bytes)")

        smoke.prepare(root / "iso", report)
        env = dict(os.environ)
        env.update(smoke.isolation_env(root / "iso"))
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("DISPLAY", None)
        xvfb = smoke.ensure_display(env, "auto", report)

        registry = root / "tray-registry.json"
        # Its own session, or stop_process()'s group kill takes this harness
        # down with it — the app and Xvfb already detach, the watcher must too.
        watcher = subprocess.Popen(
            [sys.executable, str(REPO / "scripts" / "statusnotifierwatcher.py"), str(registry)],
            env=env,
            start_new_session=True,
        )
        deadline = time.monotonic() + 15
        while not registry.exists() and time.monotonic() < deadline:
            time.sleep(0.05)

        launch = time.monotonic()
        with (root / "app.log").open("wb") as log:
            app = subprocess.Popen(
                [str(app_binary), "--hidden"],
                cwd=str(root), env=env, stdout=log, stderr=subprocess.STDOUT,
                start_new_session=True,
            )

        tray_at = wait_for_tray(registry, launch + 30)
        if tray_at is None:
            report.fail("tray never registered within 30s")
        else:
            report.ok(f"tray registered {tray_at - launch:.2f}s after launch")
        probe_at = wait_for_first_probe(app.pid, launch + 60)
        if probe_at is None:
            report.fail("no health probe (rocm child) completed within 60s")
        else:
            report.ok(f"first health probe finished {probe_at - launch:.2f}s after launch")

        remaining = args.settle - (time.monotonic() - launch)
        if remaining > 0:
            time.sleep(remaining)

        start = tree_sample(app.pid)
        if start is None:
            raise SystemExit(f"app exited before the window: {smoke.log_tail(root)}")
        samples = []
        window_start = time.monotonic()
        while time.monotonic() - window_start < args.duration:
            time.sleep(args.interval)
            sample = tree_sample(app.pid)
            if sample is None:
                report.fail(f"app died mid-window: {smoke.log_tail(root)}")
                break
            sample["atSeconds"] = round(time.monotonic() - window_start, 1)
            samples.append(sample)

        if samples:
            elapsed = samples[-1]["atSeconds"]
            cpu_seconds = (samples[-1]["cpuJiffies"] - start["cpuJiffies"]) / CLK_TCK
            cpu_pct = 100.0 * cpu_seconds / elapsed if elapsed else 0.0
            rss_values = [s["rssBytes"] for s in samples]
            peak_mib = max(rss_values) / (1 << 20)
            avg_mib = sum(rss_values) / len(rss_values) / (1 << 20)
            overlap = max(s["rocmChildren"] for s in samples)
            report.ok(f"window: {elapsed:.0f}s, {len(samples)} samples, "
                      f"{samples[-1]['processes']} process(es) in tree at end")
            report.ok(f"idle CPU: {cpu_pct:.3f}% average ({cpu_seconds:.2f} CPU-seconds)")
            report.ok(f"tree RSS: {avg_mib:.1f} MiB average, {peak_mib:.1f} MiB peak")
            report.ok(f"probe overlap: at most {overlap} concurrent rocm child(ren)")
            if cpu_pct >= args.budget_cpu:
                report.fail(f"idle CPU {cpu_pct:.3f}% breaches the {args.budget_cpu}% budget")
            if peak_mib >= args.budget_rss_mib:
                report.fail(f"peak RSS {peak_mib:.1f} MiB breaches the "
                            f"{args.budget_rss_mib:.0f} MiB budget")
            if overlap > 1:
                report.fail(f"{overlap} rocm probes ran concurrently; probes must not pile up")
            (args.out / "idle-report.json").write_text(json.dumps({
                "artifact": str(app_binary),
                "durationSeconds": elapsed,
                "trayRegisterSeconds": None if tray_at is None else round(tray_at - launch, 3),
                "firstProbeSeconds": None if probe_at is None else round(probe_at - launch, 3),
                "cpuAveragePercent": round(cpu_pct, 4),
                "cpuSeconds": round(cpu_seconds, 3),
                "rssAverageMiB": round(avg_mib, 1),
                "rssPeakMiB": round(peak_mib, 1),
                "maxConcurrentProbes": overlap,
                "samples": samples,
            }, indent=2) + "\n")
            report.ok(f"wrote {args.out / 'idle-report.json'}")
    finally:
        smoke.stop_process(app)
        smoke.stop_process(watcher)
        smoke.stop_process(xvfb)

    # The 10-minute run doubles as a long-duration isolation proof.
    verify = smoke.Report()
    smoke.verify(root / "iso", [], False, verify)
    for line in verify.lines:
        report.lines.append(line)
    report.failures.extend(verify.failures)
    shutil.rmtree(root, ignore_errors=True)

    code = 1 if report.failures else 0
    report.head(f"{len(report.failures)} failure(s)" if report.failures else "within budget")
    report.emit()
    return code


if __name__ == "__main__":
    raise SystemExit(main())
