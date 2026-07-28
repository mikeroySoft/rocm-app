// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/** Spec-facing helpers: window routing, waiting, and the standing invariants. */

import { strict as assert } from "node:assert";

import { driverMutations, journal, mutations, paths, switchSnapshot } from "./harness";

export { journal, mutations, driverMutations, switchSnapshot, paths };

/** The compact panel the tray shows; the full window is everything else. */
const QUICK_MARKER = "window=quick";

async function handleFor(quick: boolean): Promise<string> {
  const handles = await browser.getWindowHandles();
  for (const handle of handles) {
    await browser.switchToWindow(handle);
    const url = await browser.getUrl();
    // `cargo build` alone produces a *dev* binary whose windows point at the
    // Vite dev server, because Tauri's `dev` cfg is the absence of the
    // `custom-protocol` feature that only `tauri build` passes. Every
    // selector then misses against a connection-refused page, which looks
    // like a hundred product failures instead of one build mistake.
    if (url.startsWith("http://localhost:")) {
      throw new Error(
        `the app under test is a development build (window at ${url}). ` +
          "Rebuild it with `npx tauri build --no-bundle`, not `cargo build`.",
      );
    }
    if (url.includes(QUICK_MARKER) === quick) {
      return handle;
    }
  }
  throw new Error(
    `no ${quick ? "compact" : "full"} window among ${handles.length} handle(s); ` +
      "the app declares both in tauri.conf.json",
  );
}

/** Focus the full 1024x700 window. */
export async function fullWindow(): Promise<string> {
  return handleFor(false);
}

/** Focus the 380x300 compact panel. */
export async function compactWindow(): Promise<string> {
  return handleFor(true);
}

export function testId(id: string) {
  return browser.$(`[data-testid="${id}"]`);
}

/** Wait until an element exists and is displayed, then return it. */
export async function waitForTestId(id: string, timeout = 30_000) {
  const element = browser.$(`[data-testid="${id}"]`);
  await element.waitForDisplayed({
    timeout,
    timeoutMsg: `[data-testid="${id}"] never appeared`,
  });
  return element;
}

/** Wait for a button whose visible text contains `text`, then click it. */
export async function clickButton(text: string, timeout = 30_000): Promise<void> {
  const button = browser.$(`button*=${text}`);
  await button.waitForClickable({ timeout, timeoutMsg: `button "${text}" never became clickable` });
  await button.click();
}

/** Poll until `check` is true, so a spec never sleeps a fixed amount. */
export async function until(
  what: string,
  check: () => boolean | Promise<boolean>,
  timeout = 30_000,
): Promise<void> {
  await browser.waitUntil(check, {
    timeout,
    interval: 250,
    timeoutMsg: `timed out waiting: ${what}`,
  });
}

// ---------------------------------------------------------------------------
// Standing invariants
// ---------------------------------------------------------------------------

/**
 * Nothing on this machine changed.
 *
 * Read from the stand-in CLI's journal rather than from the UI, because the UI
 * is exactly the thing under test: a screen that says "nothing happened" while
 * a process ran is the failure this catches.
 */
export function assertNoMutationYet(context: string): void {
  const changed = mutations();
  assert.deepEqual(
    changed.map((entry) => entry.argv.join(" ")),
    [],
    `${context}: the app ran a change before it was approved`,
  );
}

/** The app must never touch a kernel driver, on any path, ever. */
export function assertNoDriverMutation(): void {
  const touched = driverMutations();
  assert.deepEqual(
    touched.map((entry) => entry.argv.join(" ")),
    [],
    "the app invoked a driver command; driver mutation is out of scope for this product",
  );
}

/**
 * Every CLI invocation ran against the isolated roots.
 *
 * The sentinel side of isolation is `fresh_user_smoke.py --verify`, which the
 * config runs at the end. This is the other half: proof that the roots the
 * suite set are the roots the app actually handed down.
 */
export function assertIsolatedRoots(): void {
  const entries = journal();
  assert.ok(entries.length > 0, "the app never invoked the CLI, so nothing was proven");
  const root = paths.state();
  for (const entry of entries) {
    for (const name of [
      "ROCM_CLI_CONFIG_DIR",
      "ROCM_CLI_DATA_DIR",
      "ROCM_CLI_CACHE_DIR",
      "HOME",
      "XDG_DATA_HOME",
      "XDG_CONFIG_HOME",
      "XDG_CACHE_HOME",
    ]) {
      const value = entry.env[name];
      if (value === undefined) {
        continue;
      }
      assert.ok(
        value.startsWith(root),
        `${entry.argv.join(" ")} ran with ${name}=${value}, which is outside ${root}`,
      );
    }
  }
}

/** Argv of every change the app made, joined, in order. */
export function changes(): string[] {
  return mutations().map((entry) => entry.argv.join(" "));
}
