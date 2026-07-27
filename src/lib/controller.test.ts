// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";
import attentionJson from "../../fixtures/contract/attention.json";
import healthyJson from "../../fixtures/contract/healthy.json";
import partialJson from "../../fixtures/contract/partial.json";
import setupRequiredJson from "../../fixtures/contract/setup-required.json";
import wslJson from "../../fixtures/contract/unsupported-wsl.json";
import type { AppSnapshot } from "./contract";
import { activeRuntime, installAllowedFor, offerableActions } from "./contract";
import type { ChangePlan, OperationRequest, ProgressEvent } from "./controller";
import { approvalFor, isPlanApprovable, isTerminal } from "./controller";

/**
 * Goldens are imported rather than read from disk: the same files the Rust
 * consumer decodes, resolved by the bundler so the renderer suite needs no
 * filesystem access under jsdom.
 */
const GOLDENS: Record<string, AppSnapshot> = {
  healthy: healthyJson as unknown as AppSnapshot,
  "setup-required": setupRequiredJson as unknown as AppSnapshot,
  attention: attentionJson as unknown as AppSnapshot,
  "unsupported-wsl": wslJson as unknown as AppSnapshot,
  partial: partialJson as unknown as AppSnapshot,
};

function golden(name: string): AppSnapshot {
  const found = GOLDENS[name];
  if (!found) {
    throw new Error(`unknown golden fixture: ${name}`);
  }
  return found;
}
const PLAN: ChangePlan = {
  id: "plan-1767225600000-000001",
  request: { operation: "activate-runtime", key: "nightly-wheel-gfx120x-all-7-14-0" },
  steps: [
    { stage: "validate", summary: "Check the selected version works", mutating: false },
    { stage: "activate", summary: "Make this the version ROCm uses", mutating: true },
  ],
  resolvedVersion: null,
  createdAtUnixMs: 1_767_225_600_000,
  expiresAtUnixMs: 1_767_225_900_000,
  digest: "abc123",
};

describe("controller request vocabulary", () => {
  // The real assertion is at the type level: this file would not compile if a
  // request could carry a command. The runtime check guards the wire shape.
  it("expresses operations as names, never commands", () => {
    const requests: OperationRequest[] = [
      { operation: "install-runtime", channel: "nightly", family: "gfx120X-all", version: { kind: "latest" }, installRoot: null },
      { operation: "update-runtime", key: "k" },
      { operation: "activate-runtime", key: "k" },
      { operation: "remove-runtime", key: "k" },
      { operation: "validate-runtime", key: "k" },
    ];

    for (const request of requests) {
      const keys = Object.keys(request);
      for (const forbidden of ["command", "argv", "args", "program", "exe", "shell", "env", "cwd"]) {
        expect(keys).not.toContain(forbidden);
      }
      expect(JSON.stringify(request)).not.toContain("driver");
    }
    expect(requests).toHaveLength(5);
  });

  it("builds an approval that carries no plan body", () => {
    const approval = approvalFor(PLAN);
    // Only an id, a digest, and the operation. The authoritative plan stays in
    // the backend, so there is nothing here worth tampering with.
    expect(Object.keys(approval).sort()).toEqual(["planDigest", "planId", "request"]);
    expect(approval.planId).toBe(PLAN.id);
    expect(approval.planDigest).toBe(PLAN.digest);
  });

  it("stops offering an expired plan", () => {
    expect(isPlanApprovable(PLAN, PLAN.expiresAtUnixMs - 1)).toBe(true);
    expect(isPlanApprovable(PLAN, PLAN.expiresAtUnixMs)).toBe(false);
    expect(isPlanApprovable(PLAN, PLAN.expiresAtUnixMs + 60_000)).toBe(false);
  });
});

describe("controller progress stream", () => {
  it("identifies exactly the terminal events", () => {
    const events: ProgressEvent[] = [
      { event: "started", operationId: "op", operation: "activate-runtime", stage: "plan" },
      { event: "stage", operationId: "op", stage: "activate", message: "Activating", count: null },
      { event: "completed", operationId: "op", message: "Done" },
    ];
    expect(events.map(isTerminal)).toEqual([false, false, true]);
    expect(events.filter(isTerminal)).toHaveLength(1);
  });

  it("treats failure and cancellation as terminal", () => {
    expect(
      isTerminal({
        event: "failed",
        operationId: "op",
        error: { code: "network", message: "offline", recoverable: true, detail: null },
      }),
    ).toBe(true);
    expect(isTerminal({ event: "cancelled", operationId: "op", message: "Stopped" })).toBe(true);
  });
});

describe("controller platform gate in the renderer", () => {
  it("offers nothing on the WSL fixture", () => {
    const wsl = golden("unsupported-wsl");
    expect(installAllowedFor(wsl.platform)).toBe(false);
    expect(offerableActions(wsl)).toEqual([]);
  });

  // The backend already filters. The renderer filters again so a backend bug
  // cannot put an Install button in front of a WSL user.
  it("refuses actions on an unsupported host even if the backend lists them", () => {
    const wsl = golden("unsupported-wsl");
    const buggy: AppSnapshot = {
      ...wsl,
      eligibleActions: ["install-runtime", "update-runtime"],
    };
    expect(offerableActions(buggy)).toEqual([]);
  });

  it("drops an action a newer backend introduced", () => {
    const healthy = golden("healthy");
    const forward: AppSnapshot = {
      ...healthy,
      eligibleActions: ["install-runtime", "unrecognised"],
    };
    expect(offerableActions(forward)).toEqual(["install-runtime"]);
  });

  it("offers real actions on a supported host", () => {
    const healthy = golden("healthy");
    expect(installAllowedFor(healthy.platform)).toBe(true);
    expect(offerableActions(healthy)).toContain("install-runtime");
    expect(activeRuntime(healthy)?.active).toBe(true);
  });

  it("never surfaces a driver action from any fixture", () => {
    for (const name of ["healthy", "setup-required", "attention", "unsupported-wsl", "partial"]) {
      for (const action of offerableActions(golden(name))) {
        expect(action).not.toContain("driver");
      }
    }
  });
});
