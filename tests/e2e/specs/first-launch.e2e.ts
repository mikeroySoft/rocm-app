// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/** First launch on a machine with no managed ROCm at all. */

import { strict as assert } from "node:assert";

import {
  assertIsolatedRoots,
  assertNoDriverMutation,
  assertNoMutationYet,
  fullWindow,
  journal,
  waitForTestId,
} from "../support";

describe("first launch", () => {
  before(async () => {
    await fullWindow();
  });

  it("lands on guided setup rather than the Overview", async () => {
    // The shell asks the Overview whether this is a first run and routes on
    // the answer, so this also pins that the snapshot was read and understood.
    const facts = await waitForTestId("facts");
    assert.ok((await facts.getText()).length > 0, "the recommendation showed no facts");
    assert.equal(await browser.$('[data-testid="verdict"]').isExisting(), false);
  });

  it("read the machine through the isolated roots", () => {
    assertIsolatedRoots();
    const first = journal()[0];
    assert.deepEqual(first?.argv, ["app-snapshot"], "the first thing the app did was not a read");
  });

  it("changed nothing by starting", () => {
    assertNoMutationYet("first launch");
  });

  it("never invoked a driver command", () => {
    assertNoDriverMutation();
  });
});
