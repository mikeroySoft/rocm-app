// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The app under `prefers-reduced-motion: reduce`.
 *
 * wdio.a11y.conf.ts wrote `gtk-enable-animations=false` into the isolated
 * home's GTK settings before this spec's app process started, WebKitGTK maps
 * that onto the media feature, and the stylesheet must respond by parking
 * every animation and transition. The first test proves the propagation, so
 * a broken plumbing step fails loudly instead of skipping the point.
 */

import { strict as assert } from "node:assert";
import { join } from "node:path";

import { saveShot } from "../desktop";
import { clickButton, fullWindow, waitForTestId } from "../support";

describe("reduced motion", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("facts");
  });

  it("the app boots with prefers-reduced-motion: reduce", async () => {
    const reduced = await browser.execute(
      () => window.matchMedia("(prefers-reduced-motion: reduce)").matches,
    );
    const raw = process.env["ROCM_E2E_ENV"];
    const env: Record<string, string | undefined> =
      raw === undefined ? {} : (JSON.parse(raw) as Record<string, string | undefined>);
    const xdgConfig = env["XDG_CONFIG_HOME"];
    const settings =
      xdgConfig === undefined
        ? "<XDG_CONFIG_HOME>/gtk-3.0/settings.ini"
        : join(xdgConfig, "gtk-3.0", "settings.ini");
    assert.equal(
      reduced,
      true,
      `prefers-reduced-motion did not reach the page. wdio.a11y.conf.ts writes ${settings} ` +
        "(gtk-enable-animations=false) before this spec's session, and GTK reads it only " +
        "at app startup.",
    );
  });

  it("the install progress bar and controls hold still", async () => {
    await clickButton("Set up ROCm");
    await waitForTestId("folder-input");
    await clickButton("Review the changes");
    await waitForTestId("plan-steps");
    await clickButton("Install ROCm");
    await waitForTestId("progress-status");
    const sample = await browser.execute(() => {
      const bars = [];
      for (const el of document.querySelectorAll(".onboard__bar, .onboard__bar span")) {
        if (el instanceof HTMLElement) {
          const style = window.getComputedStyle(el);
          bars.push({
            label: `${el.tagName}.${el.className}`,
            name: style.animationName,
            duration: style.animationDuration,
          });
        }
      }
      const button = document.querySelector("button");
      return {
        bars,
        buttonTransition:
          button instanceof HTMLElement ? window.getComputedStyle(button).transitionDuration : null,
      };
    });
    assert.ok(sample.bars.length > 0, "no progress bar on the progress step");
    for (const bar of sample.bars) {
      assert.ok(
        bar.name === "none" || bar.duration.split(", ").every((piece) => piece === "0s"),
        `${bar.label} still animates under reduced motion ` +
          `(animation-name: ${bar.name}, animation-duration: ${bar.duration})`,
      );
    }
    assert.ok(sample.buttonTransition !== null, "no button to sample on the progress step");
    assert.ok(
      sample.buttonTransition.split(", ").every((piece) => piece === "0s"),
      `buttons still transition under reduced motion ` +
        `(transition-duration: ${sample.buttonTransition})`,
    );
    await saveShot("reduced-motion--progress");
  });

  it("waits out the install so the session ends clean", async () => {
    await waitForTestId("outcome", 60_000);
  });
});
