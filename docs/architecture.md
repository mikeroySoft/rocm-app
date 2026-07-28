<!--
Copyright © Advanced Micro Devices, Inc., or its affiliates.

SPDX-License-Identifier: MIT
-->

# ROCm App architecture

This document is the text companion to the [interactive GitHub Pages view](index.html#architecture). It describes the repository as implemented now.

## Status

Implemented end to end: the versioned `rocm app-snapshot` consumer with golden
and live producer/consumer checks; native Windows/Linux classification with a
fail-closed WSL result; the Tauri-free `rocm-app-core` domain crate;
`RocmController::snapshot` / `plan` / `execute` behind typed Tauri commands;
production adapters for the bundled CLI, catalog resolution, atomic storage,
clock, notifications, and diagnostics; the React renderer for onboarding, the
overview, version management, activity, diagnosis, settings, and the tray's
quick-status window; the tray monitor itself; deb/rpm/NSIS packaging with
ownership guards; and the desktop e2e, visual, and accessibility suites.

The interactive GitHub Pages view still presents the controller-era flows;
where the two disagree, this file is current.

## Product boundary

ROCm App is a desktop control plane for **managed ROCm** on Radeon and Ryzen AI systems.

It may:

- inspect platform, GPU, driver, component, runtime, health, and update state;
- read local activity records and diagnose known causes;
- install a managed runtime;
- update a managed runtime;
- activate an installed runtime;
- remove a managed runtime;
- validate a managed runtime;
- apply a diagnosed fix it can perform itself;
- export a redacted support bundle to a folder the user names.

It may not:

- install, update, or remove a driver;
- run on WSL, macOS, ARM, or AMD Instinct hardware;
- silently move GPU-required work to the CPU;
- accept an executable path, command name, argv array, shell text, or environment map from the webview.

## Components

| Layer        | Component                          | Responsibility                                                                    | Status                                   |
| ------------ | ---------------------------------- | --------------------------------------------------------------------------------- | ---------------------------------------- |
| Presentation | `src/App.tsx`                      | Route between onboarding, overview, runtimes, activity, diagnostics, settings | Implemented                              |
| Presentation | `src/lib/*.ts` backends            | One typed seam per surface (dashboard, onboarding, runtimes, tray, logs)       | Implemented                              |
| Desktop      | `src-tauri/src/lib.rs`             | Construct controller state and register typed Tauri commands                      | Implemented                              |
| Desktop      | `src-tauri/src/controller_host.rs` | Implement production adapters and renderer-safe command responses                 | Implemented                              |
| Desktop      | `src-tauri/src/tray_host.rs`       | Own the tray icon, quick window, notifications, and autostart plumbing         | Implemented                              |
| Domain       | `rocm-app-core::contract`          | Decode schema version 1 and fail closed on unknown action/support vocabulary      | Implemented                              |
| Domain       | `rocm-app-core::platform`          | Classify native Windows, native Linux, WSL, and unsupported hosts                 | Implemented                              |
| Domain       | `rocm-app-core::onboarding`        | Produce a pure ready-or-blocked first-run recommendation                          | Implemented                              |
| Domain       | `rocm-app-core::health`            | Derive the whole Overview from typed snapshot fields                           | Implemented                              |
| Domain       | `rocm-app-core::runtimes`          | Version table, action guards, and update standing                              | Implemented                              |
| Domain       | `rocm-app-core::tray`              | Status mapping, menu shape, icon rasterisation, probe schedule                 | Implemented                              |
| Domain       | `rocm-app-core::diagnostics`       | Bounded log reads, diagnosis views, fix eligibility                            | Implemented                              |
| Domain       | `rocm-app-core::controller`        | Cache snapshots, issue plans, verify approvals, serialize mutations, and re-probe | Implemented                              |
| Domain       | `controller::request`              | Define the only six operations the webview can request                        | Implemented                              |
| Domain       | `controller::plan`                 | Bind an immutable plan to request, digest, TTL, and snapshot fingerprint          | Implemented                              |
| Domain       | `controller::progress`             | Emit started/stage plus exactly one terminal event                                | Implemented                              |
| Host         | `BundledCliInspector`              | Run the bundled CLI's `app-snapshot` contract command                             | Implemented                              |
| Host         | `BundledCli`                       | Map typed operations to explicit argv and spawn without a shell                   | Implemented                              |
| Host         | `SnapshotCatalog`                  | Resolve concrete versions from the snapshot's trusted update report               | Implemented                              |
| Host         | `FileStorage`                      | Persist app-owned data with atomic replacement                                    | Implemented                              |
| Host         | `DesktopNotifier`                  | Deliver transition notifications natively and persist the last one announced  | Implemented                              |

## Runtime flows

### 1. Inspect the app snapshot

1. The renderer invokes `controller_snapshot` with `refresh: true`.
2. Tauri calls `RocmController::snapshot(Freshness::Full)`.
3. The controller calls the production `Inspector` adapter.
4. `BundledCliInspector` runs the app-bundled `rocm app-snapshot` executable without a shell.
5. The CLI inspects platform, GPU, components, managed runtimes, driver inventory, and update state.
6. The CLI emits the versioned JSON payload.
7. The adapter checks the process result and calls `contract::decode`.
8. The controller caches the typed `AppSnapshot`.
9. Tauri returns `SnapshotResponse { snapshot, deferred }` to the renderer.

A full refresh requested during a mutation returns a cached snapshot marked `deferred` instead of contending with the operation. With no cached value, the controller still performs the first read.

### 2. Recommend first-run setup

1. The renderer calls `onboarding_view` with defaults or explicit `Choices`.
2. Tauri obtains a full controller snapshot through flow 1.
3. Tauri checks available bytes for the selected target folder.
4. `onboarding::recommend` receives the snapshot, choices, and free-space result.
5. Supported machines receive `OnboardingView::Ready`, including facts, driver advice, folder options, and the exact `OperationRequest` to plan.
6. Unsupported machines receive `OnboardingView::Blocked` and no install action.

Recommendation is pure. It does not start an install or bypass plan review and approval.

### 3. Plan a runtime change

1. The renderer sends a typed `OperationRequest` to `controller_plan`.
2. The controller validates every token, requires a cached snapshot, applies the host-platform gate, and fingerprints current state.
3. A `Latest` install resolves through `SnapshotCatalog`, which reuses `BundledCliInspector` and the snapshot's trusted update report.
4. Exact versions and runtime-key operations skip catalog resolution.
5. The controller builds plain-language `PlanStep` values; they are descriptions, not commands.
6. The plan is sealed with a unique id, SHA-256 digest, five-minute expiry, request, and snapshot fingerprint.
7. The authoritative plan remains in the controller while a display copy returns through Tauri.

Planning changes nothing.

### 4. Execute and settle an approved plan

1. The renderer returns `Approval { planId, planDigest, request }` and a typed progress channel.
2. The controller rejects a plan that is missing, replayed, expired, modified, bound to another snapshot, or paired with a different request.
3. A mutating request acquires one atomic single-flight lock; validation remains read-only.
4. Progress starts with `Started`.
5. `argv_for` maps the authoritative typed request to the exact bundled CLI arguments.
6. The current `BundledCli` emits one `execute` Stage event, then `std::process::Command` launches the explicit program with separate arguments and no shell.
7. A process error emits `Failed` and returns; a cancellation request emits `Cancelled`.
8. Success triggers a fresh `app-snapshot` probe and cache update.
9. The controller emits `Completed`, then `LogNotifier` appends a completion record through `FileStorage`.
10. Tauri returns the fresh `OperationOutcome` to the renderer.

A plan is consumed on execution entry rather than success. A failed operation may already have side effects, so the same approval cannot be replayed. Every path emits exactly one terminal progress event.

## Host modes

The topology is the same on both supported hosts. Only native process and path details vary.

| Concern            | Native Linux                     | Native Windows 11                           |
| ------------------ | -------------------------------- | ------------------------------------------- |
| Bundled executable | `rocm`                           | `rocm.exe`                                  |
| Invocation         | Explicit child process and argv  | Explicit child process and argv             |
| App data           | Tauri app data directory         | `%APPDATA%`-backed Tauri app data directory |
| Shell plugin       | Not registered                   | Not registered                              |
| Driver behavior    | Inventory and support links only | Inventory and support links only            |

WSL is a separate unsupported platform result rather than a Linux mode.

## Trust and concurrency invariants

- The webview can name a product operation but cannot describe a process invocation.
- Driver mutation is unrepresentable in `OperationRequest` and `EligibleAction`.
- Unsupported and unrecognised platform support states receive no mutation offers.
- Future contract schema versions are rejected before body decoding.
- Unknown eligible actions decode but are never offered.
- Plans are immutable, expiring, snapshot-bound, and single-use.
- Only one mutation runs at a time; the second receives a deterministic `Busy` error.
- The mutation lock releases through an RAII guard on every return path.
- A successful mutation always refreshes the machine snapshot before returning.
- Every progress stream has exactly one terminal event.

## Source map

```text
rocm-app/
├── fixtures/
│   ├── contract/                       producer-generated app-snapshot goldens
│   ├── e2e/                            desktop-scenario fixtures
│   └── *.json                          per-surface views generated by the Rust suite
├── scripts/                            staging, packaging, isolation, and quality harnesses
├── tests/e2e/                          WebdriverIO desktop, visual, and a11y suites
├── src/
│   ├── App.tsx                         shell routing between surfaces
│   ├── dashboard/  onboarding/  runtimes/  logs/  tray/
│   └── lib/
│       ├── contract.ts                 typed wire mirror
│       ├── controller.ts               plan/execute/cancel bridge
│       └── dashboard.ts  onboarding.ts  runtimes.ts  tray.ts  logs.ts
└── src-tauri/
    ├── src/
    │   ├── lib.rs                      Tauri composition root and command registration
    │   ├── controller_host.rs          production adapters and command transport
    │   └── tray_host.rs                tray icon, quick window, notifications, autostart
    ├── packaging/                      installer hooks: ownership guards, autostart cleanup
    └── crates/rocm-app-core/src/
        ├── contract.rs                 schema-v1 app-snapshot consumer
        ├── platform.rs                 host classification and eligibility
        ├── onboarding.rs               pure first-run recommendation
        ├── health.rs                   overview derivation
        ├── runtimes.rs                 version management views and guards
        ├── tray.rs                     tray status, menu, icon, schedule
        ├── diagnostics.rs              activity reads and diagnosis views
        └── controller/
            ├── mod.rs                  snapshot / plan / execute
            ├── request.rs              typed operation vocabulary
            ├── plan.rs                 plans, digests, approvals, fingerprints
            ├── progress.rs             operation event protocol
            └── adapters.rs             I/O seams, argv mapping, deterministic fakes
```

## Contract proof

The producer lives in rocm-cli's binary crate, so the JSON wire format is the boundary. Drift is checked in two complementary ways:

1. Producer-generated golden fixtures cover supported verdicts, WSL, stale/offline state, malformed payloads, and a future schema.
2. `contract_producer_consumer.rs` runs a repository-built `rocm` executable against isolated state roots and decodes its current output.

See [`testing.md`](testing.md) for the exact gates and fixture-generation command.
