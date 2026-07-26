// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

import { invoke, isTauri } from "@tauri-apps/api/core";
import type { FixtureSnapshot, ScenarioName } from "./scenarios";
import { scenario } from "./scenarios";

/**
 * Read a health snapshot for `name`.
 *
 * Outside a Tauri webview — vitest, `vite dev` in a browser, renderer-only
 * screenshot runs — this resolves from the local fixture set instead of
 * failing. That is what makes the renderer testable without a WebView, a GPU,
 * a network, or any real user state: the fixture path touches none of them.
 *
 * Phase 2 replaces the payload with the versioned health contract; the seam
 * stays here so the renderer never learns whether a backend was present.
 */
export async function loadSnapshot(name: ScenarioName): Promise<FixtureSnapshot> {
  if (isTauri()) {
    return await invoke<FixtureSnapshot>("fixture_snapshot", { scenario: name });
  }
  return scenario(name);
}
