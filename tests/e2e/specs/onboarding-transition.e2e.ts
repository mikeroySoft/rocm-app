// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * First run on a machine that already has ROCm outside the app (#28).
 *
 * The load-bearing assertions: the advisory step appears before any
 * recommendation, the handover to removal guidance comes back to setup with
 * detection re-run, Continue is never blocked on removal — and the whole
 * walk changes nothing on the machine.
 */

import { strict as assert } from "node:assert";

import {
  assertIsolatedRoots,
  assertNoDriverMutation,
  assertNoMutationYet,
  clickButton,
  fullWindow,
  journal,
  until,
  waitForTestId,
} from "../support";

const snapshotReads = () => journal().filter((entry) => entry.argv[0] === "app-snapshot").length;

describe("first run beside unmanaged ROCm", () => {
  before(async () => {
    await fullWindow();
  });

  it("interposes the advisory step, listing every reported path", async () => {
    await waitForTestId("transition");
    const paths = await waitForTestId("transition-paths");
    const text = await paths.getText();
    for (const path of ["/opt/rocm", "/usr/local/rocm", "/srv/rocm-mystery"]) {
      assert.ok(text.includes(path), `the step did not list ${path}:\n${text}`);
    }
    assertNoMutationYet("the transition step");
  });

  it("hands over to removal guidance without duplicating it", async () => {
    await clickButton("Review removal guidance");
    // The one owner of origins, warnings, and commands: the ROCm versions
    // surface's own unmanaged section.
    await waitForTestId("unmanaged");
    assert.ok(
      await browser.$("button*=Back to setup").isExisting(),
      "the way back must read as a return to setup, not to the overview",
    );
  });

  it("returns to setup, re-runs detection, and shows the step again", async () => {
    const before = snapshotReads();
    await clickButton("Back to setup");
    await waitForTestId("transition");
    // The step's return must come from a fresh probe, not a remembered one.
    await until("a fresh snapshot read after returning to setup", () => snapshotReads() > before);
  });

  it("continues setup anyway, into the unchanged recommendation flow", async () => {
    await clickButton("Continue setup anyway");
    const facts = await waitForTestId("facts");
    assert.match(await facts.getText(), /R9700|gfx120X/i);
    assert.ok(
      await browser.$("button*=Set up ROCm").isExisting(),
      "the recommendation must still offer setup",
    );
  });

  it("read and reported without changing anything", () => {
    assertNoMutationYet("the whole transition walk");
    assertIsolatedRoots();
    assertNoDriverMutation();
  });
});
