// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * An install that fails with a multi-line CLI error. The failure screen has
 * to show every line the CLI wrote, keep its own controls reachable, and
 * offer the way back.
 */

import {
  assertIsolatedRoots,
  assertNoDriverMutation,
  changes,
  clickButton,
  fullWindow,
  waitForTestId,
} from "../support";
import { resizeFull } from "../desktop";
import { fullState } from "./matrix";

describe("visual: failed install", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("facts");
  });

  it("drives the install into its failure", async () => {
    await clickButton("Set up ROCm");
    await waitForTestId("folder-input");
    await clickButton("Review the changes");
    await waitForTestId("plan-steps");
    await resizeFull(1024, 700);
    await clickButton("Install ROCm");
    const outcome = await waitForTestId("outcome", 60_000);
    if ((await outcome.getAttribute("data-kind")) !== "failed") {
      throw new Error("the deliberately failing install did not fail");
    }
  });

  it("renders the CLI's error with its line breaks intact", async () => {
    await waitForTestId("outcome-detail");
    const shape = await browser.execute(() => {
      const el = document.querySelector('[data-testid="outcome-detail"]');
      if (!(el instanceof HTMLElement)) {
        return null;
      }
      const style = getComputedStyle(el);
      return {
        whiteSpace: style.whiteSpace,
        lines: el.getBoundingClientRect().height / parseFloat(style.lineHeight),
      };
    });
    if (!shape) {
      throw new Error("the failure detail did not render");
    }
    if (shape.whiteSpace !== "pre-line") {
      throw new Error(`multi-line detail collapsed: white-space is ${shape.whiteSpace}`);
    }
    if (shape.lines < 2.5) {
      throw new Error(`a three-line error rendered as ${shape.lines.toFixed(1)} lines`);
    }
    await fullState("setup-result-failed");
  });

  it("attempted exactly the approved install, against isolated roots", () => {
    const performed = changes();
    if (performed.length !== 1 || !/^install sdk /.test(performed[0] ?? "")) {
      throw new Error(`expected exactly one attempt, saw: ${JSON.stringify(performed)}`);
    }
    assertIsolatedRoots();
    assertNoDriverMutation();
  });
});
