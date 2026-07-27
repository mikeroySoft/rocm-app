// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Renderer-side view of guided setup.
 *
 * These are structural mirrors of `rocm_app_core::onboarding`. The renderer
 * decides nothing: which ROCm to install, where, and whether anything blocks
 * it is answered in Rust and arrives here already resolved. Recomputing any of
 * it in TypeScript would be a second answer that can disagree with the one the
 * install actually uses.
 */

import rawFixtures from "../../fixtures/onboarding.json";
import { invoke } from "@tauri-apps/api/core";
import type { SupportLink } from "./contract";
import type { Approval, ChangePlan, Channel, OperationRequest, ProgressEvent, VersionSelector } from "./controller";
import * as controller from "./controller";

export interface Fact {
  readonly key: string;
  readonly label: string;
  readonly value: string;
}

/** Driver information. There is no action field, by design. */
export interface DriverAdvice {
  readonly summary: string;
  readonly note: string;
  readonly links: readonly SupportLink[];
}

export type BlockerCode =
  | "unsupported-wsl"
  | "unsupported-platform"
  | "unknown-hardware"
  | "incomplete-probe"
  | "offline"
  | "untrusted-metadata"
  | "insufficient-space"
  | "protected-folder";

/** Exactly one thing the user can do about a blocker. */
export type NextAction =
  | { kind: "refresh"; label: string }
  | { kind: "choose-folder"; label: string }
  | { kind: "free-space"; label: string; neededBytes: number; availableBytes: number }
  | { kind: "nothing"; label: string };

export interface Blocker {
  readonly code: BlockerCode;
  readonly headline: string;
  readonly detail: string;
  readonly nextAction: NextAction;
}

export interface Recommendation {
  readonly facts: readonly Fact[];
  readonly driver: DriverAdvice;
  readonly firstRun: boolean;
  readonly channel: Channel;
  readonly family: string;
  readonly targetFolder: string;
  readonly folderChoices: readonly string[];
  readonly estimatedBytes: number;
  readonly availableBytes: number | null;
  readonly request: OperationRequest;
}

export type OnboardingView =
  | { state: "ready"; recommendation: Recommendation }
  | { state: "blocked"; blocker: Blocker };

export interface Choices {
  readonly channel: Channel;
  readonly version: VersionSelector;
  readonly targetFolder: string;
}

/**
 * What the flow needs from the outside world.
 *
 * A single seam, so the same component renders against the desktop backend and
 * against generated fixtures without knowing which it has.
 */
export interface OnboardingBackend {
  view(choices?: Choices): Promise<OnboardingView>;
  plan(request: OperationRequest): Promise<ChangePlan>;
  execute(approval: Approval, onEvent: (event: ProgressEvent) => void): Promise<void>;
  cancel(): Promise<void>;
}

/** The desktop backend. Every call crosses into Rust. */
export function desktopBackend(): OnboardingBackend {
  return {
    view: async (choices) => {
      controller.requireTauri();
      return await invoke<OnboardingView>("onboarding_view", { choices: choices ?? null });
    },
    plan: async (request) => await controller.plan(request),
    execute: async (approval, onEvent) => {
      await controller.execute(approval, onEvent);
    },
    cancel: controller.cancel,
  };
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

export interface ScenarioFixture {
  readonly name: string;
  readonly purpose: string;
  readonly targetFolder: string;
  readonly availableBytes: number | null;
  readonly view: OnboardingView;
  readonly plan: ChangePlan | null;
}

export interface OutcomeFixture {
  readonly name: string;
  readonly events: readonly ProgressEvent[];
}

export interface OnboardingFixtures {
  readonly scenarios: readonly ScenarioFixture[];
  readonly outcomes: readonly OutcomeFixture[];
}

/**
 * The generated fixture set.
 *
 * Statically imported, like `fixtures/scenarios.json` in `scenarios.ts`: the
 * path is a literal, and a build-time failure beats a runtime one.
 */
export const FIXTURES = rawFixtures as unknown as OnboardingFixtures;

export interface FixtureBackendOptions {
  /** Which recorded outcome the install replays. Default `success`. */
  readonly outcome?: string | undefined;
  /**
   * Stop replaying after this many events, leaving the progress screen live.
   * Used by screenshot runs to capture a mutation that is genuinely running.
   */
  readonly stopAfter?: number | undefined;
}

/** Records what a backend was asked to do. Lets a test prove ordering. */
export interface BackendCalls {
  readonly plans: OperationRequest[];
  readonly executions: Approval[];
  readonly cancels: number;
}

export interface FixtureBackend extends OnboardingBackend {
  readonly calls: BackendCalls;
}

/**
 * A backend that replays generated fixtures.
 *
 * It also counts calls, which is what lets a renderer test prove the UI issues
 * no execute before the user approves — the Rust suite proves the controller
 * runs nothing, and this proves the screen never asks it to.
 */
export function fixtureBackend(
  fixtures: OnboardingFixtures,
  scenario: string,
  options: FixtureBackendOptions = {},
): FixtureBackend {
  const found = fixtures.scenarios.find((s) => s.name === scenario);
  if (!found) {
    throw new Error(`unknown onboarding fixture scenario: ${scenario}`);
  }
  const outcomeName = options.outcome ?? "success";
  const outcome = fixtures.outcomes.find((o) => o.name === outcomeName);
  if (!outcome) {
    throw new Error(`unknown onboarding outcome fixture: ${outcomeName}`);
  }

  const calls: BackendCalls = { plans: [], executions: [], cancels: 0 };
  let cancelled = false;

  return {
    calls,
    view: () => Promise.resolve(found.view),
    plan: (request) => {
      calls.plans.push(request);
      return found.plan
        ? Promise.resolve(found.plan)
        : Promise.reject(new Error("this fixture has no plan"));
    },
    execute: (approval, onEvent) => {
      calls.executions.push(approval);
      const limit = options.stopAfter ?? outcome.events.length;
      for (const event of outcome.events.slice(0, limit)) {
        if (cancelled && !isTerminalEvent(event)) {
          continue;
        }
        onEvent(event);
      }
      return Promise.resolve();
    },
    cancel: () => {
      cancelled = true;
      return Promise.resolve();
    },
  };
}

function isTerminalEvent(event: ProgressEvent): boolean {
  return event.event === "completed" || event.event === "failed" || event.event === "cancelled";
}

/** Bytes as a person would say them. Mirrors `onboarding::format_bytes`. */
export function formatBytes(bytes: number): string {
  const gb = 1024 ** 3;
  if (bytes >= gb) {
    const whole = Math.floor(bytes / gb);
    const tenths = Math.floor(((bytes % gb) * 10) / gb);
    return tenths === 0 ? `${whole} GB` : `${whole}.${tenths} GB`;
  }
  return `${Math.ceil(bytes / 1024 ** 2)} MB`;
}
