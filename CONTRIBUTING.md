<!--
Copyright © Advanced Micro Devices, Inc., or its affiliates.

SPDX-License-Identifier: MIT
-->

# Contributing

## Before you push

Run every gate in [docs/testing.md](docs/testing.md). All must exit 0.

Commits require a DCO sign-off (`git commit -s`) and a cryptographic signature.
SSH signing is the simplest route:

```bash
git config --global gpg.format ssh
git config --global user.signingkey ~/.ssh/id_ed25519.pub
git config --global commit.gpgsign true
```

Add the same key to GitHub as a **signing** key — GitHub keeps authentication
keys and signing keys in separate lists, and a key added only for
authentication will not verify your commits.

## Rules that are not negotiable

**Never widen the platform gate.** `HostPlatform::install_allowed` is the single
place that decides whether a host may be offered a mutation. When it returns
false the control must be _omitted_, not disabled — a greyed-out Install button
on WSL still promises the operation is nearly available.

**Never add a driver mutation.** Driver data is read-only in the types, the UI,
the commands, the package manifests, and the tests. Report the version, link to
official release notes, stop there.

**Never pin a shared crate to a moving reference.** The `rocm-core`,
`rocm-dash-core`, and `rocm-dash-collectors` dependencies in
`src-tauri/Cargo.toml` use exact 40-character revisions. A branch or tag lets
the CLI's meaning of "runtime family" or "health" change underneath a released
installer. When you move a pin, re-verify everything re-exported from
`rocm_app_core::shared` — that module exists to make the blast radius visible.

**Never add a capability the app does not need.** `src-tauri/capabilities/`
grants no shell execution, no filesystem access, and no arbitrary HTTP.
Privileged work goes through a typed command, never a generic plugin
permission.

**Never let a fixture read a clock or the network.** Fixtures are deterministic
by contract. See the invariants in [docs/testing.md](docs/testing.md).

## Where code goes

Product decisions belong in `src-tauri/crates/rocm-app-core`, which has no Tauri
dependency and is testable without a WebView. The `src-tauri` crate above it is
a composition root: it wires the core to typed commands and owns no rules.

If you find yourself writing an `if` in a Tauri command, it probably belongs in
the core crate with a test beside it.
