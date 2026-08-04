// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The healthy machine, driven by keyboard alone: Tab order on the Overview,
 * visible focus, Enter and Space activation. Every key sent lands in the
 * transcript.
 */

import { strict as assert } from "node:assert";

import {
  assertNoDriverMutation,
  clickButton,
  fullWindow,
  testId,
  until,
  waitForTestId,
} from "../support";
import { describeActive, note, press, pressUntil, type ActiveElement } from "./keys";

/** The only tags keyboard focus may ever rest on. */
const INTERACTIVE: Readonly<Record<string, true>> = {
  BUTTON: true,
  A: true,
  INPUT: true,
  SELECT: true,
  SUMMARY: true,
};

describe("keyboard: healthy", () => {
  /** The Overview's Tab order as walked by the first test, reused by the second. */
  const collected: ActiveElement[] = [];

  const autostartChecked = () =>
    browser.execute(() => {
      const box = document.querySelector('[data-testid="autostart"]');
      return box instanceof HTMLInputElement ? box.checked : null;
    });

  before(async () => {
    await fullWindow();
    await waitForTestId("verdict");
    note("");
    note("## keyboard (healthy)");
  });

  it("Tab walks the Overview through interactive controls in reading order", async () => {
    for (let step = 0; step < 25; step += 1) {
      const el = await press("Tab");
      if (el.tag === "BODY" || el.tag === "HTML" || el.tag === "NONE") {
        break; // focus wrapped out of the document
      }
      const first = collected[0];
      if (first !== undefined && describeActive(el) === describeActive(first)) {
        break; // cycled back to the start
      }
      collected.push(el);
    }
    const trail = collected.map(describeActive).join("\n  ");
    const activity = collected.findIndex((el) => el.tag === "BUTTON" && el.text === "Activity");
    const diagnose = collected.findIndex((el) => el.tag === "BUTTON" && el.text === "Diagnose");
    const manage = collected.findIndex((el) => el.testid === "manage-versions");
    const settings = collected.findIndex((el) => el.testid === "open-settings");
    assert.ok(activity >= 0, `Tab never reached the Activity nav button:\n  ${trail}`);
    assert.ok(diagnose > activity, `Diagnose did not follow Activity:\n  ${trail}`);
    assert.ok(manage > diagnose, `manage-versions did not follow the nav:\n  ${trail}`);
    assert.ok(settings > manage, `open-settings did not follow manage-versions:\n  ${trail}`);
    for (const el of collected) {
      assert.ok(
        INTERACTIVE[el.tag] === true,
        `Tab rested on a non-interactive element: ${describeActive(el)}\n  ${trail}`,
      );
    }
  });

  it("Shift+Tab steps back to the previous control", async () => {
    const settingsIndex = collected.findIndex((el) => el.testid === "open-settings");
    const previous = collected[settingsIndex - 1];
    assert.ok(previous !== undefined, "nothing was recorded before open-settings");
    await pressUntil("Tab", "the Settings button", (el) => el.testid === "open-settings", 30);
    const back = await press("Shift+Tab");
    assert.equal(describeActive(back), describeActive(previous));
  });

  it("keyboard focus is visible on a focused button", async () => {
    await pressUntil("Tab", "a button", (el) => el.tag === "BUTTON", 30);
    const indicator = await browser.execute(() => {
      const el = document.activeElement;
      if (!(el instanceof HTMLElement)) {
        return null;
      }
      const style = window.getComputedStyle(el);
      return { outline: style.outlineStyle, shadow: style.boxShadow };
    });
    assert.ok(indicator !== null, "no element holds focus");
    assert.ok(
      indicator.outline !== "none" || indicator.shadow !== "none",
      `the focused button shows no focus indicator ` +
        `(outline-style: ${indicator.outline}, box-shadow: ${indicator.shadow})`,
    );
  });

  it("Enter activates the focused control, there and back", async () => {
    await pressUntil(
      "Tab",
      "the Activity nav button",
      (el) => el.tag === "BUTTON" && el.text === "Activity",
      30,
    );
    await press("Enter");
    await waitForTestId("sources");
    await pressUntil(
      "Tab",
      "the Overview nav button",
      (el) => el.tag === "BUTTON" && el.text === "Overview",
      30,
    );
    await press("Enter");
    await waitForTestId("verdict");
  });

  it("Space toggles the autostart checkbox", async () => {
    await testId("open-settings").click();
    await waitForTestId("autostart");
    await pressUntil("Tab", "the autostart checkbox", (el) => el.testid === "autostart", 30);
    const before = await autostartChecked();
    assert.ok(before !== null, "the autostart checkbox was not found");
    await press("Space");
    await until("the autostart checkbox to report the toggled state", async () => {
      return (await autostartChecked()) === !before;
    });
    // Put the setting back so the spec leaves the machine as it found it.
    await press("Space");
    await until("the autostart checkbox to report the original state", async () => {
      return (await autostartChecked()) === before;
    });
    await clickButton("Overview");
  });

  it("never touched a kernel driver", () => {
    assertNoDriverMutation();
  });
});
