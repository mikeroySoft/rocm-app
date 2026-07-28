// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Guided setup on a machine with nothing installed, up to and past approval.
 *
 * The load-bearing assertion is the one between the review screen and the
 * approve click: the app has described a change to this computer and has not
 * made it. Everything else here is scaffolding for that moment.
 */

import { strict as assert } from "node:assert";

import {
  assertNoDriverMutation,
  assertNoMutationYet,
  changes,
  clickButton,
  fullWindow,
  until,
  waitForTestId,
} from "../support";

describe("guided setup", () => {
  before(async () => {
    await fullWindow();
  });

  it("recommends an install from the machine's own report", async () => {
    const facts = await waitForTestId("facts");
    assert.match(await facts.getText(), /R9700|gfx120X/i);
    await clickButton("Set up ROCm");
  });

  it("asks where it should go before it asks for approval", async () => {
    const folder = await waitForTestId("folder-input");
    assert.ok((await folder.getValue()).length > 0, "no target folder was proposed");
    await clickButton("Review the changes");
  });

  it("lists the change and marks the step that alters this computer", async () => {
    const steps = await waitForTestId("plan-steps");
    const text = await steps.getText();
    assert.match(text, /changes this computer/, `no mutating step was flagged:\n${text}`);
  });

  it("has still not changed anything at the moment of approval", () => {
    assertNoMutationYet("the review screen");
  });

  it("runs exactly one change once approved, and it is an install", async () => {
    await clickButton("Install ROCm");
    await waitForTestId("progress-status");
    const outcome = await waitForTestId("outcome", 60_000);
    assert.equal(
      await outcome.getAttribute("data-kind"),
      "success",
      `setup did not finish: ${await outcome.getText()}`,
    );

    const ran = changes();
    assert.equal(ran.length, 1, `expected one change, got:\n${ran.join("\n")}`);
    assert.match(ran[0] ?? "", /^install sdk /, `the change was not an install: ${ran[0]}`);
    assert.match(ran[0] ?? "", /--yes\b/, "the install was not run non-interactively");
  });

  it("reports the machine as set up afterwards", async () => {
    await until("the post-install snapshot to be read", () => changes().length === 1);
    assertNoDriverMutation();
  });
});
