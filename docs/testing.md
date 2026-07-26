<!--
Copyright © Advanced Micro Devices, Inc., or its affiliates.

SPDX-License-Identifier: MIT
-->

# Testing

## Gates

Every one of these must exit 0 before a change is considered done.

| Gate | Command |
|---|---|
| Install (reproducible) | `npm ci` |
| Frontend production build | `npm run build` |
| Typecheck | `npm run typecheck` |
| Lint | `npm run lint` |
| Frontend unit tests | `npm test -- --run` |
| Rust tests | `cargo test --manifest-path src-tauri/Cargo.toml --all-targets` |
| Rust lint | `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` |

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

The producer lives in rocm-cli's `rocm` *binary* crate, which cannot be linked
as a library, and this app pins rocm-cli to a published revision. So the wire
format — not a shared Rust type — is the contract, and drift is caught two ways:

**Golden fixtures** in `fixtures/contract/` are generated from the real producer,
never hand-written. Regenerate them from the rocm-cli checkout:

```bash
ROCM_APP_GOLDEN_DIR=../rocm-app/fixtures/contract \
  cargo test -p rocm --bin rocm app_contract
```

| Fixture | Covers |
|---|---|
| `healthy`, `setup-required`, `attention`, `partial`, `offline-stale` | verdict space |
| `unsupported-wsl` | no eligible actions at all |
| `invalid-future-version` | schema version this build cannot implement |
| `invalid-payload` | right version, incomplete body |
| `invalid-malformed` | not JSON — the CLI printed an error instead |

**A live harness** (`tests/contract_producer_consumer.rs`) runs the
repository-built `rocm` binary against three empty state roots and decodes its
real output. Goldens prove the decoder handles what the producer *once* emitted;
this proves it handles what the producer emits *now*. The isolation is itself
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

`fixtures/scenarios.json` is the single source of truth, read by both
`rocm_app_core::fixtures` and `src/lib/scenarios.ts`. Two hand-maintained copies
drift silently, and a renderer test then passes against data the backend would
never produce.

Scenarios: `healthy`, `setup-required`, `attention`, `unsupported-wsl`, `partial`.

Two invariants are asserted on **both** sides, because both sides can regress
independently:

- No fixture may advertise an install on a host that cannot support one.
- Timestamps are fixed. A fixture that reads a clock breaks screenshot diffing
  on a schedule nobody can reproduce.

## Fixture mode

Set `ROCM_APP_FIXTURE=1` at build time to expose the scenario switcher. Fixture
mode touches no GPU, no network, and no real ROCm config, data, or cache root.
Production bundles have no switcher and no way to fabricate a health state.

```bash
ROCM_APP_FIXTURE=1 npm run dev
```
