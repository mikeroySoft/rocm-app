<!--
Copyright © Advanced Micro Devices, Inc., or its affiliates.

SPDX-License-Identifier: MIT
-->

# Testing

## Gates

Every one of these must exit 0 before a change is considered done.

| Gate                                | Command                                                                          |
| ----------------------------------- | -------------------------------------------------------------------------------- |
| Install (reproducible)              | `npm ci`                                                                         |
| Frontend production build           | `npm run build`                                                                  |
| Typecheck                           | `npm run typecheck`                                                              |
| Lint                                | `npm run lint`                                                                   |
| Frontend unit tests                 | `npm test -- --run`                                                              |
| Rust tests                          | `cargo test --manifest-path src-tauri/Cargo.toml --all-targets`                  |
| Rust lint                           | `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` |
| Workflow pins and platform coverage | `npm run ci:validate`                                                            |
| Desktop end-to-end                  | `npm run test:e2e`                                                               |
| Harness failure path                | `npm run test:e2e:fixture`                                                       |
| Visual matrix and contact sheets    | `npm run test:visual`                                                            |
| Accessibility and keyboard flows    | `npm run test:a11y`                                                              |
| Isolation harness                   | `python3 scripts/fresh_user_smoke.py --self-test`                                |
| Native Wayland desktop              | `python3 scripts/wayland_desktop_check.py`                                       |

`src-tauri/Cargo.toml` declares `default-members = [".", "crates/rocm-app-core"]`,
so the two `cargo` gates cover the domain crate as well as the Tauri shell.
Without that, `rocm-app-core`'s suite would silently never run.

## Test layers

**`rocm-app-core` (Rust, no Tauri).** Where the real assertions live. Platform
classification is a pure function over evidence rather than a `#[cfg]` gate, so
Windows, WSL, and unsupported-host behaviour are all reachable from a unit test
on any machine. A `#[cfg]`-only implementation would make the WSL path
untestable on the Linux box that has to prove it.

**`rocm-app` (Rust, Tauri commands).** Thin. Asserts only that the command
boundary neither widens nor narrows what the domain layer decided — for example,
that a WSL snapshot still carries no install offer after crossing into the
renderer.

**Renderer (vitest + Testing Library).** Runs against the fixture set with no
backend present. `src/lib/backend.ts` resolves from local fixtures when
`isTauri()` is false, which is what makes the UI testable without a WebView.

## The rocm-cli contract

`rocm app-snapshot` is a versioned, read-only JSON contract between the CLI and
this app. It is a **separate surface** from `rocm examine --json`, whose 50 top
level keys are a frozen wire contract that additions would break.

The producer lives in rocm-cli's `rocm` _binary_ crate, which cannot be linked
as a library, and this app pins rocm-cli to a published revision. So the wire
format — not a shared Rust type — is the contract, and drift is caught two ways:

**Golden fixtures** in `fixtures/contract/` are generated from the real producer,
never hand-written. Regenerate them from the rocm-cli checkout:

```bash
ROCM_APP_GOLDEN_DIR=../rocm-app/fixtures/contract \
  cargo test -p rocm --bin rocm app_contract
```

| Fixture                                                              | Covers                                      |
| -------------------------------------------------------------------- | ------------------------------------------- |
| `healthy`, `setup-required`, `attention`, `partial`, `offline-stale` | verdict space                               |
| `unsupported-wsl`                                                    | no eligible actions at all                  |
| `invalid-future-version`                                             | schema version this build cannot implement  |
| `invalid-payload`                                                    | right version, incomplete body              |
| `invalid-malformed`                                                  | not JSON — the CLI printed an error instead |

**A live harness** (`tests/contract_producer_consumer.rs`) runs the
repository-built `rocm` binary against three empty state roots and decodes its
real output. Goldens prove the decoder handles what the producer _once_ emitted;
this proves it handles what the producer emits _now_. The isolation is itself
asserted — a run that reached the developer's real `~/.rocm` would list runtimes,
and every other assertion in that file would be measuring the wrong machine.

Two rules the contract enforces and the tests pin:

- **Driver data is read-only.** `driver` has exactly `installed`, `latestKnown`,
  and `supportLinks`; no `EligibleAction` targets a driver. Asserted on the type,
  on every fixture, and on live producer output.
- **An unsupported host is offered nothing.** The producer omits actions, and
  `AppSnapshot::offerable_actions` re-checks. A consumer that trusted the
  producer's list alone would ship an Install button to WSL the day a producer
  bug lands.

## Fixtures

Every fixture under `fixtures/` is **generated by the Rust test suite**, never
hand-written. Snapshots come from the producer's own goldens, views from the
real derivation functions, plans from the real controller, and progress streams
from real `execute` runs against scripted adapters. A renderer test therefore
cannot pass against a screen the backend would never draw.

| File                        | Generated by                                         | Regenerate with                                                              |
| --------------------------- | ---------------------------------------------------- | ---------------------------------------------------------------------------- |
| `fixtures/contract/*.json`  | rocm-cli's `app_contract_emit_golden_fixtures`       | see [the contract section](#the-rocm-cli-contract)                           |
| `fixtures/onboarding.json`  | `onboarding_fixtures_match_the_committed_file`       | `ROCM_APP_WRITE_FIXTURES=1 cargo test -p rocm-app-core onboarding_fixtures`  |
| `fixtures/dashboard.json`   | `health_dashboard_fixtures_match_the_committed_file` | `ROCM_APP_WRITE_FIXTURES=1 cargo test -p rocm-app-core dashboard_fixtures`   |
| `fixtures/runtimes.json`    | `runtimes_fixtures_match_the_committed_file`         | `ROCM_APP_WRITE_FIXTURES=1 cargo test -p rocm-app-core runtimes_fixtures`    |
| `fixtures/tray.json`        | `tray_fixtures_match_the_committed_file`             | `ROCM_APP_WRITE_FIXTURES=1 cargo test -p rocm-app-core tray_fixtures`        |
| `fixtures/diagnostics.json` | `diagnostics_fixtures_match_the_committed_file`      | `ROCM_APP_WRITE_FIXTURES=1 cargo test -p rocm-app-core diagnostics_fixtures` |

Without `ROCM_APP_WRITE_FIXTURES` each of those tests **asserts equality**
against the committed file, so a change to a derivation that the renderer tests
were relying on fails loudly instead of silently.

Two invariants hold across all of them:

- No fixture may advertise an install on a host that cannot support one.
- Timestamps are fixed, and every environment-derived input — install folders,
  free space, the app's own version, the clock — is passed in as a parameter.
  A fixture that reads its generating machine breaks the equality assertion on
  every other machine.

## Fixture mode

Set `ROCM_APP_FIXTURE=1` at build time to expose the fixture routes below.
Fixture mode touches no GPU, no network, and no real ROCm config, data, or cache
root. Production bundles have no fixture route and no way to fabricate a health
state.

```bash
ROCM_APP_FIXTURE=1 npm run dev
ROCM_APP_FIXTURE=1 npm run build
```

| Route                                                      | States                                                                                        |
| ---------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| `?view=onboarding&scenario=<name>&outcome=<name>&stop=<n>` | see `fixtures/onboarding.json`                                                                |
| `?view=dashboard&scenario=<name>`                          | see `fixtures/dashboard.json`                                                                 |
| `?view=runtimes&scenario=<name>&plan=1&outcome=<name>`     | see `fixtures/runtimes.json`                                                                  |
| `?window=quick&scenario=<name>`                            | `checking`, `healthy`, `setup-required`, `attention`, `offline-stale`, `unsupported`, `error` |
| `?view=settings&scenario=<0\|1\|2>`                        | autostart on, off, unavailable                                                                |

`?window=quick` is the one route that is **not** fixture-only: it is how the
compact window is addressed in production too, because `tauri.conf.json`
declares that window with `url: index.html?window=quick`. Only `&scenario=` is
gated on fixture mode.

## The tray monitor

The tray has no display server in CI and no tray daemon in a unit test, so the
split is deliberate:

- **Every decision is in `rocm_app_core::tray`** — status mapping, menu shape,
  icon rasterisation, notification transitions, and probe scheduling. All of it
  is a pure function, and all of it is tested without a window.
- **`src-tauri/src/tray_host.rs` is plumbing** and is tested only where it can
  fail on its own: the storage round-trips for the persisted autostart choice and
  last-notified state, and their behaviour on a corrupt file.
- **The rest is smoke-tested against a real window.** A `WebKitWebProcess` in
  the process tree proves WebKit started, not that anything rendered, so a tray
  change is verified by launching the built binary under a persistent `Xvfb`
  display and capturing the screen.

Two claims that only a timeline can prove, and where their tests live:

- _Notifications are deduplicated across restarts_ —
  `tray_notification_dedup_survives_a_restart` round-trips the state through its
  serialized form, because that is what a restart does.
- _Full probes never overlap_ —
  `tray_no_overlapping_full_probe_across_a_mutation_lifecycle` plays out sixty
  ticks with a mutation occupying the middle third and asserts the peak number
  of outstanding full probes is exactly one.

The icon is computed, not shipped: there is no PNG asset and no generator
script, so a status added without a glyph does not compile. `fixtures/tray.json`
records each status's glyph mask and colour, so a visual change to them is a
reviewable diff.

## The desktop suite

`npm run test:e2e` drives the **shipped release binary** through WebdriverIO and
`tauri-driver`. There is no fixture build, no dev server, and no product flag:
the specs exercise the React desktop backends, Tauri IPC, the Rust controller,
and a real `std::process::Command` spawn.

What _is_ stood in for is the machine. `src-tauri/crates/rocm-fixture-cli` is a
`rocm` that answers from a directory of recorded producer output instead of
touching a GPU, and the harness copies it next to a copy of the app binary — the
same place an installed CLI lives — so the sibling-lookup rule is exercised
rather than bypassed. A machine stand-in is legitimate for the same reason the
`StatusNotifierWatcher` stand-in is: CI has no Radeon card. A _product_ stand-in
would not be.

Every invocation the app makes is appended to a journal
(`ROCM_FIXTURE_JOURNAL`), which is where three assertions come from that the UI
cannot be trusted to make about itself:

- **Nothing changed before approval.** The journal holds only reads until the
  approve click. A screen that says "nothing happened" while a process ran is
  exactly the failure this catches.
- **No driver command, anywhere.** Driver mutation is out of scope for this
  product; the check is `driver`/`--dkms` appearing in no argv on any path.
- **Isolation held.** Every invocation records the roots it was handed, so the
  roots the suite set are provably the roots the app passed down.

### Scenarios

One spec file per boot, because the landing surface is decided by the app's
first snapshot read. `tests/e2e/scenarios.ts` maps each spec to a producer
golden from `fixtures/contract/`.

| Spec             | Scenario          | Covers                                               |
| ---------------- | ----------------- | ---------------------------------------------------- |
| `first-launch`   | `setup-required`  | first launch, isolated roots, no change on start     |
| `healthy-boot`   | `healthy`         | Overview, GPU and runtime identity, refresh          |
| `onboarding`     | `setup-required`  | guided setup, review, approval, progress, result     |
| `runtime-switch` | `healthy`         | version list, review, apply, post-change state       |
| `diagnostics`    | `attention`       | Activity, support-bundle export, Diagnose            |
| `routing`        | `healthy`         | compact and full windows, surface routing, autostart |
| `unsupported`    | `unsupported-wsl` | refusal, no change controls, explanation             |

### Isolation

`scripts/fresh_user_smoke.py` owns the isolated root set and the sentinels
planted in real user state; the WebdriverIO harness shells it rather than
keeping a second copy of that policy. `--prepare` builds the roots and plants
the sentinels, `--verify` re-checks them and scans the artifacts for any marker
that leaked.

### Retries, and why they cannot hide anything

`specFileRetries` and `mochaOpts.retries` are both **zero**: either can turn a
repeated functional failure green. `connectionRetryCount` is 1 and retries only
a failed WebDriver HTTP request, which cannot rerun a test body.

`npm run test:e2e:fixture` proves it rather than asserting it. It runs one spec
that fails on purpose against a healthy app, under a config with retries turned
_up_, and checks that the bound was reached exactly, that the run is still red,
and that the screenshot, page source, failure text, driver log and fixture
journal are all in `test-results/e2e/<runId>/artifacts/` with no sentinel marker
surviving sanitisation.

### Two things that will bite you

- **Build the app with `npx tauri build --no-bundle`, never `cargo build`.**
  Tauri's `dev` cfg is the _absence_ of the `custom-protocol` feature that only
  the CLI passes, so a cargo-built binary points its windows at the Vite dev
  server. `tests/e2e/support.ts` detects this and says so, because otherwise it
  looks like a hundred product failures instead of one build mistake.
- **On Linux the harness runs `tauri-driver` under `dbus-run-session`.**
  Inheriting a live desktop's session bus while `HOME` points at a scratch root
  makes the app reach that session's portal and keyring services with none of
  the state they expect, and it dies with SIGSEGV about a second and a half in.

## The visual and accessibility suites

`npm run test:visual` and `npm run test:a11y` reuse the desktop harness — the
shipped binary, the isolation roots, the scenario stand-in — and add two
things through `scripts/ui_quality.py`: a private session bus carrying a
`StatusNotifierWatcher` stand-in (`scripts/statusnotifierwatcher.py`), and a
tray-menu clicker (`scripts/tray_menu.py`). The compact window is only ever
shown the way a user shows it, by clicking "Quick status" in the real tray
menu over `com.canonical.dbusmenu`; there is no test-only door in the app.

The visual suite photographs every product state — healthy, setup-required
(review, progress, success, multi-line failure), attention plus the applied
update, unsupported, offline-stale, partial, activity (populated, one record,
export receipt, empty), diagnosis, and a long-content stress state — at
1024x700 and 1440x900, and the compact panel besides. At every stop it
asserts: no horizontal scroll, every visible control scrollable-to and
hittable, every status carrier says its state in words, and no app-authored
copy shows placeholder text, raw backend tokens, runtime keys, or command
syntax while disclosures are shut. Shots and per-scale contact sheets land in
`test-results/visual/`.

Text scale is `ROCM_VISUAL_SCALE` → `GDK_DPI_SCALE` on the driver
environment. WebKitGTK folds that fractional font-DPI scale into the device
pixel ratio, so 200% halves the CSS viewport — 380x300 becomes 190x150 —
which is the strictest honest reading of "200% text scale" (`gtk-xft-dpi`
does nothing; measured). The compact matrix re-runs at 1.25 and 2 against
the long-content scenario, whose GPU name is a raw lspci string.

The a11y suite runs axe-core (WCAG 2.1 A + AA tags) on every major state and
fails on any violation, walks Tab/Shift+Tab/Enter/Space/Escape flows into
`test-results/a11y/shots/keyboard-transcript.md`, and proves reduced motion
by writing `gtk-enable-animations=false` into the isolated profile before
one spec's boot — WebKitGTK maps it onto `prefers-reduced-motion`.

### Quirks the suites encode

- A hidden Tauri window keeps a 0x0 webview until first shown; after it has
  been shown once, hiding keeps the last viewport and the honest signal is
  `document.visibilityState`.
- WebKitWebDriver screenshots capture the full document, not the viewport,
  so a scrolling state photographs taller than its window. Geometry
  assertions, not the image size, carry the no-clip proof.
- The compact panel boots into "Checking…" and fills in when the tray's
  first probe lands; every compact measurement waits for the settled state,
  because the placeholder is shorter than any real GPU name.

## Native Wayland

WebKitWebDriver drives an X11 (or XWayland) window, so close-to-tray and
`hide()` behaviour on a _native_ Wayland toplevel is covered separately by
`scripts/wayland_desktop_check.py`. It starts a headless nested GNOME Shell,
launches the release binary against it, drives the tray over
`com.canonical.dbusmenu`, and reads the answer off the Wayland wire log
(`WAYLAND_DEBUG=1`) — GNOME 50 refuses `Introspect.GetWindows` and
`Screenshot` to non-shell callers, and the protocol log is stronger evidence
than pixels anyway. Three checks: the tray registers with its menu, `hide()`
unmaps a toplevel, and a compositor close request hides the window while the
process survives. With no compositor available it reports a named skip rather
than a false pass.
