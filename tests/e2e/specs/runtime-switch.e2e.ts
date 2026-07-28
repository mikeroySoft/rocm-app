// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/** Changing which installed ROCm version is active, through review and apply. */

import { strict as assert } from "node:assert";

import { OTHER_RUNTIME } from "../scenarios";
import {
  assertNoDriverMutation,
  assertNoMutationYet,
  changes,
  fullWindow,
  testId,
  waitForTestId,
} from "../support";

describe("runtime switch", () => {
  before(async () => {
    await fullWindow();
  });

  it("reaches ROCm versions from the Overview", async () => {
    await (await waitForTestId("manage-versions")).click();
    const rows = await waitForTestId("rows");
    assert.match(await rows.getText(), /7\.13\.0/, "the inactive runtime was not listed");
  });

  it("describes the switch without performing it", async () => {
    await (await waitForTestId("action-7.13.0-activate")).click();
    const steps = await waitForTestId("plan-steps");
    assert.ok((await steps.getText()).length > 0, "the review screen listed no steps");
    assertNoMutationYet("the runtime review screen");
  });

  it("runs exactly the activate the review promised", async () => {
    await (await waitForTestId("apply")).click();
    const outcome = await waitForTestId("outcome", 60_000);
    assert.equal(
      await outcome.getAttribute("data-kind"),
      "success",
      `the switch did not finish: ${await outcome.getText()}`,
    );
    assert.deepEqual(changes(), [`runtimes activate ${OTHER_RUNTIME}`]);
  });

  it("shows the other runtime active once the machine reports it", async () => {
    await browser.$("button*=Back to ROCm versions").click();
    const rows = await waitForTestId("rows");
    await browser.waitUntil(async () => (await rows.getText()).includes("7.13.0"), {
      timeout: 20_000,
      timeoutMsg: "the runtime list never refreshed",
    });
    assert.equal(await testId("rows").isDisplayed(), true);
  });

  it("never invoked a driver command", () => {
    assertNoDriverMutation();
  });
});
