// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The visual suite: screenshots and geometry against the shipped binary.
 *
 * Everything is inherited from the functional e2e config — isolation, the
 * driver lifecycle, per-test failure artifacts — because a screenshot of an
 * app in a half-isolated environment proves nothing. What changes is only
 * which specs run. Text scale comes in through `ROCM_VISUAL_SCALE`, which the
 * base config maps onto `GDK_DPI_SCALE` on the driver environment, and the
 * orchestrator (`scripts/ui_quality.py`) owns the session bus and the tray
 * watcher that let a spec open the compact window the way a user does.
 */

import { join } from "node:path";

import { REPO } from "./harness";
import { config as base } from "./wdio.conf";

export const config: WebdriverIO.Config = {
  ...base,
  specs: [join(REPO, "tests", "e2e", "visual", "*.visual.ts")],
};
