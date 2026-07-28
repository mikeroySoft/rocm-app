// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The desktop end-to-end suite.
 *
 * # Retries cannot hide a failure
 *
 * `specFileRetries` reruns a whole spec file in a new session and
 * `mochaOpts.retries` reruns a test body; either can turn a repeated
 * functional failure green, so both are zero. `connectionRetryCount` retries
 * only a failed WebDriver HTTP request and cannot rerun a test body, so it
 * carries the one bounded allowance in the suite — a driver transport hiccup
 * is not a product failure, and one is worth surviving.
 * `tests/e2e/wdio.selftest.conf.ts` proves the bound holds by failing on
 * purpose.
 *
 * # One session per spec file
 *
 * The landing surface is decided by the app's first snapshot read, so the
 * machine state has to be in place before the process starts. `beforeSession`
 * writes it; each spec file therefore gets its own boot and its own scenario.
 */

import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { homedir, platform } from "node:os";
import { join } from "node:path";

import {
  captureArtifacts,
  paths,
  prepareIsolation,
  REPO,
  scenarioForSpec,
  stage,
  startDriver,
  startXvfb,
  stop,
  verifyIsolation,
  writeScenario,
  type Started,
} from "./harness";

const PORT = Number(process.env["ROCM_E2E_PORT"] ?? 4444);
const DISPLAY = process.env["ROCM_E2E_DISPLAY"] ?? ":98";
const WINDOWS = platform() === "win32";

/** A Mocha failure carries an unknown; take its stack when it has one. */
function describeError(error: unknown): string {
  if (error instanceof Error) {
    return error.stack ?? error.message;
  }
  return error === undefined || error === null ? "no error" : JSON.stringify(error);
}

let driver: Started | null = null;
let xvfb: Started | null = null;

export const config: WebdriverIO.Config = {
  runner: "local",
  hostname: "127.0.0.1",
  port: PORT,
  path: "/",
  automationProtocol: "webdriver",

  specs: [join(REPO, "tests", "e2e", "specs", "*.e2e.ts")],
  maxInstances: 1,

  capabilities: [
    {
      maxInstances: 1,
      // tauri-driver reads this from `capabilities.alwaysMatch` only. It
      // accepts `application`, `args`, and (Windows-only) `webviewOptions`,
      // which it forwards verbatim as `ms:edgeOptions.webviewOptions`. The
      // environment the app needs is set on the tauri-driver process instead;
      // see harness.ts.
      "tauri:options": { application: "", args: [] },
    } as WebdriverIO.Capabilities,
  ],

  logLevel: "warn",
  framework: "mocha",
  reporters: ["spec"],
  mochaOpts: { ui: "bdd", timeout: 120_000, retries: 0 },

  specFileRetries: 0,
  connectionRetryCount: 1,
  connectionRetryTimeout: 120_000,
  waitforTimeout: 20_000,

  // WebdriverIO ships its own Xvfb manager, and on a headless Linux host it
  // wraps each worker in `xvfb-run --auto-servernum -- node …`. That wrapper
  // re-execs, the worker loses the IPC file descriptor its parent handed it,
  // and every spec dies on `process.send: EINVAL` before a test runs. This
  // suite starts and owns its own display instead, which it needs anyway to
  // keep the artifact screenshots at one known size.
  autoXvfb: false,

  async onPrepare(_config, capabilities) {
    const runId = process.env["ROCM_E2E_RUN_ID"] ?? "local";
    const root = join(REPO, "test-results", "e2e", runId);
    rmSync(root, { recursive: true, force: true });
    mkdirSync(join(root, "logs"), { recursive: true });
    mkdirSync(join(root, "artifacts"), { recursive: true });
    process.env["ROCM_E2E_ROOT"] = root;
    process.env["ROCM_E2E_REAL_HOME"] = homedir();

    const stateRoot = join(root, "state");
    const env = prepareIsolation(stateRoot);
    stage(stateRoot);
    mkdirSync(join(stateRoot, "fixture"), { recursive: true });

    if (!WINDOWS) {
      if (!process.env["DISPLAY"]) {
        xvfb = startXvfb(DISPLAY, join(root, "logs"));
        if (!xvfb) {
          throw new Error(
            "no DISPLAY and no Xvfb; install xvfb or run with DISPLAY set to a real one",
          );
        }
        process.env["DISPLAY"] = DISPLAY;
      }
      // XWayland is fine here; native Wayland is covered separately by
      // scripts/wayland_desktop_check.py, which WebKitWebDriver cannot drive.
      env["DISPLAY"] = process.env["DISPLAY"];
      env["GDK_BACKEND"] = "x11";
      // Reusing an external X display needs its auth cookie. The isolated
      // env replaces HOME, so the ~/.Xauthority default silently vanishes and
      // GTK dies with "Authorization required" — forward the real cookie
      // path. The harness's own Xvfb runs with no auth and needs nothing.
      const xauthority = process.env["XAUTHORITY"] ?? join(homedir(), ".Xauthority");
      if (existsSync(xauthority)) {
        env["XAUTHORITY"] = xauthority;
      }
    }
    env["ROCM_FIXTURE_DIR"] = join(stateRoot, "fixture");
    env["ROCM_FIXTURE_JOURNAL"] = join(stateRoot, "fixture-journal.jsonl");
    process.env["ROCM_E2E_ENV"] = JSON.stringify(env);

    // WebKitGTK folds fractional font-DPI scale into the device pixel ratio;
    // this is the one host-level text-scale mechanism the webview honours —
    // measured, gtk-xft-dpi does nothing.
    const visualScale = process.env["ROCM_VISUAL_SCALE"];
    if (visualScale && visualScale !== "1" && visualScale !== "1.0") {
      env["GDK_DPI_SCALE"] = visualScale;
    }
    driver = await startDriver(env, join(root, "logs"), PORT);

    const application = join(stateRoot, "bin", WINDOWS ? "rocm-app.exe" : "rocm-app");
    // Windows: Tauri puts the WebView2 user data folder under
    // %LOCALAPPDATA%\<identifier>\EBWebView, and the isolation above moves
    // LOCALAPPDATA into the sandbox. msedgedriver watches its *default*
    // location (beside the exe) for DevToolsActivePort, so without this hint
    // the app launches fine while every session dies with "Microsoft Edge
    // failed to start: crashed" — the driver is watching an empty folder.
    const webviewOptions = WINDOWS
      ? {
          userDataFolder: join(
            env["LOCALAPPDATA"] ?? join(stateRoot, "localappdata"),
            "com.mikeroysoft.rocm-app",
            "EBWebView",
          ),
        }
      : undefined;
    if (webviewOptions) {
      mkdirSync(webviewOptions.userDataFolder, { recursive: true });
    }
    for (const capability of capabilities as WebdriverIO.Capabilities[]) {
      const options = (
        capability as unknown as Record<
          string,
          { application: string; webviewOptions?: { userDataFolder: string } } | undefined
        >
      )["tauri:options"];
      if (options) {
        options.application = application;
        if (webviewOptions) {
          options.webviewOptions = webviewOptions;
        }
      }
    }
  },

  beforeSession(_config, _capabilities, specs) {
    const scenario = scenarioForSpec(specs[0] ?? "");
    writeScenario(scenario);
    process.env["ROCM_E2E_SCENARIO"] = scenario;
  },

  async afterTest(test, _context, result) {
    if (result.passed) {
      return;
    }
    const label = `${test.parent} ${test.title}`;
    const dir = captureArtifacts(label, {
      "failure.txt": `${label}\n\n${describeError(result.error)}\n`,
      "page-source.html": await browser.getPageSource().catch((error) => String(error)),
      "url.txt": await browser.getUrl().catch((error) => String(error)),
    });
    await browser.saveScreenshot(join(dir, "screenshot.png")).catch(() => undefined);
    // WebKitWebDriver does not expose the Chromium log endpoint, so absence is
    // recorded rather than silently skipped.
    const logs = await (browser as unknown as { getLogs?: (kind: string) => Promise<unknown> })
      .getLogs?.("browser")
      .catch((error: unknown) => ({ unavailable: String(error) }));
    writeFileSync(
      join(dir, "browser-log.json"),
      JSON.stringify(
        logs ?? { unavailable: "this WebDriver session exposes no browser log" },
        null,
        2,
      ),
    );
  },

  async onComplete() {
    await stop(driver);
    await stop(xvfb);
    driver = null;
    xvfb = null;
    const root = process.env["ROCM_E2E_ROOT"];
    if (!root || !existsSync(root)) {
      return;
    }
    // The last word on isolation: sentinels intact, and no marker anywhere in
    // what the run is about to upload.
    const report = verifyIsolation(join(root, "state"), [
      join(root, "artifacts"),
      join(root, "logs"),
    ]);
    writeFileSync(join(root, "isolation-verify.txt"), report);
    process.stdout.write(report.endsWith("\n") ? report : `${report}\n`);
  },
};

// `paths` is re-exported so specs can import one module.
export { paths };
