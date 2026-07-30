// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * First run beside unmanaged ROCm (#28): the advisory transition step, and
 * the removal guidance it opens with the "Back to setup" return.
 */

import { clickButton, fullWindow, waitForTestId } from "../support";
import { fullState } from "./matrix";

describe("visual: first-run transition step", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("transition");
  });

  it("photographs the advisory step", async () => {
    await fullState("setup-transition");
  });

  it("photographs the removal guidance opened from setup", async () => {
    await clickButton("Review removal guidance");
    await waitForTestId("unmanaged");
    await fullState("setup-transition-guidance");
  });
});
