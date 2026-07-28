// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/** A half-observed machine: the probe did not finish, and the UI says so. */

import { assertNoDriverMutation, assertNoMutationYet, fullWindow, waitForTestId } from "../support";
import { fullState } from "./matrix";

describe("visual: partial probe", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("verdict");
  });

  it("photographs the partial-information overview", async () => {
    await fullState("partial-overview");
  });

  it("changed nothing on the machine", () => {
    assertNoMutationYet("an incomplete probe must not trigger anything");
    assertNoDriverMutation();
  });
});
