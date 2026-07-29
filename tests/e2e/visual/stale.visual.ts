// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/** Offline with stale data: the "unknown" verdict has to read honestly. */

import { assertNoDriverMutation, assertNoMutationYet, fullWindow, waitForTestId } from "../support";
import { fullState, quickState } from "./matrix";

describe("visual: offline and stale", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("verdict");
  });

  it("photographs the stale overview", async () => {
    await fullState("offline-stale-overview");
  });

  it("photographs the version list with its offline catalog notice", async () => {
    await (await waitForTestId("manage-versions")).click();
    await waitForTestId("catalog-notice");
    await fullState("offline-stale-versions");
    await browser.$("button*=Back to overview").click();
    await waitForTestId("verdict");
  });

  it("photographs the compact panel in the stale state", async () => {
    await quickState("offline-stale");
  });

  it("changed nothing on the machine", () => {
    assertNoMutationYet("a stale read is still a read");
    assertNoDriverMutation();
  });
});
