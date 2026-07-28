// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The per-state check the whole visual suite repeats.
 *
 * One state, photographed at both supported full-window sizes, with the four
 * standing assertions run at each: no horizontal scroll, every visible
 * control reachable and hittable, no app-authored copy violation, and every
 * status carrier saying its state in text. A screenshot alone proves a state
 * rendered; these prove it rendered *usably*.
 */

import {
  assertControlsReachable,
  assertNoHorizontalScroll,
  assertNoVerticalScroll,
  assertReachable,
  assertStatusesCarryText,
  recordCopyScan,
  resizeFull,
  saveShot,
  scanVisibleCopy,
  showQuickWindow,
} from "../desktop";
import { fullWindow, testId, until, waitForTestId } from "../support";

/** The text scale this run measures at; the directory names carry it. */
export const SCALE = process.env["ROCM_VISUAL_SCALE"] ?? "1";

export const SIZES = [
  [1024, 700],
  [1440, 900],
] as const;

async function scanState(state: string): Promise<void> {
  const hits = await scanVisibleCopy(state);
  recordCopyScan(state, hits);
  if (hits.length > 0) {
    throw new Error(`app copy violations:\n${hits.join("\n")}`);
  }
  await assertStatusesCarryText(state);
}

/** Photograph and check the current full-window state at both sizes. */
export async function fullState(state: string): Promise<void> {
  for (const [w, h] of SIZES) {
    const context = `${state} at ${w}x${h} (scale ${SCALE})`;
    await resizeFull(w, h);
    await assertNoHorizontalScroll(context);
    await assertControlsReachable(context);
    await saveShot(`${state}--full--${w}x${h}`);
  }
  await scanState(state);
}

/** Photograph and check the compact window, shown through the real tray. */
export async function quickState(
  state: string,
  options: { readonly allowVerticalScroll?: boolean } = {},
): Promise<void> {
  await showQuickWindow();
  await waitForTestId("quick-status");
  // The panel boots into "Checking…" and fills in when the tray's first
  // probe lands. The photograph — and the geometry it proves — must be of
  // the settled state; the checking placeholder is shorter than any real
  // GPU name and would pass vacuously.
  await until(`the ${state} panel to settle`, async () => {
    const status = await testId("quick-status").getAttribute("data-status");
    return status !== "checking";
  });
  await waitForTestId("quick-gpu");
  const context = `${state} compact (scale ${SCALE})`;
  await assertNoHorizontalScroll(context);
  if (options.allowVerticalScroll !== true) {
    await assertNoVerticalScroll(context);
  }
  await assertReachable('[data-testid="quick-open"]', context);
  await saveShot(`${state}--quick--380x300`);
  await scanState(`${state} compact`);
  await fullWindow();
}
