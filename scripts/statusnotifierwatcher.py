#!/usr/bin/env python3
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
"""A minimal org.kde.StatusNotifierWatcher, standing in for a desktop panel.

A headless test session -- Xvfb, or a nested `gnome-shell --headless` -- runs
no panel, so libayatana-appindicator has nothing to register the tray icon
with and the icon ends up an object nobody hosts. This owns the two bus names
a panel session owns, `org.kde.StatusNotifierWatcher` and a
`StatusNotifierHost-*`, plus the `org.freedesktop.Notifications` daemon the
app talks to when it hides itself. It writes what registered to a JSON file so
a test can address the tray item afterwards.

What it does not simulate: it renders nothing, hosts no menu, and never opens
one -- a test "clicks" a tray entry by calling `com.canonical.dbusmenu.Event`
on the application itself, and this process is not in that path. It never
notices an item's owner leaving the bus, so `RegisteredStatusNotifierItems`
only grows. It answers Notify without displaying anything. None of it touches
the application under test: the binary is byte-identical to what a user
launches, and it is only ever handed a bus that looks slightly more like a
desktop than an empty one does.

Written against dbus-python rather than Gio: `dbus.service` dispatches methods
and publishes introspection from the decorators, where Gio would need two
hand-maintained interface XML blobs and a method-call switch for the same
behaviour.

Usage:
    python3 scripts/statusnotifierwatcher.py /path/to/registered.json
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path

import dbus
import dbus.mainloop.glib
import dbus.service
from gi.repository import GLib

WATCHER = "org.kde.StatusNotifierWatcher"
NOTIFY = "org.freedesktop.Notifications"


class Watcher(dbus.service.Object):
    """The registry half. Records every registration to `out` as it happens.

    The file is rewritten on each change rather than at exit: a test polls it
    to learn when the tray came up, and a watcher that only reported at
    shutdown would tell it nothing while it mattered.
    """

    def __init__(self, bus: dbus.Bus, out: Path):
        super().__init__(bus, "/StatusNotifierWatcher")
        self.out = out
        self.items: list[dict[str, str]] = []
        self.hosts: list[str] = []

    def _dump(self) -> None:
        self.out.write_text(json.dumps({"items": self.items, "hosts": self.hosts}))

    @dbus.service.method(WATCHER, in_signature="s", out_signature="", sender_keyword="sender")
    def RegisterStatusNotifierItem(self, service, sender=None):
        # `service` is what libayatana passes: an object path, not a bus name.
        # `sender` is the unique name that owns it, and the only address a
        # caller can reach the item on. Both are recorded because a test needs
        # the pair.
        entry = {"service": str(service), "sender": str(sender)}
        if entry not in self.items:
            self.items.append(entry)
            self._dump()
            self.StatusNotifierItemRegistered(str(service))
        print(f"registered item: {entry}", flush=True)

    @dbus.service.method(WATCHER, in_signature="s", out_signature="", sender_keyword="sender")
    def RegisterStatusNotifierHost(self, service, sender=None):
        self.hosts.append(str(service))
        self._dump()
        self.StatusNotifierHostRegistered()

    @dbus.service.signal(WATCHER, signature="s")
    def StatusNotifierItemRegistered(self, service):
        pass

    @dbus.service.signal(WATCHER, signature="s")
    def StatusNotifierItemUnregistered(self, service):
        pass

    @dbus.service.signal(WATCHER, signature="")
    def StatusNotifierHostRegistered(self):
        pass

    @dbus.service.method(dbus.PROPERTIES_IFACE, in_signature="ss", out_signature="v")
    def Get(self, interface, prop):
        return self.GetAll(interface)[prop]

    @dbus.service.method(dbus.PROPERTIES_IFACE, in_signature="s", out_signature="a{sv}")
    def GetAll(self, interface):
        return {
            "RegisteredStatusNotifierItems": dbus.Array(
                [item["service"] for item in self.items], signature="s"
            ),
            # Always true: libayatana refuses to publish an item when no host
            # is registered, and this process is the host.
            "IsStatusNotifierHostRegistered": dbus.Boolean(True),
            "ProtocolVersion": dbus.Int32(0),
        }

    @dbus.service.method(dbus.PROPERTIES_IFACE, in_signature="ssv", out_signature="")
    def Set(self, interface, prop, value):
        pass


class Notifications(dbus.service.Object):
    """Stands in for the desktop's notification daemon.

    A headless session has none, so `NotificationBuilder::show()` would fail
    against a bus with nobody on that name and the app's own transitions could
    not be observed end to end. This records what the application asked to
    display; it displays nothing.
    """

    def __init__(self, bus: dbus.Bus, out: Path):
        super().__init__(bus, "/org/freedesktop/Notifications")
        self.out = out
        self.shown: list[dict[str, str]] = []
        self._dump()

    def _dump(self) -> None:
        self.out.write_text(json.dumps(self.shown))

    @dbus.service.method(NOTIFY, in_signature="susssasa{sv}i", out_signature="u")
    def Notify(self, app_name, replaces_id, app_icon, summary, body, actions, hints, timeout):
        self.shown.append({"app": str(app_name), "summary": str(summary), "body": str(body)})
        self._dump()
        print(f"notification: {summary!r} / {body!r}", flush=True)
        return dbus.UInt32(len(self.shown))

    @dbus.service.method(NOTIFY, in_signature="u", out_signature="")
    def CloseNotification(self, nid):
        pass

    @dbus.service.method(NOTIFY, in_signature="", out_signature="as")
    def GetCapabilities(self):
        return dbus.Array(["body", "actions"], signature="s")

    @dbus.service.method(NOTIFY, in_signature="", out_signature="ssss")
    def GetServerInformation(self):
        return ("rocm-test-panel", "rocm-test-panel", "1.0", "1.2")


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "output",
        type=Path,
        help="JSON file rewritten on every registration; "
        "notifications go to <output>.notifications",
    )
    args = parser.parse_args()

    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    # Held for the process lifetime: dropping a BusName releases the name.
    names = [
        dbus.service.BusName(WATCHER, bus, replace_existing=True),
        dbus.service.BusName(f"org.kde.StatusNotifierHost-{os.getpid()}", bus),
        dbus.service.BusName(NOTIFY, bus, replace_existing=True),
    ]
    watcher = Watcher(bus, args.output)
    Notifications(bus, Path(f"{args.output}.notifications"))
    watcher._dump()
    print(f"watcher ready on {len(names)} names", flush=True)
    GLib.MainLoop().run()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
