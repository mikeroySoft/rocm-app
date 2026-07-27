<!--
Copyright © Advanced Micro Devices, Inc., or its affiliates.

SPDX-License-Identifier: MIT
-->

# ROCm App architecture

This document is the text companion to the [interactive GitHub Pages view](index.html#architecture). It describes the repository as implemented now and marks renderer work that remains a proposal.

## Status

**Implemented backend foundation**

- Versioned `rocm app-snapshot` consumer with golden and live producer/consumer contract checks.
- Native Windows/Linux platform classification, including a distinct fail-closed WSL result.
- A Tauri-free `rocm-app-core` domain crate.
- `RocmController::snapshot`, `RocmController::plan`, and `RocmController::execute`.
- Typed Tauri commands for snapshot, planning, execution, cancellation, and progress events.
- Production adapters for the bundled CLI, catalog resolution, atomic file storage, clock, and log-backed notification records.
- Deterministic fixtures shared by Rust and TypeScript.

**Integration next**

- The React renderer still calls `fixture_snapshot`; it does not yet consume `controller_snapshot`.
- Review, approval, cancellation, and progress screens are not yet present in the renderer.
- The current `LogNotifier` records completion in app data; native desktop notification delivery is proposed behind the existing notifier seam.

The interactive flow therefore shows an implemented controller and Tauri surface plus the proposed renderer that will consume them. It is not a claim that the complete product journey is already shippable.

## Product boundary

ROCm App is a desktop control plane for **managed ROCm** on Radeon and Ryzen AI systems.

It may:

- inspect platform, GPU, driver, component, runtime, health, and update state;
- install a managed runtime;
- update a managed runtime;
- activate an installed runtime;
- remove a managed runtime;
- validate a managed runtime.

It may not:

- install, update, or remove a driver;
- run on WSL, macOS, ARM, or AMD Instinct hardware;
- silently move GPU-required work to the CPU;
- accept an executable path, command name, argv array, shell text, or environment map from the webview.

## Components

| Layer        | Component                          | Responsibility                                                                    | Status                                   |
| ------------ | ---------------------------------- | --------------------------------------------------------------------------------- | ---------------------------------------- |
| Presentation | `src/App.tsx`                      | Render a health verdict and platform-gated setup action                           | Implemented scaffold; fixture-backed     |
| Presentation | `src/lib/backend.ts`               | Keep backend discovery out of React components                                    | Implemented seam; controller wiring next |
| Desktop      | `src-tauri/src/lib.rs`             | Construct controller state and register typed Tauri commands                      | Implemented                              |
| Desktop      | `src-tauri/src/controller_host.rs` | Implement production adapters and renderer-safe command responses                 | Implemented                              |
| Domain       | `rocm-app-core::contract`          | Decode schema version 1 and fail closed on unknown action/support vocabulary      | Implemented                              |
| Domain       | `rocm-app-core::platform`          | Classify native Windows, native Linux, WSL, and unsupported hosts                 | Implemented                              |
| Domain       | `rocm-app-core::onboarding`        | Produce a pure ready-or-blocked first-run recommendation                          | Implemented                              |
| Domain       | `rocm-app-core::controller`        | Cache snapshots, issue plans, verify approvals, serialize mutations, and re-probe | Implemented                              |
| Domain       | `controller::request`              | Define the only five operations the webview can request                           | Implemented                              |
| Domain       | `controller::plan`                 | Bind an immutable plan to request, digest, TTL, and snapshot fingerprint          | Implemented                              |
| Domain       | `controller::progress`             | Emit started/stage plus exactly one terminal event                                | Implemented                              |
| Host         | `BundledCliInspector`              | Run the bundled CLI's `app-snapshot` contract command                             | Implemented                              |
| Host         | `BundledCli`                       | Map typed operations to explicit argv and spawn without a shell                   | Implemented                              |
| Host         | `SnapshotCatalog`                  | Resolve concrete versions from the snapshot's trusted update report               | Implemented                              |
| Host         | `FileStorage`                      | Persist app-owned data with atomic replacement                                    | Implemented                              |
| Host         | `LogNotifier`                      | Record completion truthfully in app data                                          | Implemented; native delivery proposed    |

## Runtime flows

### 1. Inspect the app snapshot

1. The proposed renderer integration invokes `controller_snapshot` with `refresh: true`.
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

1. The proposed renderer calls `onboarding_view` with defaults or explicit `Choices`.
2. Tauri obtains a full controller snapshot through flow 1.
3. Tauri checks available bytes for the selected target folder.
4. `onboarding::recommend` receives the snapshot, choices, and free-space result.
5. Supported machines receive `OnboardingView::Ready`, including facts, driver advice, folder options, and the exact `OperationRequest` to plan.
6. Unsupported machines receive `OnboardingView::Blocked` and no install action.

Recommendation is pure. It does not start an install or bypass plan review and approval.

### 3. Plan a runtime change

1. The proposed renderer sends a typed `OperationRequest` to `controller_plan`.
2. The controller validates every token, requires a cached snapshot, applies the host-platform gate, and fingerprints current state.
3. A `Latest` install resolves through `SnapshotCatalog`, which reuses `BundledCliInspector` and the snapshot's trusted update report.
4. Exact versions and runtime-key operations skip catalog resolution.
5. The controller builds plain-language `PlanStep` values; they are descriptions, not commands.
6. The plan is sealed with a unique id, SHA-256 digest, five-minute expiry, request, and snapshot fingerprint.
7. The authoritative plan remains in the controller while a display copy returns through Tauri.

Planning changes nothing.

### 4. Execute and settle an approved plan

1. The proposed renderer returns `Approval { planId, planDigest, request }` and a typed progress channel.
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
│   ├── scenarios.json                  shared deterministic renderer/core scenarios
│   └── contract/                       producer-generated app-snapshot goldens
├── src/
│   ├── App.tsx                         current health surface
│   └── lib/
│       ├── backend.ts                  renderer/backend seam
│       ├── platform.ts                 presentation-edge platform gate
│       └── scenarios.ts                TypeScript fixture consumer
└── src-tauri/
    ├── src/
    │   ├── lib.rs                      Tauri composition root and command registration
    │   └── controller_host.rs          production adapters and command transport
    └── crates/rocm-app-core/src/
        ├── contract.rs                 schema-v1 app-snapshot consumer
        ├── platform.rs                 host classification and eligibility
        ├── onboarding.rs               pure first-run recommendation
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
