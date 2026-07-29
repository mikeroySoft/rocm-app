// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/** Installing a picked version from the catalog, through review and apply. */

import { strict as assert } from "node:assert";

import { NIGHTLY_VERSION } from "../scenarios";
import {
  assertNoDriverMutation,
  assertNoMutationYet,
  changes,
  fullWindow,
  testId,
  waitForTestId,
} from "../support";

describe("catalog install", () => {
  before(async () => {
    await fullWindow();
  });

  it("reaches the catalog from the Overview", async () => {
    await (await waitForTestId("manage-versions")).click();
    await waitForTestId("rows");
    const catalog = await waitForTestId("catalog");
    assert.equal(await catalog.getAttribute("data-state"), "fresh");
  });

  it("keeps pre-release versions behind the opt-in", async () => {
    assert.equal(await testId(`catalog-install-${NIGHTLY_VERSION}`).isExisting(), false);
    await (await waitForTestId("catalog-prerelease")).click();
    await waitForTestId(`catalog-install-${NIGHTLY_VERSION}`);
  });

  it("reviews the exact picked version without performing anything", async () => {
    await (await waitForTestId(`catalog-install-${NIGHTLY_VERSION}`)).click();
    const resolved = await waitForTestId("resolved-version");
    assert.match(
      await resolved.getText(),
      new RegExp(NIGHTLY_VERSION.replace(/\./g, "\\.")),
      "the review screen did not name the picked version",
    );
    assertNoMutationYet("the catalog review screen");
  });

  it("runs exactly the install the review promised", async () => {
    await (await waitForTestId("apply")).click();
    const outcome = await waitForTestId("outcome", 60_000);
    assert.equal(
      await outcome.getAttribute("data-kind"),
      "success",
      `the install did not finish: ${await outcome.getText()}`,
    );
    assert.deepEqual(changes(), [
      `install sdk --channel nightly --format wheel --family gfx120X-all --yes --version ${NIGHTLY_VERSION}`,
    ]);
  });

  it("lists the new version once the machine reports it", async () => {
    await browser.$("button*=Back to ROCm versions").click();
    const rows = await waitForTestId("rows");
    await browser.waitUntil(async () => (await rows.getText()).includes(NIGHTLY_VERSION), {
      timeout: 20_000,
      timeoutMsg: "the installed list never picked up the new version",
    });
    // The catalog now points at the installed list instead of re-offering it.
    await (await waitForTestId("catalog-prerelease")).click();
    const entry = await waitForTestId(`catalog-entry-${NIGHTLY_VERSION}`);
    assert.match(await entry.getText(), /Installed/);
    assert.equal(await testId(`catalog-install-${NIGHTLY_VERSION}`).isExisting(), false);
  });

  it("never invoked a driver command", () => {
    assertNoDriverMutation();
  });
});
