#!/usr/bin/env python3
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
"""Prove close-to-tray on a real Wayland compositor, not on X11 through XWayland.

Closing to the tray is two separate promises, and on Wayland each one is
kept by a different layer than on X11. The window manager owns the close
gesture and the process has to survive it. An X11 run says nothing about
either: under XWayland the app talks a protocol the user's session does not
use, and the defect this file exists to catch -- a `close` request the client
acknowledges and then ignores, leaving a window that will not go away -- is
invisible there. This lane was written after that defect sat open for weeks purely
because no Wayland compositor was available to reproduce it on.

Observation is the Wayland wire log (`WAYLAND_DEBUG=1`), which records the
exact facts in question: whether the compositor sent `xdg_toplevel.close`, and
whether the client then destroyed the toplevel. Pixels would prove less, and
GNOME 50 denies `Shell.Introspect.GetWindows` and `Shell.Screenshot` to
non-shell callers anyway. Input goes in over `org.gnome.Mutter.RemoteDesktop`
on one held D-Bus connection -- its session objects die with the connection
that created them -- and the tray is driven over its own `dbusmenu`
interface, so no pointer coordinates are involved.

Three named checks, all against the shipped release binary:

    tray-registers  the app registers a StatusNotifierItem whose menu carries
                    the open/more info/quit entries the tray contract promises
    close-to-tray   with the main window focused, Alt+F4 makes the compositor
                    send xdg_toplevel.close, the client destroys that
                    toplevel, and the process is still alive afterwards

Usage:
    python3 scripts/wayland_desktop_check.py
    python3 scripts/wayland_desktop_check.py --compositor nested
    python3 scripts/wayland_desktop_check.py --compositor current
    python3 scripts/wayland_desktop_check.py --self-test
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

APP_ROOT = Path(__file__).resolve().parent.parent
WATCHER_SCRIPT = Path(__file__).resolve().parent / "statusnotifierwatcher.py"

DEFAULT_APP = APP_ROOT / "src-tauri" / "target" / "release" / "rocm-app"
DEFAULT_CLI = APP_ROOT / "src-tauri" / "binaries" / "rocm-x86_64-unknown-linux-gnu"

CHECKS = ("tray-registers", "close-to-tray")

# The main window's exact `set_title` string. The quick window sets a
# different one, which is how the wire log tells the two apart.
MAIN_TITLE = "ROCm"

# Every tray menu entry the app promises, as a case-insensitive fragment. A
# build that renamed one out of existence still registers a tray icon and
# still passes every other check here.
MENU_WANTED = ("open", "more info", "quit")

# evdev keycodes, which is what RemoteDesktop's NotifyKeyboardKeycode takes.
KEY_ESC = 1
KEY_LEFTALT = 56
KEY_F4 = 62

RD = "org.gnome.Mutter.RemoteDesktop"
RD_SESSION = f"{RD}.Session"
SNI = "org.kde.StatusNotifierItem"
DBUSMENU = "com.canonical.dbusmenu"

# What the app's own directories are moved to, relative to the scratch root.
# Nothing here may resolve outside it: a run that wrote to the developer's
# real config is a broken test, not a passing one.
ISOLATED_DIRS = {
    "HOME": "home",
    "XDG_CONFIG_HOME": "home/.config",
    "XDG_DATA_HOME": "home/.local/share",
    "XDG_CACHE_HOME": "home/.cache",
    "XDG_STATE_HOME": "home/.local/state",
    "ROCM_CLI_CONFIG_DIR": "cli/config",
    "ROCM_CLI_DATA_DIR": "cli/data",
    "ROCM_CLI_CACHE_DIR": "cli/cache",
}

# Inherited session variables a nested compositor must not see. With any of
# them set, mutter picks display-server mode against the outer session and
# fails logind's TakeControl instead of starting headless.
NESTED_UNSET = ("WAYLAND_DISPLAY", "DISPLAY", "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP")

NESTED_MONITOR = "1280x1000"

# How long each stage gets. The app's first toplevel waits on a webview load,
# and the settle times are what a click needs before the wire log is read:
# too short and a passing build looks like a silent client.
COMPOSITOR_TIMEOUT = 40.0
TOPLEVEL_TIMEOUT = 45.0
TRAY_TIMEOUT = 45.0
CLICK_SETTLE = 3.0
CLOSE_SETTLE = 4.0

TITLE_RE = re.compile(r'xdg_toplevel#(\d+)\.set_title\("([^"]*)"\)')
KEYBOARD_ENTER_RE = re.compile(r"wl_keyboard#\d+\.enter\(")
KEYBOARD_LEAVE_RE = re.compile(r"wl_keyboard#\d+\.leave\(")


class CheckSkipped(Exception):
    """This host cannot run the lane. The message says exactly why.

    Distinct from a failure on purpose, and never silent: no compositor and no
    built binary are facts about the host, but a lane that quietly reported
    success on a machine where it never ran would be worse than one that
    fails.
    """


@dataclass
class Report:
    """Accumulated findings, one line each, plus the per-check verdict.

    Checks record and continue rather than raising: a tray that came up with
    the wrong menu can still be driven, and stopping at the first failure
    would hide whether close-to-tray works at all.
    """

    lines: list[str] = field(default_factory=list)
    failures: list[str] = field(default_factory=list)
    checks: dict[str, bool] = field(default_factory=dict)

    def head(self, text: str) -> None:
        self.lines.append(text)

    def ok(self, text: str) -> None:
        self.lines.append(f"  ok    {text}")

    def note(self, text: str) -> None:
        self.lines.append(f"  note  {text}")

    def fail(self, text: str) -> None:
        self.lines.append(f"  FAIL  {text}")
        self.failures.append(text)

    def check(self, name: str, passed: bool, detail: str) -> bool:
        """Record one named check. Returns `passed`, so callers can branch."""
        self.checks[name] = passed
        (self.ok if passed else self.fail)(f"{name:<14} {detail}")
        return passed

    def unreached(self, reason: str) -> None:
        """Fail every check that never got to run, naming what stopped it."""
        for name in CHECKS:
            if name not in self.checks:
                self.check(name, False, f"not reached: {reason}")

    def text(self) -> str:
        return "\n".join(self.lines) + "\n"

    def emit(self, stream=sys.stdout) -> None:
        stream.write(self.text())
        stream.flush()


# --------------------------------------------------------------------------
# Reading the wire log
#
# Everything below is pure: text in, verdict out. It is what --self-test
# exercises, and it is where a wrong answer would actually come from -- the
# driving above it either works or visibly does not.
# --------------------------------------------------------------------------


def toplevels(text: str) -> list[tuple[str, str]]:
    """Ordered (object-id, title) pairs from the log, newest last.

    A title is set per toplevel, and the same id is reused after a destroy, so
    order is the only thing that identifies the current window.
    """
    return TITLE_RE.findall(text)


def toplevel_titled(text: str, title: str) -> str | None:
    """The newest toplevel with exactly this title."""
    found = [oid for oid, seen in toplevels(text) if seen == title]
    return found[-1] if found else None


def other_toplevel(text: str, exclude: str) -> str | None:
    """The newest toplevel in this text that is not `exclude`."""
    found = [oid for oid, _title in toplevels(text) if oid != exclude]
    return found[-1] if found else None


def keyboard_focused(text: str) -> bool:
    """Does a surface hold keyboard focus at the end of this text?

    Focus is state, not an event. A window that already had focus when the
    tray activated it is never sent a second `enter`, and a gesture still
    reaches it -- so asking "was there an enter just now" fails a build that
    is behaving perfectly. Ask who holds focus instead: the last `enter`
    without a `leave` after it.
    """
    entered = [m.start() for m in KEYBOARD_ENTER_RE.finditer(text)]
    left = [m.start() for m in KEYBOARD_LEAVE_RE.finditer(text)]
    if not entered:
        return False
    return not left or entered[-1] > left[-1]


def check_menu(items: list[tuple[int, str, str]]) -> tuple[bool, str]:
    """Every promised entry present in the tray menu layout."""
    labels = [label for _ident, label, _kind in items if label]
    missing = [
        want for want in MENU_WANTED
        if not any(re.search(want, label, re.IGNORECASE) for label in labels)
    ]
    if missing:
        wanted = ", ".join(f"/{want}/i" for want in missing)
        return False, f"tray menu has no item matching {wanted}; saw {labels}"
    return True, f"tray menu carries {labels}"



def check_close_to_tray(tail: str, toplevel: str, alive: bool) -> tuple[bool, str]:
    """The whole promise, in the order the three ways of breaking it happen.

    Missing close first: with no request delivered, nothing can be concluded
    about the client, and blaming it would send the next reader after the
    wrong layer.
    """
    if f"xdg_toplevel#{toplevel}.close()" not in tail:
        return False, (
            f"inconclusive: the compositor never sent xdg_toplevel#{toplevel}.close(), "
            "so the client was never asked to close -- the close gesture did not "
            "reach the window"
        )
    if f"xdg_toplevel#{toplevel}.destroy()" not in tail:
        return False, (
            f"close request was ignored: the compositor sent "
            f"xdg_toplevel#{toplevel}.close() and the client never destroyed "
            f"xdg_toplevel#{toplevel} -- the window stays on screen"
        )
    if not alive:
        return False, (
            f"the app exited on xdg_toplevel#{toplevel}.close() instead of "
            "hiding to the tray"
        )
    return True, (
        f"xdg_toplevel#{toplevel}.close() delivered, client destroyed the toplevel, "
        "process still alive"
    )


# --------------------------------------------------------------------------
# Processes
# --------------------------------------------------------------------------


def stop_process(proc: subprocess.Popen | None) -> None:
    """SIGTERM the process group, SIGKILL it five seconds later."""
    if proc is None or proc.poll() is not None:
        return
    try:
        os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except OSError:
            pass
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
    except (OSError, ProcessLookupError):
        pass


def wait_for(probe, timeout: float, interval: float = 0.5):
    """Poll `probe` until it returns something truthy, or give up."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        found = probe()
        if found:
            return found
        time.sleep(interval)
    return None


@dataclass
class Lane:
    """The compositor and bus everything else in the run talks to."""

    wayland_display: str
    bus_address: str
    kind: str
    proc: subprocess.Popen | None = None


def shell_bus_address(display: str) -> str | None:
    """The private session bus a nested shell was started on.

    `dbus-run-session` makes the bus and stays as the parent, so its address
    exists only in the environment of the shell it spawned. The display name
    is unique to this run, which is what makes the scan unambiguous.
    """
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            if (entry / "comm").read_text().strip() != "gnome-shell":
                continue
            if display not in (entry / "cmdline").read_bytes().decode().split("\0"):
                continue
            for line in (entry / "environ").read_bytes().decode().split("\0"):
                name, _, value = line.partition("=")
                if name == "DBUS_SESSION_BUS_ADDRESS":
                    return value
        except (OSError, UnicodeDecodeError):
            continue
    return None


def resolve_lane(mode: str) -> str:
    """Which lane can run here, or why none can. Cheap: no process started."""
    nested_ready = bool(shutil.which("gnome-shell") and shutil.which("dbus-run-session"))
    if mode == "nested" or (mode == "auto" and nested_ready):
        if not nested_ready:
            raise CheckSkipped("no Wayland compositor available (gnome-shell not installed)")
        if not os.environ.get("XDG_RUNTIME_DIR"):
            raise CheckSkipped("no Wayland compositor available (XDG_RUNTIME_DIR is unset)")
        return "nested"
    if mode == "auto":
        raise CheckSkipped("no Wayland compositor available")
    if not os.environ.get("WAYLAND_DISPLAY"):
        raise CheckSkipped("no Wayland compositor available (WAYLAND_DISPLAY is unset)")
    return "current"


def start_nested(root: Path, report: Report) -> Lane:
    """A headless gnome-shell of our own, on a display name nobody else has.

    Nested rather than `--nested`: on a host whose outer session is Wayland,
    mutter's nested mode picks display-server mode and fails logind's
    TakeControl. Headless with a virtual monitor is the mode that starts.
    """
    display = f"wayland-check-{os.getpid()}"
    env = {name: value for name, value in os.environ.items() if name not in NESTED_UNSET}
    log = (root / "compositor.log").open("wb")
    proc = subprocess.Popen(
        [
            "dbus-run-session", "--",
            "gnome-shell", "--headless",
            "--virtual-monitor", NESTED_MONITOR,
            "--wayland-display", display,
        ],
        env=env,
        stdout=log,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    log.close()
    lane = Lane(wayland_display=display, bus_address="", kind="nested", proc=proc)
    socket = Path(os.environ["XDG_RUNTIME_DIR"]) / display
    if not wait_for(lambda: socket.exists() or proc.poll() is not None, COMPOSITOR_TIMEOUT):
        raise CheckSkipped(f"nested compositor never created {socket}")
    if proc.poll() is not None:
        tail = (root / "compositor.log").read_text(errors="replace").strip().splitlines()[-3:]
        raise CheckSkipped(f"nested compositor exited {proc.returncode}: {' | '.join(tail)}")
    address = wait_for(lambda: shell_bus_address(display), COMPOSITOR_TIMEOUT)
    if not address:
        raise CheckSkipped("nested compositor exposed no DBUS_SESSION_BUS_ADDRESS")
    lane.bus_address = address
    report.ok(f"compositor: nested gnome-shell on {display} ({NESTED_MONITOR})")
    return lane


def open_lane(mode: str, root: Path, report: Report) -> Lane:
    if resolve_lane(mode) == "nested":
        return start_nested(root, report)
    display = os.environ["WAYLAND_DISPLAY"]
    address = os.environ.get("DBUS_SESSION_BUS_ADDRESS")
    if not address:
        address = f"unix:path={os.environ.get('XDG_RUNTIME_DIR', '/run/user/1000')}/bus"
    report.ok(f"compositor: current session on {display}")
    # The app gets an isolated HOME either way. Against a real desktop's bus
    # that combination has been seen to take the webview down a second or two
    # in, which is a property of this harness and not of the product -- the
    # nested lane exists partly to avoid it.
    report.note("current lane runs on the ambient session bus; prefer --compositor nested in CI")
    return Lane(wayland_display=display, bus_address=address, kind="current")


def isolated_env(root: Path, lane: Lane) -> dict[str, str]:
    """The app's environment: this compositor, this bus, nothing of the user's."""
    env = dict(os.environ)
    env.pop("DISPLAY", None)
    env.update(
        DBUS_SESSION_BUS_ADDRESS=lane.bus_address,
        WAYLAND_DISPLAY=lane.wayland_display,
        XDG_SESSION_TYPE="wayland",
        GDK_BACKEND="wayland",
        # The whole point: without this there is nothing to read afterwards.
        WAYLAND_DEBUG="1",
    )
    for name, relative in ISOLATED_DIRS.items():
        target = root / relative
        target.mkdir(parents=True, exist_ok=True)
        env[name] = str(target)
    return env


def stage_binaries(root: Path, app: Path, cli: Path, report: Report) -> Path:
    """Copy the app, and the CLI it must find, into the scratch root.

    The app resolves `rocm` as a sibling of its own executable, so launching
    it from `target/release` would hand it whatever is staged there.
    """
    if not app.is_file():
        raise CheckSkipped(f"no built app binary at {app}")
    bin_dir = root / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    staged = bin_dir / app.name
    shutil.copy2(app, staged)
    staged.chmod(0o755)
    if cli.is_file():
        sibling = bin_dir / "rocm"
        shutil.copy2(cli, sibling)
        sibling.chmod(0o755)
        report.ok(f"staged {app.name} with a sibling rocm in {bin_dir}")
    else:
        report.note(f"no CLI at {cli}; app will find no sibling rocm")
    return staged


def start_watcher(root: Path, lane: Lane) -> tuple[subprocess.Popen, Path]:
    """The panel stand-in. Without one, libayatana publishes no tray item."""
    registered = root / "registered.json"
    log = (root / "watcher.log").open("wb")
    proc = subprocess.Popen(
        [sys.executable, str(WATCHER_SCRIPT), str(registered)],
        env={**os.environ, "DBUS_SESSION_BUS_ADDRESS": lane.bus_address},
        stdout=log,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    log.close()
    return proc, registered


def start_app(staged: Path, root: Path, env: dict[str, str]) -> subprocess.Popen:
    log = (root / "wire.log").open("wb")
    proc = subprocess.Popen(
        [str(staged)], env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True
    )
    log.close()
    return proc


# --------------------------------------------------------------------------
# The one held D-Bus connection
# --------------------------------------------------------------------------


class Session:
    """Tray menu calls and input injection over a single connection.

    A `RemoteDesktop` session object dies with the connection that created it,
    so a sequence of one-shot calls cannot drive one. Both users of the bus
    share this connection for exactly that reason.
    """

    def __init__(self, address: str):
        # Imported here, not at module scope: --self-test must run on a host
        # with no GTK stack at all.
        import gi

        gi.require_version("Gio", "2.0")
        from gi.repository import Gio, GLib

        self.gio, self.glib = Gio, GLib
        self.conn = Gio.DBusConnection.new_for_address_sync(
            address,
            Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
            | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
            None,
            None,
        )
        self.remote_path = ""

    def call(self, dest, path, iface, method, params=None):
        return self.conn.call_sync(
            dest, path, iface, method, params, None, self.gio.DBusCallFlags.NONE, 5000, None
        )

    def prop(self, dest: str, path: str, iface: str, name: str):
        return self.call(
            dest, path, "org.freedesktop.DBus.Properties", "Get",
            self.glib.Variant("(ss)", (iface, name)),
        ).unpack()[0]

    def menu_items(self, dest: str, path: str) -> list[tuple[int, str, str]]:
        layout = self.call(
            dest, path, DBUSMENU, "GetLayout", self.glib.Variant("(iias)", (0, -1, []))
        ).unpack()[1]
        found: list[tuple[int, str, str]] = []

        def walk(node):
            ident, props, children = node
            found.append((ident, props.get("label", ""), props.get("type", "standard")))
            for child in children:
                walk(child)

        walk(layout)
        return found

    def click(self, dest: str, path: str, ident: int) -> None:
        self.call(
            dest, path, DBUSMENU, "Event",
            self.glib.Variant(
                "(isvu)", (ident, "clicked", self.glib.Variant("s", ""), int(time.time()))
            ),
        )

    def start_remote(self) -> None:
        self.remote_path = self.call(
            RD, "/org/gnome/Mutter/RemoteDesktop", RD, "CreateSession"
        ).unpack()[0]
        self.call(RD, self.remote_path, RD_SESSION, "Start")
        time.sleep(0.4)

    def keys(self, *pairs: tuple[int, bool]) -> None:
        for keycode, pressed in pairs:
            self.call(
                RD, self.remote_path, RD_SESSION, "NotifyKeyboardKeycode",
                self.glib.Variant("(ub)", (keycode, pressed)),
            )
            time.sleep(0.1)

    def close(self) -> None:
        if not self.remote_path:
            return
        try:
            self.call(RD, self.remote_path, RD_SESSION, "Stop")
        except self.glib.Error:
            pass
        self.remote_path = ""


# --------------------------------------------------------------------------
# The run
# --------------------------------------------------------------------------


def tray_item(registered: Path) -> dict[str, str] | None:
    """The first registered StatusNotifierItem, once the watcher has one."""
    try:
        items = json.loads(registered.read_text()).get("items")
    except (OSError, json.JSONDecodeError):
        return None
    return items[0] if items else None


def drive(lane: Lane, root: Path, app: subprocess.Popen, registered: Path,
          report: Report) -> None:
    """Every check, in the one order that can produce both.

    The tray must be up before anything can be clicked, and "Open ROCm App"
    is what gives the main window keyboard focus, which is what makes Alt+F4
    reach it. The app never takes focus by itself in a headless shell.
    """
    def wire() -> str:
        return (root / "wire.log").read_text(errors="replace")

    main = wait_for(lambda: toplevel_titled(wire(), MAIN_TITLE), TOPLEVEL_TIMEOUT, 1.0)
    if main is None:
        report.unreached(f"the app mapped no {MAIN_TITLE!r} toplevel in {TOPLEVEL_TIMEOUT:.0f}s")
        return
    report.ok(f"main window is xdg_toplevel#{main}")

    item = wait_for(lambda: tray_item(registered), TRAY_TIMEOUT, 1.0)
    if item is None:
        report.check("tray-registers", False,
                     f"no StatusNotifierItem registered in {TRAY_TIMEOUT:.0f}s")
        report.unreached("the tray never registered, so its menu could not be driven")
        return

    session = Session(lane.bus_address)
    try:
        dest = item["sender"]
        # libayatana registers an object path, not a bus name: the path is the
        # item, and the sender is the only address it can be reached on.
        menu = session.prop(dest, item["service"], SNI, "Menu")
        items = session.menu_items(dest, menu)
        report.check("tray-registers", *check_menu(items))
        report.note(f"tray item {item['service']} on {dest}, menu {menu}")

        def ident(fragment: str) -> int | None:
            for entry, label, _kind in items:
                if re.search(fragment, label, re.IGNORECASE):
                    return entry
            return None

        open_id = ident("open")
        if open_id is None:
            report.unreached("the tray menu has no open entry to click")
            return

        session.start_remote()
        session.keys((KEY_ESC, True), (KEY_ESC, False))  # leave the shell overview
        time.sleep(1.0)

        mark = len(wire())
        session.click(dest, menu, open_id)
        time.sleep(CLICK_SETTLE)
        tail = wire()[mark:]
        (root / "open-tail.log").write_text(tail)
        # The whole log, not the tail: focus is held state, and the window may
        # have taken it at launch and never given it back.
        if not keyboard_focused(wire()):
            report.check("close-to-tray", False,
                         "no surface holds keyboard focus, so a close gesture "
                         "could not be delivered to the main window")
            return
        report.ok("main window holds keyboard focus")

        mark = len(wire())
        session.keys(
            (KEY_LEFTALT, True), (KEY_F4, True), (KEY_F4, False), (KEY_LEFTALT, False)
        )
        time.sleep(CLOSE_SETTLE)
        tail = wire()[mark:]
        (root / "close-tail.log").write_text(tail)
        report.check("close-to-tray", *check_close_to_tray(tail, main, app.poll() is None))
    finally:
        session.close()


def run(args: argparse.Namespace) -> int:
    report = Report()
    report.head("wayland desktop check")
    root = Path(tempfile.mkdtemp(prefix="rocm-wayland-check-", dir="/tmp"))
    lane: Lane | None = None
    watcher: subprocess.Popen | None = None
    app: subprocess.Popen | None = None
    skipped = ""
    try:
        # Cheap and process-free, so a host that cannot run this lane says so
        # before 55MB of binaries are copied anywhere.
        resolve_lane(args.compositor)
        staged = stage_binaries(root, args.app, args.cli, report)
        lane = open_lane(args.compositor, root, report)
        watcher, registered = start_watcher(root, lane)
        time.sleep(1.5)
        app = start_app(staged, root, isolated_env(root, lane))
        report.ok(f"launched {staged.name} pid={app.pid} under WAYLAND_DEBUG=1")
        drive(lane, root, app, registered, report)
    except CheckSkipped as reason:
        skipped = str(reason)
    except Exception as error:  # noqa: BLE001 - the report and the teardown must still happen
        report.fail(f"harness error: {type(error).__name__}: {error}")
        report.unreached("the harness itself failed")
    finally:
        # Everything this run started dies here, on every path. A leaked
        # nested shell holds its socket and its bus for the next run to trip
        # over.
        stop_process(app)
        stop_process(watcher)
        if lane is not None:
            stop_process(lane.proc)

    if skipped:
        report.note(f"skipped: {skipped}")
        report.head(f"SKIPPED: {skipped} (no check ran)")
    else:
        report.head(
            f"{len(report.failures)} check(s) failed" if report.failures else "all checks passed"
        )
    # A skip that produced no log produced nothing worth keeping; anything
    # that got as far as starting a process left evidence, and that evidence
    # is the whole reason a failure here is diagnosable without a rerun.
    if skipped and not any(root.glob("*.log")):
        shutil.rmtree(root, ignore_errors=True)
    else:
        report.note(f"wire log and verdict in {root}")
        (root / "verdict.txt").write_text(report.text())
    report.emit()
    return 0 if skipped else (1 if report.failures else 0)


# --------------------------------------------------------------------------
# Self-test
#
# The compositor lane cannot run everywhere, so the part that decides what the
# wire log means is exercised against synthetic logs instead: no compositor,
# no app, no bus. Each case below is a way this check could quietly return the
# wrong verdict.
# --------------------------------------------------------------------------

CLOSE_LOG = """\
 -> xdg_toplevel#39.set_title("ROCm")
xdg_toplevel#39.configure(1280, 1000, array)
xdg_toplevel#39.close()
 -> xdg_toplevel#39.destroy()
 -> wl_surface#38.destroy()
"""

IGNORED_LOG = """\
 -> xdg_toplevel#39.set_title("ROCm")
xdg_toplevel#39.close()
 -> wl_surface#38.frame(new id wl_callback#44)
"""

NO_CLOSE_LOG = """\
 -> xdg_toplevel#39.set_title("ROCm")
xdg_toplevel#39.configure(1280, 1000, array)
 -> wl_surface#38.commit()
"""

MULTI_TITLE_LOG = """\
 -> xdg_toplevel#39.set_title("ROCm")
 -> xdg_toplevel#52.set_title("ROCm Quick Status")
 -> xdg_toplevel#39.set_title("ROCm")
 -> xdg_toplevel#61.set_title("ROCm Logs")
"""

FULL_MENU = [(0, "", "standard"), (1, "More Info", "standard"),
             (2, "Open ROCm App", "standard"), (3, "Quit ROCm App", "standard")]


def self_test() -> int:
    report = Report()
    report.head("wayland desktop check self-test")
    failures: list[str] = []

    def expect(case: str, condition: bool, detail: str) -> None:
        if condition:
            report.ok(f"{case}: {detail}")
        else:
            failures.append(case)
            report.fail(f"{case}: {detail}")

    passed, message = check_close_to_tray(CLOSE_LOG, "39", alive=True)
    expect("close honoured", passed and "still alive" in message, message)

    passed, message = check_close_to_tray(IGNORED_LOG, "39", alive=True)
    # The exact wording is the point: this is the defect the lane exists to
    # catch, and a reader who greps the report for it must find it.
    expect(
        "close ignored",
        not passed and "close request was ignored" in message,
        message,
    )

    passed, message = check_close_to_tray(NO_CLOSE_LOG, "39", alive=True)
    expect(
        "no close delivered",
        not passed
        and "never sent xdg_toplevel#39.close()" in message
        and "close request was ignored" not in message,
        message,
    )

    passed, message = check_close_to_tray(CLOSE_LOG, "39", alive=False)
    expect("app exited on close", not passed and "instead of hiding" in message, message)

    pairs = toplevels(MULTI_TITLE_LOG)
    expect(
        "title extractor",
        pairs == [("39", "ROCm"), ("52", "ROCm Quick Status"),
                  ("39", "ROCm"), ("61", "ROCm Logs")]
        and toplevel_titled(MULTI_TITLE_LOG, "ROCm") == "39"
        and other_toplevel(MULTI_TITLE_LOG, "39") == "61",
        f"{pairs} -> main #39, newest other #61",
    )


    passed, message = check_menu(FULL_MENU)
    expect("full menu", passed, message)

    passed, message = check_menu([entry for entry in FULL_MENU if entry[0] != 2])
    expect("menu without open", not passed and "/open/i" in message, message)

    expect(
        "keyboard focus",
        keyboard_focused("wl_keyboard#25.enter(31, wl_surface#38, array)")
        and not keyboard_focused("wl_keyboard#25.leave(33, wl_surface#38)"),
        "enter counts as focus, leave does not",
    )

    report.head(
        f"{len(failures)} self-test case(s) misbehaved" if failures else "every case behaved"
    )
    report.emit()
    return 1 if failures else 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--compositor", choices=("auto", "nested", "current"), default="auto",
        help="nested: start a headless gnome-shell and tear it down; "
             "current: use $WAYLAND_DISPLAY and the ambient bus; "
             "auto: nested when gnome-shell is installed, else skip (default)",
    )
    parser.add_argument(
        "--app", type=Path, default=DEFAULT_APP, metavar="PATH",
        help=f"app binary to drive (default {DEFAULT_APP})",
    )
    parser.add_argument(
        "--cli", type=Path, default=DEFAULT_CLI, metavar="PATH",
        help=f"CLI staged as the app's sibling rocm (default {DEFAULT_CLI})",
    )
    parser.add_argument(
        "--self-test", action="store_true",
        help="exercise the wire-log parsing and verdicts against synthetic logs; "
             "no compositor, no app",
    )
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    return run(args)


if __name__ == "__main__":
    raise SystemExit(main())
