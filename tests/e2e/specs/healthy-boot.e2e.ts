// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/** A machine with a validated, active runtime: the Overview, and nothing to do. */

import { strict as assert } from "node:assert";

import {
  assertIsolatedRoots,
  assertNoDriverMutation,
  assertNoMutationYet,
  fullWindow,
  testId,
  until,
  waitForTestId,
} from "../support";

describe("healthy boot", () => {
  before(async () => {
    await fullWindow();
  });

  it("lands on the Overview with a healthy verdict", async () => {
    const verdict = await waitForTestId("verdict");
    assert.equal(await verdict.getAttribute("data-value"), "healthy");
  });

  it("names the GPU and the active runtime", async () => {
    const facts = await waitForTestId("headline-facts");
    const text = await facts.getText();
    assert.match(text, /R9700/, `the Overview did not name the GPU: ${text}`);
    assert.match(text, /7\.14\.0/, `the Overview did not name the active runtime: ${text}`);
  });

  it("offers the read-only surfaces and the version manager", async () => {
    for (const id of ["manage-versions", "open-settings", "refresh"]) {
      assert.equal(await (testId(id)).isDisplayed(), true, `${id} was not offered`);
    }
  });

  it("changes nothing when the user refreshes", async () => {
    const before = (await (testId("freshness")).getText()) || "";
    await (testId("refresh")).click();
    await until("the Overview to refresh", async () => {
      const now = await (testId("freshness")).getText();
      return now !== before || now.length > 0;
    });
    assertNoMutationYet("refreshing the Overview");
    assertIsolatedRoots();
    assertNoDriverMutation();
  });
});
