// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Pathological lengths: a raw lspci GPU name, a nightly version string, a
 * deep install path, and a long support-link label. Nothing may overlap and
 * nothing may push the page sideways.
 */

import { assertNoDriverMutation, assertNoMutationYet, fullWindow, waitForTestId } from "../support";
import { assertNoHorizontalScroll, assertNoOverlap, saveShot } from "../desktop";
import { fullState, quickState } from "./matrix";

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
    await (await waitForTestId("manage-versions")).click();
    await waitForTestId("rows");
    await browser.$("summary*=Details").click();
    await assertNoHorizontalScroll("runtime details with a deep path");
    await saveShot("long-content-runtimes-details--full--1440x900");
  });

  it("photographs the compact panel with the long name", async () => {
    // Long content may scroll vertically in the fixed panel; it may not
    // scroll sideways and the way out must stay reachable.
    await quickState("long-content", { allowVerticalScroll: true });
  });

  it("changed nothing on the machine", () => {
    assertNoMutationYet("long strings are a rendering problem, not a mutation");
    assertNoDriverMutation();
  });
});
