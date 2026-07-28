// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/** WSL: supported hardware, unsupported host. Refusal must look calm too. */

import { assertNoDriverMutation, assertNoMutationYet, fullWindow, waitForTestId } from "../support";
import { fullState } from "./matrix";

describe("visual: unsupported host", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("verdict");
  });

  it("photographs the unsupported overview", async () => {
    await waitForTestId("summary");
    await waitForTestId("next-step");
    await fullState("unsupported-overview");
  });

  it("photographs the version list with every action blocked", async () => {
    await (await waitForTestId("manage-versions")).click();
    await waitForTestId("rows");
    await fullState("unsupported-versions");
  });

  it("offered nothing and changed nothing", () => {
    assertNoMutationYet("an unsupported host gets no mutation path");
    assertNoDriverMutation();
  });
});
