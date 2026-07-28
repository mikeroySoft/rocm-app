// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The compact matrix — the one spec that also runs at 125% and 200% text
 * scale. It boots the long-content scenario on purpose: the raised scales
 * shrink the panel's CSS viewport (to ~304px and ~190px), and the raw lspci
 * GPU name is the worst string the panel will ever hold.
 */

import { fullWindow, testId, until, waitForTestId } from "../support";
import {
  assertNoHorizontalScroll,
  assertReachable,
  assertStatusesCarryText,
  resizeFull,
  saveShot,
  showQuickWindow,
} from "../desktop";
import { SCALE } from "./matrix";

describe(`visual: compact matrix at scale ${SCALE}`, () => {
  it("photographs the full overview at this scale", async () => {
    await fullWindow();
    await waitForTestId("verdict");
    await resizeFull(1024, 700);
    await assertNoHorizontalScroll(`overview at scale ${SCALE}`);
    await saveShot("overview--full--1024x700");
  });

  it("keeps the compact panel usable at this scale", async () => {
    await showQuickWindow();
    await waitForTestId("quick-status");
    // Measure the settled state: the "Checking…" placeholder is shorter
    // than any real GPU name, and this scenario's name is the longest one.
    await until(`the panel to settle at scale ${SCALE}`, async () => {
      const status = await testId("quick-status").getAttribute("data-status");
      return status !== "checking";
    });
    await waitForTestId("quick-gpu");
    await assertNoHorizontalScroll(`compact panel at scale ${SCALE}`);
    await assertReachable('[data-testid="quick-open"]', `compact panel at scale ${SCALE}`);
    await assertStatusesCarryText(`compact panel at scale ${SCALE}`);
    await saveShot("quick--380x300");
  });
});
