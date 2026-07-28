#!/usr/bin/env python3
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
"""Run the visual or a11y wdio suite on a private bus and gather the output.

Why the private bus: the app, the tray watcher, and the menu clicker must
share one session bus that is NOT the developer's desktop bus -- a live
desktop bus combined with the harness's isolated HOME segfaults the release
binary about 1.5 s in, and a fresh private bus is what CI has and what a
fresh user effectively has. So unless already inside one (ROCM_UIQ_BUS=1),
this script re-executes itself under `dbus-run-session --`.

Why DISPLAY is dropped: without one the harness starts its own Xvfb at a
deterministic size; running on the developer's live display would change
font rendering and pop windows on their desktop.

The tray watcher (scripts/statusnotifierwatcher.py) is started first because
the app only registers its icon if org.kde.StatusNotifierWatcher is owned on
the bus; its registry JSON is handed to the suites via ROCM_TRAY_REGISTRY.

Usage:
    python3 scripts/ui_quality.py --suite visual [--scale all|1|1.25|2]
    python3 scripts/ui_quality.py --suite a11y
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
WATCHER_NAME = "org.kde.StatusNotifierWatcher"
SCALES = {"1": "scale-100", "1.25": "scale-125", "2": "scale-200"}
#: Only the compact matrix reruns at raised scales; the full suite runs at 1x.
COMPACT_SPEC = "tests/e2e/visual/compact.visual.ts"


def reexec_on_private_bus() -> None:
    """Replace this process with itself under `dbus-run-session --`."""
    if os.environ.get("ROCM_UIQ_BUS") == "1":
        return
    env = dict(os.environ, ROCM_UIQ_BUS="1")
    argv = ["dbus-run-session", "--", sys.executable, str(Path(__file__).resolve())]
    argv += sys.argv[1:]
    try:
        os.execvpe(argv[0], argv, env)
    except OSError as error:
        print(f"cannot start dbus-run-session: {error}", file=sys.stderr)
        raise SystemExit(2) from error


def start_watcher(registry: Path) -> subprocess.Popen[bytes]:
    """Start the tray watcher and wait until it owns its bus name."""
    watcher = subprocess.Popen(
        [sys.executable, str(REPO / "scripts" / "statusnotifierwatcher.py"), str(registry)]
    )
    import dbus  # late: --help and the pre-reexec pass must not need it

    bus = dbus.SessionBus()
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if watcher.poll() is not None:
            print(f"tray watcher exited early ({watcher.returncode})", file=sys.stderr)
            raise SystemExit(2)
        if bus.name_has_owner(WATCHER_NAME):
            return watcher
        time.sleep(0.1)
    watcher.terminate()
    print(f"{WATCHER_NAME} not owned within 10 s", file=sys.stderr)
    raise SystemExit(2)


def child_env(registry: Path) -> dict[str, str]:
    env = dict(os.environ)
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)
    env["ROCM_E2E_BUS"] = "inherit"  # the bus dbus-run-session gave us is the point
    env["ROCM_TRAY_REGISTRY"] = str(registry)
    return env


def run_wdio(config: str, env: dict[str, str], extra: list[str]) -> int:
    """One wdio run, output streamed through."""
    cmd = ["npx", "wdio", "run", config, *extra]
    print(f"$ {' '.join(cmd)}", flush=True)
    return subprocess.run(cmd, cwd=REPO, env=env, check=False).returncode


def run_visual(scales: list[str], out: Path, registry: Path, keep_going: bool) -> dict[str, int]:
    codes: dict[str, int] = {}
    for scale in scales:
        tag = SCALES[scale]
        env = child_env(registry)
        env["ROCM_VISUAL_SCALE"] = scale
        env["ROCM_VISUAL_DIR"] = str(out / "shots" / tag)
        env["ROCM_E2E_RUN_ID"] = f"visual-{tag}"
        extra = [] if scale == "1" else ["--spec", COMPACT_SPEC]
        codes[tag] = run_wdio("tests/e2e/wdio.visual.conf.ts", env, extra)
        if codes[tag] != 0 and not keep_going:
            break
    return codes


def run_a11y(out: Path, registry: Path) -> dict[str, int]:
    env = child_env(registry)
    env["ROCM_VISUAL_DIR"] = str(out / "shots")
    env["ROCM_E2E_RUN_ID"] = "a11y"
    return {"a11y": run_wdio("tests/e2e/wdio.a11y.conf.ts", env, [])}


def contact_sheet(shots: list[tuple[Path, str]], dest: Path) -> None:
    """Grid of thumbnails, 3 per row, filename captions, white background."""
    from PIL import Image, ImageDraw, ImageFont

    cols, thumb_w, pad, caption_h = 3, 460, 12, 16
    cells = []
    for path, caption in shots:
        with Image.open(path) as raw:
            image = raw.convert("RGB")
        if image.width > thumb_w:
            image = image.resize((thumb_w, max(1, round(image.height * thumb_w / image.width))))
        cells.append((image, caption))

    rows = [cells[i : i + cols] for i in range(0, len(cells), cols)]
    row_heights = [max(image.height for image, _ in row) + caption_h + pad for row in rows]
    sheet = Image.new(
        "RGB", (cols * (thumb_w + pad) + pad, sum(row_heights) + pad), color="white"
    )
    draw = ImageDraw.Draw(sheet)
    font = ImageFont.load_default()
    y = pad
    for row, height in zip(rows, row_heights):
        for column, (image, caption) in enumerate(row):
            x = pad + column * (thumb_w + pad)
            sheet.paste(image, (x, y))
            if len(caption) > 72:  # default bitmap font, ~6 px/char in 460 px
                caption = caption[:71] + "…"
            draw.text((x, y + image.height + 2), caption, fill="black", font=font)
        y += height
    sheet.save(dest)


def build_sheets(out: Path) -> list[Path]:
    try:
        import PIL  # noqa: F401  -- probed here so every sheet gets one clear error
    except ImportError:
        print("contact sheets need Pillow: pip install pillow", file=sys.stderr)
        raise SystemExit(3) from None

    sheets: list[Path] = []
    scale_dirs = sorted(d for d in (out / "shots").glob("*") if d.is_dir())
    compact: list[tuple[Path, str]] = []
    for scale_dir in scale_dirs:
        shots = sorted(scale_dir.glob("*.png"))
        compact += [
            (s, f"{scale_dir.name}/{s.name}")
            for s in shots
            if "quick" in s.name or s.name.startswith("compact")
        ]
        if not shots:
            print(f"note: no shots in {scale_dir}, skipping its sheet")
            continue
        dest = out / f"contact-sheet-{scale_dir.name}.png"
        contact_sheet([(s, s.name) for s in shots], dest)
        sheets.append(dest)
    if compact:
        dest = out / "contact-sheet-compact.png"
        contact_sheet(compact, dest)
        sheets.append(dest)
    else:
        print("note: no compact/quick shots anywhere, skipping the compact sheet")
    return sheets


def write_summary(out: Path, codes: dict[str, int], sheets: list[Path]) -> None:
    lines = ["# UI quality run", "", "## wdio exit codes", ""]
    lines += [f"- {run}: {code}" for run, code in codes.items()]
    lines += ["", "## Shots", ""]
    for directory in sorted(p for p in out.glob("shots/**/") if p.is_dir()):
        shots = sorted(directory.glob("*.png"))
        if not shots:
            continue
        lines.append(f"### {directory.relative_to(out)} ({len(shots)})")
        lines += [f"- {shot.name}" for shot in shots]
        lines.append("")
    if sheets:
        lines += ["## Contact sheets", ""]
        lines += [f"- {sheet.name}" for sheet in sheets]
        lines.append("")
    scans = sorted(out.glob("shots/**/copy-scan.txt"))
    if scans:
        lines += ["## Copy scan", ""]
        for scan in scans:
            lines += [f"### {scan.relative_to(out)}", "", scan.read_text().rstrip(), ""]
    (out / "SUMMARY.md").write_text("\n".join(lines) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--suite", required=True, choices=("visual", "a11y"))
    parser.add_argument(
        "--scale",
        default="all",
        choices=("all", *SCALES),
        help="visual suite only; 'all' runs every scale",
    )
    parser.add_argument("--out", type=Path, help="default test-results/<suite>")
    parser.add_argument(
        "--keep-going",
        default=True,
        action=argparse.BooleanOptionalAction,
        help="run every scale even after a failure",
    )
    args = parser.parse_args()
    reexec_on_private_bus()

    out = (args.out or REPO / "test-results" / args.suite).resolve()
    out.mkdir(parents=True, exist_ok=True)
    registry = out / "tray-registry.json"
    watcher = start_watcher(registry)
    try:
        if args.suite == "visual":
            scales = list(SCALES) if args.scale == "all" else [args.scale]
            codes = run_visual(scales, out, registry, args.keep_going)
        else:
            codes = run_a11y(out, registry)
        sheets = build_sheets(out) if args.suite == "visual" else []
        write_summary(out, codes, sheets)
    finally:
        watcher.terminate()
        try:
            watcher.wait(timeout=5)
        except subprocess.TimeoutExpired:
            watcher.kill()

    print(f"summary: {out / 'SUMMARY.md'}")
    return 1 if any(codes.values()) else 0


if __name__ == "__main__":
    raise SystemExit(main())
