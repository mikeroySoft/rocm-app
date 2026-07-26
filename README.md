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

## Supported platforms

| Platform | Status |
|---|---|
| Native Windows 11 x86_64 | Supported |
| Native Linux x86_64 | Supported |
| WSL | **Not supported** — reported explicitly, with no install actions offered |
| macOS, ARM, Instinct hardware | **Not supported** — out of scope |

The app fails to compile on a target that is neither Windows nor Linux. A
best-effort build on an unsupported host would ship something that cannot reach
a GPU and cannot explain why.

## What it will and will not do

- **Drivers are read-only.** The app reports your installed driver version and
  links to official release notes or OEM guidance. It never installs, updates,
  or removes a driver.
- **Managed ROCm runtimes install side by side.** "Change ROCm" validates and
  activates a runtime you already have installed; it does not mutate system
  drivers.
- **Every mutation has a review step.** Install, update, activate, remove, fix,
  autostart changes, and app self-update all show an explicit plan first.
- **Installing the app installs the CLI. Installing the CLI does not install the
  app** — only `rocm install app` does that.
- **No CPU fallback.** GPU-required work fails loudly rather than silently
  producing slow, wrong-looking results.

## Layout

```
rocm-app/
├── fixtures/scenarios.json     Deterministic fixtures, shared by Rust and TypeScript
├── src/                        React + TypeScript renderer
│   └── lib/                    Platform gate, fixture access, backend bridge
└── src-tauri/                  Tauri v2 desktop shell
    ├── src/                    Composition root: typed commands only, no product rules
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

See [docs/testing.md](docs/testing.md) for the full gate list.

## License

MIT — see [LICENSE](LICENSE).
