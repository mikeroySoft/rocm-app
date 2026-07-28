// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Guided setup driven by keyboard alone, appending to the same transcript as
 * the healthy keyboard spec. The focus-management contract under test: the
 * flow moves focus to the step title on every step change
 * (`heading.current?.focus()`), so a keyboard or screen-reader user always
 * reads where they are before what they can do.
 */

import { strict as assert } from "node:assert";

import {
  assertNoDriverMutation,
  assertNoMutationYet,
  fullWindow,
  until,
  waitForTestId,
} from "../support";
import { active, note, press, pressUntil } from "./keys";

describe("keyboard: guided setup flows", () => {
  /** Whether the <details> around the focused summary is open. */
  const disclosureOpen = () =>
    browser.execute(() => {
      const el = document.activeElement;
      const details = el instanceof HTMLElement ? el.closest("details") : null;
      return details === null ? null : details.open;
    });

  /** The value of the checked radio in the builds group, if any. */
  const checkedChannel = () =>
    browser.execute(() => {
      const checked = document.querySelector('input[name="channel"]:checked');
      return checked instanceof HTMLInputElement ? checked.value : null;
    });

  before(async () => {
    await fullWindow();
    await waitForTestId("facts");
    note("");
    note("## keyboard-flows (setup-required)");
  });

  it("Enter on Set up ROCm advances and hands focus to the step title", async () => {
    await pressUntil(
      "Tab",
      'the "Set up ROCm" button',
      (el) => el.tag === "BUTTON" && el.text === "Set up ROCm",
      30,
    );
    await press("Enter");
    await waitForTestId("folder-input");
    await until("focus to land on the step title", async () => (await active()).tag === "H1");
  });

  it("Back returns to the recommendation and focus lands on the title again", async () => {
    await pressUntil("Tab", "the install folder input", (el) => el.testid === "folder-input", 10);
    // Type nothing: the suggested folder must survive a keyboard round trip.
    await pressUntil(
      "Tab",
      'the "Back" button',
      (el) => el.tag === "BUTTON" && el.text === "Back",
      15,
    );
    await press("Enter");
    await waitForTestId("facts");
    await until("focus to return to the step title", async () => (await active()).tag === "H1");
  });

  it("Enter toggles the Advanced options disclosure", async () => {
    await pressUntil("Tab", 'the "Advanced options" disclosure', (el) => el.tag === "SUMMARY", 15);
    assert.equal(await disclosureOpen(), false, "Advanced options started open");
    await press("Enter");
    assert.equal(await disclosureOpen(), true, "Enter did not open the disclosure");
    await press("Enter");
    assert.equal(await disclosureOpen(), false, "Enter again did not close the disclosure");
  });

  it("ArrowDown moves the checked radio inside the builds group", async () => {
    await press("Enter"); // the previous test left the disclosure closed; reopen it
    assert.equal(await disclosureOpen(), true, "the disclosure did not reopen");
    await pressUntil("Tab", "the builds radio group", (el) => el.tag === "INPUT", 5);
    const before = await checkedChannel();
    assert.ok(before !== null, "no radio in the builds group is checked");
    await press("ArrowDown");
    await until("the checked radio to move", async () => {
      const now = await checkedChannel();
      return now !== null && now !== before;
    });
  });

  it("changed nothing and never touched a kernel driver", () => {
    assertNoMutationYet("keyboard flows");
    assertNoDriverMutation();
  });
});
