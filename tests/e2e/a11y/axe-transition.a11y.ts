// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * axe across the first-run transition step (#28) and the removal guidance
 * it opens — the one onboarding state the setup scan cannot reach, because
 * it only exists beside unmanaged ROCm.
 */

import { assertNoDriverMutation, clickButton, fullWindow, waitForTestId } from "../support";
import { checkA11y, injectAxe } from "./axe";

describe("axe: first-run transition", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("transition");
    await injectAxe();
  });

  it("transition step", async () => {
    await checkA11y("onboarding-transition");
  });

  it("removal guidance opened from setup", async () => {
    await clickButton("Review removal guidance");
    await waitForTestId("unmanaged");
    await injectAxe();
    await checkA11y("runtimes-from-setup");
  });

  it("never touched a kernel driver", () => {
    assertNoDriverMutation();
  });
});
