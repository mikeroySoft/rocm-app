// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/** Healthy machine: overview, settings, version list, compact panel. */

import { assertNoDriverMutation, assertNoMutationYet, fullWindow, waitForTestId } from "../support";
import { fullState, quickState } from "./matrix";

describe("visual: healthy", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("verdict");
  });

  it("photographs the overview", async () => {
    await fullState("healthy-overview");
  });

  it("photographs settings", async () => {
    await (await waitForTestId("open-settings")).click();
    await waitForTestId("autostart");
    await fullState("settings");
  });

  it("photographs the version list, closed and with details open", async () => {
    await browser.$("button*=Back to overview").click();
    await waitForTestId("verdict");
    await (await waitForTestId("manage-versions")).click();
    await waitForTestId("rows");
    await fullState("runtimes-list");
    await browser.$("summary*=Details").click();
    await fullState("runtimes-details-open");
  });

  it("photographs the catalog with pre-release tiers revealed", async () => {
    await (await waitForTestId("catalog-prerelease")).click();
    await waitForTestId("catalog-nightly");
    await fullState("runtimes-catalog-prerelease");
  });

  it("photographs the compact panel", async () => {
    await quickState("healthy");
  });

  it("changed nothing on the machine", () => {
    assertNoMutationYet("the visual pass is read-only");
    assertNoDriverMutation();
  });
});
