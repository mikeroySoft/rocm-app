// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * axe on the machine that needs attention: the Overview wearing its warning,
 * the versions list with its Updates section, and the update review. The
 * review is only looked at, never applied — the visual suite owns the apply
 * path.
 */

import {
  assertNoDriverMutation,
  assertNoMutationYet,
  clickButton,
  fullWindow,
  testId,
  waitForTestId,
} from "../support";
import { checkA11y, injectAxe } from "./axe";

describe("axe: attention", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("verdict");
    await injectAxe();
  });

  it("overview", async () => {
    await checkA11y("overview-attention");
  });

  it("runtimes list with the Updates section", async () => {
    await testId("manage-versions").click();
    await waitForTestId("rows");
    await checkA11y("runtimes-update");
  });

  it("update review, backed out of", async () => {
    await testId("update-action").click();
    await waitForTestId("plan-steps");
    await checkA11y("update-review");
    await clickButton("Back");
  });

  it("changed nothing and never touched a kernel driver", () => {
    assertNoMutationYet("a11y attention");
    assertNoDriverMutation();
  });
});
