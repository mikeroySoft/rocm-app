// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

import rawScenarios from "../../fixtures/scenarios.json";
import type { HostPlatform } from "./platform";

/**
 * Deterministic fixture snapshots.
 *
 * `fixtures/scenarios.json` is the single source of truth, read by both this
 * module and `rocm_app_core::fixtures`. Two hand-maintained copies of the same
 * fixture set drift silently, and a renderer test then passes against data the
 * backend would never produce.
 */
export const SCENARIO_NAMES = [
  "healthy",
  "setup-required",
  "attention",
  "unsupported-wsl",
  "partial",
] as const;

export type ScenarioName = (typeof SCENARIO_NAMES)[number];

export const VERDICTS = ["healthy", "setup-required", "attention", "unsupported", "unknown"] as const;

export type Verdict = (typeof VERDICTS)[number];

export interface FixtureSnapshot {
  readonly scenario: ScenarioName;
  readonly platform: HostPlatform;
  readonly verdict: Verdict;
  readonly reasonCode: string;
  readonly headline: string;
  readonly detail: string;
  readonly installAvailable: boolean;
  readonly checkedAt: string;
}

export const SCENARIOS: readonly FixtureSnapshot[] = rawScenarios as readonly FixtureSnapshot[];

export function scenario(name: ScenarioName): FixtureSnapshot {
  const found = SCENARIOS.find((s) => s.scenario === name);
  if (!found) {
    throw new Error(`unknown fixture scenario: ${name}`);
  }
  return found;
}

export function isScenarioName(value: string): value is ScenarioName {
  return (SCENARIO_NAMES as readonly string[]).includes(value);
}
