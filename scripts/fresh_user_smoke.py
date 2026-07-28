#!/usr/bin/env python3
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
"""Prove an e2e run never reads or writes the real user's state.

Desktop test isolation is a pile of environment variables, and environment
variables fail silently. Miss `XDG_STATE_HOME` and the app writes to the
developer's real `~/.local/state`; the test still passes, the next test picks
up state it did not create, and the suite is green until it runs on a machine
that has never launched the app -- or until it eats somebody's real config.
Nothing in a passing test log distinguishes "isolated" from "happened to
work".

So this script does not assert that the variables are set. It plants a
tripwire file with a unique, greppable marker in each real location the app or
CLI would touch if isolation leaked, records the exact bytes and mtime of
each, runs the app under the isolated root, then proves afterwards that every
tripwire is byte-identical, that no real directory came into existence, and
that no marker token ever appeared inside the sandbox or the test artifacts.
A directory that was absent before and is absent after is the strongest
evidence available that nothing leaked.

It is also the single definition of the isolation environment: the
WebdriverIO harness and CI both read `--emit-env`/`--prepare` rather than
spelling the variables out again, so there is one place to add the next one.

The app is launched on a session bus of its own. Sharing the developer's is
what a fresh user never does, and doing it anyway -- a live desktop's bus
reached with `HOME` pointing at a scratch root -- segfaults the release
binary about 1.5s in.

Usage:
    python3 scripts/fresh_user_smoke.py                      # full smoke
    python3 scripts/fresh_user_smoke.py --real-gpu-read-only # real CLI + GPU, state untouched
    python3 scripts/fresh_user_smoke.py --emit-env /tmp/iso
    python3 scripts/fresh_user_smoke.py --prepare /tmp/iso
    python3 scripts/fresh_user_smoke.py --verify /tmp/iso --scan test-results/e2e
    python3 scripts/fresh_user_smoke.py --verify /tmp/iso --allow-unused
    python3 scripts/fresh_user_smoke.py --self-test
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

APP_ROOT = Path(__file__).resolve().parent.parent

# The bundle identifier from src-tauri/tauri.conf.json. Tauri derives
# `app_data_dir()` from it, so `~/.local/share/com.mikeroysoft.rocm-app` is
# the directory a leaking app actually creates -- the plain `rocm-app` name is
# watched too because the CLI and the packaging scripts use it.
APP_IDENTIFIER = "com.mikeroysoft.rocm-app"
STATE_DIR_NAMES = ("rocm", "rocm-app", APP_IDENTIFIER)

SENTINEL_NAME = "SUPERGOAL-E2E-SENTINEL.txt"
MARKER_PREFIX = "ROCM-E2E-SENTINEL-"
MARKER_RE = re.compile(MARKER_PREFIX + r"[0-9a-f]{16}")

# Where the manifest lives inside the prepared root. `--verify` reads it back,
# and the leak scan must skip it: it is the one file that legitimately holds
# every marker token.
MANIFEST_NAME = "isolation.json"

# Every variable that has to point inside the scratch root, and the
# subdirectory each one gets. `HOME`/`USERPROFILE` share a directory on
# purpose -- they are the same concept under two names, and a test that writes
# through one must see it through the other. Adding a variable here is the
# only edit needed to isolate it everywhere.
ENV_LAYOUT = {
    "HOME": "home",
    "USERPROFILE": "home",
    "XDG_CONFIG_HOME": "xdg-config",
    "XDG_DATA_HOME": "xdg-data",
    "XDG_CACHE_HOME": "xdg-cache",
    "XDG_STATE_HOME": "xdg-state",
    "ROCM_CLI_CONFIG_DIR": "cli-config",
    "ROCM_CLI_DATA_DIR": "cli-data",
    "ROCM_CLI_CACHE_DIR": "cli-cache",
    "APPDATA": "appdata",
    "LOCALAPPDATA": "localappdata",
}

# Read-and-search cap. Anything larger is a build output or a recording, not
# something a marker string plausibly hides in, and reading it would turn the
# scan into the slowest part of the suite.
MAX_SCAN_BYTES = 8 << 20
NUL_SNIFF_BYTES = 8192

DEFAULT_APP = APP_ROOT / "src-tauri" / "target" / "release" / "rocm-app"
DEFAULT_FIXTURE_CLI = APP_ROOT / "src-tauri" / "target" / "release" / "rocm-fixture-cli"


def default_real_cli() -> Path:
    """The real staged CLI `npm run stage` writes, whatever the host triple.

    `rocm-*` cannot match the daemon (`rocmd-*` has a `d` where the glob wants
    a `-`), so the first hit is the CLI itself.
    """
    binaries = APP_ROOT / "src-tauri" / "binaries"
    for path in sorted(binaries.glob("rocm-*")):
        if path.is_file():
            return path
    return binaries / "rocm-x86_64-unknown-linux-gnu"


class SmokeSkipped(Exception):
    """The smoke run cannot happen here. The message says exactly why.

    Distinct from a failure on purpose: no built binary and no display are
    facts about the host, and CI runs this in a job that may legitimately have
    neither.
    """


class HarnessError(RuntimeError):
    """The harness itself could not proceed (missing manifest, bad JSON)."""


@dataclass
class Report:
    """Accumulated findings. Checks record and continue rather than raising.

    One leak usually implies several -- a missed variable shows up in every
    directory that variable governs -- and a run that stops at the first one
    hides the pattern that names the cause.
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

    def emit(self, stream=sys.stdout) -> None:
        stream.write("\n".join(self.lines) + "\n")
        stream.flush()


# --------------------------------------------------------------------------
# The isolation environment
# --------------------------------------------------------------------------


def isolation_env(root: Path) -> dict[str, str]:
    """The full variable set, every value absolute and under `root`."""
    base = root.resolve()
    return {name: str(base / sub) for name, sub in ENV_LAYOUT.items()}


def env_dirs(root: Path) -> list[Path]:
    """The directories the variables name, deduplicated, in a stable order."""
    base = root.resolve()
    return [base / sub for sub in dict.fromkeys(ENV_LAYOUT.values())]


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def non_empty(path: Path) -> bool:
    if not path.is_dir():
        return False
    with os.scandir(path) as entries:
        return any(True for _ in entries)


def _tmp_base() -> str:
    return "/tmp" if Path("/tmp").is_dir() else tempfile.gettempdir()


# --------------------------------------------------------------------------
# Sentinels
# --------------------------------------------------------------------------


def real_user_dirs() -> list[Path]:
    """Every real location the app or CLI would touch if isolation leaked.

    Resolved from the *ambient* environment, which is the whole point: if the
    caller has already exported an isolated `XDG_CONFIG_HOME`, the sentinels
    would be planted inside the sandbox and prove nothing. `--prepare` is
    therefore documented as running outside isolation, and the prepared root
    is excluded below as a cheap guard against the nested case.
    """
    home = Path.home()
    bases = [
        Path(os.environ.get("XDG_CONFIG_HOME") or home / ".config"),
        Path(os.environ.get("XDG_DATA_HOME") or home / ".local" / "share"),
        Path(os.environ.get("XDG_CACHE_HOME") or home / ".cache"),
    ]
    bases += [Path(v) for v in (os.environ.get("APPDATA"), os.environ.get("LOCALAPPDATA")) if v]
    candidates = [base / name for base in bases for name in STATE_DIR_NAMES]
    return list(dict.fromkeys(candidates))


def existing_marker(path: Path) -> str:
    """The marker already in a sentinel, or "" if the file is not one of ours.

    A pre-existing file is never rewritten. A repeat run reuses its marker so
    two runs stay comparable, and a stranger's file that happens to share the
    name is hashed and watched but contributes no token to the leak scan.
    """
    try:
        text = path.read_text(encoding="utf-8", errors="ignore")
    except OSError:
        return ""
    found = MARKER_RE.search(text)
    return found.group(0) if found else ""


def sentinel_body(marker: str) -> str:
    return (
        f"{marker}\n"
        "Planted by scripts/fresh_user_smoke.py to prove the ROCm App e2e suite\n"
        "never touches real user state. Safe to delete.\n"
    )


def plant_sentinel(directory: Path) -> dict[str, object]:
    """One manifest entry for one real-state directory.

    An absent directory is left absent. Creating `~/.cache/rocm` just to hold
    a tripwire would manufacture the very state this script exists to prove
    nobody manufactured, and it would destroy the strongest signal available:
    a path that did not exist before and does not exist after.
    """
    path = directory / SENTINEL_NAME
    if not directory.is_dir():
        return {"path": str(path), "dir": str(directory), "absent": True}

    created = not path.exists()
    marker = "" if created else existing_marker(path)
    if created:
        marker = MARKER_PREFIX + secrets.token_hex(8)
        path.write_text(sentinel_body(marker), encoding="utf-8")
    stat = path.stat()
    return {
        "path": str(path),
        "dir": str(directory),
        "marker": marker,
        "sha256": sha256_file(path),
        "mtimeNs": stat.st_mtime_ns,
        "created": created,
    }


# --------------------------------------------------------------------------
# --prepare
# --------------------------------------------------------------------------


def prepare(root: Path, report: Report, user_dirs: list[Path] | None = None) -> dict[str, object]:
    """Build the pristine root, plant the tripwires, write the manifest."""
    base = root.resolve()
    for directory in env_dirs(base):
        directory.mkdir(parents=True, exist_ok=True)
    report.ok(f"created {len(env_dirs(base))} isolated directories under {base}")

    candidates = real_user_dirs() if user_dirs is None else [Path(p) for p in user_dirs]
    sentinels = []
    for directory in candidates:
        resolved = Path(os.path.abspath(directory))
        if resolved == base or base in resolved.parents:
            report.note(f"skipped sentinel inside the prepared root: {resolved}")
            continue
        sentinels.append(plant_sentinel(resolved))

    planted = [s for s in sentinels if not s.get("absent")]
    absent = [s for s in sentinels if s.get("absent")]
    report.ok(
        f"{len(planted)} sentinel(s) planted or reused, {len(absent)} real path(s) absent"
    )
    for entry in planted:
        if not entry["marker"]:
            report.note(f"pre-existing unmarked file left untouched: {entry['path']}")

    manifest = {"root": str(base), "env": isolation_env(base), "sentinels": sentinels}
    (base / MANIFEST_NAME).write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return manifest


def load_manifest(root: Path) -> dict[str, object]:
    path = root.resolve() / MANIFEST_NAME
    if not path.is_file():
        raise HarnessError(f"no prepare manifest at {path}; run --prepare {root} first")
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise HarnessError(f"{path} is not valid JSON: {error}") from error


# --------------------------------------------------------------------------
# --verify
# --------------------------------------------------------------------------


def check_sentinels(sentinels: list[dict], report: Report) -> None:
    """Every tripwire byte-identical, every absent path still absent."""
    intact = 0
    for entry in sentinels:
        path = Path(str(entry["path"]))
        if entry.get("absent"):
            check_absent(entry, report)
            continue
        if not path.exists():
            report.fail(f"sentinel deleted: {path}")
            continue
        digest = sha256_file(path)
        mtime = path.stat().st_mtime_ns
        if digest != entry["sha256"]:
            report.fail(
                f"sentinel modified: {path} sha256 {digest[:16]}… != recorded "
                f"{str(entry['sha256'])[:16]}…"
            )
            continue
        if mtime != entry["mtimeNs"]:
            report.fail(
                f"sentinel rewritten: {path} mtime_ns {mtime} != recorded {entry['mtimeNs']}"
            )
            continue
        intact += 1
    if intact:
        report.ok(f"{intact} sentinel(s) unchanged in real user state")


def check_absent(entry: dict, report: Report) -> None:
    directory = Path(str(entry["dir"]))
    if not directory.exists():
        report.ok(f"still absent: {directory}")
        return
    contents = sorted(p.name for p in directory.iterdir())[:5] if directory.is_dir() else []
    detail = f" containing {', '.join(contents)}" if contents else ""
    report.fail(f"real user state created: {directory}{detail}")


def scan_for_leaks(
    targets: list[Path], markers: list[str], skip: set[Path], report: Report
) -> None:
    """No marker token anywhere inside the sandbox or the test artifacts.

    A marker in an artifact means something read a real user file and copied
    its contents in -- the read half of a leak, which the byte-for-byte
    sentinel check cannot see because reading changes nothing.
    """
    if not markers:
        report.note("no marked sentinels to scan for")
        return
    scanned = skipped = 0
    for target in targets:
        if not target.exists():
            report.note(f"scan target does not exist: {target}")
            continue
        for path in walk_files(target):
            if path in skip:
                continue
            read, hits = scan_file(path, markers)
            scanned += read
            skipped += not read
            for marker in hits:
                report.fail(f"marker leaked: {marker} appears in {path}")
    report.ok(
        f"scanned {scanned} text file(s) for {len(markers)} marker(s), "
        f"{skipped} binary/oversized file(s) skipped"
    )


def walk_files(target: Path):
    if target.is_file():
        yield target.resolve()
        return
    for parent, _dirs, names in os.walk(target):
        for name in names:
            path = Path(parent) / name
            if not path.is_symlink() and path.is_file():
                yield path.resolve()


def scan_file(path: Path, markers: list[str]) -> tuple[bool, list[str]]:
    """(was it searched, which markers it holds). Binaries are not searched."""
    try:
        if path.stat().st_size > MAX_SCAN_BYTES:
            return False, []
        with path.open("rb") as handle:
            head = handle.read(NUL_SNIFF_BYTES)
            if b"\0" in head:
                return False, []
            body = head + handle.read()
    except OSError:
        return False, []
    text = body.decode("utf-8", errors="ignore")
    if MARKER_PREFIX not in text:
        return True, []
    return True, [marker for marker in markers if marker in text]


def check_usage(root: Path, allow_unused: bool, report: Report) -> None:
    """The isolated roots must have actually been written to.

    Without this, a run in which the app never started looks exactly like a
    perfectly isolated one: no sentinel touched, nothing leaked, nothing
    proved. `--allow-unused` is for callers that only want the leak half --
    a bare `--prepare`/`--verify` pair with no app in between.
    """
    used = [d for d in env_dirs(root) if non_empty(d)]
    if used:
        report.ok(f"isolated roots used: {', '.join(d.name for d in used)}")
    elif allow_unused:
        report.note("isolated roots unused (--allow-unused): nothing ran under them")
    else:
        report.fail(
            f"isolated roots unused: no directory under {root.resolve()} received "
            "anything, so this run proves nothing about isolation"
        )


def verify(root: Path, scans: list[Path], allow_unused: bool, report: Report) -> None:
    base = root.resolve()
    manifest = load_manifest(base)
    sentinels = list(manifest.get("sentinels", []))
    report.head(f"verify {base}")
    check_sentinels(sentinels, report)
    markers = [str(s["marker"]) for s in sentinels if s.get("marker")]
    scan_for_leaks(
        [base, *(Path(s) for s in scans)],
        markers,
        {(base / MANIFEST_NAME).resolve()},
        report,
    )
    check_usage(base, allow_unused, report)


# --------------------------------------------------------------------------
# --real-gpu-read-only: the real CLI, the real GPU, and proof that the real
# runtime registry/config/cache never changed.
# --------------------------------------------------------------------------

# Files at or under this size are hashed; larger ones (runtime archives in the
# cache) are compared by size and mtime alone, which any write disturbs.
HASH_CAP_BYTES = 4 << 20


def real_rocm_watch_targets() -> list[Path]:
    """The real registry/config/cache locations, mirrored from rocm-core.

    `AppPaths::discover` (rocm-cli crates/rocm-core/src/lib.rs) resolves
    config/data/cache from the `ROCM_CLI_*` overrides, then `~/.rocm` (cache:
    `~/.rocm/cache`, runtime.rs `default_*_dir`), then redirects data into
    `setup.therock_venv` from config.json with cache at `<venv>/cache`. Both
    the default and the redirected data roots are watched, because the
    registry has lived under the default root across that redirect. Logs and
    engine scratch are deliberately not watched: they are volatile, and they
    are not what "read-only" promises to preserve.
    """
    home = Path.home()
    config_dir = Path(os.environ.get("ROCM_CLI_CONFIG_DIR") or home / ".rocm")
    data_dirs = [Path(os.environ.get("ROCM_CLI_DATA_DIR") or home / ".rocm")]
    cache_dirs = [Path(os.environ.get("ROCM_CLI_CACHE_DIR") or home / ".rocm" / "cache")]
    if not os.environ.get("ROCM_CLI_DATA_DIR"):
        try:
            raw = json.loads((config_dir / "config.json").read_text(encoding="utf-8"))
            venv = str((raw.get("setup") or {}).get("therock_venv") or "").strip()
        except (OSError, ValueError):
            venv = ""
        if venv:
            data_dirs.append(Path(venv))
            if not os.environ.get("ROCM_CLI_CACHE_DIR"):
                cache_dirs.append(Path(venv) / "cache")
    targets = [config_dir / "config.json"]
    for base in data_dirs:
        targets += [base / "runtimes" / "active.json", base / "runtimes" / "registry"]
    targets += cache_dirs
    return list(dict.fromkeys(targets))


def _file_signature(path: Path) -> str:
    try:
        stat = path.lstat()
        signature = f"{stat.st_size}:{stat.st_mtime_ns}"
        if stat.st_size <= HASH_CAP_BYTES and path.is_file():
            signature += ":" + sha256_file(path)
        return signature
    except OSError as error:
        return f"unreadable:{type(error).__name__}"


def state_inventory(targets: list[Path]) -> dict[str, str]:
    """Every watched path's identity, absent paths included.

    An absent target is recorded as such -- a path that did not exist before
    the run must still not exist after it, and a directory that appears reads
    as `changed` rather than vanishing from the comparison.
    """
    entries: dict[str, str] = {}
    for target in targets:
        if not target.exists():
            entries[str(target)] = "absent"
            continue
        if target.is_dir():
            entries[str(target)] = "present"
            for parent, _dirs, names in os.walk(target):
                for name in names:
                    path = Path(parent) / name
                    entries[str(path)] = _file_signature(path)
        else:
            entries[str(target)] = _file_signature(target)
    return entries


def check_state_unchanged(before: dict[str, str], after: dict[str, str],
                          report: Report) -> None:
    """--real-gpu-read-only's core promise: nothing real changed."""
    problems = (
        [f"changed: {p}" for p in sorted(before.keys() & after.keys()) if before[p] != after[p]]
        + [f"created: {p}" for p in sorted(after.keys() - before.keys())]
        + [f"removed: {p}" for p in sorted(before.keys() - after.keys())]
    )
    if problems:
        shown = "; ".join(problems[:10])
        if len(problems) > 10:
            shown += f" … +{len(problems) - 10} more"
        report.fail(f"real runtime state changed during the run — {shown}")
    else:
        files = sum(1 for v in before.values() if v not in ("absent", "present"))
        report.ok(
            f"real registry/config/cache unchanged: {files} file(s) re-checked "
            "byte- or stat-identical"
        )


def check_real_gpu(cli: Path, env: dict[str, str], root: Path, report: Report) -> None:
    """The staged real CLI must see real AMD hardware from inside isolation.

    This is the half that makes `--real-gpu-read-only` mean what it says: the
    snapshot the app consumes names a physical GPU, and it is produced with
    every ROCm root pointed at the sandbox -- so the pre/post inventory also
    polices this very probe.
    """
    try:
        result = subprocess.run(
            [str(cli), "app-snapshot"], env=env, cwd=str(root),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=120,
        )
    except (OSError, subprocess.SubprocessError) as error:
        report.fail(f"could not run the staged rocm app-snapshot: {error}")
        return
    if result.returncode != 0:
        tail = " | ".join(result.stderr.strip().splitlines()[-3:])
        report.fail(f"rocm app-snapshot exited {result.returncode}: {tail or '(no stderr)'}")
        return
    try:
        snapshot = json.loads(result.stdout)
    except ValueError:
        report.fail("rocm app-snapshot printed no parseable JSON")
        return
    gpu = snapshot.get("gpu") or {}
    name = gpu.get("name") or ""
    target = gpu.get("gfxTarget") or ""
    if not (name or target):
        report.fail("app-snapshot names no GPU; --real-gpu-read-only requires real AMD hardware")
        return
    report.ok(f"real GPU visible under isolation: {name or 'unnamed'} ({target or 'no gfx target'})")


# --------------------------------------------------------------------------
# Display
# --------------------------------------------------------------------------


def free_display(start: int = 99) -> int:
    for number in range(start, start + 32):
        if not Path(f"/tmp/.X11-unix/X{number}").exists() and not Path(f"/tmp/.X{number}-lock").exists():
            return number
    raise SmokeSkipped("no free X display number")


def start_xvfb(number: int, report: Report) -> subprocess.Popen:
    binary = shutil.which("Xvfb")
    proc = subprocess.Popen(
        [binary, f":{number}", "-screen", "0", "1280x900x24", "-nolisten", "tcp"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    socket = Path(f"/tmp/.X11-unix/X{number}")
    lock = Path(f"/tmp/.X{number}-lock")
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline:
        if socket.exists() or lock.exists():
            report.ok(f"display: started Xvfb :{number}")
            return proc
        if proc.poll() is not None:
            raise SmokeSkipped(f"Xvfb :{number} exited {proc.returncode}")
        time.sleep(0.1)
    stop_process(proc)
    raise SmokeSkipped(f"Xvfb :{number} never came up")


def ensure_display(env: dict[str, str], xvfb: str, report: Report) -> subprocess.Popen | None:
    """Give `env` a usable display, returning the Xvfb we own (if any)."""
    inherited = env.get("DISPLAY") or env.get("WAYLAND_DISPLAY")
    if inherited:
        report.ok(f"display: inherited {inherited}")
        return None
    if sys.platform == "win32":
        return None
    if xvfb == "never" or not shutil.which("Xvfb"):
        raise SmokeSkipped("no display")
    number = int(xvfb[1:]) if xvfb.startswith(":") else free_display()
    proc = start_xvfb(number, report)
    env["DISPLAY"] = f":{number}"
    env.pop("WAYLAND_DISPLAY", None)
    # webkitgtk negotiates dma-buf with the compositor and hard-fails against
    # a bare Xvfb, taking the whole webview down before the app writes
    # anything. Only forced for the server we started; a real session is left
    # to its own rendering path.
    env["WEBKIT_DISABLE_DMABUF_RENDERER"] = "1"
    env["WEBKIT_DISABLE_COMPOSITING_MODE"] = "1"
    return proc


# --------------------------------------------------------------------------
# The app under isolation
# --------------------------------------------------------------------------


def stop_process(proc: subprocess.Popen | None) -> None:
    """SIGTERM the process group, SIGKILL it five seconds later."""
    if proc is None or proc.poll() is not None:
        return
    try:
        if os.name == "posix":
            os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
        else:
            proc.terminate()
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            if os.name == "posix":
                os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
            else:
                proc.kill()
        except OSError:
            pass
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
    except (OSError, ProcessLookupError):
        pass


def stage_binaries(root: Path, app: Path, cli: Path, env: dict[str, str],
                   report: Report, fixture: bool = True) -> Path:
    """Copy the app -- and the CLI it must find -- into the sandbox.

    The app resolves its CLI as a sibling of its own executable, so running it
    from `target/release` would hand it the real staged `rocm`. Copying both
    into `<root>/bin` is what makes the sibling lookup land on the one under
    test -- the scripted fixture normally, the real staged CLI under
    `--real-gpu-read-only`.
    """
    if not app.is_file():
        raise SmokeSkipped(f"no built app binary at {app}")
    bin_dir = root / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    staged = bin_dir / app.name
    shutil.copy2(app, staged)
    staged.chmod(0o755)

    if cli.is_file():
        sibling = bin_dir / ("rocm.exe" if os.name == "nt" else "rocm")
        shutil.copy2(cli, sibling)
        sibling.chmod(0o755)
        if fixture:
            fixture_dir = root / "fixture"
            fixture_dir.mkdir(parents=True, exist_ok=True)
            env["ROCM_FIXTURE_DIR"] = str(fixture_dir)
            env["ROCM_FIXTURE_JOURNAL"] = str(root / "fixture-journal.jsonl")
            report.ok(f"staged fixture CLI as {sibling}")
        else:
            report.ok(f"staged real CLI as {sibling}")
    else:
        report.note(f"no fixture CLI at {cli}; app will find no sibling rocm")
    return staged


def app_state_dir(root: Path) -> Path:
    """Where Tauri puts this app's own data under the isolated root.

    Watching *this* rather than "any isolated directory got a byte" is the
    difference between proving the app ran and proving GTK did: webkit and
    mesa drop caches into `XDG_CACHE_HOME` within milliseconds of exec, long
    before the app reaches its own setup.
    """
    env = isolation_env(root)
    base = env["APPDATA"] if os.name == "nt" else env["XDG_DATA_HOME"]
    return Path(base) / APP_IDENTIFIER


def log_tail(root: Path, lines: int = 5) -> str:
    try:
        text = (root / "app.log").read_text(errors="ignore")
    except OSError:
        return "(no output captured)"
    return " | ".join(text.strip().splitlines()[-lines:]) or "(no output)"


def report_exit(proc: subprocess.Popen, root: Path, report: Report) -> None:
    """Classify a launch that ended on its own.

    A crash is never a pass. The first version of this harness returned as
    soon as a directory went non-empty and reported success for a build that
    segfaulted a second later -- the state was real, the app was gone, and
    the isolation verdict below it was worthless.
    """
    code = proc.returncode
    if code == 0:
        report.note(f"app exited cleanly before the settle window: {log_tail(root)}")
        return
    report.fail(f"app died under isolation ({describe_exit(code)}): {log_tail(root)}")


def signal_name(number: int) -> str:
    try:
        return signal.Signals(number).name
    except ValueError:
        return f"signal {number}"


def describe_exit(code: int) -> str:
    """Name the signal however the status reaches us.

    A direct child killed by a signal reports a negative returncode, but the
    launch goes through `dbus-run-session`, which converts the same death
    into a shell-style 128+N exit status. Reporting that as a bare
    "exit 139" would throw away the one detail that makes the failure
    diagnosable.
    """
    if code < 0:
        return f"killed by {signal_name(-code)}"
    if code > 128:
        return f"exit {code}, killed by {signal_name(code - 128)}"
    return f"exit {code}"


# How long the app must stay up after creating its own data directory. The
# crash this catches happens ~1.5s in, once the webview finishes loading.
SETTLE_SECONDS = 3.0


def private_bus_prefix(env: dict[str, str], report: Report) -> list[str]:
    """A session bus of our own. A fresh user does not share the dev's.

    Inheriting a live desktop's bus while `HOME` points at a scratch root is
    a configuration no real user is ever in: the app reaches that session's
    portal, keyring and accessibility services with none of the state they
    expect, and the release binary segfaults about 1.5s in. Measured on this
    workstation -- same binary, same isolated root -- that happens on the
    developer's live `:0` and under Xvfb alike, while a private bus runs 25s+
    clean with or without a StatusNotifierWatcher. So the bus is the
    variable, a missing tray host is not, and this is unconditional: the
    inherited bus is exactly the thing a fresh-user run must not keep.
    """
    env.pop("DBUS_SESSION_BUS_ADDRESS", None)
    launcher = shutil.which("dbus-run-session")
    if not launcher:
        report.note("no dbus-run-session on PATH; launching without a private session bus")
        return []
    report.ok("session bus: private (dbus-run-session)")
    return [launcher, "--"]


def run_app(staged: Path, root: Path, env: dict[str, str], timeout: float,
            report: Report, prefix: list[str]) -> subprocess.Popen:
    """Launch hidden, wait for the app's own state, require it to survive."""
    with (root / "app.log").open("wb") as log:
        proc = subprocess.Popen(
            [*prefix, str(staged), "--hidden"],
            cwd=str(root),
            env=env,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )

    state = app_state_dir(root)
    deadline = time.monotonic() + timeout
    settle = deadline
    wrote = False
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            report_exit(proc, root, report)
            return proc
        if not wrote and non_empty(state):
            wrote = True
            settle = min(time.monotonic() + SETTLE_SECONDS, deadline)
            report.ok(f"app created {state.name} under the isolated root")
        if wrote and time.monotonic() >= settle:
            report.ok(f"app still running {SETTLE_SECONDS:.0f}s after writing its state")
            return proc
        time.sleep(0.25)
    if not wrote:
        report.fail(
            f"app wrote no state to {state} within {timeout:.0f}s: {log_tail(root)}"
        )
    return proc


def smoke(args: argparse.Namespace) -> int:
    """--prepare, launch under isolation, --verify, tear everything down."""
    report = Report()
    real = args.real_gpu_read_only
    report.head("fresh-user smoke (real GPU, read-only)" if real else "fresh-user smoke")
    root = Path(tempfile.mkdtemp(prefix="rocm-fresh-user-", dir=_tmp_base()))
    app_proc: subprocess.Popen | None = None
    xvfb_proc: subprocess.Popen | None = None
    skipped = ""
    try:
        state_before: dict[str, str] = {}
        cli = args.fixture_cli
        if real:
            cli = default_real_cli()
            if not cli.is_file():
                raise HarnessError(
                    f"--real-gpu-read-only needs the staged CLI at {cli}; "
                    "run `npm run stage` first"
                )
            targets = real_rocm_watch_targets()
            state_before = state_inventory(targets)
            recorded = sum(1 for v in state_before.values() if v not in ("absent", "present"))
            report.ok(
                f"recorded real registry/config/cache: {recorded} file(s) "
                f"under {len(targets)} watched path(s)"
            )
        prepare(root, report)
        env = dict(os.environ)
        env.update(isolation_env(root))
        staged = stage_binaries(root, args.app, cli, env, report, fixture=not real)
        xvfb_proc = ensure_display(env, args.xvfb, report)
        prefix = private_bus_prefix(env, report)
        app_proc = run_app(staged, root, env, args.timeout, report, prefix)
        stop_process(app_proc)
        app_proc = None
        if real:
            check_real_gpu(root / "bin" / ("rocm.exe" if os.name == "nt" else "rocm"),
                           env, root, report)
        verify(root, args.scan, args.allow_unused, report)
        if real:
            check_state_unchanged(state_before, state_inventory(real_rocm_watch_targets()), report)
    except SmokeSkipped as reason:
        skipped = str(reason)
    except (HarnessError, OSError) as error:
        report.fail(f"harness error: {error}")
    finally:
        # Every process this function started dies here, on every path. A
        # leaked Xvfb outlives the shell that ran the suite and quietly holds
        # the next run's display number.
        stop_process(app_proc)
        stop_process(xvfb_proc)
        if args.keep:
            report.note(f"kept scratch root {root}")
        else:
            shutil.rmtree(root, ignore_errors=True)

    if skipped:
        report.note(f"skipped: {skipped}")
    else:
        report.head(f"{len(report.failures)} failure(s)" if report.failures else "isolation held")
    report.emit()
    return 0 if skipped else (1 if report.failures else 0)


# --------------------------------------------------------------------------
# Self-test
#
# The checks above are worth their runtime only if they fail when they should.
# Each case below prepares a fresh root against a *synthetic* real-user tree,
# breaks exactly one thing, and requires the report to name that one thing --
# a check that quietly stopped looking would otherwise pass every case.
# --------------------------------------------------------------------------


def synthetic_scenario(tmp: Path, label: str) -> tuple[Path, list[Path], Path]:
    """A sandbox root, a fake real-user tree, and an artifact directory.

    The fake user tree sits outside the root on purpose: sentinels planted
    inside the scanned root would be found by the leak scan itself.
    """
    case = tmp / label
    root = case / "iso"
    user = case / "user"
    artifacts = case / "artifacts"
    artifacts.mkdir(parents=True)
    present = [user / ".config" / "rocm", user / ".local" / "share" / "rocm"]
    for directory in present:
        directory.mkdir(parents=True)
    absent = [user / ".cache" / "rocm", user / ".config" / "rocm-app"]
    return root, present + absent, artifacts


def simulate_app_write(root: Path) -> None:
    target = root / "xdg-data" / APP_IDENTIFIER
    target.mkdir(parents=True, exist_ok=True)
    (target / "state.json").write_text('{"windows":{}}\n', encoding="utf-8")


def self_test() -> int:
    report = Report()
    report.head("self-test")
    failures: list[str] = []

    def expect(label: str, root: Path, scans: list[Path], should_pass: bool,
               contains: str = "", allow_unused: bool = False) -> None:
        result = Report()
        try:
            verify(root, scans, allow_unused, result)
        except HarnessError as error:
            result.fail(str(error))
        passed = not result.failures
        detail = "; ".join(result.failures)
        if passed != should_pass:
            failures.append(label)
            report.fail(f"{label}: expected {'pass' if should_pass else 'failure'} — {detail}")
        elif contains and contains not in detail:
            failures.append(label)
            report.fail(f"{label}: failed for the wrong reason — {detail}")
        else:
            report.ok(f"{label}: {'passed' if passed else 'failed as expected'}")

    def prepared(tmp: Path, label: str) -> tuple[Path, list[dict], Path]:
        root, user_dirs, artifacts = synthetic_scenario(tmp, label)
        manifest = prepare(root, Report(), user_dirs)
        simulate_app_write(root)
        return root, list(manifest["sentinels"]), artifacts

    with tempfile.TemporaryDirectory(prefix="rocm-fresh-user-selftest-", dir=_tmp_base()) as name:
        tmp = Path(name)

        root, _sentinels, artifacts = prepared(tmp, "clean")
        expect("clean run", root, [artifacts], True)

        # Content changed, mtime restored: this is the case a mtime-only check
        # sails straight past, and it is the realistic one -- a restore from
        # backup or an editor that preserves timestamps.
        root, sentinels, artifacts = prepared(tmp, "modified")
        entry = next(s for s in sentinels if not s.get("absent"))
        victim = Path(str(entry["path"]))
        victim.write_text("tampered\n", encoding="utf-8")
        os.utime(victim, ns=(int(entry["mtimeNs"]), int(entry["mtimeNs"])))
        expect("sentinel contents modified", root, [artifacts], False,
               f"sentinel modified: {victim}")

        # The mirror image: identical bytes, new mtime. Something rewrote the
        # file with what it already held, which is still a write to real state.
        root, sentinels, artifacts = prepared(tmp, "rewritten")
        entry = next(s for s in sentinels if not s.get("absent"))
        victim = Path(str(entry["path"]))
        bumped = int(entry["mtimeNs"]) + 1_000_000_000
        os.utime(victim, ns=(bumped, bumped))
        expect("sentinel rewritten with identical bytes", root, [artifacts], False,
               f"sentinel rewritten: {victim}")

        root, sentinels, artifacts = prepared(tmp, "created")
        gone = Path(str(next(s for s in sentinels if s.get("absent"))["dir"]))
        gone.mkdir(parents=True)
        (gone / "config.json").write_text("{}\n", encoding="utf-8")
        expect("absent real path created during the run", root, [artifacts], False, str(gone))

        root, sentinels, artifacts = prepared(tmp, "leaked")
        marker = next(str(s["marker"]) for s in sentinels if s.get("marker"))
        leak = artifacts / "spec-console.log"
        leak.write_text(f"read user file containing {marker}\n", encoding="utf-8")
        expect("marker leaked into a scanned artifact", root, [artifacts], False, str(leak))

        root, sentinels, artifacts = prepared(tmp, "binary")
        marker = next(str(s["marker"]) for s in sentinels if s.get("marker"))
        (artifacts / "screenshot.png").write_bytes(
            b"\x89PNG\r\n\x1a\n\x00\x00" + marker.encode() + b"\x00" * 32
        )
        expect("marker inside a binary ignored by the NUL sniff", root, [artifacts], True)

        root, user_dirs, artifacts = synthetic_scenario(tmp, "unused")
        prepare(root, Report(), user_dirs)
        expect("isolated roots never written to", root, [artifacts], False, "roots unused")
        expect("same root with --allow-unused", root, [artifacts], True, allow_unused=True)

        expect("verify without a prepare manifest", tmp / "never-prepared", [], False,
               "no prepare manifest")

        report_crash_case(report, failures)
        report_env_case(tmp / "envcase", report, failures)
        report_state_case(report, failures)

    if failures:
        report.fail(f"{len(failures)} self-test expectation(s) did not hold")
    else:
        report.ok("every self-test expectation held; temp root removed")
    report.emit()
    return 1 if failures else 0


# Restated literally rather than derived from ENV_LAYOUT, which is the only
# way this assertion can fail: this is the set the WebdriverIO harness and CI
# were written against, so a variable quietly dropped from the layout has to
# break here instead of silently weakening every test that trusts it.
REQUIRED_ENV_KEYS = (
    "ROCM_CLI_CONFIG_DIR", "ROCM_CLI_DATA_DIR", "ROCM_CLI_CACHE_DIR",
    "HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME",
    "USERPROFILE", "APPDATA", "LOCALAPPDATA",
)


def report_state_case(report: Report, failures: list[str]) -> None:
    """The read-only inventory must name a changed and a created file.

    Run against a synthetic registry, never the real one: the self-test has
    to tamper with what it watches, and the point of the real mode is that
    nothing ever tampers with the real one.
    """
    with tempfile.TemporaryDirectory(prefix="rocm-state-case-", dir=_tmp_base()) as name:
        registry = Path(name) / "runtimes" / "registry"
        registry.mkdir(parents=True)
        record = registry / "runtime.json"
        record.write_text("{}\n", encoding="utf-8")
        absent = Path(name) / "cache"
        before = state_inventory([registry, absent])
        record.write_text('{"tampered": true}\n', encoding="utf-8")
        (registry / "created.json").write_text("{}\n", encoding="utf-8")
        result = Report()
        check_state_unchanged(before, state_inventory([registry, absent]), result)
        detail = "; ".join(result.failures)
        if result.failures and str(record) in detail and "created.json" in detail:
            report.ok("state inventory: changed and created files both named")
        else:
            failures.append("state inventory")
            report.fail(
                f"state inventory: expected the changed and created files to be "
                f"named — {detail or 'no failure recorded'}"
            )


def report_crash_case(report: Report, failures: list[str]) -> None:
    """A crash must be named by signal whichever way the status reaches us.

    Needs no app and no display, and guards the single most useful line this
    script prints: "exit 139" sends a reader hunting, "killed by SIGSEGV"
    does not.
    """
    wanted = {-11: "SIGSEGV", 139: "SIGSEGV", -6: "SIGABRT", 134: "SIGABRT", 3: "exit 3"}
    wrong = {code: describe_exit(code) for code, want in wanted.items()
             if want not in describe_exit(code)}
    if wrong:
        failures.append("crash naming")
        report.fail(f"crash naming lost the signal: {wrong}")
    else:
        report.ok("crash naming: signal named from both -11 and 139 forms")


def report_env_case(root: Path, report: Report, failures: list[str]) -> None:
    """--emit-env must name every variable, all of them under the root."""
    env = isolation_env(root)
    missing = sorted(set(REQUIRED_ENV_KEYS) - set(env))
    stray = sorted(k for k, v in env.items() if not v.startswith(str(root.resolve()) + os.sep))
    relative = sorted(k for k, v in env.items() if not os.path.isabs(v))
    problems = [
        f"missing keys {missing}" if missing else "",
        f"values outside the root {stray}" if stray else "",
        f"relative values {relative}" if relative else "",
    ]
    detail = "; ".join(p for p in problems if p)
    if detail:
        failures.append("emit-env")
        report.fail(f"emit-env covers the isolation set: {detail}")
    else:
        report.ok(f"emit-env covers the isolation set: {len(env)} keys, all absolute under root")


# --------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--prepare", type=Path, metavar="ROOT",
                      help="build a pristine isolated root, plant sentinels, print the manifest")
    mode.add_argument("--verify", type=Path, metavar="ROOT",
                      help="check a prepared root's manifest: sentinels, absences, leaks, usage")
    mode.add_argument("--emit-env", type=Path, metavar="ROOT",
                      help="print just the isolation environment for ROOT as JSON")
    mode.add_argument("--self-test", action="store_true",
                      help="exercise these checks against synthetic trees; no app, no display")
    parser.add_argument("--scan", type=Path, action="append", default=[], metavar="DIR",
                        help="also scan DIR for leaked markers; repeatable")
    parser.add_argument("--allow-unused", action="store_true",
                        help="do not fail --verify when nothing was written under the root")
    parser.add_argument("--app", type=Path, default=DEFAULT_APP, metavar="PATH",
                        help=f"app binary to launch (default {DEFAULT_APP})")
    parser.add_argument("--fixture-cli", type=Path, default=DEFAULT_FIXTURE_CLI, metavar="PATH",
                        help=f"fixture CLI staged as the sibling rocm (default {DEFAULT_FIXTURE_CLI})")
    parser.add_argument("--real-gpu-read-only", action="store_true",
                        help="stage the real staged rocm CLI instead of the fixture, require a "
                             "real AMD GPU in its snapshot, and prove the real runtime "
                             "registry/config/cache were untouched")
    parser.add_argument("--timeout", type=float, default=45.0, metavar="SECONDS",
                        help="how long the app gets to write isolated state (default 45)")
    parser.add_argument("--xvfb", default="auto", metavar="auto|never|:N",
                        help="headless display policy when no DISPLAY/WAYLAND_DISPLAY is set")
    parser.add_argument("--keep", action="store_true",
                        help="keep the smoke run's scratch root instead of removing it")
    args = parser.parse_args()

    try:
        if args.self_test:
            return self_test()
        if args.emit_env is not None:
            json.dump(isolation_env(args.emit_env), sys.stdout, indent=2, sort_keys=True)
            sys.stdout.write("\n")
            return 0
        if args.prepare is not None:
            report = Report()
            report.head(f"prepare {args.prepare.resolve()}")
            manifest = prepare(args.prepare, report)
            report.emit(sys.stderr)
            json.dump(manifest, sys.stdout, indent=2)
            sys.stdout.write("\n")
            return 0
        if args.verify is not None:
            report = Report()
            verify(args.verify, args.scan, args.allow_unused, report)
            report.head(
                f"{len(report.failures)} violation(s)" if report.failures else "isolation held"
            )
            report.emit()
            return 1 if report.failures else 0
        return smoke(args)
    except (HarnessError, OSError) as error:
        sys.stderr.write(f"fresh_user_smoke: {error}\n")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
