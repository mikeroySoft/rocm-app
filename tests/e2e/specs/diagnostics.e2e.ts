// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/** Activity, Diagnose, and writing a support bundle. */

import { strict as assert } from "node:assert";

import {
  assertIsolatedRoots,
  assertNoDriverMutation,
  assertNoMutationYet,
  clickButton,
  journal,
  testId,
  waitForTestId,
} from "../support";

describe("diagnostics", () => {
  before(async () => {
    await fullWindowThenNav();
  });

  async function fullWindowThenNav() {
    const { fullWindow } = await import("../support");
    await fullWindow();
    await waitForTestId("verdict");
  }

  it("offers removal guidance from the Overview notice, count first", async () => {
    // Three unmanaged installs ride the attention golden (#28): one counted
    // notice, no paths here, and no verdict change — coexistence is legal.
    const notices = await waitForTestId("notices");
    assert.match(await notices.getText(), /3 ROCm installs outside ROCm App/);
    await (await waitForTestId("review-removal")).click();
    await waitForTestId("unmanaged");
    await clickButton("Overview");
    await waitForTestId("verdict");
  });

  it("shows Activity with the sources the CLI reported", async () => {
    await clickButton("Activity");
    const sources = await waitForTestId("sources");
    const text = await sources.getText();
    for (const label of ["ROCm command history", "CLI activity"]) {
      assert.ok(text.includes(label), `Activity did not list "${label}":\n${text}`);
    }
  });

  it("explains an empty timeline instead of showing a blank page", async () => {
    const empty = testId("empty");
    const records = testId("records");
    assert.ok(
      (await empty.isExisting()) || (await records.isExisting()),
      "Activity rendered neither records nor an explanation",
    );
  });

  it("offers the support bundle behind a native folder picker", async () => {
    // The destination now comes from a native dialog, which WebDriver cannot
    // drive. What stays provable end to end: the picker is offered, nothing
    // is chosen yet, the export is disabled, and no CLI export ever fired.
    const chooser = await waitForTestId("choose-destination");
    assert.equal(await chooser.isDisplayed(), true, "the folder picker button is not offered");
    const destination = await waitForTestId("destination");
    assert.match(await destination.getText(), /No folder chosen yet\./);
    assert.equal(
      await testId("export").isEnabled(),
      false,
      "export was enabled without a chosen folder",
    );
    const exported = journal().filter((entry) => entry.argv[0] === "app-support-bundle");
    assert.equal(exported.length, 0, "an export ran without a chosen destination");
  });

  it("diagnoses from the CLI's own report", async () => {
    await clickButton("Overview");
    await waitForTestId("verdict");
    await clickButton("Diagnose");
    const verdict = await waitForTestId("verdict");
    assert.ok((await verdict.getText()).length > 0, "Diagnose showed no verdict");
    const findings = testId("findings");
    if (await findings.isExisting()) {
      assert.match(await findings.getText(), /\w/, "the findings list was empty markup");
    }
  });

  it("read and reported without changing anything", () => {
    assertNoMutationYet("reading Activity, exporting a bundle, and diagnosing");
    assertIsolatedRoots();
    assertNoDriverMutation();
  });
});
