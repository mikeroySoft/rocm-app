// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/** Guided setup, end to end: recommend, location, review, progress, result. */

import {
  assertIsolatedRoots,
  assertNoDriverMutation,
  assertNoMutationYet,
  changes,
  clickButton,
  fullWindow,
  until,
  waitForTestId,
} from "../support";
import { assertNoHorizontalScroll, assertReachable, resizeFull, saveShot } from "../desktop";
import { fullState } from "./matrix";

describe("visual: guided setup", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("facts");
  });

  it("photographs the recommendation", async () => {
    await fullState("setup-required");
  });

  it("photographs the location step", async () => {
    await clickButton("Set up ROCm");
    await waitForTestId("folder-input");
    await fullState("setup-location");
  });

  it("photographs the review step, still without a mutation", async () => {
    await clickButton("Review the changes");
    await waitForTestId("plan-steps");
    await fullState("setup-review");
    assertNoMutationYet("review must not have run anything");
  });

  it("photographs the install in progress and its result", async () => {
    // One known size before the clock starts: the stand-in install takes
    // four seconds, and a resize mid-capture would race it.
    await resizeFull(1024, 700);
    await clickButton("Install ROCm");
    await waitForTestId("progress-status");
    await assertNoHorizontalScroll("setup progress");
    await assertReachable('[data-testid="stop"]', "setup progress");
    await saveShot("setup-progress--full--1024x700");

    await waitForTestId("outcome", 60_000);
    await until("the post-install snapshot to be read", () => changes().length === 1);
    await fullState("setup-result-success");
  });

  it("performed exactly the approved install, against isolated roots", () => {
    const performed = changes();
    if (performed.length !== 1 || !/^install sdk /.test(performed[0] ?? "")) {
      throw new Error(`expected exactly one install, saw: ${JSON.stringify(performed)}`);
    }
    assertIsolatedRoots();
    assertNoDriverMutation();
  });
});
