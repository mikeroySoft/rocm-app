// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/** Activity, Diagnose, and writing a support bundle. */

import { strict as assert } from "node:assert";
import { join } from "node:path";

import {
  assertIsolatedRoots,
  assertNoDriverMutation,
  assertNoMutationYet,
  clickButton,
  journal,
  paths,
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
    await clickButton("Back to overview");
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

  it("writes a support bundle to the folder the user names", async () => {
    const destination = join(paths.state(), "bundle-out");
    const field = await waitForTestId("destination");
    await field.setValue(destination);
    await testId("export").click();
    const receipt = await waitForTestId("export-receipt", 60_000);
    const text = await receipt.getText();
    assert.match(text, /[0-9a-f]{8}/i, `the receipt showed no digest:\n${text}`);

    const exported = journal().filter((entry) => entry.argv[0] === "app-support-bundle");
    assert.equal(exported.length, 1, "the export did not go through the CLI exactly once");
    assert.ok(
      exported[0]?.argv.includes(destination),
      `the destination was not passed as one argument: ${exported[0]?.argv.join(" ")}`,
    );
  });

  it("diagnoses from the CLI's own report", async () => {
    await clickButton("Back to overview");
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
