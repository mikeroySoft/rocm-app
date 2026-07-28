// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The machine states the desktop suite runs against.
 *
 * Each scenario is a real producer snapshot from `fixtures/contract/` — the
 * same goldens the contract harness pins — plus, where the scenario performs a
 * change, the responses the stand-in CLI gives for it and the snapshot the
 * machine reports afterwards. Nothing here is hand-written JSON pretending to
 * be a machine: a scenario that drifts from what `rocm app-snapshot` really
 * emits fails the Phase 2 harness first.
 *
 * `prefix:` keys match on an argv prefix. The install command ends with
 * `--prefix <install root>`, and that root is a per-run temporary directory, so
 * an exact key could not name it.
 */

export interface Scenario {
  /** Path under `fixtures/`, relative, of the snapshot served first. */
  readonly snapshot: string;
  /** Extra snapshots this scenario can switch to, by file name. */
  readonly extraSnapshots?: Readonly<Record<string, string>>;
  /** `mutations.json` for the stand-in CLI. */
  readonly mutations?: Readonly<Record<string, MutationResponse>>;
}

export interface MutationResponse {
  readonly exit?: number;
  readonly stdout?: string;
  readonly stderr?: string;
  readonly delayMs?: number;
  readonly thenSnapshot?: string;
}

/** Runtime keys carried by the contract goldens. */
export const ACTIVE_RUNTIME = "nightly-wheel-gfx120x-all-7-14-0";
export const OTHER_RUNTIME = "nightly-wheel-gfx120x-all-7-13-0";

export const SCENARIOS: Readonly<Record<string, Scenario>> = {
  /** Nothing installed yet: the state guided setup exists for. */
  "setup-required": {
    snapshot: "contract/setup-required.json",
    extraSnapshots: { "installed.json": "contract/healthy.json" },
    mutations: {
      // Long enough that the progress screen is a state the suite can observe
      // and assert on, rather than a frame that flashes between two polls. A
      // real install takes minutes; this is the smallest stand-in that still
      // makes "review, then progress, then result" three checkable steps.
      "prefix:install sdk": {
        exit: 0,
        stdout: "installed\n",
        delayMs: 4000,
        thenSnapshot: "installed.json",
      },
    },
  },

  /** A validated runtime is active and there is nothing to do. */
  healthy: {
    snapshot: "contract/healthy.json",
    extraSnapshots: { "switched.json": "e2e/switched.json" },
    mutations: {
      [`runtimes activate ${OTHER_RUNTIME}`]: {
        exit: 0,
        stdout: "activated\n",
        delayMs: 600,
        thenSnapshot: "switched.json",
      },
      [`runtimes list --runtime ${OTHER_RUNTIME}`]: { exit: 0, stdout: "ok\n" },
      [`runtimes uninstall ${OTHER_RUNTIME} --yes`]: { exit: 0, stdout: "removed\n" },
    },
  },

  /** The active runtime failed validation and an update is available. */
  attention: {
    snapshot: "contract/attention.json",
    extraSnapshots: { "updated.json": "contract/healthy.json" },
    mutations: {
      [`update --apply --runtime ${ACTIVE_RUNTIME} --yes`]: {
        exit: 0,
        stdout: "updated\n",
        // Long enough that the updating progress screen is a state the suite
        // can observe and assert on, rather than a frame that flashes between
        // two polls. Nothing consumed the shorter delay: no spec drives an
        // update to completion today.
        delayMs: 4000,
        thenSnapshot: "updated.json",
      },
      [`runtimes activate ${OTHER_RUNTIME}`]: {
        exit: 0,
        stdout: "activated\n",
        delayMs: 400,
      },
    },
  },

  /** WSL: supported hardware, unsupported host. Nothing may be offered. */
  "unsupported-wsl": {
    snapshot: "contract/unsupported-wsl.json",
  },

  /** Offline for long enough that the last update check has gone stale. */
  "offline-stale": { snapshot: "contract/offline-stale.json" },

  /** The producer could only report part of the machine. */
  partial: { snapshot: "contract/partial.json" },

  /** Real-world worst-case string lengths; a UI stress state, not a golden. */
  "long-content": { snapshot: "e2e/long-content.json" },

  /** An install that fails mid-download with a realistic multi-line error. */
  "install-fails": {
    snapshot: "contract/setup-required.json",
    mutations: {
      "prefix:install sdk": {
        exit: 1,
        delayMs: 1500,
        stderr:
          "error: the download was interrupted after 312 MB\n" +
          "caused by: connection reset by peer\n" +
          "help: check the network connection and try again\n",
      },
    },
  },
} as const;

/** Which scenario a spec file boots into, keyed by file name. */
export const SPEC_SCENARIOS: Readonly<Record<string, string>> = {
  "first-launch.e2e.ts": "setup-required",
  "healthy-boot.e2e.ts": "healthy",
  "onboarding.e2e.ts": "setup-required",
  "runtime-switch.e2e.ts": "healthy",
  "diagnostics.e2e.ts": "attention",
  "routing.e2e.ts": "healthy",
  "unsupported.e2e.ts": "unsupported-wsl",
  "deliberate-failure.e2e.ts": "healthy",
  "overview.visual.ts": "healthy",
  "setup.visual.ts": "setup-required",
  "attention.visual.ts": "attention",
  "unsupported.visual.ts": "unsupported-wsl",
  "stale.visual.ts": "offline-stale",
  "partial.visual.ts": "partial",
  "activity.visual.ts": "attention",
  "long-content.visual.ts": "long-content",
  "failure.visual.ts": "install-fails",
  "compact.visual.ts": "long-content",
  "axe-healthy.a11y.ts": "healthy",
  "axe-setup.a11y.ts": "setup-required",
  "axe-attention.a11y.ts": "attention",
  "axe-unsupported.a11y.ts": "unsupported-wsl",
  "keyboard.a11y.ts": "healthy",
  "keyboard-flows.a11y.ts": "setup-required",
  "reduced-motion.a11y.ts": "setup-required",
};
