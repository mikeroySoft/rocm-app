<!--
Copyright © Advanced Micro Devices, Inc., or its affiliates.

SPDX-License-Identifier: MIT
-->

# ROCm App

A desktop tray application that installs, updates, changes, monitors, and diagnoses
**managed ROCm** for Radeon and Ryzen AI users.

ROCm App is a separate product from [`rocm-cli`](https://github.com/mikeroysoft/rocm-cli).
It depends on rocm-cli's shared crates at an exact pinned revision and bundles a
compatible `rocm` / `rocmd` build, but it is not a workspace member of that
repository and does not add frontend weight to it.

## Install

Linux (x86_64, deb or rpm hosts):

```bash
curl -fsSL https://raw.githubusercontent.com/mikeroySoft/rocm-app/main/install.sh | sh
```

The script downloads the newest [release](https://github.com/mikeroySoft/rocm-app/releases)
package for this host, verifies its published SHA-256, and installs it with
your package manager — which also installs the bundled `rocm` / `rocmd`
command-line tools. It never touches GPU drivers. Pin a version with
`ROCM_APP_VERSION=v0.0.1`.

Windows: download `rocm-app_<version>_x64-setup.exe` from the
[releases page](https://github.com/mikeroySoft/rocm-app/releases) and run it.

## Supported platforms

| Platform                      | Status                                                                   |
| ----------------------------- | ------------------------------------------------------------------------ |
| Native Windows 11 x86_64      | Supported                                                                |
| Native Linux x86_64           | Supported                                                                |
| WSL                           | **Not supported** — reported explicitly, with no install actions offered |
| macOS, ARM, Instinct hardware | **Not supported** — out of scope                                         |

The app fails to compile on a target that is neither Windows nor Linux. A
best-effort build on an unsupported host would ship something that cannot reach
a GPU and cannot explain why.

Hardware eligibility is decided by the GPU's ROCm family, not its marketing
name: setup is offered only when the detected GPU maps to a TheRock family the
managed runtime publishes builds for. An AMD GPU the mapping does not
recognise is reported honestly — with the GPU named — and offered no install.

## What it will and will not do

- **Drivers are read-only.** The app reports your installed driver version and
  links to official release notes or OEM guidance. It never installs, updates,
  or removes a driver.
- **Managed ROCm runtimes install side by side.** "Change ROCm" validates and
  activates a runtime you already have installed; it does not mutate system
  drivers.
- **Every runtime change has a review step.** Install, update, activate,
  remove, validate, and diagnosed fixes all show an explicit plan first, and
  nothing runs until exactly that plan is approved. Start at login is a direct
  settings toggle — Settings shows what the operating system reports, not what
  was asked. The app has no silent self-update: new app builds arrive through
  `rocm install app`, which has its own review and signature checks.
- **Installing the app installs the CLI. Installing the CLI does not install the
  app** — only `rocm install app` does that.
- **No CPU fallback.** GPU-required work fails loudly rather than silently
  producing slow, wrong-looking results.

## The tray monitor

The app's normal state is a tray icon, not a window. Monitoring runs whether or
not anything is open.

- **A boot launch shows no window.** The login item passes `--hidden`; the tray
  icon is created before the first health probe runs, so the icon appears while
  the machine is still being read rather than after.
- **Closing a window closes a view, not the product.** An ordinary close hides
  the window and monitoring continues. **Quit** in the tray menu ends the
  process, and is the only thing that does.
- **One process.** Launching the app again focuses the window that is already
  open. A second launch that carries `--hidden` does not steal focus.
- **The tray menu offers no changes.** Three facts — graphics card, system,
  ROCm in use — then Open ROCm App, More Info (the project page in the
  default browser), and Quit ROCm App. Install, update, activate, and remove
  stay behind a reviewed plan in a real window.
- **Left click opens the compact window on Windows only.** Tauri documents tray
  icon click events as unsupported on Linux, so the main window is reachable
  from the menu on both platforms and nothing depends on a click.
- **Notifications are transition-only.** The app speaks up when the health
  verdict _changes_, when an update it would actually offer becomes available,
  and when an operation finishes or fails. The last thing said is persisted, so
  quitting and relaunching does not re-announce it. An unchanged verdict is
  never repeated.
- **Start at login is on by default in an installed build, and off in a debug
  one.** A development binary in `target/` must not register a login item for
  itself. Settings shows what the operating system actually reports, not what
  the app asked for, so a refused change is visible.
- **Probes are bounded and never overlap.** GPU metrics every two seconds, full
  health every minute, update availability every six hours — and the update
  check rides on the same snapshot as a health probe rather than costing a
  second one. Full probes are withheld while a change is running and resume
  once after it ends; metrics keep flowing so the tray stays alive during a long
  install.

## What the version numbers mean

Four version numbers appear in the product, and they move independently:

| Number          | Where it appears        | What it is                                                              |
| --------------- | ----------------------- | ----------------------------------------------------------------------- |
| App version     | Package name, inventory | The `rocm-app` desktop package itself                                   |
| CLI version     | Overview inventory      | The bundled `rocm` build the app ships and drives                       |
| Contract schema | Error copy only         | The `app-snapshot` JSON version; a future schema means "update the app" |
| ROCm version    | Overview, Manage ROCm   | The managed runtime a change installs, activates, or removes            |

The installer bundles a CLI it is compatible with and records the pairing in
`compatibility.json`, so app and CLI versions move together on an installed
machine.

## Privacy and support data

- **No telemetry.** The app sends nothing anywhere on its own. Network activity
  happens inside the bundled CLI, when a reviewed plan runs or when the bounded
  update check does.
- **Local records only.** The Activity screen reads local log files. The app's
  own audit ring keeps the last 200 operations and records operation, outcome,
  and error code — never argv, paths, or URLs.
- **Support bundles are explicit and redacted.** Exporting from the Activity
  screen runs `rocm app-support-bundle`: a fixed allowlist of members, redacted
  (usernames, hostnames, tokens) before writing. Nothing is uploaded; you choose
  where the file goes. The allowlist and redaction policy are documented in
  rocm-cli's `docs/support-bundle.md`.

## When something is broken

- **The app says the command-line tool is too old.** The bundled CLI predates
  the app's contract. Reinstall the app package — it carries a matched CLI.
- **The app says it is too old itself.** The CLI's snapshot schema is newer
  than this app. Update the app: `rocm install app`.
- **Something on this machine is wrong.** Use Diagnose: it matches known causes
  against this computer and offers only fixes it can apply itself; anything
  else names the change it cannot make for you.
- **None of the above.** Export a support bundle from the Activity screen and
  attach it to an issue. Its member list is fixed and its content is redacted.

## Layout

```
rocm-app/
├── fixtures/                   Deterministic fixtures, generated by the Rust suite
├── scripts/                    CLI staging and the two packaging harnesses
├── src/                        React + TypeScript renderer
│   └── lib/                    Platform gate, fixture access, backend bridge
└── src-tauri/                  Tauri v2 desktop shell
    ├── src/                    Composition root: typed commands only, no product rules
    ├── packaging/              Installer hooks: ownership guards, autostart cleanup
    └── crates/rocm-app-core/   Domain logic, no Tauri dependency
```

`rocm-app-core` carries every decision the product makes, so it can be tested
without a WebView, a GPU, a network, or a display. The `src-tauri` crate above
it only wires those decisions to Tauri commands.

## Development

Native prerequisites on Ubuntu / Debian:

```bash
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev \
  build-essential curl wget file libssl-dev pkg-config
```

Then:

```bash
npm ci
npm run tauri dev
```

### Stage the CLI before building the app crate

The app **ships** the `rocm` and `rocmd` binaries, declared as Tauri
`externalBin` sidecars. `tauri-build` resolves them at compile time, so anything
that compiles the `rocm-app` crate — including `cargo test` — fails on a clean
tree with `resource path binaries/rocm-… doesn't exist` until they are staged:

```bash
cargo build --release --manifest-path ../rocm-cli/Cargo.toml -p rocm -p rocmd
npm run stage
```

`cargo test -p rocm-app-core` needs none of this: the domain crate has no
`tauri-build` step, which is the same reason CI can run it in a fast job.

See [docs/packaging.md](docs/packaging.md) for the installed layout, the
ownership model, and the signing scheme.

### Running a binary you built yourself

`npm run tauri dev` is the only way to run a **debug** build. A debug binary
has `devUrl` (`http://localhost:1420`) compiled into it, so launching
`src-tauri/target/debug/rocm-app` on its own opens a window that says
_"Could not connect to localhost: Connection refused"_ — the Vite dev server
it expects is not running. That is the frontend failing to load, not the app
failing to start.

For a standalone binary that serves its own bundled frontend, build a release:

```bash
npm run tauri build     # or: npm run build && cargo build --release --manifest-path src-tauri/Cargo.toml
```

The app runs the `rocm` command-line tool that sits **beside its own
executable** — never one found elsewhere on `PATH`, so an installed app cannot
be redirected to a stranger's CLI. For a self-built binary, copy or symlink a
compatible CLI next to it; without a sibling the app reports the CLI as
missing instead of guessing.

See [docs/testing.md](docs/testing.md) for the full gate list.

## License

MIT — see [LICENSE](LICENSE).
