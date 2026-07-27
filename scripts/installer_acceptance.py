#!/usr/bin/env python3
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
"""Install-lifecycle acceptance for the ROCm App packages.

`package_verify.py` answers "is this artifact the one we built and signed".
This answers the different question nobody finds out about until a user hits
it: "does installing, upgrading, and removing this package do the right thing
to the machine". The failures it exists to catch are all silent ones --

  * a sidecar that stops landing beside the app binary, so the app's
    `current_exe().parent.join("rocm")` resolves to nothing at runtime while
    every checksum still matches;
  * a package that overwrites, then on uninstall deletes, a `rocm` the user
    built themselves and dpkg/rpm never owned;
  * a postrm that grew a glob and takes other applications' autostart entries
    with it;
  * an upgrade that silently turns off "start at login" because the erase
    scriptlet stopped distinguishing erase from upgrade;
  * a driver payload sneaking into a product whose whole premise is that it
    never touches the kernel driver.

It deliberately does not run `dpkg -i`. A test that needs root is a test
nobody runs, and it would have to be run on a throwaway machine to be safe.
Instead every package is unpacked into a fixture root and the maintainer
scripts are *executed* -- the ones extracted from the artifact, not the ones
in the source tree -- inside an unprivileged bubblewrap sandbox whose
`/usr/bin`, `/home` and `/root` are fixtures. That exercises the real branch
of the real shipped script without being able to damage the host.

Usage:
    python3 scripts/installer_acceptance.py
    python3 scripts/installer_acceptance.py --self-test
"""
from __future__ import annotations

import argparse
import functools
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

APP_ROOT = Path(__file__).resolve().parent.parent

# What the app looks for at runtime, and where Tauri puts it. `externalBin`
# sidecars install beside the main binary with the target triple stripped.
APP_BINARY = "rocm-app"
CLI_BINARIES = ("rocm", "rocmd")
UNIX_BIN_DIR = "/usr/bin"
AUTOSTART_ENTRY = ".config/autostart/rocm-app.desktop"

# A driver payload is the one thing this product must never ship. Matched
# against packaged *paths* and *dependency metadata*, never against binary
# contents: the CLI legitimately mentions amdgpu in its diagnostics text, so a
# byte scan would fail on a correct package.
DRIVER_NAME_PAT = re.compile(r"\b(amdgpu[\w.-]*|dkms)\b", re.IGNORECASE)
KERNEL_MODULE_PAT = re.compile(r"\.ko(\.(xz|zst|gz))?$", re.IGNORECASE)

PASS, FAIL, SKIP = "PASS", "FAIL", "SKIP"


class CheckFailed(Exception):
    """A named check did not hold. The message is the reported reason."""


class SkipCheck(Exception):
    """A check cannot run here. The message must say exactly why."""


class HarnessError(RuntimeError):
    """The harness itself could not proceed (bad fixture, missing tool)."""


def need(condition: object, message: str) -> None:
    if not condition:
        raise CheckFailed(message)


def run(argv: list[str], **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(argv, capture_output=True, text=True, timeout=120, **kwargs)


def must_run(argv: list[str], **kwargs) -> subprocess.CompletedProcess:
    result = run(argv, **kwargs)
    if result.returncode != 0:
        raise HarnessError(f"{argv[0]} failed: {(result.stderr or result.stdout).strip()[:400]}")
    return result


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


# --------------------------------------------------------------------------
# Sandbox
#
# The maintainer scripts hardcode /usr/bin, /home/* and /root. To execute them
# honestly we have to make those paths point at fixtures, and to do it without
# root we need a user namespace. `unshare --map-root-user` is blocked on
# AppArmor-restricted distributions; bubblewrap ships with a profile that is
# allowed to create one, so it is the mechanism of record here. When neither
# works the affected checks degrade to text assertions and say so out loud --
# they never silently pass.
# --------------------------------------------------------------------------


@functools.lru_cache(maxsize=1)
def sandbox_status() -> tuple[bool, str]:
    """Whether shipped maintainer scripts can be executed against fixtures."""
    bwrap = shutil.which("bwrap")
    if bwrap is None:
        return False, "bwrap (bubblewrap) is not installed"
    probe = run([bwrap, "--dev-bind", "/", "/", "--tmpfs", "/mnt", "/bin/true"])
    if probe.returncode != 0:
        reason = (probe.stderr or probe.stdout).strip().splitlines()
        return False, f"bwrap cannot create a namespace here: {reason[0] if reason else '?'}"
    return True, "bwrap"


def _bwrap() -> str:
    ok, reason = sandbox_status()
    if not ok:
        raise HarnessError(reason)
    return shutil.which("bwrap")  # type: ignore[return-value]


def _shell() -> str:
    return os.path.realpath(shutil.which("sh") or "/bin/sh")


def run_with_fake_usr_bin(
    script: Path,
    args: list[str],
    *,
    present: tuple[str, ...],
    ownership_tool: str,
    owned: bool,
) -> subprocess.CompletedProcess:
    """Execute a preinstall script against a fixture `/usr/bin`.

    `present` are the CLI names that already exist there; `owned` is what the
    fake `dpkg-query`/`rpm` reports about them. Everything else in /usr/bin is
    hidden, which also proves the script does not depend on anything it did
    not declare.
    """
    work = Path(tempfile.mkdtemp(prefix="usrbin-", dir=str(script.parent)))
    fake = work / ownership_tool
    fake.write_text("#!/bin/sh\nexit %d\n" % (0 if owned else 1))
    fake.chmod(0o755)
    stub = work / "stub-binary"
    stub.write_text("#!/bin/sh\nexit 0\n")
    stub.chmod(0o755)

    argv = [_bwrap(), "--dev-bind", "/", "/", "--tmpfs", UNIX_BIN_DIR]
    for tool in ("sh", "cat", "rm"):
        real = shutil.which(tool)
        if real:
            argv += ["--ro-bind", os.path.realpath(real), f"{UNIX_BIN_DIR}/{tool}"]
    argv += ["--ro-bind", str(fake), f"{UNIX_BIN_DIR}/{ownership_tool}"]
    for name in present:
        argv += ["--ro-bind", str(stub), f"{UNIX_BIN_DIR}/{name}"]
    argv += [f"{UNIX_BIN_DIR}/sh", str(script), *args]
    return run(argv)


def run_with_fake_homes(
    script: Path, args: list[str], homes: Path, root_home: Path
) -> subprocess.CompletedProcess:
    """Execute a removal script against fixture `/home` and `/root` trees."""
    argv = [
        _bwrap(),
        "--dev-bind",
        "/",
        "/",
        "--bind",
        str(homes),
        "/home",
        "--bind",
        str(root_home),
        "/root",
        _shell(),
        str(script),
        *args,
    ]
    return run(argv)


def make_home_fixture(work: Path) -> tuple[Path, Path, list[Path], list[Path]]:
    """Two users plus root, each with our autostart entry and a stranger's.

    The stranger's entry is the point: it is what a glob in the removal script
    would take with it.
    """
    homes = work / "homes"
    root_home = work / "roothome"
    ours: list[Path] = []
    theirs: list[Path] = []
    for base in (homes / "alice", homes / "bob", root_home):
        autostart = base / ".config" / "autostart"
        autostart.mkdir(parents=True, exist_ok=True)
        mine = base / AUTOSTART_ENTRY
        mine.write_text("[Desktop Entry]\nExec=/usr/bin/rocm-app\n")
        other = autostart / "other-app.desktop"
        other.write_text("[Desktop Entry]\nExec=/usr/bin/other-app\n")
        ours.append(mine)
        theirs.append(other)
    return homes, root_home, ours, theirs


# --------------------------------------------------------------------------
# Package readers
# --------------------------------------------------------------------------


def extract_deb(deb: Path, dest: Path) -> Path:
    """Unpack a .deb's payload with ar+tar, the way dpkg would lay it down."""
    staging = dest.parent / (dest.name + ".ar")
    staging.mkdir(parents=True, exist_ok=True)
    dest.mkdir(parents=True, exist_ok=True)
    must_run(["ar", "x", str(deb.resolve())], cwd=staging)
    members = sorted(staging.glob("data.tar*"))
    if not members:
        raise HarnessError(f"{deb.name} has no data.tar member")
    must_run(["tar", "-xf", str(members[0]), "-C", str(dest)])
    return dest


def extract_deb_control(deb: Path, dest: Path) -> Path:
    """Unpack the control archive: this is where the *shipped* scripts live."""
    staging = dest.parent / (dest.name + ".ar")
    if not staging.is_dir():
        staging.mkdir(parents=True, exist_ok=True)
        must_run(["ar", "x", str(deb.resolve())], cwd=staging)
    dest.mkdir(parents=True, exist_ok=True)
    members = sorted(staging.glob("control.tar*"))
    if not members:
        raise HarnessError(f"{deb.name} has no control.tar member")
    must_run(["tar", "-xf", str(members[0]), "-C", str(dest)])
    return dest


def extract_rpm(rpm: Path, dest: Path) -> Path:
    dest.mkdir(parents=True, exist_ok=True)
    with rpm.open("rb") as handle:
        cpio_stream = subprocess.run(
            ["rpm2cpio", "-"], stdin=handle, capture_output=True, timeout=120
        )
    if cpio_stream.returncode != 0:
        raise HarnessError(f"rpm2cpio failed: {cpio_stream.stderr.decode()[:200]}")
    unpack = subprocess.run(
        ["cpio", "-idm", "--quiet"],
        input=cpio_stream.stdout,
        capture_output=True,
        cwd=dest,
        timeout=120,
    )
    if unpack.returncode != 0:
        raise HarnessError(f"cpio failed: {unpack.stderr.decode()[:200]}")
    return dest


def deb_paths(deb: Path) -> list[str]:
    """Regular files as dpkg sees them -- this list is what dpkg removes.

    Directories are dropped. Tauri names the resource directory after the main
    binary (`/usr/lib/rocm-app/`), so a listing that kept directories would
    report two entries called `rocm-app` and make the sibling check ambiguous
    for a perfectly good package.
    """
    listing = must_run(["dpkg-deb", "-c", str(deb)]).stdout
    paths = []
    for line in listing.splitlines():
        fields = line.split(None, 5)
        if len(fields) < 6 or not fields[0].startswith("-"):
            continue
        name = fields[5].split(" -> ")[0]
        paths.append("/" + name.lstrip("./").lstrip("/"))
    return paths


def rpm_paths(rpm: Path) -> list[str]:
    """Regular files as rpm sees them. Directories dropped, as for the deb."""
    listing = must_run(
        ["rpm", "-qp", "--qf", "[%{FILEMODES:perms} %{FILENAMES}\n]", str(rpm)]
    ).stdout
    paths = []
    for line in listing.splitlines():
        mode, _, name = line.strip().partition(" ")
        if mode.startswith("-") and name:
            paths.append(name)
    return paths


def deb_control_fields(control_dir: Path) -> dict[str, str]:
    text = (control_dir / "control").read_text()
    fields: dict[str, str] = {}
    key = ""
    for line in text.splitlines():
        if line.startswith((" ", "\t")) and key:
            fields[key] += " " + line.strip()
        elif ":" in line:
            key, _, value = line.partition(":")
            key = key.strip()
            fields[key] = value.strip()
    return fields


# rpm only prints "(using <interp>)" when the scriptlet declares one. Tauri's
# rpm builder does not, so the clause has to be optional -- requiring it made
# every real scriptlet look absent.
SCRIPTLET_HEADER = re.compile(r"^(\w+) scriptlet(?: \(using [^)]*\))?:$")


def rpm_scriptlets(rpm: Path) -> dict[str, str]:
    """The scriptlets as rpm will actually run them, read back off the artifact."""
    out = must_run(["rpm", "-qp", "--scripts", str(rpm)]).stdout
    scripts: dict[str, list[str]] = {}
    current = None
    for line in out.splitlines():
        header = SCRIPTLET_HEADER.match(line)
        if header:
            current = header.group(1)
            scripts[current] = []
        elif current is not None:
            scripts[current].append(line)
    return {name: "\n".join(body).strip() + "\n" for name, body in scripts.items()}


# --------------------------------------------------------------------------
# Context
# --------------------------------------------------------------------------


@dataclass
class Ctx:
    """Everything a check needs, resolved once so the checks stay short."""

    app_root: Path
    work: Path
    source_level: list[str] = field(default_factory=list)
    _cache: dict = field(default_factory=dict)

    def note(self, item: str, reason: str) -> None:
        entry = f"{item}: {reason}"
        if entry not in self.source_level:
            self.source_level.append(entry)

    @property
    def bundle_dir(self) -> Path:
        return self.app_root / "src-tauri" / "target" / "release" / "bundle"

    def _one(self, kind: str, pattern: str) -> Path:
        found = sorted(self.bundle_dir.glob(f"{kind}/{pattern}"))
        if not found:
            raise CheckFailed(
                f"no {kind} bundle under {self.bundle_dir}/{kind}; run `npm run tauri build`"
            )
        if len(found) > 1:
            raise CheckFailed(f"ambiguous {kind} bundles: {[p.name for p in found]}")
        return found[0]

    @property
    def deb(self) -> Path:
        return self._memo("deb", lambda: self._one("deb", "*.deb"))

    @property
    def rpm(self) -> Path:
        return self._memo("rpm", lambda: self._one("rpm", "*.rpm"))

    @property
    def deb_root(self) -> Path:
        return self._memo("deb_root", lambda: extract_deb(self.deb, self.work / "deb-root"))

    @property
    def deb_files(self) -> list[str]:
        return self._memo("deb_files", lambda: deb_paths(self.deb))

    @property
    def rpm_files(self) -> list[str]:
        return self._memo("rpm_files", lambda: rpm_paths(self.rpm))

    @property
    def rpm_root(self) -> Path:
        return self._memo("rpm_root", lambda: extract_rpm(self.rpm, self.work / "rpm-root"))

    @property
    def deb_control(self) -> Path:
        return self._memo(
            "deb_control", lambda: extract_deb_control(self.deb, self.work / "deb-control")
        )

    @property
    def manifest(self) -> dict:
        path = self.app_root / "src-tauri" / "compatibility.json"
        if not path.is_file():
            raise CheckFailed(f"missing {path}; run scripts/stage_cli.py first")
        return self._memo("manifest", lambda: json.loads(path.read_text()))

    @property
    def conf(self) -> dict:
        path = self.app_root / "src-tauri" / "tauri.conf.json"
        if not path.is_file():
            raise CheckFailed(f"missing {path}")
        return self._memo("conf", lambda: json.loads(path.read_text()))

    def _memo(self, key: str, factory):
        if key not in self._cache:
            self._cache[key] = factory()
        return self._cache[key]

    def scratch(self, name: str) -> Path:
        path = self.work / "scratch" / name
        if path.exists():
            shutil.rmtree(path)
        path.mkdir(parents=True)
        return path

    def shipped_script(self, which: str) -> Path:
        """A maintainer script as extracted from the artifact, ready to run.

        Read out of the package rather than out of `src-tauri/packaging/`
        because a build that forgot to wire a script in would otherwise pass
        every assertion about a file that never shipped.
        """
        key = f"script-{which}"
        if key in self._cache:
            return self._cache[key]
        out = self.work / "shipped"
        out.mkdir(parents=True, exist_ok=True)
        if which in ("preinst", "postrm"):
            src = self.deb_control / which
            if not src.is_file():
                raise CheckFailed(f"deb ships no {which} maintainer script")
            text = src.read_text()
        else:
            scriptlets = rpm_scriptlets(self.rpm)
            rpm_key = {"prein": "preinstall", "postun": "postuninstall"}[which]
            if rpm_key not in scriptlets:
                raise CheckFailed(f"rpm ships no {rpm_key} scriptlet")
            text = scriptlets[rpm_key]
        path = out / f"{which}.sh"
        path.write_text(text)
        path.chmod(0o755)
        self._cache[key] = path
        return path


# --------------------------------------------------------------------------
# Checks
# --------------------------------------------------------------------------


def check_fresh_install_layout(ctx: Ctx) -> str:
    """Both packages must lay down all three binaries, executable.

    A sidecar that silently stopped being bundled leaves an app that launches
    and then fails on the first CLI call with a file-not-found nobody can act
    on.
    """
    expected = (APP_BINARY, *CLI_BINARIES)
    for label, root in (("deb", ctx.deb_root), ("rpm", ctx.rpm_root)):
        for name in expected:
            path = root / "usr" / "bin" / name
            need(path.is_file(), f"{label}: missing {UNIX_BIN_DIR}/{name}")
            need(
                os.access(path, os.X_OK),
                f"{label}: {UNIX_BIN_DIR}/{name} is not executable (mode "
                f"{path.stat().st_mode & 0o777:o})",
            )
    return f"deb and rpm both install {', '.join(expected)} into {UNIX_BIN_DIR}, all executable"


def check_cli_version_matches_manifest(ctx: Ctx) -> str:
    """Run the *extracted* CLI and hold it to the compatibility manifest.

    The manifest is what the app and `rocm install app` reason about. If the
    bundle picked up a stale sidecar, the manifest would describe a binary
    that is not the one a user ends up with -- so the version is read from the
    unpacked artifact, not from the staging directory.
    """
    entries = {entry["name"]: entry for entry in ctx.manifest["binaries"]}
    reported = []
    for label, root in (("deb", ctx.deb_root), ("rpm", ctx.rpm_root)):
        for name in CLI_BINARIES:
            entry = entries.get(name)
            need(entry is not None, f"compatibility.json has no entry for {name}")
            path = root / "usr" / "bin" / name
            need(path.is_file(), f"{label}: missing {UNIX_BIN_DIR}/{name}")

            digest = sha256_of(path)
            need(
                digest == entry["sha256"],
                f"{label}: {name} sha256 {digest[:16]}… != manifest {entry['sha256'][:16]}…",
            )
            need(
                path.stat().st_size == entry["sizeBytes"],
                f"{label}: {name} size {path.stat().st_size} != manifest {entry['sizeBytes']}",
            )

            result = run([str(path), "--version"])
            need(
                result.returncode == 0,
                f"{label}: {name} --version exited {result.returncode}: "
                f"{(result.stderr or result.stdout).strip()[:160]}",
            )
            printed = (result.stdout.strip() or result.stderr.strip()).splitlines()
            need(printed, f"{label}: {name} --version printed nothing")
            actual = printed[0].strip()
            need(
                actual == entry["version"],
                f"{label}: {name} version mismatch: binary says {actual!r}, "
                f"manifest says {entry['version']!r}",
            )
            reported.append(f"{label}/{name}={actual}")
    return "extracted binaries agree with compatibility.json: " + ", ".join(reported)


def check_cli_sibling_of_app(ctx: Ctx) -> str:
    """The CLI must land in the same directory as the app binary.

    The app resolves its CLI as `current_exe().parent().join("rocm")`. A
    packaging change that moved the sidecar into a resource directory would
    keep every checksum valid and break the app at runtime, so the location is
    read out of the package file list rather than assumed.
    """
    for label, paths in (("deb", ctx.deb_files), ("rpm", ctx.rpm_files)):
        located: dict[str, str] = {}
        for name in (APP_BINARY, *CLI_BINARIES):
            matches = [p for p in paths if p.rsplit("/", 1)[-1] == name]
            need(matches, f"{label}: {name} appears nowhere in the package file list")
            need(len(matches) == 1, f"{label}: {name} appears more than once: {matches}")
            located[name] = matches[0]
        app_dir = located[APP_BINARY].rsplit("/", 1)[0]
        need(
            app_dir == UNIX_BIN_DIR,
            f"{label}: {APP_BINARY} installs to {app_dir}, not {UNIX_BIN_DIR}",
        )
        for name in CLI_BINARIES:
            cli_dir = located[name].rsplit("/", 1)[0]
            need(
                cli_dir == app_dir,
                f"{label}: {located[name]} is not beside {located[APP_BINARY]} "
                f"-- current_exe().parent().join({name!r}) would not resolve",
            )
    return f"deb and rpm both put {', '.join(CLI_BINARIES)} beside {APP_BINARY} in {UNIX_BIN_DIR}"


def _guard_text_assertions(text: str, query: str, label: str) -> None:
    need(query in text, f"{label}: shipped script never asks {query.split()[0]} who owns the path")
    for name in CLI_BINARIES:
        need(
            name in text,
            f"{label}: shipped script does not mention {name}",
        )
    need("exit 1" in text, f"{label}: shipped script has no failing exit path")


def check_ownership_guard(ctx: Ctx) -> str:
    """Refusing to clobber an unowned /usr/bin/rocm, proved by running it.

    dpkg and rpm will not overwrite another *package's* file, but a binary a
    user compiled and copied into /usr/bin is owned by nobody: it would be
    overwritten on install and deleted on uninstall, with no trace of why.
    The guard is only worth anything if it fires, so the shipped script is
    executed against a fixture /usr/bin with a fake ownership tool.
    """
    cases = (
        ("deb", "preinst", ["install"], "dpkg-query", "dpkg-query -S"),
        ("rpm", "prein", ["1"], "rpm", "rpm -qf"),
    )
    executed: list[str] = []
    for label, which, args, tool, query in cases:
        script = ctx.shipped_script(which)
        _guard_text_assertions(script.read_text(), query, label)

        ok, reason = sandbox_status()
        if not ok:
            ctx.note(
                f"ownership-guard/{label}",
                f"shipped script inspected as text, not executed, because {reason}",
            )
            continue

        absent = run_with_fake_usr_bin(
            script, args, present=(), ownership_tool=tool, owned=False
        )
        need(
            absent.returncode == 0,
            f"{label}: nothing in {UNIX_BIN_DIR} yet, but the guard exited "
            f"{absent.returncode}: {absent.stderr.strip()[:200]}",
        )

        unowned = run_with_fake_usr_bin(
            script, args, present=CLI_BINARIES, ownership_tool=tool, owned=False
        )
        need(
            unowned.returncode != 0,
            f"{label}: {UNIX_BIN_DIR}/rocm exists and no package owns it, but the "
            f"guard exited 0 -- expected non-zero",
        )
        for name in CLI_BINARIES:
            need(
                f"{UNIX_BIN_DIR}/{name}" in unowned.stderr,
                f"{label}: refusal does not name {UNIX_BIN_DIR}/{name}; "
                f"stderr was {unowned.stderr.strip()[:200]!r}",
            )

        owned = run_with_fake_usr_bin(
            script, args, present=CLI_BINARIES, ownership_tool=tool, owned=True
        )
        need(
            owned.returncode == 0,
            f"{label}: a package-owned {UNIX_BIN_DIR}/rocm is an ordinary upgrade, "
            f"but the guard exited {owned.returncode}",
        )
        executed.append(f"{label}(absent=0, unowned!=0, owned=0)")

    if not executed:
        return "shipped guards inspected as text only -- see source-level notes"

    # dpkg calls preinst with abort-upgrade during a rollback. Refusing there
    # would strand a half-removed package, so the argument gate must hold.
    rollback = run_with_fake_usr_bin(
        ctx.shipped_script("preinst"),
        ["abort-upgrade", "0.1.0"],
        present=CLI_BINARIES,
        ownership_tool="dpkg-query",
        owned=False,
    )
    need(
        rollback.returncode == 0,
        f"deb: preinst abort-upgrade exited {rollback.returncode}; a rollback must "
        f"not be blocked by the ownership guard",
    )
    return "shipped guards executed: " + ", ".join(executed) + ", deb(abort-upgrade=0)"


def check_uninstall_removes_app_state(ctx: Ctx) -> str:
    """Uninstall takes the app's own state and nothing else.

    Two halves. The package must own /usr/bin/rocm -- that ownership is what
    makes dpkg remove it, and there is deliberately no second bookkeeping
    scheme beside it. And the postrm must delete exactly one autostart file
    per home: a glob there would remove other applications' entries on a
    shared machine, which is unrecoverable and invisible.
    """
    paths = ctx.deb_files
    for name in CLI_BINARIES:
        need(
            f"{UNIX_BIN_DIR}/{name}" in paths,
            f"deb does not own {UNIX_BIN_DIR}/{name}, so dpkg will leave it behind on removal",
        )

    script = ctx.shipped_script("postrm")
    text = script.read_text()
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("#") or " rm " not in f" {stripped} ":
            continue
        need(
            "*" not in stripped and "?" not in stripped,
            f"shipped postrm removes a glob: {stripped!r}",
        )
    need(
        AUTOSTART_ENTRY.rsplit("/", 1)[-1] in text,
        "shipped postrm never names rocm-app.desktop",
    )

    ok, reason = sandbox_status()
    if not ok:
        ctx.note(
            "uninstall-removes-app-state",
            f"shipped postrm inspected as text, not executed, because {reason}",
        )
        return f"deb owns {UNIX_BIN_DIR}/rocm and rocmd; postrm inspected as text only"

    work = ctx.scratch("postrm-remove")
    homes, root_home, ours, theirs = make_home_fixture(work)
    removal = run_with_fake_homes(script, ["remove"], homes, root_home)
    need(removal.returncode == 0, f"postrm remove exited {removal.returncode}")
    for entry in ours:
        need(not entry.exists(), f"postrm remove left our own autostart entry {entry}")
    for entry in theirs:
        need(
            entry.exists(),
            f"postrm remove deleted a stranger's autostart entry {entry.name} "
            f"under {entry.parent.parent.parent.name}",
        )

    work = ctx.scratch("postrm-upgrade")
    homes, root_home, ours, theirs = make_home_fixture(work)
    upgrade = run_with_fake_homes(script, ["upgrade"], homes, root_home)
    need(upgrade.returncode == 0, f"postrm upgrade exited {upgrade.returncode}")
    for entry in ours + theirs:
        need(entry.exists(), f"postrm upgrade removed {entry}, but an upgrade keeps user state")

    return (
        f"deb owns {UNIX_BIN_DIR}/rocm and rocmd; shipped postrm removed 3 rocm-app.desktop "
        f"entries and left 3 other-app.desktop entries; postrm upgrade removed nothing"
    )


def check_upgrade_preserves_autostart(ctx: Ctx) -> str:
    """`$1 == 1` means upgrade, and an upgrade must keep "start at login".

    rpm runs the same postuninstall scriptlet on upgrade as on erase, with
    only `$1` to tell them apart. Dropping that test silently turns off the
    user's autostart choice on every update -- the kind of regression that
    gets reported as "it stopped launching" months later.
    """
    script = ctx.shipped_script("postun")
    text = script.read_text()
    need(
        re.search(r'\[\s*"?\$1"?\s*=\s*"?0"?\s*\]', text) is not None,
        "shipped rpm postuninstall never tests $1, so it cannot tell erase from upgrade",
    )

    ok, reason = sandbox_status()
    if not ok:
        ctx.note(
            "upgrade-preserves-autostart",
            f"shipped scriptlet inspected as text, not executed, because {reason}",
        )
        return "shipped rpm postuninstall gates on $1 (text assertion only)"

    work = ctx.scratch("postun-upgrade")
    homes, root_home, ours, theirs = make_home_fixture(work)
    during_upgrade = run_with_fake_homes(script, ["1"], homes, root_home)
    need(during_upgrade.returncode == 0, f"postun 1 exited {during_upgrade.returncode}")
    for entry in ours:
        need(
            entry.exists(),
            f"upgrade ($1=1) removed {entry} -- the user's autostart choice must survive",
        )

    work = ctx.scratch("postun-erase")
    homes, root_home, ours, theirs = make_home_fixture(work)
    during_erase = run_with_fake_homes(script, ["0"], homes, root_home)
    need(during_erase.returncode == 0, f"postun 0 exited {during_erase.returncode}")
    for entry in ours:
        need(not entry.exists(), f"erase ($1=0) left {entry} behind")
    for entry in theirs:
        need(entry.exists(), f"erase ($1=0) deleted a stranger's entry {entry}")

    return "shipped rpm postuninstall: $1=1 kept all 3 autostart entries, $1=0 removed all 3"


def check_windows_config(ctx: Ctx) -> str:
    """The NSIS side, asserted from configuration because it cannot be built here.

    Per-user install mode is what makes the Windows story safe: $INSTDIR is
    this app's own directory, so the bundled rocm.exe cannot collide with a
    CLI the user installed elsewhere. A switch to perMachine would silently
    put our sidecars on the shared PATH.
    """
    nsis = ctx.conf.get("bundle", {}).get("windows", {}).get("nsis", {})
    need(nsis, "tauri.conf.json declares no bundle.windows.nsis block")
    mode = nsis.get("installMode")
    need(
        mode == "currentUser",
        f"bundle.windows.nsis.installMode is {mode!r}; perMachine would put the bundled "
        f"rocm.exe on the shared PATH where it can collide with a user's own CLI",
    )

    hooks_rel = nsis.get("installerHooks")
    need(hooks_rel, "bundle.windows.nsis.installerHooks is not set")
    hooks = (ctx.app_root / "src-tauri" / hooks_rel).resolve()
    need(hooks.is_file(), f"installerHooks points at {hooks}, which does not exist")
    ctx.note(
        "windows-config",
        "read from src-tauri/tauri.conf.json and packaging/nsis/hooks.nsh because no "
        "NSIS artifact is produced on a Linux host",
    )

    body = re.search(
        r"!macro\s+NSIS_HOOK_PREUNINSTALL(.*?)!macroend", hooks.read_text(), re.DOTALL
    )
    need(body is not None, f"{hooks.name} defines no NSIS_HOOK_PREUNINSTALL macro")
    preuninstall = body.group(1)
    need(
        "DeleteRegValue" in preuninstall,
        "NSIS_HOOK_PREUNINSTALL does not DeleteRegValue; the autostart Run entry would "
        "survive uninstall and point at a deleted binary",
    )
    need(
        r"Software\Microsoft\Windows\CurrentVersion\Run" in preuninstall,
        "NSIS_HOOK_PREUNINSTALL deletes a registry value, but not from the autostart Run key",
    )
    return (
        f"installMode=currentUser, installerHooks={hooks_rel} exists, "
        f"NSIS_HOOK_PREUNINSTALL deletes the HKCU Run value"
    )


def check_nsis_native_run(ctx: Ctx) -> str:
    """Never claim Windows installer behaviour from configuration alone."""
    reasons = []
    if shutil.which("makensis") is None:
        reasons.append("makensis is not installed")
    if sys.platform != "win32":
        reasons.append(f"host platform is {sys.platform}, not win32")
    nsis_bundles = sorted(ctx.bundle_dir.glob("nsis/*.exe")) if ctx.bundle_dir.is_dir() else []
    if not nsis_bundles:
        reasons.append("no nsis/*.exe bundle was produced")
    if reasons:
        raise SkipCheck(
            "the NSIS installer was not built, installed, upgraded or removed here: "
            + "; ".join(reasons)
            + ". Windows behaviour above is asserted from configuration only."
        )
    raise SkipCheck(
        "an NSIS installer exists but driving a Windows install requires a Windows host"
    )


def _driver_hits(values, origin: str) -> list[str]:
    hits = []
    for value in values:
        if DRIVER_NAME_PAT.search(value) or KERNEL_MODULE_PAT.search(value):
            hits.append(f"{origin}: {value}")
    return hits


def _config_strings(node) -> list[str]:
    if isinstance(node, str):
        return [node]
    if isinstance(node, dict):
        return [s for value in node.values() for s in _config_strings(value)]
    if isinstance(node, list):
        return [s for value in node for s in _config_strings(value)]
    return []


def check_no_driver_payload(ctx: Ctx) -> str:
    """No kernel driver, anywhere, by payload or by dependency.

    The product manages ROCm runtimes and never touches the kernel driver. A
    packaging change that pulled in amdgpu-dkms would make uninstalling this
    app capable of breaking a machine's graphics, and nobody reviews a
    dependency list they did not expect to change.
    """
    hits: list[str] = []
    hits += _driver_hits(ctx.deb_files, "deb payload")
    hits += _driver_hits(ctx.rpm_files, "rpm payload")

    fields = deb_control_fields(ctx.deb_control)
    for key in ("Depends", "Pre-Depends", "Recommends", "Suggests", "Enhances", "Breaks"):
        if fields.get(key):
            hits += _driver_hits([fields[key]], f"deb control {key}")

    requires = must_run(["rpm", "-qp", "--requires", str(ctx.rpm)]).stdout.splitlines()
    hits += _driver_hits([line.strip() for line in requires if line.strip()], "rpm Requires")

    hits += _driver_hits(_config_strings(ctx.conf.get("bundle", {})), "tauri.conf.json bundle")

    need(not hits, "driver payload or dependency found -- " + "; ".join(sorted(hits)[:6]))
    ctx.note(
        "no-driver-payload (bundle config half)",
        "the deb and rpm halves are matched against packaged paths and packaged "
        "dependency metadata, but src-tauri/tauri.conf.json is read from the source "
        "tree because it is a build input and never appears inside an artifact. "
        "Binary contents are deliberately not scanned: the CLI legitimately mentions "
        "amdgpu in its diagnostics text, so a byte scan would fail a correct package",
    )
    return (
        f"{len(ctx.deb_files)} deb paths, {len(ctx.rpm_files)} rpm paths, both "
        f"dependency lists and the bundle config carry no amdgpu/dkms/*.ko"
    )


CHECKS = (
    ("fresh-install-layout", check_fresh_install_layout),
    ("cli-version-matches-manifest", check_cli_version_matches_manifest),
    ("cli-sibling-of-app", check_cli_sibling_of_app),
    ("ownership-guard", check_ownership_guard),
    ("uninstall-removes-app-state", check_uninstall_removes_app_state),
    ("upgrade-preserves-autostart", check_upgrade_preserves_autostart),
    ("windows-config", check_windows_config),
    ("nsis-native-run", check_nsis_native_run),
    ("no-driver-payload", check_no_driver_payload),
)


@dataclass
class Outcome:
    name: str
    verdict: str
    detail: str


def run_checks(ctx: Ctx) -> list[Outcome]:
    outcomes = []
    for name, fn in CHECKS:
        try:
            outcomes.append(Outcome(name, PASS, fn(ctx)))
        except CheckFailed as exc:
            outcomes.append(Outcome(name, FAIL, str(exc)))
        except SkipCheck as exc:
            outcomes.append(Outcome(name, SKIP, str(exc)))
        except HarnessError as exc:
            outcomes.append(Outcome(name, FAIL, f"harness error: {exc}"))
    return outcomes


def report(outcomes: list[Outcome], ctx: Ctx) -> None:
    width = max(len(o.name) for o in outcomes)
    for outcome in outcomes:
        print(f"{outcome.verdict}  {outcome.name.ljust(width)}  {outcome.detail}")
    if ctx.source_level:
        print("\nasserted at source level rather than against the packaged artifact:")
        for note in ctx.source_level:
            print(f"  - {note}")
    failed = sum(1 for o in outcomes if o.verdict == FAIL)
    skipped = sum(1 for o in outcomes if o.verdict == SKIP)
    print(
        f"\n{len(outcomes) - failed - skipped} passed, {failed} failed, {skipped} skipped"
    )


# --------------------------------------------------------------------------
# Self-test
#
# The checks above are only worth their runtime if they fail when they should.
# So: build a conforming pair of packages, assert everything holds, then build
# one deliberately broken pair per check and assert that check -- and that
# check's own stated reason -- is what breaks.
#
# The fixture packages are real: a real ar/tar .deb that dpkg-deb reads, and a
# real rpmbuild .rpm that rpm queries. Only the binaries are substituted --
# they are /bin/sh scripts that answer `--version`, because the checks care
# about layout, mode, digest and printed version, none of which need ELF.
# --------------------------------------------------------------------------

FIXTURE_VERSIONS = {"rocm": "rocm 0.1.0", "rocmd": "rocmd 0.1.0"}


def fake_cli(name: str) -> bytes:
    return (
        "#!/bin/sh\n"
        'case "$1" in\n'
        f'  --version) echo "{FIXTURE_VERSIONS[name]}" ;;\n'
        '  *) echo "usage" >&2; exit 2 ;;\n'
        "esac\n"
    ).encode()


def build_deb_fixture(work: Path, out: Path, payload: dict[str, bytes], scripts: dict[str, str],
                      depends: str) -> Path:
    stage = work / "debstage"
    ctl = work / "debctl"
    for directory in (stage, ctl):
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True)
    for rel, data in payload.items():
        path = stage / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        path.chmod(0o755)
    (ctl / "control").write_text(
        "Package: rocm\nVersion: 0.1.0\nArchitecture: amd64\n"
        "Maintainer: mikeroysoft <packaging@example.invalid>\n"
        f"Depends: {depends}\nDescription: fixture\n"
    )
    for name, text in scripts.items():
        script = ctl / name
        script.write_text(text)
        script.chmod(0o755)
    (work / "debian-binary").write_text("2.0\n")
    must_run(["tar", "-C", str(ctl), "-czf", str(work / "control.tar.gz"), "."])
    must_run(["tar", "-C", str(stage), "-czf", str(work / "data.tar.gz"), "."])
    out.parent.mkdir(parents=True, exist_ok=True)
    if out.exists():
        out.unlink()
    must_run(
        ["ar", "rc", str(out.resolve()), "debian-binary", "control.tar.gz", "data.tar.gz"],
        cwd=work,
    )
    return out


def build_rpm_fixture(work: Path, out: Path, payload: dict[str, bytes], scripts: dict[str, str],
                      requires: list[str]) -> Path:
    top = work / "rpmtop"
    stage = work / "rpmstage"
    for directory in (top, stage):
        if directory.exists():
            shutil.rmtree(directory)
    stage.mkdir(parents=True)
    for name in ("SPECS", "BUILD", "RPMS", "SOURCES", "BUILDROOT"):
        (top / name).mkdir(parents=True)
    for rel, data in payload.items():
        path = stage / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        path.chmod(0o755)
    # rpm macro-expands scriptlet files too, so `%` has to survive as `%%`.
    for tag, text in scripts.items():
        (top / "SPECS" / f"{tag}.sh").write_text(text.replace("%", "%%"))
    spec = [
        "%global __os_install_post %{nil}",
        "%define _build_id_links none",
        "Name: ROCm",
        "Version: 0.1.0",
        "Release: 1",
        "Summary: fixture",
        "License: MIT",
        "BuildArch: x86_64",
        "AutoReqProv: no",
        *[f"Requires: {r}" for r in requires],
        "%description",
        "fixture",
        "%install",
        "mkdir -p %{buildroot}",
        f"cp -a {stage}/. %{{buildroot}}/",
        *[f"%{tag} -f {top / 'SPECS' / (tag + '.sh')}" for tag in scripts],
        "%files",
        *[f"/{rel}" for rel in payload],
    ]
    spec_path = top / "SPECS" / "fixture.spec"
    spec_path.write_text("\n".join(spec) + "\n")
    must_run(["rpmbuild", "-bb", "--define", f"_topdir {top}", str(spec_path)])
    built = sorted(top.glob("RPMS/*/*.rpm"))
    if not built:
        raise HarnessError("rpmbuild produced no package")
    out.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(built[0], out)
    return out


def build_fixture_tree(root: Path, mutation: str | None) -> Ctx:
    """A miniature rocm-app tree with a real deb and rpm under it."""
    app = root / "app"
    src_tauri = app / "src-tauri"
    packaging = src_tauri / "packaging"
    shutil.copytree(APP_ROOT / "src-tauri" / "packaging", packaging)
    shutil.copy2(APP_ROOT / "src-tauri" / "tauri.conf.json", src_tauri / "tauri.conf.json")

    bin_dir = "usr/bin"
    if mutation == "sibling":
        bin_dir = "usr/lib/ROCm"
    payload: dict[str, bytes] = {"usr/bin/rocm-app": b"#!/bin/sh\nexit 0\n"}
    for name in CLI_BINARIES:
        if mutation == "layout" and name == "rocm":
            continue
        payload[f"{bin_dir}/{name}"] = fake_cli(name)
    if mutation == "driver":
        payload["usr/lib/modules/6.0.0/extra/amdgpu.ko"] = b"\x7fELF fixture\n"

    manifest = {
        "schemaVersion": 1,
        "appVersion": "0.1.0",
        "target": "x86_64-unknown-linux-gnu",
        "stagedAtUnixMs": 0,
        "sourceCommit": "0" * 40,
        "binaries": [],
    }
    for name in CLI_BINARIES:
        data = fake_cli(name)
        version = FIXTURE_VERSIONS[name]
        if mutation == "version" and name == "rocm":
            version = "rocm 9.9.9"
        manifest["binaries"].append(
            {
                "name": name,
                "fileName": f"{name}-x86_64-unknown-linux-gnu",
                "version": version,
                "sizeBytes": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
            }
        )
    (src_tauri / "compatibility.json").write_text(json.dumps(manifest, indent=2) + "\n")

    if mutation == "nsis":
        conf = json.loads((src_tauri / "tauri.conf.json").read_text())
        conf["bundle"]["windows"]["nsis"]["installMode"] = "perMachine"
        (src_tauri / "tauri.conf.json").write_text(json.dumps(conf, indent=2) + "\n")

    preinst = (packaging / "deb" / "preinst").read_text()
    postrm = (packaging / "deb" / "postrm").read_text()
    prein = (packaging / "rpm" / "preinstall.sh").read_text()
    postun = (packaging / "rpm" / "postremove.sh").read_text()
    if mutation == "guard":
        preinst = "#!/bin/sh\nexit 0\n"
        prein = "#!/bin/sh\nexit 0\n"
    if mutation == "guard_silent":
        # Looks right to a reader and to a text scan -- mentions both binaries,
        # queries ownership, has a failing exit path -- but warns instead of
        # refusing. Only running it reveals that the guard does not guard.
        preinst = (
            "#!/bin/sh\nset -e\n"
            'case "$1" in install|upgrade) ;; *) exit 0 ;; esac\n'
            "for binary in rocm rocmd; do\n"
            '  path="/usr/bin/$binary"\n'
            '  [ -e "$path" ] || continue\n'
            '  dpkg-query -S "$path" >/dev/null 2>&1 || echo "warning: $path" >&2\n'
            "done\n"
            'if [ "$UNREACHABLE" = "yes" ]; then exit 1; fi\n'
            "exit 0\n"
        )
        prein = (
            "#!/bin/sh\nset -e\n"
            "for binary in rocm rocmd; do\n"
            '  path="/usr/bin/$binary"\n'
            '  [ -e "$path" ] || continue\n'
            '  rpm -qf "$path" >/dev/null 2>&1 || echo "warning: $path" >&2\n'
            "done\n"
            'if [ "$UNREACHABLE" = "yes" ]; then exit 1; fi\n'
            "exit 0\n"
        )
    if mutation == "postrm_glob":
        postrm = (
            "#!/bin/sh\nset -e\n"
            'case "$1" in remove|purge) ;; *) exit 0 ;; esac\n'
            "for home in /home/* /root; do\n"
            '  [ -d "$home" ] || continue\n'
            '  rm -f "$home/.config/autostart/"*.desktop\n'
            "done\nexit 0\n"
        )
    if mutation == "postrm_find":
        # The same over-broad deletion with no `rm` and no glob on an rm line,
        # so the text scan cannot see it. Executing it can.
        postrm = (
            "#!/bin/sh\nset -e\n"
            'case "$1" in remove|purge) ;; *) exit 0 ;; esac\n'
            "for home in /home/* /root; do\n"
            '  [ -d "$home/.config/autostart" ] || continue\n'
            '  find "$home/.config/autostart" -name "rocm-app.desktop" -delete\n'
            '  find "$home/.config/autostart" -name "other-app.desktop" -delete\n'
            "done\nexit 0\n"
        )
    if mutation == "upgrade":
        postun = postun.replace('[ "$1" = "0" ] || exit 0', 'true')
    if mutation == "upgrade_inverted":
        # Still tests $1, so the text assertion is satisfied -- but the sense is
        # backwards, which is exactly the typo that erases autostart on upgrade.
        postun = postun.replace('[ "$1" = "0" ] || exit 0', '[ "$1" = "0" ] && exit 0')

    depends = "libwebkit2gtk-4.1-0"
    requires = ["webkit2gtk4.1"]
    if mutation == "driver":
        depends += ", amdgpu-dkms"
        requires.append("amdgpu-dkms")

    bundle = src_tauri / "target" / "release" / "bundle"
    build_deb_fixture(
        root / "build",
        bundle / "deb" / "ROCm_0.1.0_amd64.deb",
        payload,
        {"preinst": preinst, "postrm": postrm},
        depends,
    )
    build_rpm_fixture(
        root / "build",
        bundle / "rpm" / "ROCm-0.1.0-1.x86_64.rpm",
        payload,
        {"pre": prein, "postun": postun},
        requires,
    )
    work = root / "work"
    work.mkdir(parents=True, exist_ok=True)
    return Ctx(app_root=app, work=work)


# Each mutation must break exactly the check it targets, with a reason that
# names the actual defect rather than some downstream symptom. The last field
# says whether catching it needs the sandbox: those three defects are written
# to survive text inspection, so they are the ones that prove the executed
# assertions do work rather than merely run.
EXPECTATIONS = (
    ("layout", "fresh-install-layout", r"deb: missing /usr/bin/rocm\b", False),
    ("version", "cli-version-matches-manifest", r"rocm version mismatch", False),
    ("sibling", "cli-sibling-of-app", r"is not beside /usr/bin/rocm-app", False),
    ("guard", "ownership-guard", r"never asks dpkg-query who owns", False),
    ("guard_silent", "ownership-guard", r"expected non-zero", True),
    ("postrm_glob", "uninstall-removes-app-state", r"removes a glob", False),
    ("postrm_find", "uninstall-removes-app-state", r"deleted a stranger's autostart", True),
    ("upgrade", "upgrade-preserves-autostart", r"never tests \$1", False),
    ("upgrade_inverted", "upgrade-preserves-autostart", r"upgrade \(\$1=1\) removed", True),
    ("nsis", "windows-config", r"installMode is 'perMachine'", False),
    ("driver", "no-driver-payload", r"amdgpu", False),
)


def self_test() -> int:
    sandbox_ok, sandbox_reason = sandbox_status()
    print(f"sandbox: {'bubblewrap' if sandbox_ok else 'unavailable -- ' + sandbox_reason}")
    root = Path(tempfile.mkdtemp(prefix="installer-acceptance-selftest-", dir=_tmp_base()))
    problems: list[str] = []
    try:
        ctx = build_fixture_tree(root / "conforming", None)
        outcomes = run_checks(ctx)
        width = max(len(o.name) for o in outcomes)
        for outcome in outcomes:
            if outcome.verdict == FAIL:
                problems.append(f"conforming fixture failed {outcome.name}: {outcome.detail}")
            print(
                f"  conforming  {outcome.verdict}  {outcome.name.ljust(width)}  "
                f"{outcome.detail[:110]}"
            )

        for mutation, target, pattern, needs_sandbox in EXPECTATIONS:
            if needs_sandbox and not sandbox_ok:
                print(f"  broken/{mutation:<17} SKIP  {target}: needs the sandbox")
                continue
            ctx = build_fixture_tree(root / mutation, mutation)
            outcomes = {o.name: o for o in run_checks(ctx)}
            outcome = outcomes[target]
            if outcome.verdict != FAIL:
                problems.append(
                    f"{mutation}: expected {target} to FAIL, got {outcome.verdict} "
                    f"({outcome.detail})"
                )
                print(f"  broken/{mutation:<17} NOT DETECTED by {target}")
                continue
            if not re.search(pattern, outcome.detail):
                problems.append(
                    f"{mutation}: {target} failed for the wrong reason: {outcome.detail!r} "
                    f"does not match /{pattern}/"
                )
                print(f"  broken/{mutation:<17} WRONG REASON  {target}: {outcome.detail}")
                continue
            print(f"  broken/{mutation:<17} FAIL  {target}: {outcome.detail[:100]}")
    finally:
        shutil.rmtree(root, ignore_errors=True)

    print()
    if problems:
        for problem in problems:
            print(f"FAIL  {problem}")
        print(f"\nself-test failed: {len(problems)} expectation(s) did not hold")
        return 1
    print(
        f"self-test passed: conforming fixture clean, {len(EXPECTATIONS)} broken fixtures each "
        f"caught by the intended check for the intended reason"
    )
    if not sandbox_ok:
        print(
            "note: maintainer scripts were asserted as text only on this host "
            f"({sandbox_reason}); install bubblewrap to execute them"
        )
    return 0


def _tmp_base() -> str:
    """Keep scratch out of /home: the sandbox rebinds /home over it."""
    return "/tmp" if Path("/tmp").is_dir() else tempfile.gettempdir()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="verify the checks against synthetic packages instead of the real bundles",
    )
    args = parser.parse_args()
    if args.self_test:
        return self_test()

    work = Path(tempfile.mkdtemp(prefix="installer-acceptance-", dir=_tmp_base()))
    try:
        ctx = Ctx(app_root=APP_ROOT, work=work)
        outcomes = run_checks(ctx)
        report(outcomes, ctx)
        return 1 if any(o.verdict == FAIL for o in outcomes) else 0
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
