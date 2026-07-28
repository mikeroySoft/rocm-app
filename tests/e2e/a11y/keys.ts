// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Keyboard driving shared by the keyboard specs, with a transcript.
 *
 * Every key sent lands in `<shotDir()>/keyboard-transcript.md` together with
 * the element that held focus afterwards, so a failed ordering assertion can
 * be read back as the walk a keyboard user actually took. Both keyboard
 * specs append to the same file; the orchestrator hands each run a fresh
 * directory.
 */

import { appendFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";

import { Key } from "webdriverio";

import { shotDir } from "../desktop";

/** The keys these specs send, by the name the transcript shows. */
const NAMED = {
  Tab: [Key.Tab],
  "Shift+Tab": [Key.Shift, Key.Tab],
  Enter: [Key.Enter],
  Space: [Key.Space],
  Escape: [Key.Escape],
  ArrowDown: [Key.ArrowDown],
} satisfies Record<string, string[]>;

export type KeyName = keyof typeof NAMED;

/** What held focus after a key: enough identity to assert on and to log. */
export interface ActiveElement {
  readonly tag: string;
  readonly testid: string | null;
  readonly text: string;
}

/** The element that holds focus right now, reduced to its transcript identity. */
export async function active(): Promise<ActiveElement> {
  return browser.execute(() => {
    const el = document.activeElement;
    if (!(el instanceof HTMLElement)) {
      return { tag: el === null ? "NONE" : el.tagName, testid: null, text: "" };
    }
    return {
      tag: el.tagName,
      testid: el.getAttribute("data-testid"),
      text: el.textContent.replace(/\s+/g, " ").trim().slice(0, 60),
    };
  });
}

/** One human-readable identity: data-testid, else a text slice, else the tag. */
export function describeActive(el: ActiveElement): string {
  if (el.testid !== null) {
    return `${el.tag} [${el.testid}]`;
  }
  if (el.text !== "") {
    return `${el.tag} "${el.text}"`;
  }
  return el.tag;
}

/** Append one free-form line to the transcript (headers, window switches). */
export function note(line: string): void {
  const file = join(shotDir(), "keyboard-transcript.md");
  mkdirSync(dirname(file), { recursive: true });
  appendFileSync(file, `${line}\n`);
}

/** Send one named key, log it with the element focused afterwards, return that element. */
export async function press(key: KeyName): Promise<ActiveElement> {
  await browser.keys(NAMED[key]);
  const el = await active();
  note(`- ${key} → ${describeActive(el)}`);
  return el;
}

/** Press `key` until `match` accepts the focused element, bounded so a loop cannot hang. */
export async function pressUntil(
  key: KeyName,
  what: string,
  match: (el: ActiveElement) => boolean,
  limit = 25,
): Promise<ActiveElement> {
  for (let step = 0; step < limit; step += 1) {
    const el = await press(key);
    if (match(el)) {
      return el;
    }
  }
  throw new Error(`${what} was not reached within ${String(limit)} ${key} presses`);
}
