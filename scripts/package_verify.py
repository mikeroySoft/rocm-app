#!/usr/bin/env python3
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
"""Verify the installers this repo produces, before anyone can download one.

Installing ROCm App installs the CLI, and installing the CLI never installs
ROCm App. Both halves of that promise are made by configuration — an
`externalBin` entry here, the absence of an app payload over in rocm-cli — and
configuration fails quietly. A dropped `externalBin` still produces a bundle
that installs, launches, and cannot manage anything; a CLI release archive that
picked up a `.desktop` file still installs. Neither shows up in a build log.

So this reads the finished artifacts instead of the config that made them: what
is actually inside the deb and the rpm, hashed against the compatibility
manifest `stage_cli.py` recorded, and what is actually named by the CLI's own
release scripts. It also writes the `.sha256`/`.sig` sidecars the download path
expects, and can emit the `AppReleaseManifest` that `rocm install app` consumes
— then hands that manifest back to the real `rocm` binary to prove the two ends
agree about the artifacts this script just verified.

Usage:
    python3 scripts/package_verify.py
    python3 scripts/package_verify.py --require-signatures
    python3 scripts/package_verify.py --emit-manifest dist/app.json --base-url https://example/dl
    python3 scripts/package_verify.py --self-test
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Mapping

APP_ROOT = Path(__file__).resolve().parent.parent
def _cli_root() -> Path:
    """Where the rocm-cli checkout is.

    A sibling directory on a developer machine, but CI clones it *inside* the
    workspace, and the first Windows run failed four checks on a path that
    simply was not there. `ROCM_CLI_REPO` is the same variable the live contract
    harness already uses, so one setting covers both.
    """
    explicit = os.environ.get("ROCM_CLI_REPO")
    if explicit:
        return Path(explicit)
    for candidate in (APP_ROOT / "rocm-cli", APP_ROOT.parent / "rocm-cli"):
        if candidate.is_dir():
            return candidate
    return APP_ROOT.parent / "rocm-cli"


CLI_ROOT = _cli_root()

# Every bundle must carry these three beside each other. `rocm-app` alone is the
# silent failure this list exists to catch: the app installs and runs, and every
# action it offers fails because the tool it drives was never shipped.
CLI_MEMBERS = ("usr/bin/rocm", "usr/bin/rocmd")
APP_MEMBER = "usr/bin/rocm-app"

# The arch token each bundler writes into a filename, for the one architecture
# `rocm install app` accepts. Anything else is refused up front rather than
# silently checked against names that could never match.
ARCH_TOKEN = {"deb": "amd64", "rpm": "x86_64", "nsis": "x64"}
# The one architecture this product supports, under every name a host calls it.
# Linux `platform.machine()` says `x86_64` and Windows says `AMD64`; treating
# those as different architectures failed the Windows CI job after it had
# already built a correct installer.
SUPPORTED_MACHINES = frozenset({"x86_64", "amd64"})

# What the release manifest calls it. `rocm install app` matches an asset on
# this exact string, so it is the contract's spelling and never the host's.
MANIFEST_ARCH = "x86_64"


def host_machine_supported() -> bool:
    return platform.machine().lower() in SUPPORTED_MACHINES

# Which bundle targets this host can produce. An absent nsis directory on Linux
# is a fact about the host, not a defect in the release.
HOST_TARGETS = {"linux": ("deb", "rpm"), "win32": ("nsis",)}

# Sidecars this script owns. Present or absent, they are never orphans.
SIDECAR_SUFFIXES = (".sha256", ".sig")

# The four rocm-cli entry points that must never mention an app payload, and the
# strings that would prove they do.
CLI_ONLY_SOURCES = {
    "scripts/package-linux-release.sh": (
        "rocm-app",
        ".deb",
        ".rpm",
        "-setup.exe",
        ".desktop",
        "autostart",
    ),
    "scripts/package-windows-release.ps1": (
        "rocm-app",
        ".deb",
        ".rpm",
        "-setup.exe",
        ".desktop",
        "autostart",
    ),
    "install.sh": ("rocm-app", "install app", "autostart", ".desktop"),
    "install.ps1": ("rocm-app", "install app", "autostart", ".desktop"),
}

# Where a built CLI release archive would sit. Checked so "no app payload" is a
# statement about a real archive when one exists, not only about scripts.
CLI_ARCHIVE_PATTERNS = ("dist/*.tar.gz", "dist/*.zip", "target/dist/*.tar.gz", "target/dist/*.zip")

APP_MANIFEST_SCHEMA_VERSION = 1


class VerifyError(RuntimeError):
    """A check could not run at all, as opposed to running and failing."""


@dataclass
class Ctx:
    """Everything a check needs to run somewhere other than this repo.

    The roots and the environment are parameters, not globals, so `--self-test`
    can point the same code at synthetic artifacts under a temp root. A
    self-test that exercised a separate copy of the logic would prove only that
    the copy works.
    """

    app_root: Path = APP_ROOT
    cli_root: Path = CLI_ROOT
    require_signatures: bool = False
    env: Mapping[str, str] = field(default_factory=lambda: os.environ)

    @property
    def bundle_dir(self) -> Path:
        return self.app_root / "src-tauri" / "target" / "release" / "bundle"

    @property
    def tauri_conf(self) -> Path:
        return self.app_root / "src-tauri" / "tauri.conf.json"

    @property
    def compatibility(self) -> Path:
        return self.app_root / "src-tauri" / "compatibility.json"


@dataclass
class Report:
    """Accumulated findings. Checks record and continue rather than raising.

    One failure usually implies others, and a run that stops at the first one
    hides them — the reader fixes it, reruns, and finds the next. Every check
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


@dataclass
class Artifact:
    target: str
    path: Path
    size: int
    sha256: str
    signature_b64: str = ""


def sha256_stream(handle) -> str:
    digest = hashlib.sha256()
    for chunk in iter(lambda: handle.read(1 << 20), b""):
        digest.update(chunk)
    return digest.hexdigest()


def sha256_file(path: Path) -> str:
    with path.open("rb") as handle:
        return sha256_stream(handle)


def member_name(raw: str) -> str:
    """Normalize an archive member to a comparable path.

    A deb's own `data.tar.gz` names members `./usr/bin/rocm`, `dpkg-deb
    --fsys-tarfile` re-emits them as `usr/bin/rocm`, and `rpm -qlp` prints
    `/usr/bin/rocm`. All three are the same file; comparing them literally makes
    a present binary look missing.
    """
    return raw.removeprefix("./").removeprefix("/")


def product_and_version(ctx: Ctx) -> tuple[str, str]:
    conf = json.loads(ctx.tauri_conf.read_text())
    return conf["productName"], conf["version"]


def expected_names(product: str, version: str) -> dict[str, str]:
    """The exact filename each bundler writes, derived from the config.

    Hardcoding these would make the check agree with itself after a rename
    instead of agreeing with the config the bundler read.
    """
    return {
        "deb": f"{product}_{version}_{ARCH_TOKEN['deb']}.deb",
        "rpm": f"{product}-{version}-1.{ARCH_TOKEN['rpm']}.rpm",
        "nsis": f"{product}_{version}_{ARCH_TOKEN['nsis']}-setup.exe",
    }


def buildable_here() -> tuple[str, ...]:
    return HOST_TARGETS.get(sys.platform, ())


def discover(ctx: Ctx, expected: dict[str, str], report: Report) -> list[Artifact]:
    """Find each artifact and record its identity.

    A target this host cannot build is reported and skipped. A target it can
    build and did not is a failure: a release that quietly shipped one of the
    two Linux packages is the accident worth catching here.
    """
    report.head("artifacts")
    artifacts: list[Artifact] = []
    for target, name in expected.items():
        path = ctx.bundle_dir / target / name
        if not path.is_file():
            if target in buildable_here():
                report.fail(f"{target:<5} missing {path}")
            else:
                report.note(f"{target:<5} not built on this host ({name})")
            continue
        digest = sha256_file(path)
        size = path.stat().st_size
        artifacts.append(Artifact(target=target, path=path, size=size, sha256=digest))
        report.ok(f"{target:<5} {name:<32} {size:>12,} bytes  {digest[:16]}…")
    return artifacts


def check_asset_set(ctx: Ctx, expected: dict[str, str], report: Report) -> None:
    """Assert each bundle directory holds exactly its expected artifact.

    A stale `.deb` from an earlier version sitting beside the current one is a
    release accident: both are uploadable, both look plausible, and the older
    one installs an app whose CLI no longer matches. Directories are skipped —
    the bundler leaves its staging tree beside the package it produced, and that
    is scratch, not a shippable asset.
    """
    report.head("asset set")
    for target, name in expected.items():
        directory = ctx.bundle_dir / target
        if not directory.is_dir():
            continue
        allowed = {name, *(name + suffix for suffix in SIDECAR_SUFFIXES)}
        present = {entry.name for entry in directory.iterdir() if entry.is_file()}
        if orphans := sorted(present - allowed):
            report.fail(f"{target:<5} unaccounted artifacts beside {name}: {orphans}")
        else:
            report.ok(f"{target:<5} exactly {name} (+ sidecars), no orphans")
        if scratch := sorted(e.name for e in directory.iterdir() if e.is_dir()):
            report.note(f"{target:<5} bundler scratch directories ignored: {scratch}")


def scan_deb(path: Path, wanted: set[str]) -> tuple[set[str], dict[str, str]]:
    """List a deb's `data.tar.*` and hash the wanted members in one pass.

    Streamed through `dpkg-deb --fsys-tarfile` so any payload compression the
    bundler picks (gzip today, zstd on some hosts) is the packaging tool's
    problem rather than this script's.
    """
    proc = subprocess.Popen(
        ["dpkg-deb", "--fsys-tarfile", str(path)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    names: set[str] = set()
    digests: dict[str, str] = {}
    try:
        with tarfile.open(fileobj=proc.stdout, mode="r|*") as archive:
            for member in archive:
                name = member_name(member.name)
                names.add(name)
                if name in wanted and member.isfile():
                    extracted = archive.extractfile(member)
                    if extracted is not None:
                        digests[name] = sha256_stream(extracted)
    finally:
        # stdout closes first: on an early exit dpkg-deb may be blocked writing
        # to a full pipe, and reading stderr before releasing it would deadlock.
        proc.stdout.close()
        stderr = proc.stderr.read().decode(errors="replace")
        proc.stderr.close()
        code = proc.wait()
    if code != 0 and not names:
        raise VerifyError(f"dpkg-deb could not read {path.name}: {stderr.strip()}")
    return names, digests


def scan_rpm(path: Path, wanted: set[str]) -> tuple[set[str], dict[str, str]]:
    """List an rpm with `rpm -qlp` and hash the wanted members.

    `rpm -qlp` exits non-zero on a host with no rpm database even though it
    prints the listing correctly (it fails to take a transaction lock it does
    not need for a file query). Treating that exit code as fatal would make this
    check unrunnable on any Debian host, so an empty listing — not the exit
    code — is what counts as failure.
    """
    listing = subprocess.run(
        ["rpm", "-qlp", str(path)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    names = {member_name(line) for line in listing.stdout.split() if line.strip()}
    if not names:
        raise VerifyError(f"rpm -qlp produced no listing for {path.name}: {listing.stderr.strip()}")

    digests: dict[str, str] = {}
    with tempfile.TemporaryDirectory(prefix="rocm-rpm-") as scratch:
        # `rpm2archive -n` writes a plain tar to stdout, which the standard
        # library reads directly. The obvious alternative, `rpm2cpio | cpio`,
        # went wrong twice: `rpm2cpio` is not installed on this development host
        # at all, and on the CI runner it exited non-zero with nothing on stderr
        # while `rpm -qlp` worked fine. One tool, one stream, no pipe, no
        # external extractor.
        payload = Path(scratch) / "payload.tar"
        with payload.open("wb") as sink:
            unpacked = subprocess.run(
                ["rpm2archive", "-n", str(path)],
                stdout=sink,
                stderr=subprocess.PIPE,
                check=False,
            )
        if unpacked.returncode != 0 or payload.stat().st_size == 0:
            raise VerifyError(
                f"rpm2archive failed for {path.name} "
                f"(exit {unpacked.returncode}, {payload.stat().st_size} bytes): "
                f"{unpacked.stderr.decode(errors='replace').strip()}"
            )

        with tarfile.open(payload) as archive:
            for member in archive:
                name = member_name(member.name)
                if name not in wanted or not member.isfile():
                    continue
                handle = archive.extractfile(member)
                if handle is None:
                    continue
                digest = hashlib.sha256()
                for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                    digest.update(chunk)
                digests[name] = digest.hexdigest()
    return names, digests


def check_embedded_cli(ctx: Ctx, artifacts: list[Artifact], report: Report) -> None:
    """Prove the staged CLI is inside each artifact, byte for byte.

    This is the load-bearing check for "installing the app installs the CLI". A
    config change that dropped `externalBin` produces a bundle that builds,
    installs, and launches, and every hash below would have nothing to compare
    against — which is exactly why the comparison is against
    `compatibility.json` rather than against whatever the artifact happens to
    contain.
    """
    report.head("embedded CLI")
    if not ctx.compatibility.is_file():
        report.fail(f"compatibility manifest is missing: {ctx.compatibility}")
        return
    manifest = json.loads(ctx.compatibility.read_text())
    staged = {entry["name"]: entry["sha256"] for entry in manifest["binaries"]}
    wanted = set(CLI_MEMBERS)
    required = wanted | {APP_MEMBER}

    for artifact in artifacts:
        scanner = {"deb": scan_deb, "rpm": scan_rpm}.get(artifact.target)
        if scanner is None:
            report.note(f"{artifact.target:<5} contents not inspected")
            continue
        try:
            names, digests = scanner(artifact.path, wanted)
        except VerifyError as error:
            report.fail(f"{artifact.target:<5} {error}")
            continue
        if missing := sorted(required - names):
            report.fail(f"{artifact.target:<5} does not install {missing}")
            continue
        report.ok(f"{artifact.target:<5} installs {', '.join('/' + m for m in sorted(required))}")
        for member in sorted(wanted):
            name = Path(member).name
            actual = digests.get(member)
            expected = staged.get(name)
            if expected is None:
                report.fail(f"{artifact.target:<5} /{member} is not named by compatibility.json")
            elif actual != expected:
                report.fail(
                    f"{artifact.target:<5} /{member} sha256 {actual} != staged {expected}"
                )
            else:
                report.ok(f"{artifact.target:<5} /{member} matches staged {expected[:16]}…")


def forbidden_hits(text: str, tokens: tuple[str, ...]) -> list[str]:
    lowered = text.lower()
    return [token for token in tokens if token in lowered]


def check_cli_only(ctx: Ctx, report: Report) -> None:
    """Assert the CLI's own release path carries no app payload.

    The asymmetry is the product decision: ROCm App installs the CLI, the CLI
    never installs ROCm App. Nothing in this repo enforces it — it holds only as
    long as rocm-cli's packaging and install scripts stay free of app payload.
    A `.desktop` file or an autostart entry landing there would ship a tray app
    to everyone who ran `install.sh`, which is precisely the surprise the
    asymmetry exists to prevent.
    """
    report.head("CLI-only proof")
    clean = True
    for relative, tokens in CLI_ONLY_SOURCES.items():
        source = ctx.cli_root / relative
        if not source.is_file():
            report.fail(f"cannot prove absence in a missing file: {source}")
            clean = False
            continue
        if hits := forbidden_hits(source.read_text(errors="replace"), tokens):
            report.fail(f"{relative} mentions app payload: {hits}")
            clean = False

    archives = [
        path
        for pattern in CLI_ARCHIVE_PATTERNS
        for path in sorted(ctx.cli_root.glob(pattern))
    ]
    if not archives:
        report.note(
            "no CLI release archive present to inspect "
            f"(looked for {', '.join(CLI_ARCHIVE_PATTERNS)} under {ctx.cli_root})"
        )
    for archive in archives:
        members = archive_members(archive)
        tokens = ("rocm-app", ".deb", ".rpm", "-setup.exe", ".desktop", "autostart")
        if hits := sorted({t for m in members for t in forbidden_hits(m, tokens)}):
            report.fail(f"{archive.name} contains app payload: {hits}")
            clean = False
        else:
            report.ok(f"{archive.name}: {len(members)} members, none app payload")

    if clean:
        report.ok(f"CLI-only: no app payload in {', '.join(CLI_ONLY_SOURCES)}")


def archive_members(archive: Path) -> list[str]:
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as bundle:
            return bundle.namelist()
    with tarfile.open(archive) as bundle:
        return bundle.getnames()


def resolve_private_key(ctx: Ctx, scratch: Path) -> Path | None:
    if path := ctx.env.get("ROCM_APP_SIGNING_PRIVATE_KEY_PATH"):
        key = Path(path)
        if not key.is_file():
            raise VerifyError(f"ROCM_APP_SIGNING_PRIVATE_KEY_PATH is not a file: {key}")
        return key
    if pem := ctx.env.get("ROCM_APP_SIGNING_PRIVATE_KEY_PEM"):
        key = scratch / "signing-private-key.pem"
        key.write_text(pem if pem.endswith("\n") else pem + "\n")
        key.chmod(0o600)
        return key
    return None


def resolve_public_key(ctx: Ctx, private_key: Path, scratch: Path) -> Path:
    """The key a signature is checked against.

    Prefers a configured public key, because that is the key users will pin: a
    signature that verifies only against the private half proves the file is
    intact but not that it was signed with the published identity. Derived from
    the private key when none is configured, so the verify step still runs and
    still catches a stale or truncated `.sig`.
    """
    if path := ctx.env.get("ROCM_APP_SIGNING_PUBLIC_KEY_PATH"):
        return Path(path)
    if pem := ctx.env.get("ROCM_APP_SIGNING_PUBLIC_KEY_PEM"):
        key = scratch / "signing-public-key.pem"
        key.write_text(pem if pem.endswith("\n") else pem + "\n")
        return key
    key = scratch / "derived-public-key.pem"
    result = subprocess.run(
        ["openssl", "rsa", "-in", str(private_key), "-pubout", "-out", str(key)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise VerifyError(f"could not derive a public key: {result.stderr.strip()}")
    return key


def cargo_xtask(ctx: Ctx, args: list[str]) -> subprocess.CompletedProcess[str]:
    """Run rocm-cli's signing task runner from its repo root.

    Signing is rocm-cli's scheme (raw RSASSA-PKCS#1 v1.5 over SHA-256, verified
    against a SubjectPublicKeyInfo PEM), not Tauri's Minisign updater, so the
    signatures this repo emits are the ones `rocm install app` already knows how
    to check. Paths are absolutized because the subprocess runs from that root.
    """
    cargo = shutil.which("cargo")
    if cargo is None:
        raise VerifyError("cargo is required to sign or verify artifacts")
    return subprocess.run(
        [cargo, "xtask", *args],
        cwd=ctx.cli_root,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        check=False,
    )


def check_checksums_and_signatures(ctx: Ctx, artifacts: list[Artifact], report: Report) -> None:
    """Write the download sidecars, and refuse to overwrite a disagreeing one.

    `<artifact>.sha256` is written in the one format the installers parse. An
    existing sidecar that disagrees is a failure rather than something to
    silently correct: it means the artifact changed after the checksum was
    published, and rewriting it would launder exactly the mismatch a checksum
    exists to expose.
    """
    report.head("checksums and signatures")
    with tempfile.TemporaryDirectory(prefix="rocm-signing-") as scratch_name:
        scratch = Path(scratch_name)
        try:
            private_key = resolve_private_key(ctx, scratch)
        except VerifyError as error:
            report.fail(str(error))
            private_key = None

        if private_key is None:
            if ctx.require_signatures:
                report.fail(
                    "--require-signatures was given but no signing key is configured "
                    "(set ROCM_APP_SIGNING_PRIVATE_KEY_PATH or ..._PEM)"
                )
            else:
                report.note("unsigned (no key configured)")

        public_key: Path | None = None
        for artifact in artifacts:
            sidecar = artifact.path.with_name(artifact.path.name + ".sha256")
            content = f"{artifact.sha256}  {artifact.path.name}\n"
            stale = sidecar.is_file() and sidecar.read_text() != content
            if stale and ctx.require_signatures:
                # On the release path nothing may have changed since the
                # artifact was signed, so a disagreeing sidecar is a hard stop:
                # it means the bytes moved after somebody vouched for them.
                report.fail(f"{sidecar.name} does not match the artifact on disk")
                continue
            sidecar.write_text(content)
            if stale:
                # Off the release path a rebuild legitimately produces new
                # bytes, and the previous run's sidecar describes an artifact
                # that no longer exists. Refusing here would make the documented
                # `tauri build` then `package:verify` order impossible to run.
                report.note(f"{sidecar.name} refreshed (the artifact changed since the last run)")
            else:
                report.ok(f"{sidecar.name}")

            if private_key is None:
                continue
            signature = artifact.path.with_name(artifact.path.name + ".sig")
            try:
                if public_key is None:
                    public_key = resolve_public_key(ctx, private_key, scratch)
                signed = cargo_xtask(
                    ctx,
                    [
                        "sign",
                        "--private-key",
                        str(private_key.resolve()),
                        "--in",
                        str(artifact.path.resolve()),
                        "--out",
                        str(signature.resolve()),
                    ],
                )
                if signed.returncode != 0:
                    report.fail(f"signing {artifact.path.name} failed: {signed.stdout.strip()}")
                    continue
                verified = cargo_xtask(
                    ctx,
                    [
                        "verify",
                        "--public-key",
                        str(public_key.resolve()),
                        "--in",
                        str(artifact.path.resolve()),
                        "--signature",
                        str(signature.resolve()),
                    ],
                )
                if verified.returncode != 0:
                    report.fail(
                        f"signature for {artifact.path.name} does not verify: "
                        f"{verified.stdout.strip()}"
                    )
                    continue
            except VerifyError as error:
                report.fail(str(error))
                continue
            artifact.signature_b64 = base64.b64encode(signature.read_bytes()).decode()
            report.ok(f"{signature.name} verified against {public_key.name}")


def staged_cli_version(ctx: Ctx) -> str:
    """The CLI version this app build was staged against.

    `compatibility.json` records what the binary printed, so the compatible
    range published to `rocm install app` describes a CLI that exists rather
    than a range someone typed.
    """
    manifest = json.loads(ctx.compatibility.read_text())
    for entry in manifest["binaries"]:
        if entry["name"] == "rocm":
            return entry["version"].split()[-1]
    raise VerifyError("compatibility.json names no rocm binary")


def emit_manifest(
    ctx: Ctx, artifacts: list[Artifact], destination: Path, base_url: str, report: Report
) -> None:
    """Write the `AppReleaseManifest` that `rocm install app` consumes."""
    report.head("app release manifest")
    _, version = product_and_version(ctx)
    cli_version = staged_cli_version(ctx)
    conf = json.loads(ctx.tauri_conf.read_text())
    homepage = conf.get("bundle", {}).get("homepage", "").rstrip("/")
    prefix = base_url.rstrip("/")
    manifest = {
        "schemaVersion": APP_MANIFEST_SCHEMA_VERSION,
        "appVersion": version,
        "compatibleCli": {"min": cli_version, "max": cli_version},
        "publishedAtUnixMs": int(time.time() * 1000),
        "releaseNotesUrl": f"{homepage}/releases/tag/v{version}",
        "assets": [
            {
                "os": "windows" if artifact.target == "nsis" else "linux",
                "arch": MANIFEST_ARCH,
                "format": artifact.target,
                "url": f"{prefix}/{artifact.path.name}",
                "fileName": artifact.path.name,
                "sizeBytes": artifact.size,
                "sha256": artifact.sha256,
                "signatureB64": artifact.signature_b64,
            }
            for artifact in artifacts
        ],
    }
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(manifest, indent=2) + "\n")
    report.ok(f"{destination} describes {len(manifest['assets'])} asset(s)")
    round_trip(ctx, destination, report)


def round_trip(ctx: Ctx, manifest: Path, report: Report) -> None:
    """Hand the manifest to the real installer and require it to resolve.

    A manifest this script accepts but `rocm install app` rejects is worse than
    no manifest: it is published, downloaded, and refused at install time. The
    dry run is the cheapest way to make the consumer the judge — it parses,
    validates, and selects an asset for this host without touching the network.
    """
    rocm = ctx.cli_root / "target" / "release" / ("rocm.exe" if sys.platform == "win32" else "rocm")
    if not rocm.is_file():
        report.note(f"round-trip skipped: rocm binary not built ({rocm})")
        return
    result = subprocess.run(
        [str(rocm), "install", "app", "--manifest", str(manifest), "--dry-run"],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        report.fail(f"rocm install app --dry-run rejected the manifest: {result.stdout.strip()}")
        return
    selected = next(
        (line.strip() for line in result.stdout.splitlines() if line.strip().startswith("asset:")),
        "asset: <unreported>",
    )
    report.ok(f"rocm install app --dry-run accepted the manifest ({selected})")


def run_checks(ctx: Ctx) -> tuple[Report, list[Artifact]]:
    report = Report()
    if not host_machine_supported():
        report.fail(
            f"unsupported host architecture: {platform.machine()} "
            f"(this product ships x86_64 only)"
        )
        return report, []
    product, version = product_and_version(ctx)
    report.head(f"{product} {version}  {platform.machine()}  ({ctx.bundle_dir})")
    expected = expected_names(product, version)
    artifacts = discover(ctx, expected, report)
    check_asset_set(ctx, expected, report)
    check_embedded_cli(ctx, artifacts, report)
    check_cli_only(ctx, report)
    check_checksums_and_signatures(ctx, artifacts, report)
    return report, artifacts


# --------------------------------------------------------------------------
# Self-test
# --------------------------------------------------------------------------


def write_deb(destination: Path, payload: dict[str, bytes]) -> None:
    """Craft a real deb: `ar` over `debian-binary`, `control.tar.gz`, `data.tar.gz`.

    Real rather than mocked because the check under test is the extraction
    itself. A stub archive would pass a stub reader and prove nothing about
    `dpkg-deb`.
    """
    with tempfile.TemporaryDirectory(prefix="rocm-deb-fixture-") as scratch_name:
        scratch = Path(scratch_name)
        data = scratch / "data"
        (data / "usr" / "bin").mkdir(parents=True)
        for name, content in payload.items():
            target = data / "usr" / "bin" / name
            target.write_bytes(content)
            target.chmod(0o755)
        control = scratch / "control"
        control.mkdir()
        (control / "control").write_text(
            "Package: rocm-app\nVersion: 9.9.9\nArchitecture: amd64\n"
            "Maintainer: fixture <fixture@example.invalid>\nDescription: fixture\n"
        )
        with tarfile.open(scratch / "data.tar.gz", "w:gz") as archive:
            archive.add(data / "usr", arcname="./usr")
        with tarfile.open(scratch / "control.tar.gz", "w:gz") as archive:
            archive.add(control / "control", arcname="./control")
        (scratch / "debian-binary").write_text("2.0\n")
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.unlink(missing_ok=True)
        subprocess.run(
            ["ar", "rc", str(destination), "debian-binary", "control.tar.gz", "data.tar.gz"],
            cwd=scratch,
            check=True,
            stdout=subprocess.DEVNULL,
        )


def write_rpm(destination: Path, payload: dict[str, bytes]) -> None:
    """Craft a real rpm with `rpmbuild`, for the same reason as the deb."""
    with tempfile.TemporaryDirectory(prefix="rocm-rpm-fixture-") as scratch_name:
        scratch = Path(scratch_name)
        buildroot = scratch / "buildroot"
        (buildroot / "usr" / "bin").mkdir(parents=True)
        for name, content in payload.items():
            target = buildroot / "usr" / "bin" / name
            target.write_bytes(content)
            target.chmod(0o755)
        files = "\n".join(f"/usr/bin/{name}" for name in payload)
        spec = scratch / "fixture.spec"
        spec.write_text(
            "Name:           ROCm\n"
            "Version:        9.9.9\n"
            "Release:        1\n"
            "Summary:        fixture\n"
            "License:        MIT\n"
            "BuildArch:      x86_64\n"
            "%description\nfixture\n"
            f"%files\n{files}\n"
        )
        subprocess.run(
            [
                "rpmbuild",
                "-bb",
                str(spec),
                "--define",
                f"_topdir {scratch / 'top'}",
                "--define",
                f"_rpmdir {scratch / 'out'}",
                "--define",
                "_build_id_links none",
                "--buildroot",
                str(buildroot),
            ],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        built = next((scratch / "out").rglob("*.rpm"))
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(built, destination)


CLEAN_CLI_SOURCES = {
    "scripts/package-linux-release.sh": "#!/usr/bin/env bash\ntar czf rocm-cli.tar.gz rocm rocmd\n",
    "scripts/package-windows-release.ps1": "Compress-Archive rocm.exe,rocmd.exe rocm-cli.zip\n",
    "install.sh": "#!/bin/sh\ninstall -m755 rocm rocmd \"$prefix/bin\"\n",
    "install.ps1": "Copy-Item rocm.exe,rocmd.exe $InstallDir\n",
}


def build_fixture(root: Path) -> Ctx:
    """A throwaway tree shaped exactly like this repo, with synthetic artifacts."""
    payload = {
        "rocm": b"fixture-rocm-binary",
        "rocmd": b"fixture-rocmd-binary",
        "rocm-app": b"fixture-app-binary",
    }
    app_root = root / "app"
    cli_root = root / "cli"
    (app_root / "src-tauri").mkdir(parents=True)
    (app_root / "src-tauri" / "tauri.conf.json").write_text(
        json.dumps(
            {
                "productName": "ROCm",
                "version": "9.9.9",
                "bundle": {"homepage": "https://example.invalid/rocm-app"},
            }
        )
    )
    (app_root / "src-tauri" / "compatibility.json").write_text(
        json.dumps(
            {
                "schemaVersion": 1,
                "appVersion": "9.9.9",
                "target": "x86_64-unknown-linux-gnu",
                "stagedAtUnixMs": 0,
                "sourceCommit": "fixture",
                "binaries": [
                    {
                        "name": name,
                        "fileName": f"{name}-x86_64-unknown-linux-gnu",
                        "version": f"{name} 9.9.9",
                        "sizeBytes": len(payload[name]),
                        "sha256": hashlib.sha256(payload[name]).hexdigest(),
                    }
                    for name in ("rocm", "rocmd")
                ],
            }
        )
    )
    for relative, content in CLEAN_CLI_SOURCES.items():
        source = cli_root / relative
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_text(content)

    ctx = Ctx(app_root=app_root, cli_root=cli_root, env={})
    names = expected_names("ROCm", "9.9.9")
    write_deb(ctx.bundle_dir / "deb" / names["deb"], payload)
    write_rpm(ctx.bundle_dir / "rpm" / names["rpm"], payload)
    return ctx


def self_test() -> int:
    """Exercise every check against artifacts built here, then break each one.

    A self-test that only proved the passing case would pass just as happily if
    a check silently stopped looking — which is the way these checks fail.
    """
    report = Report()
    report.head("self-test")
    failures: list[str] = []

    def expect(label: str, ctx: Ctx, should_pass: bool, contains: str = "") -> None:
        result, _ = run_checks(ctx)
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

    with tempfile.TemporaryDirectory(prefix="rocm-package-verify-") as root_name:
        root = Path(root_name)
        ctx = build_fixture(root)
        names = expected_names("ROCm", "9.9.9")
        payload = {
            "rocm": b"fixture-rocm-binary",
            "rocmd": b"fixture-rocmd-binary",
            "rocm-app": b"fixture-app-binary",
        }
        deb = ctx.bundle_dir / "deb" / names["deb"]
        rpm = ctx.bundle_dir / "rpm" / names["rpm"]
        compatibility = ctx.compatibility

        expect("good set", ctx, True)

        aside = root / "set-aside.rpm"
        rpm.rename(aside)
        expect("rpm this host can build but did not", ctx, False, "missing")
        aside.rename(rpm)

        stale = deb.with_name("ROCm_9.9.8_amd64.deb")
        stale.write_bytes(b"stale release")
        expect("stale orphan artifact", ctx, False, "unaccounted artifacts")
        stale.unlink()
        stale.with_name(stale.name + ".sha256").unlink(missing_ok=True)

        sidecar = deb.with_name(deb.name + ".sha256")
        good_sidecar = sidecar.read_text()
        sidecar.write_text(f"{'0' * 64}  {deb.name}\n")
        # Off the release path a disagreeing sidecar means the artifact was
        # rebuilt, which is ordinary; the sidecar is refreshed and the run
        # continues. `tauri build` writes new bytes every time, so failing here
        # would make the documented build-then-verify order impossible.
        expect("stale sidecar refreshed off the release path", ctx, True)
        assert sidecar.read_text() != f"{'0' * 64}  {deb.name}\n"
        # On the release path the same disagreement is a hard stop: the bytes
        # moved after somebody signed them.
        sidecar.write_text(f"{'0' * 64}  {deb.name}\n")
        expect(
            "stale sidecar rejected when signatures are required",
            Ctx(
                app_root=ctx.app_root,
                cli_root=ctx.cli_root,
                require_signatures=True,
                env={},
            ),
            False,
            "does not match the artifact",
        )
        sidecar.write_text(good_sidecar)

        without_rocm = {k: v for k, v in payload.items() if k != "rocm"}
        write_deb(deb, without_rocm)
        deb.with_name(deb.name + ".sha256").unlink(missing_ok=True)
        expect("deb without /usr/bin/rocm", ctx, False, "does not install")
        write_deb(deb, payload)
        deb.with_name(deb.name + ".sha256").unlink(missing_ok=True)

        good_compatibility = compatibility.read_text()
        broken = json.loads(good_compatibility)
        broken["binaries"][0]["sha256"] = "f" * 64
        compatibility.write_text(json.dumps(broken))
        expect("mismatched compatibility hash", ctx, False, "!= staged")
        compatibility.write_text(good_compatibility)
        for artifact in (deb, rpm):
            artifact.with_name(artifact.name + ".sha256").unlink(missing_ok=True)

        leaky = ctx.cli_root / "install.sh"
        clean_install = leaky.read_text()
        leaky.write_text(clean_install + "install -m644 rocm-app.desktop ~/.config/autostart/\n")
        expect("CLI installer staging app payload", ctx, False, "mentions app payload")
        leaky.write_text(clean_install)

        expect(
            "--require-signatures without a key",
            Ctx(app_root=ctx.app_root, cli_root=ctx.cli_root, require_signatures=True, env={}),
            False,
            "no signing key is configured",
        )

        expect("restored fixture", ctx, True)

    if failures:
        report.fail(f"{len(failures)} self-test expectation(s) did not hold")
    else:
        report.ok("every self-test expectation held; temp root removed")
    report.emit()
    return 1 if failures else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--require-signatures",
        action="store_true",
        help="fail instead of reporting `unsigned` when no signing key is configured",
    )
    parser.add_argument(
        "--emit-manifest",
        type=Path,
        metavar="PATH",
        help="write the AppReleaseManifest `rocm install app` consumes, then round-trip it",
    )
    parser.add_argument(
        "--base-url",
        metavar="URL",
        help="download URL prefix recorded in the emitted manifest; required with --emit-manifest",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="verify these checks against synthetic artifacts under a temp root",
    )
    args = parser.parse_args()
    # A manifest is published. One whose `url` prefix defaulted to a placeholder
    # would resolve, download nothing, and blame the network.
    if args.emit_manifest is not None and not args.base_url:
        parser.error("--emit-manifest requires --base-url")

    if args.self_test:
        return self_test()

    ctx = Ctx(require_signatures=args.require_signatures)
    try:
        report, artifacts = run_checks(ctx)
        if args.emit_manifest is not None:
            emit_manifest(ctx, artifacts, args.emit_manifest, args.base_url, report)
    except (VerifyError, OSError, json.JSONDecodeError) as error:
        sys.stderr.write(f"package_verify: {error}\n")
        return 1
    report.head(
        f"{len(report.failures)} failure(s)" if report.failures else "all checks passed"
    )
    report.emit()
    return 1 if report.failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
