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
  assertStatusesCarryText,
  recordCopyScan,
  resizeFull,
  saveShot,
  scanVisibleCopy,
} from "../desktop";

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
