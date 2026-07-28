// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The shared axe-core runner.
 *
 * axe runs inside the page, so the library source is injected into the window
 * under test once per window, and `axe.run` is driven through the W3C
 * "Execute Async Script" endpoint (`browser.executeAsync`). WebKitWebDriver
 * has shipped that endpoint for years, while its support for awaiting a
 * promise returned through the synchronous "Execute Script" endpoint varies
 * by WebKit build; WebdriverIO v9's deprecation of `executeAsync` is a
 * client-side preference, and over the plain `webdriver` protocol the command
 * still maps straight onto the standard endpoint.
 */

import { appendFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";

import type { AxeResults, Result, RunOptions } from "axe-core";

import { shotDir } from "../desktop";

const require = createRequire(import.meta.url);

/** The axe-core source, read once per spec process. */
const AXE_SOURCE = readFileSync(require.resolve("axe-core/axe.min.js"), "utf8");

/** The bar the app is held to: WCAG 2.1 AA, which subsumes A and WCAG 2.0. */
const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];

/** The one axe-core entry point the in-page runner calls. */
interface AxeRunner {
  run(context: Document, options: RunOptions): Promise<AxeResults>;
}

/** The page window once `injectAxe` has run; axe-core attaches itself there. */
interface AxeWindow {
  readonly axe: AxeRunner;
}

/** What the in-page runner hands back: results, or why it could not run. */
interface AxeOutcome {
  readonly results?: AxeResults;
  readonly error?: string;
}

/**
 * Inject axe-core into the focused window unless it is already there. Each
 * window is its own JavaScript world, so a spec that switches to the quick
 * panel injects again; the guard makes repeat calls free.
 */
export async function injectAxe(): Promise<void> {
  const present = await browser.execute(() => "axe" in window);
  if (!present) {
    await browser.execute(AXE_SOURCE);
  }
}

/** Where this run's axe artifacts land, created on first use. */
function axeDir(): string {
  const dir = join(shotDir(), "axe");
  mkdirSync(dir, { recursive: true });
  return dir;
}

/** One violation per block: rule, impact, help, then every offending node. */
function formatViolations(violations: readonly Result[]): string {
  return violations
    .map((violation) => {
      const lines = [`${violation.id} (${violation.impact ?? "no impact"}): ${violation.help}`];
      for (const node of violation.nodes) {
        lines.push(`  target: ${node.target.flat().join(" ")}`);
        if (node.failureSummary !== undefined) {
          lines.push(`  ${node.failureSummary.replace(/\n/g, "\n  ")}`);
        }
      }
      return lines.join("\n");
    })
    .join("\n\n");
}

/** Append one state's tallies to the run's accumulating axe report. */
export function appendReport(state: string, result: AxeResults): void {
  appendFileSync(
    join(axeDir(), "report.md"),
    `- ${state}: ${String(result.passes.length)} passes, ${String(result.violations.length)} violations\n`,
  );
}

/**
 * Run axe against the focused window, write the full result JSON under
 * `<shotDir()>/axe/<state>.json`, tally it into the report, and fail on any
 * WCAG 2.1 A/AA violation.
 */
export async function checkA11y(state: string): Promise<AxeResults> {
  await injectAxe();
  const outcome = await browser.executeAsync<AxeOutcome, [string[]]>((tags, done) => {
    // `injectAxe` ran first; what a page script attached is invisible to the compiler.
    const scope = window as unknown as AxeWindow;
    void scope.axe
      .run(document, { runOnly: { type: "tag", values: tags } })
      .then((results) => {
        done({ results });
      })
      .catch((error: unknown) => {
        done({ error: String(error) });
      });
  }, TAGS);
  if (outcome.results === undefined) {
    throw new Error(`axe.run failed on "${state}": ${outcome.error ?? "no result came back"}`);
  }
  const results = outcome.results;
  writeFileSync(join(axeDir(), `${state}.json`), JSON.stringify(results, null, 2));
  appendReport(state, results);
  if (results.violations.length > 0) {
    throw new Error(
      `${state}: ${String(results.violations.length)} accessibility violation(s)\n\n` +
        formatViolations(results.violations),
    );
  }
  return results;
}
