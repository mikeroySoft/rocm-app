// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * A test that fails, on purpose, every time.
 *
 * `scripts/e2e_selftest.py` runs this file under a config with retries turned
 * *up*, and asserts three things the real suite depends on: the artifacts are
 * captured, the retries stop at the configured bound, and the run is still red
 * at the end. A harness whose failure path has never executed is a harness
 * that reports green for reasons nobody has checked.
 *
 * The assertion is deliberately a *functional* one against a real, healthy
 * app — not a thrown string. A retry policy that can turn this green is a
 * retry policy that can turn a genuine regression green.
 */

import { strict as assert } from "node:assert";
import { appendFileSync } from "node:fs";
import { join } from "node:path";

import { fullWindow, paths, waitForTestId } from "../support";

describe("deliberate failure", () => {
  it("records every attempt and then fails", async () => {
    appendFileSync(join(paths.artifacts(), "..", "attempts.log"), `${new Date().toISOString()}\n`);
    await fullWindow();
    const verdict = await waitForTestId("verdict");
    const actual = await verdict.getAttribute("data-value");
    assert.equal(
      actual,
      "this-verdict-does-not-exist",
      `deliberate failure: the app reported "${actual}", which is correct`,
    );
  });
});
