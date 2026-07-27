// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Renderer-side view of the controller.
 *
 * The renderer can name an operation from a closed union and nothing else.
 * There is no type here that can carry a program name, an argument list, shell
 * text, or an environment map, so "run this command" is not expressible — the
 * backend maps a typed operation to argv in Rust.
 *
 * Approvals carry only a plan id and the digest the user was shown. The
 * authoritative plan never leaves the backend, so a tampered payload is
 * rejected by comparison rather than trusted.
 */

import { invoke, isTauri } from "@tauri-apps/api/core";
import type { AppSnapshot } from "./contract";

export type Channel = "release" | "nightly";

export type VersionSelector = { kind: "latest" } | { kind: "exact"; version: string };

/** Every change the app may request. No variant targets a driver. */
export type OperationRequest =
  | { operation: "install-runtime"; channel: Channel; family: string; version: VersionSelector }
  | { operation: "update-runtime"; key: string }
  | { operation: "activate-runtime"; key: string }
  | { operation: "remove-runtime"; key: string }
  | { operation: "validate-runtime"; key: string };

export interface PlanStep {
  readonly stage: string;
  readonly summary: string;
  readonly mutating: boolean;
}

export interface ChangePlan {
  readonly id: string;
  readonly request: OperationRequest;
  readonly steps: readonly PlanStep[];
  /** Concrete version. The review screen never shows "latest". */
  readonly resolvedVersion: string | null;
  readonly createdAtUnixMs: number;
  readonly expiresAtUnixMs: number;
  readonly digest: string;
}

export interface Approval {
  readonly planId: string;
  readonly planDigest: string;
  readonly request: OperationRequest;
}

export interface ProgressCount {
  readonly current: number;
  readonly total: number | null;
  readonly unit: "bytes" | "items";
}

export interface OperationFailure {
  readonly code: string;
  readonly message: string;
  readonly recoverable: boolean;
  readonly detail: string | null;
}

export type ProgressEvent =
  | { event: "started"; operationId: string; operation: string; stage: string }
  | { event: "stage"; operationId: string; stage: string; message: string; count: ProgressCount | null }
  | { event: "completed"; operationId: string; message: string }
  | { event: "failed"; operationId: string; error: OperationFailure }
  | { event: "cancelled"; operationId: string; message: string };

/** A refusal from the backend, already written for a user. */
export interface CommandError {
  readonly code: string;
  readonly message: string;
}

export interface SnapshotResponse {
  readonly snapshot: AppSnapshot;
  /** True when a refresh was deferred behind a running mutation. */
  readonly deferred: boolean;
}

/** Terminal events end a progress stream. Exactly one arrives per operation. */
export function isTerminal(event: ProgressEvent): boolean {
  return event.event === "completed" || event.event === "failed" || event.event === "cancelled";
}

/**
 * Whether an approval may be sent for a plan.
 *
 * Checked in the renderer purely so an expired review screen disables its own
 * button; the backend re-checks and is the actual gate. A renderer-only check
 * would be security theatre, and a backend-only check would leave a live-looking
 * button that fails on click.
 */
export function isPlanApprovable(plan: ChangePlan, nowUnixMs: number): boolean {
  return nowUnixMs < plan.expiresAtUnixMs;
}

/** Build the approval for a plan the user accepted. */
export function approvalFor(plan: ChangePlan): Approval {
  return { planId: plan.id, planDigest: plan.digest, request: plan.request };
}

function requireTauri(): void {
  if (!isTauri()) {
    throw new Error("controller operations require the desktop backend");
  }
}

export async function snapshot(refresh: boolean): Promise<SnapshotResponse> {
  requireTauri();
  return await invoke<SnapshotResponse>("controller_snapshot", { refresh });
}

export async function plan(request: OperationRequest): Promise<ChangePlan> {
  requireTauri();
  return await invoke<ChangePlan>("controller_plan", { request });
}

export async function cancel(): Promise<void> {
  requireTauri();
  await invoke("controller_cancel");
}
