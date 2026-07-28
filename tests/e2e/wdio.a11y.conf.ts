// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The accessibility suite: the base desktop configuration pointed at the
 * `a11y/` specs, with one extra per-session step. The reduced-motion spec
 * needs the app to boot under `prefers-reduced-motion: reduce`, and on this
 * stack that is a GTK setting, not a browser flag.
 */

import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { REPO } from "./harness";
import { config as base } from "./wdio.conf";

/** The GTK settings file inside this run's isolated home. */
function gtkSettingsFile(): string {
  const raw = process.env["ROCM_E2E_ENV"];
  if (raw === undefined) {
    throw new Error("ROCM_E2E_ENV is not set; the base onPrepare publishes it before sessions");
  }
  const env = JSON.parse(raw) as Record<string, string | undefined>;
  const xdgConfig = env["XDG_CONFIG_HOME"];
  if (xdgConfig === undefined) {
    throw new Error("the isolated environment carries no XDG_CONFIG_HOME");
  }
  return join(xdgConfig, "gtk-3.0", "settings.ini");
}

export const config: WebdriverIO.Config = {
  ...base,
  specs: [join(REPO, "tests", "e2e", "a11y", "*.a11y.ts")],

  beforeSession(cfg, capabilities, specs, cid) {
    // GTK reads settings.ini once at app startup and each spec file boots its
    // own app process, so writing the file here is exactly early enough.
    // WebKitGTK maps `gtk-enable-animations=false` onto the page-visible
    // `prefers-reduced-motion: reduce` media feature. Every other spec runs
    // with the file absent, i.e. with animations at the platform default.
    const settings = gtkSettingsFile();
    if (specs.some((spec) => spec.includes("reduced-motion"))) {
      mkdirSync(dirname(settings), { recursive: true });
      writeFileSync(settings, "[Settings]\ngtk-enable-animations=false\n");
    } else {
      rmSync(settings, { force: true });
    }
    // The hook type is a function or an array of them; the base defines one function.
    const inherited = base.beforeSession;
    if (typeof inherited === "function") {
      return inherited(cfg, capabilities, specs, cid);
    }
    return undefined;
  },
};
