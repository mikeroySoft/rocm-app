// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * axe on the healthy machine: every full-window surface a user can reach from
 * the Overview, plus the quick panel. Statuses must also carry their state in
 * text on the two surfaces that summarise the machine.
 */

import { assertStatusesCarryText, showQuickWindow } from "../desktop";
import { assertNoDriverMutation, clickButton, fullWindow, testId, waitForTestId } from "../support";
import { checkA11y, injectAxe } from "./axe";

describe("axe: healthy", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("verdict");
    await injectAxe();
  });

  it("overview", async () => {
    await checkA11y("overview-healthy");
    await assertStatusesCarryText("overview-healthy");
  });

  it("runtimes list, with the first Details disclosure open", async () => {
    await testId("manage-versions").click();
    await waitForTestId("rows");
    // The per-version disclosure is a <summary>, not a <button>, so
    // `clickButton` cannot reach it.
    const details = browser.$("summary*=Details");
    await details.waitForClickable({ timeout: 30_000, timeoutMsg: "no Details disclosure" });
    await details.click();
    await checkA11y("runtimes-list");
    await clickButton("Back to overview");
  });

  it("settings", async () => {
    await testId("open-settings").click();
    await waitForTestId("autostart");
    await checkA11y("settings");
    await clickButton("Back to overview");
  });

  it("activity, and one opened record", async () => {
    await clickButton("Activity");
    await waitForTestId("sources");
    await checkA11y("activity");
    if (await testId("records").isExisting()) {
      await browser.$("button.logs__record").click();
      await checkA11y("activity-record");
    }
    await clickButton("Back to overview");
  });

  it("diagnose", async () => {
    await clickButton("Diagnose");
    await waitForTestId("verdict");
    await checkA11y("diagnose");
  });

  it("quick panel", async () => {
    await showQuickWindow();
    await checkA11y("quick");
    await assertStatusesCarryText("quick");
  });

  it("never touched a kernel driver", () => {
    assertNoDriverMutation();
  });
});
