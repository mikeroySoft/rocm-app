// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Helpers shared by the visual and accessibility suites.
 *
 * Everything here runs against the shipped release binary through the same
 * harness the functional e2e uses. The one extra capability these suites need
 * is a *mapped* compact window: a hidden Tauri window keeps a 0x0 webview
 * viewport, geometry reads all come back zero, and a WebDriver screenshot of
 * it never completes. The only product path that shows the compact window on
 * Linux is the tray menu, so that is the path used — a real click on the real
 * StatusNotifierItem menu over D-Bus, not a test-only IPC door.
 */

import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { REPO } from "./harness";
import { compactWindow, fullWindow, until } from "./support";

/** Where this run's screenshots land; the orchestrator sets the directory. */
export function shotDir(): string {
  return process.env["ROCM_VISUAL_DIR"] ?? join(REPO, "test-results", "visual", "adhoc");
}

/** Screenshot the current window as `<name>.png` under the run's shot dir. */
export async function saveShot(name: string): Promise<string> {
  const file = join(shotDir(), `${name}.png`);
  mkdirSync(dirname(file), { recursive: true });
  await browser.saveScreenshot(file);
  return file;
}

/**
 * Show the compact window the way a user does: by clicking "Quick status" in
 * the tray menu. Requires the orchestrator's StatusNotifierWatcher session
 * (`ROCM_TRAY_REGISTRY` names its registration file).
 */
export async function showQuickWindow(): Promise<void> {
  const registry = process.env["ROCM_TRAY_REGISTRY"];
  if (!registry) {
    throw new Error(
      "ROCM_TRAY_REGISTRY is not set. The visual and a11y suites must run through " +
        "scripts/ui_quality.py, which owns the session bus and the tray watcher.",
    );
  }
  execFileSync("python3", [join(REPO, "scripts", "tray_menu.py"), registry, "Quick status"], {
    stdio: "pipe",
  });
  await compactWindow();
  await until("the compact window to be mapped", async () => {
    const width = await browser.execute(() => window.innerWidth);
    return width > 0;
  });
}

/**
 * Whether the compact window is hidden.
 *
 * A window that was never mapped keeps a 0x0 viewport; one that was shown
 * and then hidden keeps its last size, and the honest signal for it is the
 * page's visibility state, which WebKit ties to the native mapping.
 */
export async function quickWindowHidden(): Promise<boolean> {
  await compactWindow();
  const state = await browser.execute(() => ({
    width: window.innerWidth,
    visibility: document.visibilityState,
  }));
  await fullWindow();
  return state.width === 0 || state.visibility === "hidden";
}

/**
 * Resize the full window and wait for the webview to agree.
 *
 * `setWindowSize` speaks physical pixels; the webview's `innerWidth` speaks
 * CSS pixels, and at a raised text scale WebKitGTK divides the two by the
 * device pixel ratio. 1024 physical at 200% is 512 CSS — that is correct,
 * not a failure to resize.
 */
export async function resizeFull(width: number, height: number): Promise<void> {
  await fullWindow();
  await browser.setWindowSize(width, height);
  await until(`the viewport to reach ${width}px wide`, async () => {
    const inner = await browser.execute(() => window.innerWidth * window.devicePixelRatio);
    return Math.abs(inner - width) <= 3;
  });
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/** The page must not scroll sideways, at any text scale. */
export async function assertNoHorizontalScroll(context: string): Promise<void> {
  const overflow = await browser.execute(() => {
    const root = document.documentElement;
    return root.scrollWidth - root.clientWidth;
  });
  if (overflow > 1) {
    throw new Error(`${context}: horizontal overflow of ${overflow}px`);
  }
}

/** The compact window is designed to fit its fixed height outright. */
export async function assertNoVerticalScroll(context: string): Promise<void> {
  const overflow = await browser.execute(() => {
    const root = document.documentElement;
    return root.scrollHeight - root.clientHeight;
  });
  if (overflow > 1) {
    throw new Error(`${context}: vertical overflow of ${overflow}px`);
  }
}

interface Reach {
  readonly missing?: boolean;
  readonly rect?: { x: number; y: number; width: number; height: number };
  readonly viewport?: { width: number; height: number };
  readonly clipped?: string;
  readonly covered?: string;
}

/**
 * A control is "not clipped" when, after scrolling to it, its whole box sits
 * inside the viewport and a hit test at its centre lands on it. Content below
 * the fold that scrolls into reach is fine; content that cannot be brought
 * fully on screen, or that something else paints over, is not.
 */
export async function assertReachable(selector: string, context: string): Promise<void> {
  const result = (await browser.execute((sel: string) => {
    const el = document.querySelector(sel);
    if (!el) {
      return { missing: true };
    }
    el.scrollIntoView({ block: "nearest", inline: "nearest" });
    const rect = el.getBoundingClientRect();
    const viewport = { width: window.innerWidth, height: window.innerHeight };
    const out: Record<string, unknown> = {
      rect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
      viewport,
    };
    if (rect.width < 1 || rect.height < 1) {
      out["clipped"] = "zero-size box";
    } else if (rect.left < -0.5 || rect.right > viewport.width + 0.5) {
      out["clipped"] =
        `horizontal: box ${Math.round(rect.left)}..${Math.round(rect.right)} in 0..${viewport.width}`;
    } else if (rect.top < -0.5 || rect.bottom > viewport.height + 0.5) {
      out["clipped"] =
        `vertical: box ${Math.round(rect.top)}..${Math.round(rect.bottom)} in 0..${viewport.height}`;
    }
    const hit = document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2);
    if (hit !== el && !el.contains(hit) && !(hit && hit.contains(el))) {
      out["covered"] = hit
        ? `covered by <${hit.tagName.toLowerCase()} class="${hit.className}">`
        : "nothing at centre";
    }
    return out;
  }, selector)) as Reach;

  if (result.missing) {
    throw new Error(`${context}: ${selector} not found`);
  }
  if (result.clipped) {
    throw new Error(`${context}: ${selector} clipped — ${result.clipped}`);
  }
  if (result.covered) {
    throw new Error(`${context}: ${selector} ${result.covered}`);
  }
}

/** No two of the named elements may overlap. */
export async function assertNoOverlap(
  selectors: readonly string[],
  context: string,
): Promise<void> {
  const boxes = (await browser.execute((sels: readonly string[]) => {
    return sels.map((sel) => {
      const el = document.querySelector(sel);
      if (!el) {
        return null;
      }
      const r = el.getBoundingClientRect();
      return { sel, x: r.x, y: r.y, w: r.width, h: r.height };
    });
  }, selectors)) as ({ sel: string; x: number; y: number; w: number; h: number } | null)[];
  for (let i = 0; i < boxes.length; i += 1) {
    for (let j = i + 1; j < boxes.length; j += 1) {
      const a = boxes[i];
      const b = boxes[j];
      if (!a || !b || a.w < 1 || a.h < 1 || b.w < 1 || b.h < 1) {
        continue;
      }
      const x = Math.max(0, Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x));
      const y = Math.max(0, Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y));
      // Sub-pixel kisses are rounding, not overlap.
      if (x > 1 && y > 1) {
        throw new Error(
          `${context}: ${a.sel} overlaps ${b.sel} by ${Math.round(x)}x${Math.round(y)}px`,
        );
      }
    }
  }
}

/**
 * Every visible control on the page can be scrolled to and hit. This is the
 * blanket form of `assertReachable`: it catches a button pushed out of the
 * viewport by long content, and a control something else paints over.
 */
export async function assertControlsReachable(context: string): Promise<void> {
  const failures = await browser.execute(() => {
    const out: string[] = [];
    const controls = document.querySelectorAll("button, a[href], input, select, summary");
    for (const el of controls) {
      if (!(el instanceof HTMLElement) || el.offsetParent === null) {
        continue;
      }
      // WebKitGTK keeps an `offsetParent` on children of a closed <details>
      // even though it paints none of them, so a hit test lands on whatever
      // sits behind. Skip content the disclosure is deliberately hiding; the
      // summary itself stays checked. (No nested <details> in this app —
      // revisit the one-level check if that ever changes.)
      if (el.tagName !== "SUMMARY" && el.closest("details:not([open])") !== null) {
        continue;
      }
      el.scrollIntoView({ block: "center", inline: "nearest" });
      const rect = el.getBoundingClientRect();
      const name = `<${el.tagName.toLowerCase()}> "${el.textContent.trim().slice(0, 40)}"`;
      if (rect.width < 1 || rect.height < 1) {
        out.push(`${name}: zero-size box`);
        continue;
      }
      if (rect.left < -0.5 || rect.right > window.innerWidth + 0.5) {
        out.push(
          `${name}: horizontally clipped (${Math.round(rect.left)}..${Math.round(rect.right)} in 0..${window.innerWidth})`,
        );
        continue;
      }
      if (rect.top < -0.5 || rect.bottom > window.innerHeight + 0.5) {
        out.push(`${name}: vertically clipped after scrollIntoView`);
        continue;
      }
      const hit = document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2);
      if (hit !== el && !el.contains(hit) && !(hit && hit.contains(el))) {
        out.push(`${name}: covered by <${hit ? hit.tagName.toLowerCase() : "nothing"}>`);
      }
    }
    window.scrollTo(0, 0);
    return out;
  });
  if (failures.length > 0) {
    throw new Error(`${context}: controls not reachable:\n${failures.join("\n")}`);
  }
}

// ---------------------------------------------------------------------------
// Copy and status semantics
// ---------------------------------------------------------------------------

/**
 * App-authored copy the primary surfaces must never show. Content the user or
 * the machine produced (audit summaries, chosen folders) is fact, not copy —
 * the scan runs on states whose fixtures keep the two apart, and the runtime
 * key pattern is scoped to the shapes this product actually generates.
 */
const FORBIDDEN_COPY: readonly { name: string; pattern: RegExp }[] = [
  { name: "placeholder text", pattern: /\b(?:TODO|FIXME|lorem ipsum|placeholder|xxx)\b/i },
  {
    name: "raw backend status token",
    pattern:
      /\b(?:setup-required|update-available|not-applicable|not-checked|unrecognised-family|first-run|in-progress|gpu-absent|driver-not-detected)\b/,
  },
  { name: "runtime key", pattern: /\b(?:nightly|stable)-(?:wheel|tar)-gfx[0-9a-z]+/i },
  { name: "command syntax", pattern: /\brocm\s+(?:install|runtimes|update|diagnose|logs|app-)\S*/ },
];

/**
 * Scan what a user can currently read.
 *
 * Open disclosures are shut for the duration of the measurement: raw
 * identifiers are allowed to live behind them, and a state photographed
 * *after* the user opened one must not turn that permission into a failure.
 * The primary surface is what shows before any opt-in.
 */
export async function scanVisibleCopy(state: string): Promise<string[]> {
  const text = await browser.execute(() => {
    const opened = [...document.querySelectorAll("details[open]")];
    for (const el of opened) {
      el.removeAttribute("open");
    }
    const visible = document.body.innerText;
    for (const el of opened) {
      el.setAttribute("open", "");
    }
    return visible;
  });
  const hits: string[] = [];
  for (const { name, pattern } of FORBIDDEN_COPY) {
    const match = pattern.exec(text);
    if (match) {
      hits.push(`${state}: ${name}: "${match[0]}"`);
    }
  }
  return hits;
}

/** Every status carrier must say its state in text, not colour alone. */
export async function assertStatusesCarryText(context: string): Promise<void> {
  const bare = await browser.execute(() => {
    const carriers = document.querySelectorAll(
      "[data-status],[data-value],[data-check],[data-severity],[data-state],[data-confidence],[data-kind],[data-code]",
    );
    const empty: string[] = [];
    for (const el of carriers) {
      if (!(el instanceof HTMLElement) || el.offsetParent === null) {
        continue;
      }
      if (el.closest("details:not([open])") !== null) {
        continue;
      }
      if (el.textContent.trim().length === 0) {
        empty.push(el.outerHTML.slice(0, 120));
      }
    }
    return empty;
  });
  if (bare.length > 0) {
    throw new Error(`${context}: status carriers with no text:\n${bare.join("\n")}`);
  }
}

/** Append one state's copy-scan outcome to the run's report file. */
export function recordCopyScan(state: string, hits: readonly string[]): void {
  const file = join(shotDir(), "copy-scan.txt");
  mkdirSync(dirname(file), { recursive: true });
  const line = hits.length === 0 ? `${state}: clean\n` : `${hits.join("\n")}\n`;
  writeFileSync(file, line, { flag: "a" });
}
