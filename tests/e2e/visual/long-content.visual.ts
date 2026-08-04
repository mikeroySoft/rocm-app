// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Pathological lengths: a raw lspci GPU name, a nightly version string, a
 * deep install path, and a long support-link label. Nothing may overlap and
 * nothing may push the page sideways.
 *
 * The compact panel is no longer photographed: the tray menu lost its
 * "Quick status" door, so there is no user path that shows it on Linux.
 */

import { assertNoDriverMutation, assertNoMutationYet, fullWindow, waitForTestId } from "../support";
import { assertNoHorizontalScroll, assertNoOverlap, saveShot } from "../desktop";
import { fullState } from "./matrix";

/**
 * Reach a control by CSS selector, without a pointer when the scale is raised.
 *
 * At 125% and 200% the driver's pointer maths and the page's CSS pixels
 * disagree: a click aimed at an element's centre lands near centre ÷ scale,
 * which on a rail of adjacent doors is the door to its left — the run that
 * caught this opened Diagnose instead of ROCm versions. Whether a control is
 * reachable and hittable is asserted by the geometry checks in `desktop.ts`;
 * here a click is only a way to reach the next state, so above 1x it goes
 * through the DOM.
 */
async function reach(selector: string): Promise<void> {
  if ((process.env["ROCM_VISUAL_SCALE"] ?? "1") === "1") {
    await browser.$(selector).click();
    return;
  }
  await browser.execute((css: string) => {
    const el = document.querySelector(css);
    if (el instanceof HTMLElement) {
      el.click();
    }
  }, selector);
}

describe("visual: long content", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("verdict");
  });

  it("photographs the overview with a raw lspci GPU name", async () => {
    await fullState("long-content-overview");
    // The GPU value wraps inside its own grid track; the label column and
    // the value column must never intersect.
    await assertNoOverlap(
      [".dash__facts dt", '[data-testid="fact-gpu"]'],
      "long GPU name in the facts grid",
    );
  });

  it("keeps a deep install path inside the details disclosure", async () => {
    await waitForTestId("manage-versions");
    await reach('[data-testid="manage-versions"]');
    await waitForTestId("rows");
    await reach(".runtimes__row details > summary");
    await assertNoHorizontalScroll("runtime details with a deep path");
    await saveShot("long-content-runtimes-details--full--1440x900");
  });

  it("changed nothing on the machine", () => {
    assertNoMutationYet("long strings are a rendering problem, not a mutation");
    assertNoDriverMutation();
  });
});
