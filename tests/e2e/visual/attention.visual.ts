// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * A machine that needs attention, driven through the update it asks for.
 * This is also the only place the update flow runs to its outcome — the
 * functional suite reviews it and backs out.
 */

import {
  assertIsolatedRoots,
  assertNoDriverMutation,
  changes,
  fullWindow,
  testId,
  waitForTestId,
} from "../support";
import { assertNoHorizontalScroll, assertReachable, resizeFull, saveShot } from "../desktop";
import { ACTIVE_RUNTIME } from "../scenarios";
import { fullState } from "./matrix";

describe("visual: attention and update", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("verdict");
  });

  it("photographs the attention overview", async () => {
    await fullState("attention-overview");
  });

  it("photographs the update offer", async () => {
    await (await waitForTestId("manage-versions")).click();
    await waitForTestId("rows");
    await fullState("attention-updates");
  });

  it("photographs the update review, the update running, and its result", async () => {
    await (await waitForTestId("update-action")).click();
    await waitForTestId("plan-steps");
    await fullState("update-review");

    await resizeFull(1024, 700);
    await (await waitForTestId("apply")).click();
    await waitForTestId("progress-status");
    await assertNoHorizontalScroll("update in progress");
    await assertReachable('[data-testid="stop"]', "update in progress");
    await saveShot("updating--full--1024x700");

    const outcome = await waitForTestId("outcome", 60_000);
    if ((await outcome.getAttribute("data-kind")) !== "success") {
      throw new Error("the update did not finish as a success");
    }
    await fullState("update-result-success");
  });

  it("performed exactly the approved update, against isolated roots", () => {
    const performed = changes();
    const expected = `update --apply --runtime ${ACTIVE_RUNTIME} --yes`;
    if (performed.length !== 1 || performed[0] !== expected) {
      throw new Error(`expected [${expected}], saw: ${JSON.stringify(performed)}`);
    }
    assertIsolatedRoots();
    assertNoDriverMutation();
  });

  it("shows the healed overview after the update", async () => {
    await browser.$("button*=Back to ROCm versions").click();
    await waitForTestId("rows");
    await browser.$("button*=Overview").click();
    await waitForTestId("verdict");
    // The snapshot behind the fixture switched to healthy when the update
    // ran; the overview must have followed it.
    await browser.waitUntil(
      async () => (await testId("verdict").getAttribute("data-value")) === "healthy",
      { timeout: 30_000, timeoutMsg: "the overview never picked up the post-update snapshot" },
    );
    await fullState("update-healed-overview");
  });
});
