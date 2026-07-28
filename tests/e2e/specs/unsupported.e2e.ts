// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * WSL: supported hardware, unsupported host.
 *
 * The product is native Windows and native Linux. Under WSL the app must say
 * so and offer nothing — not a disabled button, not a control that fails on
 * click. The refusal is computed twice on purpose (a pure predicate for the UI
 * and again in the controller), so this checks the one a user actually sees.
 */

import { strict as assert } from "node:assert";

import {
  assertNoDriverMutation,
  assertNoMutationYet,
  fullWindow,
  testId,
  waitForTestId,
} from "../support";

describe("unsupported host", () => {
  before(async () => {
    await fullWindow();
  });

  it("says the host is unsupported", async () => {
    const verdict = await waitForTestId("verdict");
    assert.equal(await verdict.getAttribute("data-value"), "unsupported");
  });

  it("says so in words, not just a label", async () => {
    const summary = await waitForTestId("summary");
    assert.match(
      await summary.getText(),
      /WSL/i,
      "the Overview never named the reason this host is unsupported",
    );
    const next = await waitForTestId("next-step");
    assert.match(await next.getText(), /No setup is available/i);
  });

  it("offers no setup and no change controls on the Overview", async () => {
    // Reading stays available: someone debugging a WSL setup still needs the
    // Overview, the versions list, and the logs. Only the change controls go.
    for (const id of ["apply", "update-action", "next-action"]) {
      assert.equal(
        await testId(id).isExisting(),
        false,
        `the Overview offered "${id}" on an unsupported host`,
      );
    }
  });

  it("offers no action on any installed version", async () => {
    await (await waitForTestId("manage-versions")).click();
    const rows = await waitForTestId("rows");
    const actions = await rows.$$("button").getElements();
    assert.equal(
      actions.length,
      0,
      `ROCm versions offered ${actions.length} action(s) on an unsupported host`,
    );
    const blocked = await browser.$$('[data-testid^="blocked-"]').getElements();
    assert.ok(blocked.length > 0, "no version explained why its actions are unavailable");
  });

  it("ran nothing at all", () => {
    assertNoMutationYet("an unsupported host");
    assertNoDriverMutation();
  });
});
