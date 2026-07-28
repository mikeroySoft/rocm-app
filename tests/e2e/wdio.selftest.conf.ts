// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The harness proving its own failure path.
 *
 * Identical to `wdio.conf.ts` except that it runs the one spec that always
 * fails and turns `specFileRetries` **up** to the bound the project allows.
 * `scripts/e2e_selftest.py` then checks that the bound was reached, that the
 * run is still red, and that the artifacts a failure must leave behind are
 * there. The real suite keeps every retry at zero; this config exists so the
 * bound is a measured number rather than a claim.
 */

import { join } from "node:path";

import { REPO } from "./harness";
import { config as base } from "./wdio.conf";

/** How many times a spec file may be retried before the run is red for good. */
export const RETRY_BOUND = 2;

export const config: WebdriverIO.Config = {
  ...base,
  specs: [join(REPO, "tests", "e2e", "self-test", "*.e2e.ts")],
  specFileRetries: RETRY_BOUND,
  specFileRetriesDelay: 0,
  mochaOpts: { ui: "bdd", timeout: 120_000, retries: 0 },
};
