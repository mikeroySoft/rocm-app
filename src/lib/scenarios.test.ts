// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";
import { installAllowed } from "./platform";
import { SCENARIO_NAMES, SCENARIOS, isScenarioName, scenario } from "./scenarios";

describe("fixture scenarios", () => {
  it("exposes exactly the declared scenario set", () => {
    expect(SCENARIOS.map((s) => s.scenario)).toEqual([...SCENARIO_NAMES]);
  });

  it("resolves every scenario by name", () => {
    for (const name of SCENARIO_NAMES) {
      expect(scenario(name).scenario).toBe(name);
    }
  });

  it("rejects an unknown scenario instead of defaulting", () => {
    // A silent fallback to `healthy` would render a reassuring screen from a typo.
    expect(() => scenario("nope" as never)).toThrow(/unknown fixture scenario/);
    expect(isScenarioName("nope")).toBe(false);
    expect(isScenarioName("healthy")).toBe(true);
  });

  // The same safety property `rocm-app-core` asserts over the shared JSON. Both
  // sides check it because both sides can independently regress.
  it("never advertises install on an ineligible host", () => {
    for (const snap of SCENARIOS) {
      if (snap.installAvailable) {
        expect(installAllowed(snap.platform)).toBe(true);
      }
    }
  });

  it("treats WSL as unsupported with nothing on offer", () => {
    const wsl = scenario("unsupported-wsl");
    expect(wsl.platform).toBe("wsl");
    expect(wsl.verdict).toBe("unsupported");
    expect(wsl.installAvailable).toBe(false);
  });

  it("uses a fixed timestamp so screenshots do not drift", () => {
    for (const snap of SCENARIOS) {
      expect(snap.checkedAt).toBe("2026-01-01T00:00:00Z");
    }
  });
});
