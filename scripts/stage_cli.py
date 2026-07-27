#!/usr/bin/env python3
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
"""Stage the `rocm` and `rocmd` binaries this app will ship with.

Installing ROCm App installs the CLI. That is a promise the installer has to
keep byte for byte, so the binaries are copied here from a real rocm-cli build
and their identity is recorded in `src-tauri/compatibility.json` before anything
is bundled.

Two things this deliberately does *not* do. It does not download: the CLI it
ships is the one on this machine, built from a commit this records, so there is
no window in which the bundled tool came from somewhere nobody can name. And it
does not guess a version: it runs each binary's `--version` and records what it
actually printed, because a version read from a manifest is a claim while a
version read from the binary is a fact.

Usage:
    python3 scripts/stage_cli.py --from ../rocm-cli/target/release
    python3 scripts/stage_cli.py --check
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

APP_ROOT = Path(__file__).resolve().parent.parent
BINARIES_DIR = APP_ROOT / "src-tauri" / "binaries"
MANIFEST = APP_ROOT / "src-tauri" / "compatibility.json"
SCHEMA_VERSION = 1

# The two binaries the app installs. `rocmd` is here because the CLI's own
# installers require it beside `rocm`; an app that shipped only `rocm` would be
# a quieter, harder-to-diagnose partial install.
CLI_BINARIES = ("rocm", "rocmd")


class StageError(RuntimeError):
    pass


def host_triple() -> str:
    """The target triple Tauri appends to a sidecar's filename."""
    out = subprocess.run(
        ["rustc", "-vV"], capture_output=True, text=True, check=True
    ).stdout
    for line in out.splitlines():
        if line.startswith("host:"):
            return line.split(":", 1)[1].strip()
    raise StageError("rustc -vV did not report a host triple")


def exe_suffix(triple: str) -> str:
    return ".exe" if "windows" in triple else ""


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def binary_version(path: Path) -> str:
    """What the binary itself says, not what a manifest claims."""
    try:
        result = subprocess.run(
            [str(path), "--version"], capture_output=True, text=True, timeout=60
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise StageError(f"{path.name} --version did not run: {exc}") from exc
    if result.returncode != 0:
        raise StageError(
            f"{path.name} --version exited {result.returncode}: "
            f"{result.stderr.strip()[:200]}"
        )
    version = result.stdout.strip() or result.stderr.strip()
    if not version:
        raise StageError(f"{path.name} --version printed nothing")
    return version.splitlines()[0].strip()


def source_commit(source: Path) -> str:
    """The rocm-cli commit these binaries were built from, when it is knowable.

    Recorded rather than required: a binary staged from a release tarball has no
    repository behind it, and refusing to package in that case would make the
    tarball path unusable for no safety gain.
    """
    for candidate in (source, *source.parents):
        if (candidate / ".git").exists():
            result = subprocess.run(
                ["git", "-C", str(candidate), "rev-parse", "HEAD"],
                capture_output=True,
                text=True,
            )
            if result.returncode == 0:
                return result.stdout.strip()
    return "unknown"


def stage(source: Path, triple: str) -> dict:
    if not source.is_dir():
        raise StageError(f"not a directory: {source}")
    BINARIES_DIR.mkdir(parents=True, exist_ok=True)

    suffix = exe_suffix(triple)
    entries = []
    for name in CLI_BINARIES:
        origin = source / f"{name}{suffix}"
        if not origin.is_file():
            raise StageError(
                f"{origin} is missing. Build it first:\n"
                f"    cargo build --release -p rocm -p rocmd"
            )
        # Tauri resolves a sidecar by appending the target triple, and strips it
        # back off on install. The suffix is not decoration: a bundle built for
        # one triple must not silently pick up a binary built for another.
        destination = BINARIES_DIR / f"{name}-{triple}{suffix}"
        shutil.copy2(origin, destination)
        destination.chmod(0o755)
        entries.append(
            {
                "name": name,
                "fileName": destination.name,
                "version": binary_version(destination),
                "sizeBytes": destination.stat().st_size,
                "sha256": sha256_of(destination),
            }
        )

    manifest = {
        "schemaVersion": SCHEMA_VERSION,
        "appVersion": app_version(),
        "target": triple,
        "stagedAtUnixMs": int(time.time() * 1000),
        "sourceCommit": source_commit(source.resolve()),
        "binaries": entries,
    }
    MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest


def app_version() -> str:
    package = json.loads((APP_ROOT / "package.json").read_text())
    return package["version"]


def check() -> dict:
    """Re-verify that what is staged is what the manifest says.

    Run before bundling. A stale sidecar left behind by an earlier build is the
    failure this catches: it would ship, and its recorded hash would describe a
    file that is no longer there.
    """
    if not MANIFEST.is_file():
        raise StageError(f"{MANIFEST} is missing; run without --check first")
    manifest = json.loads(MANIFEST.read_text())
    if manifest.get("schemaVersion") != SCHEMA_VERSION:
        raise StageError(f"unsupported compatibility schema: {manifest.get('schemaVersion')}")

    staged = {p.name for p in BINARIES_DIR.glob("*")} if BINARIES_DIR.is_dir() else set()
    expected = {entry["fileName"] for entry in manifest["binaries"]}
    if extra := staged - expected:
        raise StageError(f"stale sidecars not named by the manifest: {sorted(extra)}")

    for entry in manifest["binaries"]:
        path = BINARIES_DIR / entry["fileName"]
        if not path.is_file():
            raise StageError(f"missing staged binary: {path}")
        if path.stat().st_size != entry["sizeBytes"]:
            raise StageError(f"{path.name}: size {path.stat().st_size} != {entry['sizeBytes']}")
        actual = sha256_of(path)
        if actual != entry["sha256"]:
            raise StageError(f"{path.name}: sha256 {actual} != {entry['sha256']}")
    if manifest["appVersion"] != app_version():
        raise StageError(
            f"compatibility manifest is for app {manifest['appVersion']}, "
            f"but this tree is {app_version()}"
        )
    return manifest


def report(manifest: dict) -> None:
    print(f"app        {manifest['appVersion']}  target {manifest['target']}")
    print(f"source     rocm-cli {manifest['sourceCommit'][:12]}")
    for entry in manifest["binaries"]:
        print(
            f"  {entry['name']:<6} {entry['version']:<28} "
            f"{entry['sizeBytes']:>12,} bytes  {entry['sha256'][:16]}…"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--from",
        dest="source",
        default=os.environ.get("ROCM_CLI_BUILD_DIR", "../rocm-cli/target/release"),
        help="directory holding the built rocm and rocmd binaries",
    )
    parser.add_argument("--target", default=None, help="target triple (default: this host)")
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify the staged binaries still match the compatibility manifest",
    )
    args = parser.parse_args()

    try:
        if args.check:
            report(check())
            print("staged CLI matches the compatibility manifest")
            return 0
        triple = args.target or host_triple()
        manifest = stage(Path(args.source).expanduser(), triple)
        report(manifest)
        print(f"wrote {MANIFEST.relative_to(APP_ROOT)}")
        return 0
    except StageError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
