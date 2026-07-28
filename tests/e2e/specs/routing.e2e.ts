// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/** Both windows exist, each renders its own surface, and the full one routes. */

import { strict as assert } from "node:assert";

import {
  assertNoDriverMutation,
  assertNoMutationYet,
  clickButton,
  compactWindow,
  fullWindow,
  testId,
  waitForTestId,
} from "../support";

describe("compact and full routing", () => {
  it("declares both windows", async () => {
    const handles = await browser.getWindowHandles();
    assert.equal(handles.length, 2, `expected the full and compact windows, got ${handles.length}`);
  });

  it("renders the compact panel in the compact window", async () => {
    await compactWindow();
    const status = await waitForTestId("quick-status");
    assert.ok((await status.getText()).length > 0, "the compact panel showed no status");
    assert.equal(
      await testId("manage-versions").isExisting(),
      false,
      "the compact panel rendered a full-window control",
    );
  });

  it("renders the Overview in the full window", async () => {
    await fullWindow();
    await waitForTestId("verdict");
    assert.equal(
      await testId("quick-status").isExisting(),
      false,
      "the full window rendered the compact panel",
    );
  });

  it("routes Overview to ROCm versions and back", async () => {
    await (await waitForTestId("manage-versions")).click();
    await waitForTestId("rows");
    await clickButton("Back to overview");
    await waitForTestId("verdict");
  });

  it("routes Overview to Settings and shows the autostart control", async () => {
    await (await waitForTestId("open-settings")).click();
    const autostart = await waitForTestId("autostart");
    assert.equal(
      await autostart.getAttribute("type"),
      "checkbox",
      "starting at login is not a checkbox",
    );
  });

  it("changes nothing on this machine while routing", () => {
    assertNoMutationYet("routing between surfaces");
    assertNoDriverMutation();
  });
});
