#!/usr/bin/env python3
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
"""Click one tray menu entry over D-Bus, by label.

Why this exists: a hidden Tauri window keeps a 0x0 webview and cannot be
screenshotted, and the tray menu is the only product path that shows the
compact window on Linux. Rather than add a test-only door to the app, tests
click the real menu item the way a desktop panel would: over the bus, via
`com.canonical.dbusmenu.Event`. The app under test stays byte-identical to
what a user launches.

The registry file is written by scripts/statusnotifierwatcher.py as items
register: `{"items": [{"service": ..., "sender": ...}], "hosts": [...]}`,
where `service` is the StatusNotifierItem OBJECT PATH (libayatana passes a
path, not a name) and `sender` is the unique bus name that owns it. Later
sessions re-register, so entries are tried newest to oldest; stale entries
belong to dead bus names and are skipped when the call fails.

Usage:
    python3 scripts/tray_menu.py <registry.json> <label>

Exit 0 once the entry was clicked, 1 (with the labels that were seen on
stderr) when no registration carries a matching item.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections.abc import Iterator
from pathlib import Path

import dbus

SNI = "org.kde.StatusNotifierItem"
DBUSMENU = "com.canonical.dbusmenu"
#: A dead unique name fails fast, but a wedged app would hang the default
#: 25 s per call; the menu is either up now or never will be.
TIMEOUT = 5


def walk(node) -> Iterator[tuple[int, str]]:
    """Yield (id, label) for `node` and its children, depth-first.

    A dbusmenu layout node is (id, props, children); children arrive as
    variants that dbus-python already unwraps into the inner structs.
    """
    node_id, props, children = node
    yield int(node_id), str(props.get("label", ""))
    for child in children:
        yield from walk(child)


def click(bus: dbus.Bus, sender: str, path: str, needle: str, seen: list[str]) -> bool:
    """Try one registration; True once the matching item was clicked."""
    item = bus.get_object(sender, path)
    menu_path = item.Get(SNI, "Menu", dbus_interface=dbus.PROPERTIES_IFACE, timeout=TIMEOUT)
    menu = bus.get_object(sender, str(menu_path))
    try:
        # Some implementations refresh the layout here; the answer is noise.
        menu.AboutToShow(0, dbus_interface=DBUSMENU, timeout=TIMEOUT)
    except dbus.DBusException:
        pass
    _revision, layout = menu.GetLayout(0, -1, ["label"], dbus_interface=DBUSMENU, timeout=TIMEOUT)
    for item_id, label in walk(layout):
        # dbusmenu labels carry GTK-style '_' mnemonics ("_Quit").
        stripped = label.replace("_", "")
        if not stripped:
            continue
        seen.append(stripped)
        if needle in stripped:
            menu.Event(
                item_id,
                "clicked",
                dbus.String("", variant_level=1),
                0,
                dbus_interface=DBUSMENU,
                timeout=TIMEOUT,
            )
            return True
    return False


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("registry", type=Path, help="JSON written by statusnotifierwatcher.py")
    parser.add_argument("label", help="menu entry to click (substring, mnemonics stripped)")
    args = parser.parse_args()

    try:
        items = json.loads(args.registry.read_text())["items"]
    except (OSError, ValueError, KeyError) as error:
        print(f"unreadable registry {args.registry}: {error}", file=sys.stderr)
        return 1

    bus = dbus.SessionBus()
    seen: list[str] = []
    for entry in reversed(items):
        try:
            if click(bus, entry["sender"], entry["service"], args.label, seen):
                return 0
        except dbus.DBusException as error:
            print(f"skipping {entry}: {error.get_dbus_name()}", file=sys.stderr)

    print(
        f"no menu item matching {args.label!r}; saw: {', '.join(seen) or '(none)'}",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
