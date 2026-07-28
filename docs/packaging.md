<!--
Copyright © Advanced Micro Devices, Inc., or its affiliates.

SPDX-License-Identifier: MIT
-->

# Packaging

Installing ROCm App installs the ROCm command-line tool. Installing the CLI does
not install the app. Everything below exists to make that asymmetry true in real
artifacts rather than in intent.

## What ships

| Target         | Artifact                           | Built where                         |
| -------------- | ---------------------------------- | ----------------------------------- |
| Linux x86_64   | `rocm-app_<version>_amd64.deb`     | any x86_64/amd64 Linux host, and CI |
| Linux x86_64   | `rocm-app-<version>-1.x86_64.rpm`  | any x86_64/amd64 Linux host, and CI |
| Windows x86_64 | `rocm-app_<version>_x64-setup.exe` | **`windows-latest` in CI only**     |

NSIS bundling needs a Windows host — the bundler downloads and runs `makensis`
there. The development machine this project is built on has neither, so Windows
artifacts are produced by the `package (windows nsis)` job or they are not
produced. They are never inferred from a Linux build or from `tauri dev`.

macOS and WSL are out of scope and the app fails to compile on them.

## Installed layout

```
/usr/bin/rocm-app                       the app
/usr/bin/rocm                           the CLI it shipped with
/usr/bin/rocmd                          the CLI's daemon
/usr/lib/rocm-app/compatibility.json    what those two are, exactly
/usr/share/applications/rocm-app.desktop
```

On Windows everything lands in `$INSTDIR`, which is `%LOCALAPPDATA%\rocm-app` in
the default per-user install mode.

The CLI is a **sibling of the app binary** because that is how the app finds it:
`bundled_cli_path()` joins `current_exe().parent()` with `rocm`. A layout change
that moved the sidecar elsewhere would leave the app silently falling back to
whatever is on `PATH`, so `installer_acceptance.py` asserts the sibling
relationship against the packaged file list rather than against the config.

## Why `productName` is `rocm-app` and not `ROCm`

Two reasons, neither cosmetic.

Tauri kebab-cases `productName` into the package name, and `ROCm` becomes
`ro-cm` — what a user would have to type to remove it. And the Linux bundler
writes resources to `/usr/lib/<productName>` while the runtime resolver reads
`/usr/lib/<cargo package name>`; with `ROCm` those two disagree and the installed
`compatibility.json` is unreadable.

The name users see is still `ROCm`: a shared desktop template sets `Name=ROCm`
for both deb and rpm, and the NSIS start-menu folder is `ROCm`.

## Ownership: the package manager already knows

dpkg and rpm track which package owns which file, and they remove exactly the
files they own. That **is** the ownership metadata — there is deliberately no
second bookkeeping scheme beside it to disagree with. Uninstalling ROCm App
removes `/usr/bin/rocm` because this package owns it, and leaves a CLI in
`~/.local/bin` alone because it never owned that.

The gap is a binary somebody copied into `/usr/bin` by hand. Nothing owns it, so
dpkg will happily overwrite it and then delete it on uninstall — somebody else's
tool, removed by our uninstaller, with no way for them to know why. The shipped
`preinst` (deb) and `preinstall.sh` (rpm) refuse in that case and name the exact
files and the exact command to move them aside. Refusing an install is
recoverable in one command; deleting a binary somebody built is not.

Autostart is the one piece the package manager cannot see: it lives in each
user's home. `postrm` removes exactly `~/.config/autostart/rocm-app.desktop` and
nothing globbed, and the rpm equivalent exits early on an upgrade (`$1 == 1`) so
a reinstall does not silently turn a user's choice off.

## Signing

Tauri's own updater signing is **Minisign and is deliberately not enabled**.
Artifacts are signed with the scheme `rocm install app` already verifies and
[`rocm-cli/docs/release-trust.md`](https://github.com/mikeroysoft/rocm-cli/blob/main/docs/release-trust.md)
already specifies:

- `<artifact>.sha256` containing exactly `"<lowercase-hex>  <basename>\n"`.
- `<artifact>.sig` containing a raw RSASSA-PKCS#1 v1.5 SHA-256 signature,
  verified against a SubjectPublicKeyInfo `-----BEGIN PUBLIC KEY-----`.

Both are produced by `cargo xtask sign` in the rocm-cli tree. Two signing
systems for one product is one too many, and the one that survives is the one
the installer on the other end can check.

Production keys are owner-controlled inputs supplied through
`ROCM_APP_SIGNING_PRIVATE_KEY_PATH` or `..._PEM`. Acceptance runs generate an
ephemeral key with `cargo xtask keygen`; **a generated test key is never a
production trust root.**

## Building a release locally

```bash
# 1. Build the CLI this release will carry, then record what it is.
cargo build --release --manifest-path ../rocm-cli/Cargo.toml -p rocm -p rocmd
python3 scripts/stage_cli.py --from ../rocm-cli/target/release

# 2. Bundle.
npm run tauri build -- --bundles deb,rpm

# 3. Verify, sign, and emit the manifest `rocm install app` consumes.
export ROCM_APP_SIGNING_PRIVATE_KEY_PATH=/path/to/private.pem
npm run package:verify -- --require-signatures \
  --emit-manifest dist/app-release.json \
  --base-url https://github.com/mikeroysoft/rocm-app/releases/download/v0.1.0

# 4. Prove the artifacts install the way they claim to.
python3 scripts/installer_acceptance.py
```

`scripts/stage_cli.py` records each binary's `--version` output, size, SHA-256,
and the rocm-cli commit it came from. It runs the binary rather than reading a
version from a manifest, because a version read from a manifest is a claim and a
version read from the binary is a fact.

## The two harnesses

`package_verify.py` owns **artifact** truth and `installer_acceptance.py` owns
**install-lifecycle** truth. Neither imports the other.

Both `--self-test` against fixtures they build, and both test the failing cases
— nine negatives and eleven negatives respectively, each asserted to fail for
the intended reason. A packaging harness that only ever sees a good bundle
proves nothing.

Two choices in the acceptance harness are worth knowing about:

- It **executes the shipped maintainer scripts**, extracted from inside the
  packaged artifact (`control.tar` for deb, `rpm -qp --scripts` for rpm), under a
  sandboxed `/usr/bin`. Not the copies in the source tree: what ships is what
  matters.
- It does **not** require root and does not run `dpkg -i` against the live
  system. It extracts and reasons about the result. A test that needs `sudo` is
  a test nobody runs.

## The CLI-only negative case

`package_verify.py` asserts that `rocm-cli`'s own installers and release scripts
carry no app payload — no `rocm-app`, no `.deb`/`.rpm`/`-setup.exe`, no
autostart, no desktop entry. rocm-cli enforces the same asymmetry from its side
with `install_app_is_not_reachable_from_any_other_install_path`, which scans the
repository rather than trusting convention. Two independent checks, one from
each repository, because either alone passes with the other half broken.
