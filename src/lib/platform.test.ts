// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";
import type { HostPlatform } from "./platform";
import { HOST_PLATFORMS, installAllowed, unsupportedReason } from "./platform";

describe("platform gate", () => {
  it("allows install only on native Windows and Linux", () => {
    expect(installAllowed("windows")).toBe(true);
    expect(installAllowed("linux")).toBe(true);
    expect(installAllowed("wsl")).toBe(false);
    expect(installAllowed("unsupported")).toBe(false);
  });

  it("explains every refusal and stays silent when supported", () => {
    expect(unsupportedReason("windows")).toBeNull();
    expect(unsupportedReason("linux")).toBeNull();
    expect(unsupportedReason("wsl")).toMatch(/native Windows and native Linux/);
    expect(unsupportedReason("unsupported")).toMatch(/native Windows and native Linux/);
  });

  // Mirrors `reason_and_gate_agree` in rocm-app-core: the two answers must never
  // disagree, or the UI shows an Install button beside a "not supported" notice.
  it("keeps the gate and the reason consistent", () => {
    for (const platform of HOST_PLATFORMS satisfies readonly HostPlatform[]) {
      expect(installAllowed(platform)).toBe(unsupportedReason(platform) === null);
    }
  });
});
