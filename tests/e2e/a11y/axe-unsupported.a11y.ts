// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * axe on the unsupported host (WSL): the refusal on the Overview and the
 * versions list where every action is blocked with a reason.
 */

import {
  assertNoDriverMutation,
  assertNoMutationYet,
  fullWindow,
  testId,
  waitForTestId,
} from "../support";
import { checkA11y, injectAxe } from "./axe";

describe("axe: unsupported host", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("verdict");
    await waitForTestId("summary");
    await injectAxe();
  });

  it("overview refusal", async () => {
    await checkA11y("overview-unsupported");
  });

  it("runtimes list with blocked actions", async () => {
    await testId("manage-versions").click();
    await waitForTestId("rows");
    await checkA11y("runtimes-blocked");
  });

  it("changed nothing and never touched a kernel driver", () => {
    assertNoMutationYet("a11y unsupported");
    assertNoDriverMutation();
  });
});
