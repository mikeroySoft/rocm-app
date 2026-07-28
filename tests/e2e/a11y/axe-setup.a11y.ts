// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * axe across the guided setup, one step at a time, including the progress
 * step while the stand-in install is actually running.
 */

import { assertNoDriverMutation, clickButton, fullWindow, waitForTestId } from "../support";
import { checkA11y, injectAxe } from "./axe";

describe("axe: guided setup", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("facts");
    await injectAxe();
  });

  it("recommend step", async () => {
    await checkA11y("onboarding-recommend");
  });

  it("location step", async () => {
    await clickButton("Set up ROCm");
    await waitForTestId("folder-input");
    await checkA11y("onboarding-location");
  });

  it("review step", async () => {
    await clickButton("Review the changes");
    await waitForTestId("plan-steps");
    await checkA11y("onboarding-review");
  });

  it("progress step, checked while the install runs", async () => {
    await clickButton("Install ROCm");
    await waitForTestId("progress-status");
    // The stand-in install takes four seconds and axe on this small page
    // finishes well inside that; if this ever proves flaky, shrink the run
    // to a smaller rule subset for this one state.
    await checkA11y("onboarding-progress");
  });

  it("result step", async () => {
    await waitForTestId("outcome", 60_000);
    await checkA11y("onboarding-result");
  });

  it("never touched a kernel driver", () => {
    assertNoDriverMutation();
  });
});
